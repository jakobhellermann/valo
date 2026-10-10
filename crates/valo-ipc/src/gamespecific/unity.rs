// TODO(ai-review): generated, not yet audited
//! Unity 6 player internals hooked in `UnityPlayer.so` (mangled names).

pub const MODULE: &str = "UnityPlayer.so";

pub const PRELOAD_ADD_TO_QUEUE: &str = "_ZN14PreloadManager10AddToQueueEP23PreloadManagerOperation";
pub const PRELOAD_UPDATE: &str = "_ZN14PreloadManager16UpdatePreloadingEv";
pub const PRELOAD_UPDATE_SINGLE_STEP: &str =
    "_ZN14PreloadManager26UpdatePreloadingSingleStepENS_21UpdatePreloadingFlagsEi";
pub const PRELOAD_RUN: &str = "_ZN14PreloadManager3RunEPv";
pub const PRELOAD_WAIT_FOR_ALL: &str = "_ZN14PreloadManager35WaitForAllAsyncOperationsToCompleteEv";
pub const ASYNC_READ_REQUEST: &str = "_ZN24AsyncReadManagerThreaded7RequestEP16AsyncReadCommand";
pub const SYNC_READ_REQUEST: &str = "_Z15SyncReadRequestP16AsyncReadCommand";
pub const JOB_SCHEDULE: &str = "_ZL26ujob_schedule_job_internalP14ujob_control_t13ujob_handle_tj";
pub const JOB_ENQUEUE: &str =
    "_ZL37ujobs_add_to_lane_and_wake_one_threadP14ujob_control_tP10ujob_job_tP11ujob_lane_t";
pub const JOB_EXECUTE: &str =
    "_ZL16ujob_execute_jobP14ujob_control_tP11ujob_lane_tP10ujob_job_t13ujob_handle_tj";
pub const JOB_COMPLETED: &str =
    "_ZL13job_completedP14ujob_control_tP11ujob_lane_tP10ujob_job_t13ujob_handle_t";

pub const SYMBOLS: &[&str] = &[
    PRELOAD_ADD_TO_QUEUE,
    PRELOAD_UPDATE,
    PRELOAD_UPDATE_SINGLE_STEP,
    PRELOAD_RUN,
    PRELOAD_WAIT_FOR_ALL,
    ASYNC_READ_REQUEST,
    SYNC_READ_REQUEST,
    JOB_SCHEDULE,
    JOB_ENQUEUE,
    JOB_EXECUTE,
    JOB_COMPLETED,
];
