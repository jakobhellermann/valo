use std::os::unix::process::CommandExt;
use std::process::Command;

use anyhow::Result;

use crate::paths;

/// Apply the runtime environment expected by valo to a target process.
pub fn configure_target(cmd: &mut Command, gpu: bool) -> Result<()> {
    // Set env vars for the child to nudge it onto a deterministic, software rendering path.
    // Taken from https://github.com/clementgallet/libTAS/blob/86b04134d1a0dc918c0d72f09d0a6441d42f99ec/src/program/GameThread.cpp#L33

    // Disable vsync
    cmd.env("vblank_mode", "0");
    // Mesa's X11 swapchain adds images for the non-blocking present mode the lib substitutes;
    // games may index per-image state with the count they asked for.
    cmd.env("vk_x11_strict_image_count", "true");
    // The lib forces the Mesa GLX vendor once its libraries are loaded.
    cmd.env_remove("__GLX_VENDOR_LIBRARY_NAME");
    // TODO: there's an upstream bug often triggered by swapchain reset after restore.
    cmd.env("DISABLE_VK_LAYER_VALVE_steam_overlay_1", "1");

    // Force llvmpipe for savestate compatibility
    cmd.env("LIBGL_ALWAYS_SOFTWARE", "1");
    // Disable Wayland since we better support X11
    cmd.env("WAYLAND_DISPLAY", "");
    // Deterministic time zone.
    cmd.env("TZ", "UTC0");
    // Old Mono keeps file handle share info in /dev/shm segments that a restore rewinds only
    // partially; private tables rewind with the process.
    // TODO: save shared file mappings whole, not only their pages mapped at the checkpoint
    cmd.env("MONO_DISABLE_SHM", "1");

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
        // WSLg's XWayland has no DRI3, which the Vulkan X11 WSI needs for presentation.
        if crate::is_wsl_gpu() {
            let mut wsi_debug = std::env::var("MESA_VK_WSI_DEBUG").unwrap_or_default();
            if !wsi_debug.split(',').any(|flag| flag == "sw") {
                if !wsi_debug.is_empty() {
                    wsi_debug.push(',');
                }
                wsi_debug.push_str("sw");
            }
            cmd.env("MESA_VK_WSI_DEBUG", wsi_debug);

            if let Some(zink) = crate::resolve_zink_library_path()? {
                cmd.env("VALO_ZINK_LIBRARY_PATH", zink);
            }
        }
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

    Ok(())
}
