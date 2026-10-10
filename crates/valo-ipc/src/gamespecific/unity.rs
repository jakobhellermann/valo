// TODO(ai-review): generated, not yet audited
//! Unity 6 player internals hooked in `UnityPlayer.so` (mangled names).

pub const MODULE: &str = "UnityPlayer.so";

pub const PRELOAD_ADD_TO_QUEUE: &str = "_ZN14PreloadManager10AddToQueueEP23PreloadManagerOperation";
pub const PRELOAD_UPDATE: &str = "_ZN14PreloadManager16UpdatePreloadingEv";
pub const PRELOAD_UPDATE_SINGLE_STEP: &str =
    "_ZN14PreloadManager26UpdatePreloadingSingleStepENS_21UpdatePreloadingFlagsEi";
pub const PRELOAD_RUN: &str = "_ZN14PreloadManager3RunEPv";
pub const PRELOAD_WAIT_FOR_ALL: &str = "_ZN14PreloadManager35WaitForAllAsyncOperationsToCompleteEv";
pub const SEMAPHORE_TRY_TIMED_ACQUIRE: &str = "_ZN7baselib12UnityClassic15CappedSemaphore15TryTimedAcquireENSt6chrono8durationIjSt5ratioILl1ELl1000EEEEj";
pub const ASYNC_READ_REQUEST: &str = "_ZN24AsyncReadManagerThreaded7RequestEP16AsyncReadCommand";
pub const SYNC_READ_REQUEST: &str = "_Z15SyncReadRequestP16AsyncReadCommand";
pub const JOB_SCHEDULE: &str = "_ZL26ujob_schedule_job_internalP14ujob_control_t13ujob_handle_tj";
pub const JOB_ENQUEUE: &str =
    "_ZL37ujobs_add_to_lane_and_wake_one_threadP14ujob_control_tP10ujob_job_tP11ujob_lane_t";
pub const JOB_EXECUTE: &str =
    "_ZL16ujob_execute_jobP14ujob_control_tP11ujob_lane_tP10ujob_job_t13ujob_handle_tj";
pub const JOB_COMPLETED: &str =
    "_ZL13job_completedP14ujob_control_tP11ujob_lane_tP10ujob_job_t13ujob_handle_t";

pub const LOAD_PERFORM: &str = "_ZN13LoadOperation7PerformEv";
pub const LOAD_INTEGRATE: &str = "_ZN13LoadOperation19IntegrateMainThreadEv";
pub const LOAD_SCENE_PERFORM: &str = "_ZN18LoadSceneOperation7PerformEv";
pub const LOAD_SCENE_INTEGRATE: &str = "_ZN18LoadSceneOperation19IntegrateMainThreadEv";
pub const LOAD_SCENE_GET_ALLOW_ACTIVATION: &str =
    "_ZN18LoadSceneOperation23GetAllowSceneActivationEv";
pub const LOAD_SCENE_SET_ALLOW_ACTIVATION: &str =
    "_ZN18LoadSceneOperation23SetAllowSceneActivationEb";
pub const UNLOAD_SCENE_PERFORM: &str = "_ZN20UnloadSceneOperation7PerformEv";
pub const UNLOAD_SCENE_INTEGRATE: &str = "_ZN20UnloadSceneOperation19IntegrateMainThreadEv";
pub const UNLOAD_UNUSED_PERFORM: &str = "_ZN27UnloadUnusedAssetsOperation7PerformEv";
pub const UNLOAD_UNUSED_INTEGRATE: &str = "_ZN27UnloadUnusedAssetsOperation19IntegrateMainThreadEv";
pub const BUNDLE_LOAD_ASSET_PERFORM: &str = "_ZN29AssetBundleLoadAssetOperation7PerformEv";
pub const BUNDLE_LOAD_PERFORM: &str = "_ZN33AssetBundleLoadFromAsyncOperation7PerformEv";
pub const BUNDLE_LOAD_INTEGRATE: &str =
    "_ZN33AssetBundleLoadFromAsyncOperation19IntegrateMainThreadEv";
pub const BUNDLE_UNLOAD_PERFORM: &str = "_ZN26AssetBundleUnloadOperation7PerformEv";
pub const BUNDLE_UNLOAD_INTEGRATE: &str = "_ZN26AssetBundleUnloadOperation19IntegrateMainThreadEv";

pub const SYMBOLS: &[&str] = &[
    PRELOAD_ADD_TO_QUEUE,
    PRELOAD_UPDATE,
    PRELOAD_UPDATE_SINGLE_STEP,
    PRELOAD_RUN,
    PRELOAD_WAIT_FOR_ALL,
    SEMAPHORE_TRY_TIMED_ACQUIRE,
    ASYNC_READ_REQUEST,
    SYNC_READ_REQUEST,
    JOB_SCHEDULE,
    JOB_ENQUEUE,
    JOB_EXECUTE,
    JOB_COMPLETED,
    LOAD_PERFORM,
    LOAD_INTEGRATE,
    LOAD_SCENE_PERFORM,
    LOAD_SCENE_INTEGRATE,
    LOAD_SCENE_GET_ALLOW_ACTIVATION,
    LOAD_SCENE_SET_ALLOW_ACTIVATION,
    UNLOAD_SCENE_PERFORM,
    UNLOAD_SCENE_INTEGRATE,
    UNLOAD_UNUSED_PERFORM,
    UNLOAD_UNUSED_INTEGRATE,
    BUNDLE_LOAD_ASSET_PERFORM,
    BUNDLE_LOAD_PERFORM,
    BUNDLE_LOAD_INTEGRATE,
    BUNDLE_UNLOAD_PERFORM,
    BUNDLE_UNLOAD_INTEGRATE,
];
