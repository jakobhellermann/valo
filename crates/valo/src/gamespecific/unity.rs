// TODO(ai-review): generated, not yet audited
//! Unity 6 player internals: preload queue, async reads and the job system.
//! For now the hooks only log each call with its thread and frame, to see
//! which work finishes on which frame.

use std::ffi::{CStr, c_int, c_uint, c_void};
use std::sync::atomic::Ordering;

use valo_ipc::config::ResolvedModule;
use valo_ipc::gamespecific::unity as sym;

use crate::global_state::GlobalState;
use crate::{Result, debug, info, trace};

detour::static_detour! {
    static AddToQueue: unsafe extern "C" fn(*mut c_void, *mut c_void);
    static UpdatePreloading: unsafe extern "C" fn(*mut c_void);
    static UpdatePreloadingSingleStep: unsafe extern "C" fn(*mut c_void, c_int, c_int) -> u64;
    static PreloadRun: unsafe extern "C" fn(*mut c_void) -> u64;
    static WaitForAll: unsafe extern "C" fn(*mut c_void);
    static AsyncReadRequest: unsafe extern "C" fn(*mut c_void, *mut c_void);
    static SyncReadRequest: unsafe extern "C" fn(*mut c_void);
    static JobSchedule: unsafe extern "C" fn(*mut c_void, u64, c_uint) -> u64;
    static JobEnqueue: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void);
    static JobExecute: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void, u64, c_uint);
    static JobCompleted: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void, u64) -> u64;
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
                    AddToQueue.call(this, op)
                })?
                .enable()?;
            UpdatePreloading
                .initialize(function(addr(sym::PRELOAD_UPDATE)), |this| {
                    log_call("PreloadManager::UpdatePreloading", format_args!(""));
                    UpdatePreloading.call(this)
                })?
                .enable()?;
            UpdatePreloadingSingleStep
                .initialize(
                    function(addr(sym::PRELOAD_UPDATE_SINGLE_STEP)),
                    |this, flags, budget| {
                        let ret = UpdatePreloadingSingleStep.call(this, flags, budget);
                        log_call(
                            "PreloadManager::UpdatePreloadingSingleStep",
                            format_args!("flags={flags} budget={budget} -> {ret}"),
                        );
                        ret
                    },
                )?
                .enable()?;
            PreloadRun
                .initialize(function(addr(sym::PRELOAD_RUN)), |arg| {
                    log_call("PreloadManager::Run", format_args!("thread starts"));
                    PreloadRun.call(arg)
                })?
                .enable()?;
            WaitForAll
                .initialize(function(addr(sym::PRELOAD_WAIT_FOR_ALL)), |this| {
                    log_call(
                        "PreloadManager::WaitForAllAsyncOperationsToComplete",
                        format_args!("begin"),
                    );
                    WaitForAll.call(this);
                    log_call(
                        "PreloadManager::WaitForAllAsyncOperationsToComplete",
                        format_args!("end"),
                    );
                })?
                .enable()?;
            AsyncReadRequest
                .initialize(function(addr(sym::ASYNC_READ_REQUEST)), |this, command| {
                    log_call(
                        "AsyncReadManagerThreaded::Request",
                        format_args!("command={command:p}"),
                    );
                    AsyncReadRequest.call(this, command)
                })?
                .enable()?;
            SyncReadRequest
                .initialize(function(addr(sym::SYNC_READ_REQUEST)), |command| {
                    log_call("SyncReadRequest", format_args!("command={command:p}"));
                    SyncReadRequest.call(command)
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
                    JobEnqueue.call(control, job, lane)
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
                        JobCompleted.call(control, lane, job, handle)
                    },
                )?
                .enable()?;
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
