use std::process::Command;

/// Apply the runtime environment expected by valo to a target process.
pub fn configure_target(cmd: &mut Command) {
    // Set env vars for the child to nudge it onto a deterministic, software rendering path.
    // Taken from https://github.com/clementgallet/libTAS/blob/86b04134d1a0dc918c0d72f09d0a6441d42f99ec/src/program/GameThread.cpp#L33

    // Disable vsync
    cmd.env("vblank_mode", "0");
    // Force Mesa
    cmd.env("__GLX_VENDOR_LIBRARY_NAME", "mesa");
    // Force llvmpipe for savestate compatibility
    cmd.env("LIBGL_ALWAYS_SOFTWARE", "1");
    // Disable Wayland since we better support X11
    cmd.env("WAYLAND_DISPLAY", "");
    // Deterministic time zone.
    cmd.env("TZ", "UTC0");

    // https://github.com/clementgallet/libTAS/blob/32245db8e35ab8f2389bdb781e81ab5e5b92c4db/src/program/GameThread.cpp#L96-L102
    // Prevent glibc from initializing rseq for each thread. TODO: implement rseq properly
    cmd.env("GLIBC_TUNABLES", "glibc.pthread.rseq=0");

    // TODO: opengl_soft config from libTAS:
    // - VK_DRIVER_FILES -> Lavapipe ICDs
    // - VDPAU_DRIVER=va_gl, VDPAU_QUIRKS=AvoidVA
}
