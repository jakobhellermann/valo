// TODO(ai-review): generated, not yet audited
//! Applies `valo_launch::setup_lib` in the target.

use std::ffi::{CStr, CString, c_char, c_int};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::Ordering;

use crate::determinism::time::CLOCK;
use crate::global_state::GlobalState;
use crate::{Result, info};

static ARGS: OnceLock<&'static [&'static str]> = OnceLock::new();

/// Applies the environment; the arguments wait for the process start.
pub(crate) fn init(exe: &Path) -> Result<()> {
    let gpu = GlobalState::get().use_gpu.load(Ordering::Relaxed);
    let setup = valo_launch::setup_lib::setup_lib(exe, gpu)
        .map_err(|e| std::io::Error::other(format!("engine setup: {e:#}")))?;
    for &(key, value) in &setup.env {
        // SAFETY: init_array code runs before main, single-threaded, before
        // any other code of this process can read the environment.
        unsafe {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
    let zink_libraries = match setup.zink {
        true => preload_zink()?,
        false => Vec::new(),
    };
    CLOCK.set_frame_waits_pass_time(setup.frame_waits_pass_time);
    info!(
        Init,
        "engine setup for {}: engine {:?}, zink {} {:?}, env {:?}, args {:?}, frame waits pass time: {}",
        exe.display(),
        setup.engine,
        setup.zink,
        zink_libraries,
        setup.env,
        setup.args,
        setup.frame_waits_pass_time
    );
    ARGS.set(setup.args)
        .expect("engine setup initialized twice");
    Ok(())
}

/// Loads libgallium and libGLX_mesa from `VALO_ZINK_LIBRARY_PATH` by path. glvnd later opens
/// the vendor library by soname and gets the loaded one.
fn preload_zink() -> Result<Vec<PathBuf>> {
    let Some(path) = std::env::var_os("VALO_ZINK_LIBRARY_PATH").filter(|v| !v.is_empty()) else {
        return Ok(Vec::new());
    };
    let mut gallium = Vec::new();
    let mut glx = Vec::new();
    for dir in std::env::split_paths(&path) {
        let entries = std::fs::read_dir(&dir).map_err(|e| {
            std::io::Error::other(format!("read zink library dir {}: {e}", dir.display()))
        })?;
        for entry in entries {
            let path = entry?.path();
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            if name.starts_with("libgallium-") && name.ends_with(".so") {
                gallium.push(path);
            } else if name == "libGLX_mesa.so.0" {
                glx.push(path);
            }
        }
    }
    if glx.len() != 1 || gallium.len() != 1 {
        return Err(std::io::Error::other(format!(
            "VALO_ZINK_LIBRARY_PATH {path:?} needs one libgallium and one libGLX_mesa.so.0, found {gallium:?} and {glx:?}"
        ))
        .into());
    }
    // libGLX_mesa needs libgallium, which it may not find on its own
    let libraries = [gallium.remove(0), glx.remove(0)];
    for library in &libraries {
        let c_path = CString::new(library.as_os_str().as_encoded_bytes())
            .map_err(|e| std::io::Error::other(format!("{}: {e}", library.display())))?;
        // SAFETY: c_path is a NUL-terminated path; init_array code runs single-threaded.
        let handle = unsafe { libc::dlopen(c_path.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
        if handle.is_null() {
            // SAFETY: dlerror returns a NUL-terminated string after a failed dlopen.
            let err = unsafe { CStr::from_ptr(libc::dlerror()) }
                .to_string_lossy()
                .into_owned();
            return Err(std::io::Error::other(format!("load {}: {err}", library.display())).into());
        }
    }
    Ok(libraries.into())
}

/// Appends the engine arguments missing from `argv`; the new argv is leaked.
///
/// # Safety
/// `argv` must point to `argc` NUL-terminated strings.
pub(crate) unsafe fn extend_argv(argc: c_int, argv: *mut *mut c_char) -> (c_int, *mut *mut c_char) {
    let Some(args) = ARGS.get().filter(|args| !args.is_empty()) else {
        return (argc, argv);
    };
    // SAFETY: `argv` holds `argc` entries per this fn's contract.
    let mut new: Vec<*mut c_char> =
        unsafe { std::slice::from_raw_parts(argv, argc as usize) }.to_vec();
    for arg in args.iter() {
        // SAFETY: each entry is a NUL-terminated string per this fn's contract.
        let present = new
            .iter()
            .any(|&existing| unsafe { CStr::from_ptr(existing) }.to_bytes() == arg.as_bytes());
        if !present {
            new.push(CString::new(*arg).unwrap().into_raw());
        }
    }
    let new_argc = new.len() as c_int;
    new.push(std::ptr::null_mut());
    (new_argc, new.leak().as_mut_ptr())
}
