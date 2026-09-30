use std::ffi::{c_int, c_void};

use libc::{clockid_t, pthread_attr_t, pthread_t, timespec};

use crate::override_sym;
use crate::thread_tracker::{self, ThreadId};

type StartRoutine = extern "C" fn(*mut c_void) -> *mut c_void;

unsafe extern "C" {
    fn pthread_attr_getdetachstate(attr: *const pthread_attr_t, state: *mut c_int) -> c_int;
}

struct StartPayload {
    id: ThreadId,
    routine: StartRoutine,
    arg: *mut c_void,
}

extern "C" fn start_trampoline(raw: *mut c_void) -> *mut c_void {
    // Consume and free the payload before calling user code: pthread_exit does
    // not return through this Rust frame and therefore cannot drop a Box here.
    let payload = unsafe { *Box::from_raw(raw.cast::<StartPayload>()) };
    thread_tracker::start(payload.id);
    let result = (payload.routine)(payload.arg);
    thread_tracker::exit_current();
    result
}

fn initially_detached(attr: *const pthread_attr_t) -> bool {
    if attr.is_null() {
        return false;
    }
    let mut state = 0;
    // SAFETY: pthread_create receives this same attribute pointer. It remains
    // valid for the duration of the call, and this query does not mutate it.
    let result = unsafe { pthread_attr_getdetachstate(attr, &mut state) };
    assert_eq!(result, 0, "cannot read pthread detach state");
    state == libc::PTHREAD_CREATE_DETACHED
}

override_sym!(pthread_create(
    thread: *mut pthread_t,
    attr: *const pthread_attr_t,
    start_routine: StartRoutine,
    arg: *mut c_void
) -> c_int |real| {
    thread_tracker::require_ready();
    let Some(id) = thread_tracker::reserve(initially_detached(attr)) else {
        eprintln!("valo: thread registry capacity exhausted");
        std::process::abort();
    };
    let payload = Box::into_raw(Box::new(StartPayload {
        id,
        routine: start_routine,
        arg,
    }));
    let result = real(thread, attr, start_trampoline, payload.cast());
    if result == 0 {
        // SAFETY: pthread_create initialized `thread` on success.
        thread_tracker::attach(id, unsafe { *thread });
    } else {
        thread_tracker::creation_failed(id);
        // SAFETY: the child was not started, so it cannot own the payload.
        unsafe { drop(Box::from_raw(payload)) };
    }
    result
});

override_sym!(pthread_join(thread: pthread_t, retval: *mut *mut c_void) -> c_int |real| {
    thread_tracker::require_ready();
    let result = real(thread, retval);
    if result == 0 {
        thread_tracker::joined(thread);
    }
    result
});

override_sym!(pthread_tryjoin_np(_thread: pthread_t, _retval: *mut *mut c_void) -> c_int |_real| {
    todo!("pthread_tryjoin_np must update the thread registry");
});

override_sym!(pthread_timedjoin_np(
    _thread: pthread_t,
    _retval: *mut *mut c_void,
    _abstime: *const timespec
) -> c_int |_real| {
    todo!("pthread_timedjoin_np must update the thread registry");
});

override_sym!(pthread_clockjoin_np(
    _thread: pthread_t,
    _retval: *mut *mut c_void,
    _clockid: clockid_t,
    _abstime: *const timespec
) -> c_int |_real| {
    todo!("pthread_clockjoin_np must update the thread registry");
});

override_sym!(pthread_detach(thread: pthread_t) -> c_int |real| {
    thread_tracker::require_ready();
    let result = real(thread);
    if result == 0 {
        thread_tracker::detached(thread);
    }
    result
});

override_sym!(pthread_exit(retval: *mut c_void) -> ! |real| {
    thread_tracker::require_ready();
    thread_tracker::exit_current();
    real(retval)
});

override_sym!(pthread_cancel(thread: pthread_t) -> c_int |real| {
    thread_tracker::require_ready();
    let result = real(thread);
    if result == 0 {
        // Cancellation can bypass both the trampoline return and pthread_exit.
        thread_tracker::mark_cancellation();
    }
    result
});
