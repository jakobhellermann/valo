// TODO(ai-review): generated, not yet audited
//! Unity 6 player internals: preload queue, async reads and the job system.
//! For now the hooks only log each call with its thread and frame, to see
//! which work finishes on which frame.

use std::ffi::{CStr, c_int, c_uint, c_void};
use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};
use std::time::Duration;

use rustix::thread::futex;
use valo_ipc::config::ResolvedModule;
use valo_ipc::gamespecific::unity as sym;

use crate::determinism::time;
use crate::determinism::time::real_monotonic;
use crate::global_state::GlobalState;
use crate::thread_tracker;

mod jobs;
mod preload;

/// Offset of the count in a `baselib::UnityClassic::CappedSemaphore`.
const SEMAPHORE_COUNT_OFFSET: usize = 0x40;
/// `TryTimedAcquire`'s timeout, a constant in this build (`add $0xf4240` to the RAW start time).
const TRY_TIMED_ACQUIRE_TIMEOUT_NS: u64 = 1_000_000;

std::thread_local! {
    /// Nesting depth of PreloadManager::WaitForAllAsyncOperationsToComplete on
    /// the calling thread; integrating an operation can wait for all again.
    static IN_WAIT_FOR_ALL: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}
use crate::{Result, debug, error, info, trace};

detour::static_detour! {
    static AddToQueue: unsafe extern "C" fn(*mut c_void, *mut c_void);
    static UpdatePreloading: unsafe extern "C" fn(*mut c_void);
    static UpdatePreloadingSingleStep: unsafe extern "C" fn(*mut c_void, c_int, c_int) -> u64;
    static PreloadRun: unsafe extern "C" fn(*mut c_void) -> u64;
    static WaitForAll: unsafe extern "C" fn(*mut c_void);
    // the duration parameter of the mangled name is folded into the function in this
    // build; the registers are forwarded as they come
    static TryTimedAcquire: unsafe extern "C" fn(*mut c_void, u64, u64) -> u64;
    static AsyncReadRequest: unsafe extern "C" fn(*mut c_void, *mut c_void);
    static SyncReadRequest: unsafe extern "C" fn(*mut c_void);
    static JobSchedule: unsafe extern "C" fn(*mut c_void, u64, c_uint) -> u64;
    static JobEnqueue: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void);
    static JobExecute: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void, u64, c_uint);
    static JobCompleted: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void, u64) -> u64;
}

detour::static_detour! {
    static LoadPerform: unsafe extern "C" fn(*mut c_void) -> u64;
    static LoadIntegrate: unsafe extern "C" fn(*mut c_void) -> u64;
    static LoadScenePerform: unsafe extern "C" fn(*mut c_void) -> u64;
    static LoadSceneIntegrate: unsafe extern "C" fn(*mut c_void) -> u64;
    static UnloadScenePerform: unsafe extern "C" fn(*mut c_void) -> u64;
    static UnloadSceneIntegrate: unsafe extern "C" fn(*mut c_void) -> u64;
    static UnloadUnusedPerform: unsafe extern "C" fn(*mut c_void) -> u64;
    static UnloadUnusedIntegrate: unsafe extern "C" fn(*mut c_void) -> u64;
    static BundleLoadAssetPerform: unsafe extern "C" fn(*mut c_void) -> u64;
    static BundleLoadPerform: unsafe extern "C" fn(*mut c_void) -> u64;
    static BundleLoadIntegrate: unsafe extern "C" fn(*mut c_void) -> u64;
    static BundleUnloadPerform: unsafe extern "C" fn(*mut c_void) -> u64;
    static BundleUnloadIntegrate: unsafe extern "C" fn(*mut c_void) -> u64;
}

detour::static_detour! {
    static LoadSceneGetAllowActivation: unsafe extern "C" fn(*mut c_void) -> bool;
    static LoadSceneSetAllowActivation: unsafe extern "C" fn(*mut c_void, bool);
}

/// Hooks an operation's background `Perform(this)`: logs it and counts it for
/// [`preload`]; rax is forwarded whatever the method returns.
macro_rules! perform_hook {
    ($detour:ident, $symbol:expr, $addr:expr, $name:literal, non_parallel: $non_parallel:literal) => {
        $detour
            .initialize(function($addr($symbol)), |op| {
                log_call($name, format_args!("op={op:p} begin"));
                preload::perform_begin(op);
                let ret = $detour.call(op);
                preload::perform_end(op, $non_parallel);
                log_call($name, format_args!("op={op:p} end"));
                ret
            })?
            .enable()?
    };
}

/// Hooks an operation's `IntegrateMainThread(this)` and logs its begin and end.
macro_rules! integrate_hook {
    ($detour:ident, $symbol:expr, $addr:expr, $name:literal, non_parallel: $non_parallel:literal) => {
        $detour
            .initialize(function($addr($symbol)), |op| {
                log_call($name, format_args!("op={op:p} begin"));
                let ret = $detour.call(op);
                preload::integrated(op, $non_parallel);
                log_call($name, format_args!("op={op:p} end"));
                ret
            })?
            .enable()?
    };
}

/// Hooks the player if the client resolved its symbols.
pub(crate) fn install(modules: &[ResolvedModule]) -> Result<()> {
    let Some(player) = modules.iter().find(|m| m.name == sym::MODULE) else {
        return Ok(());
    };
    let base = loaded_module_base(&player.name, &player.build_id)?;
    if let Some(missing) = sym::SYMBOLS
        .iter()
        .find(|name| !player.symbols.iter().any(|(symbol, _)| symbol == *name))
    {
        return Err(std::io::Error::other(format!("{missing} not resolved")));
    }
    let addr = |name: &str| {
        let (_, offset) = player
            .symbols
            .iter()
            .find(|(symbol, _)| symbol == name)
            .unwrap();
        (base + *offset as usize) as *const ()
    };

    let hooked = || -> detour::Result<()> {
        // SAFETY: each address is the function of that mangled name in the
        // loaded player whose build id was checked; the detour signatures
        // follow its mangled parameter types (Itanium ABI, integer and
        // pointer registers only).
        unsafe {
            AddToQueue
                .initialize(function(addr(sym::PRELOAD_ADD_TO_QUEUE)), |this, op| {
                    log_call("PreloadManager::AddToQueue", format_args!("op={op:p}"));
                    // counted first: the preload thread may begin Perform before the call returns
                    preload::queued();
                    AddToQueue.call(this, op)
                })?
                .enable()?;
            UpdatePreloading
                .initialize(function(addr(sym::PRELOAD_UPDATE)), |this| {
                    log_call("PreloadManager::UpdatePreloading", format_args!(""));
                    preload::update(|| UpdatePreloading.call(this))
                })?
                .enable()?;
            UpdatePreloadingSingleStep
                .initialize(
                    function(addr(sym::PRELOAD_UPDATE_SINGLE_STEP)),
                    |this, flags, budget| {
                        let ret = UpdatePreloadingSingleStep.call(this, flags, budget);
                        // the function returns bool; only al is defined
                        let done = ret as u8 != 0;
                        log_call(
                            "PreloadManager::UpdatePreloadingSingleStep",
                            format_args!("flags={flags} budget={budget} -> {done}"),
                        );
                        ret
                    },
                )?
                .enable()?;
            PreloadRun
                .initialize(function(addr(sym::PRELOAD_RUN)), |arg| {
                    log_call("PreloadManager::Run", format_args!("thread starts"));
                    preload::thread_started();
                    PreloadRun.call(arg)
                })?
                .enable()?;
            WaitForAll
                .initialize(function(addr(sym::PRELOAD_WAIT_FOR_ALL)), |this| {
                    log_call(
                        "PreloadManager::WaitForAllAsyncOperationsToComplete",
                        format_args!("begin"),
                    );
                    IN_WAIT_FOR_ALL.set(IN_WAIT_FOR_ALL.get() + 1);
                    WaitForAll.call(this);
                    IN_WAIT_FOR_ALL.set(IN_WAIT_FOR_ALL.get() - 1);
                    log_call(
                        "PreloadManager::WaitForAllAsyncOperationsToComplete",
                        format_args!("end"),
                    );
                })?
                .enable()?;
            SyncReadRequest
                .initialize(function(addr(sym::SYNC_READ_REQUEST)), |command| {
                    log_call("SyncReadRequest", format_args!("command={command:p}"));
                    SyncReadRequest.call(command)
                })?
                .enable()?;
            TryTimedAcquire
                .initialize(
                    function(addr(sym::SEMAPHORE_TRY_TIMED_ACQUIRE)),
                    |this, unused_a, unused_b| {
                        if IN_WAIT_FOR_ALL.get() == 0 || !thread_tracker::is_main() {
                            return TryTimedAcquire.call(this, unused_a, unused_b);
                        }
                        // once the preload thread is done, nobody else signals: the
                        // acquire succeeds at once or times out, never depending on its speed
                        preload::wait_until_idle();
                        // `this` is a live CappedSemaphore; its count sits at
                        // SEMAPHORE_COUNT_OFFSET (the fast path's `lock xadd` in this build)
                        let count = &*this.byte_add(SEMAPHORE_COUNT_OFFSET).cast::<AtomicI32>();
                        if count.load(Ordering::SeqCst) > 0 {
                            return TryTimedAcquire.call(this, unused_a, unused_b);
                        }
                        // the original would wait for its timeout against the virtual clock,
                        // which stands still within a frame: time it out here instead
                        log_call(
                            "CappedSemaphore::TryTimedAcquire",
                            format_args!("timed out"),
                        );
                        time::CLOCK.sleep(TRY_TIMED_ACQUIRE_TIMEOUT_NS);
                        0
                    },
                )?
                .enable()?;
            AsyncReadRequest
                .initialize(function(addr(sym::ASYNC_READ_REQUEST)), |_this, command| {
                    log_call(
                        "AsyncReadManagerThreaded::Request",
                        format_args!("command={command:p} (read synchronously)"),
                    );
                    // the read finishes before the caller continues, instead
                    // of on the read thread at a wall-clock-dependent frame
                    SyncReadRequest
                        .trampoline()
                        .expect("SyncReadRequest is hooked first")(command)
                })?
                .enable()?;
            JobSchedule
                .initialize(
                    function(addr(sym::JOB_SCHEDULE)),
                    |control, handle, flags| {
                        let ret = JobSchedule.call(control, handle, flags);
                        trace_call(
                            "ujob_schedule_job_internal",
                            format_args!("handle={handle:#x} flags={flags} -> {ret:#x}"),
                        );
                        ret
                    },
                )?
                .enable()?;
            // every path that hands a job to the workers ends here, also parallel-for jobs
            JobEnqueue
                .initialize(function(addr(sym::JOB_ENQUEUE)), |control, job, lane| {
                    trace_call(
                        "ujobs_add_to_lane_and_wake_one_thread",
                        format_args!("job={job:p} lane={lane:p}"),
                    );
                    jobs::enqueue(job, || JobEnqueue.call(control, job, lane))
                })?
                .enable()?;
            JobExecute
                .initialize(
                    function(addr(sym::JOB_EXECUTE)),
                    |control, lane, job, handle, flags| {
                        trace_call(
                            "ujob_execute_job",
                            format_args!("job={job:p} handle={handle:#x}"),
                        );
                        JobExecute.call(control, lane, job, handle, flags)
                    },
                )?
                .enable()?;
            JobCompleted
                .initialize(
                    function(addr(sym::JOB_COMPLETED)),
                    |control, lane, job, handle| {
                        trace_call(
                            "job_completed",
                            format_args!("job={job:p} handle={handle:#x}"),
                        );
                        let ret = JobCompleted.call(control, lane, job, handle);
                        jobs::completed(job);
                        ret
                    },
                )?
                .enable()?;
            LoadSceneGetAllowActivation
                .initialize(function(addr(sym::LOAD_SCENE_GET_ALLOW_ACTIVATION)), |op| {
                    let allow = LoadSceneGetAllowActivation.call(op);
                    log_call(
                        "LoadSceneOperation::GetAllowSceneActivation",
                        format_args!("op={op:p} -> {allow}"),
                    );
                    allow
                })?
                .enable()?;
            LoadSceneSetAllowActivation
                .initialize(
                    function(addr(sym::LOAD_SCENE_SET_ALLOW_ACTIVATION)),
                    |op, allow| {
                        log_call(
                            "LoadSceneOperation::SetAllowSceneActivation",
                            format_args!("op={op:p} {allow}"),
                        );
                        LoadSceneSetAllowActivation.call(op, allow)
                    },
                )?
                .enable()?;
            perform_hook!(
                LoadPerform,
                sym::LOAD_PERFORM,
                addr,
                "LoadOperation::Perform",
                non_parallel: false
            );
            integrate_hook!(
                LoadIntegrate,
                sym::LOAD_INTEGRATE,
                addr,
                "LoadOperation::IntegrateMainThread",
                non_parallel: false
            );
            perform_hook!(
                LoadScenePerform,
                sym::LOAD_SCENE_PERFORM,
                addr,
                "LoadSceneOperation::Perform",
                non_parallel: false
            );
            integrate_hook!(
                LoadSceneIntegrate,
                sym::LOAD_SCENE_INTEGRATE,
                addr,
                "LoadSceneOperation::IntegrateMainThread",
                non_parallel: false
            );
            perform_hook!(
                UnloadScenePerform,
                sym::UNLOAD_SCENE_PERFORM,
                addr,
                "UnloadSceneOperation::Perform",
                non_parallel: false
            );
            integrate_hook!(
                UnloadSceneIntegrate,
                sym::UNLOAD_SCENE_INTEGRATE,
                addr,
                "UnloadSceneOperation::IntegrateMainThread",
                non_parallel: false
            );
            perform_hook!(
                UnloadUnusedPerform,
                sym::UNLOAD_UNUSED_PERFORM,
                addr,
                "UnloadUnusedAssetsOperation::Perform",
                non_parallel: true
            );
            integrate_hook!(
                UnloadUnusedIntegrate,
                sym::UNLOAD_UNUSED_INTEGRATE,
                addr,
                "UnloadUnusedAssetsOperation::IntegrateMainThread",
                non_parallel: true
            );
            perform_hook!(
                BundleLoadAssetPerform,
                sym::BUNDLE_LOAD_ASSET_PERFORM,
                addr,
                "AssetBundleLoadAssetOperation::Perform",
                non_parallel: false
            );
            perform_hook!(
                BundleLoadPerform,
                sym::BUNDLE_LOAD_PERFORM,
                addr,
                "AssetBundleLoadFromAsyncOperation::Perform",
                non_parallel: false
            );
            integrate_hook!(
                BundleLoadIntegrate,
                sym::BUNDLE_LOAD_INTEGRATE,
                addr,
                "AssetBundleLoadFromAsyncOperation::IntegrateMainThread",
                non_parallel: false
            );
            perform_hook!(
                BundleUnloadPerform,
                sym::BUNDLE_UNLOAD_PERFORM,
                addr,
                "AssetBundleUnloadOperation::Perform",
                non_parallel: false
            );
            integrate_hook!(
                BundleUnloadIntegrate,
                sym::BUNDLE_UNLOAD_INTEGRATE,
                addr,
                "AssetBundleUnloadOperation::IntegrateMainThread",
                non_parallel: false
            );
            Ok(())
        }
    };
    hooked().map_err(|e| std::io::Error::other(format!("unity: hooking failed: {e}")))?;
    info!(Unity, "hooked {} player functions", sym::SYMBOLS.len());
    Ok(())
}

/// `address` as a function pointer of type `F`.
///
/// # Safety
/// `address` is a function with signature `F`.
unsafe fn function<F: Copy>(address: *const ()) -> F {
    const { assert!(size_of::<F>() == size_of::<*const ()>()) };
    // SAFETY: `F` is a function pointer type of pointer size, per the contract.
    unsafe { std::mem::transmute_copy(&address) }
}

fn log_call(function: &str, details: std::fmt::Arguments) {
    let (_, tid) = crate::sys::current_thread_ids();
    let frame = GlobalState::get().frame_count.load(Ordering::Relaxed);
    debug!(Unity, "frame {frame} tid {tid} {function} {details}");
}

/// [`log_call`] at trace level, for hooks that fire many times per frame.
fn trace_call(function: &str, details: std::fmt::Arguments) {
    let (_, tid) = crate::sys::current_thread_ids();
    let frame = GlobalState::get().frame_count.load(Ordering::Relaxed);
    trace!(Unity, "frame {frame} tid {tid} {function} {details}");
}

/// A wait this long means the other threads stopped making progress.
const STALL: Duration = Duration::from_secs(5);

/// Waits until `word` changes from `seen`; false, with `stalled()` logged, after
/// [`STALL`] of real time without a change.
fn wait_for_change(word: &AtomicU32, seen: u32, stalled: impl Fn() -> String) -> bool {
    let start = real_monotonic();
    // relative timeouts are measured by the kernel in real time
    let slice = rustix::time::Timespec {
        tv_sec: 0,
        tv_nsec: 100_000_000,
    };
    while word.load(Ordering::SeqCst) == seen {
        if real_monotonic() - start > STALL {
            error!(Unity, "{} for 5s", stalled());
            return false;
        }
        match futex::wait(word, futex::Flags::PRIVATE, seen, Some(&slice)) {
            Ok(()) => {}
            Err(e)
                if e == rustix::io::Errno::AGAIN
                    || e == rustix::io::Errno::TIMEDOUT
                    || e == rustix::io::Errno::INTR => {}
            Err(e) => {
                error!(Unity, "futex wait failed: {e}");
                return false;
            }
        }
    }
    true
}

/// Bumps `word` and wakes all its waiters.
fn signal(word: &AtomicU32) {
    word.fetch_add(1, Ordering::SeqCst);
    if let Err(e) = futex::wake(word, futex::Flags::PRIVATE, u32::MAX) {
        error!(Unity, "futex wake failed: {e}");
    }
}

/// Load base of the module whose file name is `name`, checked against `build_id`.
fn loaded_module_base(name: &str, build_id: &[u8]) -> Result<usize> {
    struct Search<'a> {
        name: &'a str,
        found: Option<(usize, Option<&'static [u8]>)>,
    }

    unsafe extern "C" fn visit(
        info: *mut libc::dl_phdr_info,
        _size: usize,
        data: *mut c_void,
    ) -> c_int {
        // SAFETY: `data` is the `Search` passed to dl_iterate_phdr below, and
        // `info` is valid for the duration of this callback.
        let (search, info) = unsafe { (&mut *data.cast::<Search>(), &*info) };
        if info.dlpi_name.is_null() {
            return 0;
        }
        // SAFETY: dlpi_name is a NUL-terminated path.
        let path = unsafe { CStr::from_ptr(info.dlpi_name) }.to_bytes();
        if !path.ends_with(search.name.as_bytes())
            || path.len() <= search.name.len()
            || path[path.len() - search.name.len() - 1] != b'/'
        {
            return 0;
        }
        let base = info.dlpi_addr as usize;
        // SAFETY: dlpi_phdr points to dlpi_phnum program headers of the module.
        let phdrs = unsafe { std::slice::from_raw_parts(info.dlpi_phdr, info.dlpi_phnum as usize) };
        let build_id = phdrs
            .iter()
            .filter(|phdr| phdr.p_type == libc::PT_NOTE)
            // SAFETY: PT_NOTE segments are mapped at base + p_vaddr for p_memsz bytes.
            .find_map(|phdr| unsafe {
                gnu_build_id(std::slice::from_raw_parts(
                    (base + phdr.p_vaddr as usize) as *const u8,
                    phdr.p_memsz as usize,
                ))
            });
        search.found = Some((base, build_id));
        1
    }

    let mut search = Search { name, found: None };
    // SAFETY: `visit` only reads the module list and writes into `search`.
    unsafe { libc::dl_iterate_phdr(Some(visit), (&raw mut search).cast()) };
    match search.found {
        None => Err(std::io::Error::other(format!(
            "unity: {name} is not loaded"
        ))),
        Some((_, None)) => Err(std::io::Error::other(format!(
            "unity: {name} has no build id"
        ))),
        Some((_, Some(id))) if id != build_id => Err(std::io::Error::other(format!(
            "unity: loaded {name} has a different build id than the resolved symbols"
        ))),
        Some((base, Some(_))) => Ok(base),
    }
}

/// The descriptor of the `NT_GNU_BUILD_ID` note in a note segment.
fn gnu_build_id(mut notes: &'static [u8]) -> Option<&'static [u8]> {
    const NT_GNU_BUILD_ID: u32 = 3;
    let align4 = |n: usize| (n + 3) & !3;
    while notes.len() >= 12 {
        let word = |i: usize| u32::from_ne_bytes(notes[i..i + 4].try_into().unwrap());
        let (namesz, descsz, kind) = (word(0) as usize, word(4) as usize, word(8));
        let desc_start = 12 + align4(namesz);
        let next = desc_start + align4(descsz);
        if next > notes.len() {
            return None;
        }
        if kind == NT_GNU_BUILD_ID && &notes[12..12 + namesz] == b"GNU\0" {
            return Some(&notes[desc_start..desc_start + descsz]);
        }
        notes = &notes[next..];
    }
    None
}
