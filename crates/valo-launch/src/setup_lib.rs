// TODO(ai-review): generated, not yet audited
//! Engine-dependent setup, applied by libvalo inside the target, where the real executable is known.

use std::path::Path;

use anyhow::{Context, Result};

use crate::unity;

pub struct LibSetup {
    pub engine: Option<Engine>,
    /// Whether OpenGL runs on zink, whose libraries load from `VALO_ZINK_LIBRARY_PATH` if set.
    pub zink: bool,
    /// Applied before `main`; `None` removes the variable.
    pub env: Vec<(&'static str, Option<&'static str>)>,
    /// Appended to the target's argv unless already present.
    pub args: &'static [&'static str],
    /// Whether the frame thread's sleeps and timed-out waits pass virtual time.
    pub frame_waits_pass_time: bool,
}

pub fn setup_lib(exe: &Path, gpu: bool) -> Result<LibSetup> {
    let engine = autodetect_engine(exe)?;

    let require_zink = gpu && matches!(engine, Some(Engine::UnityGl));
    let zink_requested =
        std::env::var_os("MESA_LOADER_DRIVER_OVERRIDE").is_some_and(|v| v == "zink");

    // Not set by the launcher: glvnd's constructor loads the named vendor, ahead of the zink preload.
    let mut env = vec![("__GLX_VENDOR_LIBRARY_NAME", Some("mesa"))];
    // Under forced software GL, zink requests a CPU Vulkan device the guest ICD lacks.
    env.extend_from_slice(match (require_zink, zink_requested, crate::is_wsl_gpu()) {
        // WSLg's XWayland has no DRI3, which GLX requires for zink unless kopper may present
        // without it. dozen lacks transform feedback, so zink offers only OpenGL 2.1 on its own.
        // TODO: drop the version override once dozen implements VK_EXT_transform_feedback
        (true, _, true) => &[
            ("MESA_LOADER_DRIVER_OVERRIDE", Some("zink")),
            ("LIBGL_ALWAYS_SOFTWARE", None),
            ("LIBGL_KOPPER_DRI2", Some("1")),
            ("MESA_GL_VERSION_OVERRIDE", Some("3.3")),
            ("MESA_GLSL_VERSION_OVERRIDE", Some("330")),
        ],
        (true, _, false) => &[
            ("MESA_LOADER_DRIVER_OVERRIDE", Some("zink")),
            ("LIBGL_ALWAYS_SOFTWARE", None),
        ],
        (false, true, _) => &[("LIBGL_ALWAYS_SOFTWARE", None)],
        (false, false, _) => &[],
    });

    let args: &'static [_] = match (engine, gpu) {
        (None, _) => &[],
        (Some(Engine::UnityVulkan), true) => &["-force-gfx-direct", "-force-vulkan"],
        (Some(Engine::UnityGl), true) => &["-force-gfx-direct", "-force-glcore"],
        (Some(_), false) => &["-force-gfx-direct", "-force-opengl"],
    };

    // Unity polls background work with frame-thread sleeps (5.4: its preload queue, 10 ms); passing
    // that time races the clock ahead of the worker (libTAS never advances time on Unity sleeps).
    let frame_waits_pass_time = engine.is_none();

    Ok(LibSetup {
        engine,
        zink: require_zink,
        env,
        args,
        frame_waits_pass_time,
    })
}

#[derive(Clone, Copy, Debug)]
pub enum Engine {
    /// Unity with native Vulkan support
    UnityVulkan,
    /// Unity without Vulkan support (pre-5.6)
    UnityGl,
}

fn autodetect_engine(program: &Path) -> Result<Option<Engine>> {
    let Some(data_dir) = unity::data_dir(program) else {
        return Ok(None);
    };
    let version = unity::version(&data_dir).context("read the unity version")?;
    let (major, minor) =
        major_minor(&version).with_context(|| format!("unrecognized unity version {version}"))?;
    Ok(Some(match (major, minor) >= (5, 6) {
        true => Engine::UnityVulkan,
        false => Engine::UnityGl,
    }))
}

/// Extracts (major, minor) from a version like "5.4.3f1".
fn major_minor(version: &str) -> Option<(u32, u32)> {
    let mut parts = version.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    Some((major, minor))
}
