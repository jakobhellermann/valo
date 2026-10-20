// TODO(ai-review): generated, not yet audited
//! Applies `valo_launch::setup_lib` in the target.

use std::ffi::{CStr, CString, c_char, c_int};
use std::path::Path;
use std::sync::OnceLock;
use std::sync::atomic::Ordering;

use crate::global_state::GlobalState;
use crate::{Result, info};

static ARGS: OnceLock<&'static [&'static str]> = OnceLock::new();

/// Applies the environment; the arguments wait for the process start.
pub(crate) fn init(exe: &Path) -> Result<()> {
    let gpu = GlobalState::get().use_gpu.load(Ordering::Relaxed);
    let setup = valo_launch::setup_lib::setup_lib(exe, gpu)
        .map_err(|e| std::io::Error::other(format!("engine setup: {e:#}")))?;
    for (key, value) in setup.env {
        // SAFETY: init_array code runs before main, single-threaded, before
        // any other code of this process can read the environment.
        unsafe {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
    if !setup.env.is_empty() || !setup.args.is_empty() {
        info!(
            Init,
            "engine setup: env {:?}, args {:?}", setup.env, setup.args
        );
    }
    ARGS.set(setup.args)
        .expect("engine setup initialized twice");
    Ok(())
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
