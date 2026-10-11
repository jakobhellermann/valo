use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;

use anyhow::Result;

use crate::paths;

#[derive(Debug)]
enum Engine {
    /// Unity with native Vulkan support: the guest ICD receives its Vulkan
    /// calls directly.
    UnityVulkan,
    /// Unity without Vulkan support (pre-5.6): GL is the only backend, so a
    /// GPU run routes GL through zink onto the guest ICD.
    UnityGl,
}

fn autodetect_engine(program: &Path) -> Option<Engine> {
    let dir = program.parent()?;
    let data_dir = dir.join(format!(
        "{}_Data",
        program.file_stem()?.to_str().expect("non-utf8 game binary")
    ));

    if !data_dir.exists() {
        return None;
    }

    let version = unity_version(&data_dir);
    let vulkan_capable = version
        .as_deref()
        .and_then(parse_unity_version)
        .map(|(major, minor)| (major, minor) >= (5, 6))
        .unwrap_or(true);
    Some(if vulkan_capable {
        Engine::UnityVulkan
    } else {
        Engine::UnityGl
    })
}

/// The engine version string (e.g. "5.4.3f1") embedded near the start of
/// globalgamemanagers.
fn unity_version(data_dir: &Path) -> Option<String> {
    let bytes = std::fs::read(data_dir.join("globalgamemanagers")).ok()?;
    let head = &bytes[..bytes.len().min(4096)];
    for i in 0..head.len() {
        if let Some(v) = version_at(&head[i..]) {
            return Some(std::str::from_utf8(v).ok()?.to_string());
        }
    }
    None
}

/// Matches `digits.digits.digits[f|p]digits` at the start of `b` and
/// returns the matched slice.
fn version_at(b: &[u8]) -> Option<&[u8]> {
    let digits = |i: &mut usize| {
        let start = *i;
        while *i < b.len() && b[*i].is_ascii_digit() {
            *i += 1;
        }
        *i > start
    };
    let mut i = 0;
    let at = |i: usize| b.get(i).copied();
    if !digits(&mut i) || at(i) != Some(b'.') {
        return None;
    }
    i += 1;
    if !digits(&mut i) || at(i) != Some(b'.') {
        return None;
    }
    i += 1;
    if !digits(&mut i) || !matches!(at(i), Some(b'f') | Some(b'p')) {
        return None;
    }
    i += 1;
    if !digits(&mut i) {
        return None;
    }
    Some(&b[..i])
}

/// Extracts (major, minor) from a version like "5.4.3f1".
fn parse_unity_version(version: &str) -> Option<(u32, u32)> {
    let mut parts = version.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    if !parts.next()?.contains(['f', 'p']) {
        return None;
    }
    Some((major, minor))
}

/// Apply the runtime environment expected by valo to a target process.
pub fn configure_target(cmd: &mut Command, gpu: bool) -> Result<()> {
    let engine = autodetect_engine(Path::new(cmd.get_program()));

    // Set env vars for the child to nudge it onto a deterministic, software rendering path.
    // Taken from https://github.com/clementgallet/libTAS/blob/86b04134d1a0dc918c0d72f09d0a6441d42f99ec/src/program/GameThread.cpp#L33

    // Disable vsync
    cmd.env("vblank_mode", "0");
    // Force Mesa
    cmd.env("__GLX_VENDOR_LIBRARY_NAME", "mesa");

    // A GPU run on an engine without Vulkan support (pre-5.6 Unity) routes
    // GL through zink onto the guest ICD instead of llvmpipe.
    if gpu && matches!(engine, Some(Engine::UnityGl)) {
        cmd.env("MESA_LOADER_DRIVER_OVERRIDE", "zink");
    }

    // Force llvmpipe for savestate compatibility, except when the target
    // is set up for zink (via the command's own environment or the
    // inherited one): the software GL path makes zink request a CPU Vulkan
    // device, which the gfxstream guest ICD does not provide. A zink run
    // renders deterministically through the guest ICD instead.
    let zink = std::ffi::OsStr::new("zink");
    let zink_forced = cmd
        .get_envs()
        .any(|(k, v)| k == "MESA_LOADER_DRIVER_OVERRIDE" && v == Some(zink))
        || std::env::var_os("MESA_LOADER_DRIVER_OVERRIDE").as_deref() == Some(zink);
    if !zink_forced {
        cmd.env("LIBGL_ALWAYS_SOFTWARE", "1");
    }
    // Disable Wayland since we better support X11
    cmd.env("WAYLAND_DISPLAY", "");
    // Deterministic time zone.
    cmd.env("TZ", "UTC0");

    // Everything the target writes lands under one per-run home.
    cmd.env("VALO_HOME", paths::valo_home());

    // Inherit the home lock fd
    if let Some(fd) = paths::lock_fd() {
        // SAFETY: after fork only fcntl is called
        unsafe {
            cmd.pre_exec(move || {
                if libc::fcntl(fd, libc::F_SETFD, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }

    // https://github.com/clementgallet/libTAS/blob/32245db8e35ab8f2389bdb781e81ab5e5b92c4db/src/program/GameThread.cpp#L96-L102
    // Prevent glibc from initializing rseq for each thread. TODO: implement rseq properly
    cmd.env("GLIBC_TUNABLES", "glibc.pthread.rseq=0");

    if gpu {
        cmd.env("VIRTGPU_KUMQUAT", "1");
        if let Some(icd) = crate::resolve_gfxstream_icd()? {
            cmd.env("VK_DRIVER_FILES", icd);
            cmd.env_remove("VK_LOADER_DRIVERS_SELECT");
        } else {
            cmd.env_remove("VK_DRIVER_FILES");
            cmd.env("VK_LOADER_DRIVERS_SELECT", "gfxstream_vk_icd*");
        }
    } else {
        cmd.env_remove("VK_DRIVER_FILES");
        cmd.env("VK_LOADER_DRIVERS_SELECT", "lvp_icd*");
    }

    if let Some(engine) = engine {
        // -force-vulkan is inert on a GL-only Unity: force the GL backend
        // explicitly, the zink override above routes it through the guest
        // ICD.
        let force_graphics_api = match (&engine, gpu) {
            (Engine::UnityVulkan, true) => "-force-vulkan",
            (Engine::UnityGl, true) => "-force-glcore",
            (_, false) => "-force-opengl",
        };
        let force_render_main_thread = "-force-gfx-direct";
        ensure_cmd_argument(cmd, force_render_main_thread);
        ensure_cmd_argument(cmd, force_graphics_api);
        eprintln!(
            "[valo-launch] detected unity game ({engine:?}), adding {force_render_main_thread} {force_graphics_api}"
        )
    }
    Ok(())
}

fn ensure_cmd_argument(cmd: &mut Command, arg: &str) {
    if !cmd.get_args().any(|existing| existing == arg) {
        cmd.arg(arg);
    }
}
