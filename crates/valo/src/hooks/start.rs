// TODO(ai-review): generated, not yet audited
//! Interposed process start.

use std::ffi::{c_char, c_int, c_void};

use crate::{engine_setup, override_sym};

override_sym!(__libc_start_main(
    main: *mut c_void,
    argc: c_int,
    argv: *mut *mut c_char,
    init: *mut c_void,
    fini: *mut c_void,
    rtld_fini: *mut c_void,
    stack_end: *mut c_void
) -> c_int |real| {
    // SAFETY: `_start` passes the kernel's argv, `argc` NUL-terminated strings.
    let (argc, argv) = unsafe { engine_setup::extend_argv(argc, argv) };
    real(main, argc, argv, init, fini, rtld_fini, stack_end)
});
