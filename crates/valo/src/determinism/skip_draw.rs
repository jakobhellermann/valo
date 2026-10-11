// TODO(ai-review): generated, not yet audited
//! skip_draw: skip the Vulkan image pipeline entirely — libTAS
//! `skipping_draw` parity.
//!
//! While set (config.skip_draw — see [`active`]), every frame
//! bypasses the swapchain machinery:
//! - acquire is not forwarded to the WSI; the binary semaphore/fence the game
//!   expects is signaled with an empty queue submit (the only spec-legal
//!   signal path outside a real acquire — the wait happens on the host side,
//!   so the signal must be real), and image index 0 is returned;
//! - present drains its wait semaphores with an empty submit instead of
//!   presenting. The frame boundary still fires either way (frame counting,
//!   checkpoints, IPC, virtual clock): a skipped frame is a fully counted
//!   game frame.
//!
//! The game's render submits still run at full cost — only the image pipeline
//! (and its output-rate-coupled implicit-sync release wait, see
//! agent-docs/explicit-sync.md) is skipped. The render cadence while
//! skipping is part of the mode: `All` presents nothing; `Interval`
//! presents at most one frame per its minimum wall-clock interval. The
//! per-frame decision is computed at each frame boundary and latched for
//! the next frame — acquire and present of one frame must agree, and a
//! wall-clock due check could flip between them. The client toggles the
//! mode around frame boundaries (never mid-frame — IPC is only serviced
//! at boundaries, which is what makes the mode safe).

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use crate::global_state::GlobalState;
use crate::{debug, determinism};
use vulkan_headers::vulkan::vulkan::*;

/// The game's graphics queue, captured at the first present (the acquire
/// fake needs a queue handle, but acquire itself carries none).
static QUEUE: AtomicUsize = AtomicUsize::new(0);
/// The real `vkQueueSubmit`, captured from the proc-addr chain.
static QUEUE_SUBMIT: AtomicUsize = AtomicUsize::new(0);

static ANNOUNCED: AtomicBool = AtomicBool::new(false);

static SKIP_THIS_FRAME: AtomicBool = AtomicBool::new(false);
/// Wall-clock time of the last presented frame.
static LAST_RENDER_NS: AtomicU64 = AtomicU64::new(0);

/// Computes the next frame's skip decision.
pub(crate) fn on_frame_boundary() {
    let global = GlobalState::get();
    if !global.skip_draw.load(Ordering::Relaxed) {
        SKIP_THIS_FRAME.store(false, Ordering::Relaxed);
        return;
    }

    let now = determinism::time::real_ns();
    if !SKIP_THIS_FRAME.load(Ordering::Relaxed) {
        LAST_RENDER_NS.store(now, Ordering::Relaxed);
    }

    let min_render_ms = global.skip_draw_min_render_ms.load(Ordering::Relaxed) as u64;
    let due = min_render_ms != u32::MAX as u64
        && now.saturating_sub(LAST_RENDER_NS.load(Ordering::Relaxed)) >= min_render_ms * 1_000_000;

    SKIP_THIS_FRAME.store(!due, Ordering::Relaxed);
}

/// Capture the real `vkQueueSubmit` handed out by the proc-addr chain (the
/// empty submits need the loader-resolved original).
pub(crate) fn capture_submit(proc: *const c_void) {
    if !proc.is_null() {
        QUEUE_SUBMIT.store(proc as usize, Ordering::Release);
    }
}

/// Capture the presenting queue.
pub(crate) fn capture_queue(queue: VkQueue) {
    QUEUE.store(queue as usize, Ordering::Release);
}

/// Decide and execute the acquire fake. Returns `true` when the frame is
/// skipped (the caller must not forward the acquire to the WSI).
///
/// # Safety
/// `p_image_index` must be a valid out pointer (or null); semaphore/fence
/// are the caller's own handles.
pub(crate) unsafe fn fake_acquire(
    semaphore: VkSemaphore,
    fence: VkFence,
    p_image_index: *mut u32,
) -> bool {
    if !SKIP_THIS_FRAME.load(Ordering::Relaxed) {
        return false;
    }

    let queue = QUEUE.load(Ordering::Acquire) as VkQueue;
    // SAFETY: `queue` is the game's own graphics queue; the handles come
    // from the caller's acquire call.
    let Some(ret) = (unsafe { empty_submit(queue, std::ptr::null(), 0, semaphore, fence) }) else {
        return false; // queue not captured yet: render this one for real
    };
    if ret != VK_SUCCESS {
        return false; // signal failed: fall back to a real acquire
    }
    if !p_image_index.is_null() {
        // SAFETY: valid out pointer per the acquire contract.
        unsafe { *p_image_index = 0 };
    }
    if !ANNOUNCED.swap(true, Ordering::Relaxed) {
        debug!(Hooks, "skip_draw: skipping image pipeline");
    }
    true
}

/// Drain a skipped present's wait semaphores with an empty submit on the
/// presenting queue. Returns `None` when draining is impossible (the caller
/// must then present for real to keep semaphore state consistent).
///
/// # Safety
/// `queue` must be a valid queue; the wait semaphores must be valid Vulkan
/// handles from the game's own present info.
pub(crate) unsafe fn drain_present(queue: VkQueue, info: &VkPresentInfoKHR) -> Option<VkResult> {
    if !SKIP_THIS_FRAME.load(Ordering::Relaxed) {
        return None;
    }
    if info.waitSemaphoreCount == 0 {
        return Some(VK_SUCCESS);
    }
    // SAFETY: `queue` is the presenting queue; the wait semaphores come from
    // the caller's present info.
    unsafe {
        empty_submit(
            queue,
            info.pWaitSemaphores,
            info.waitSemaphoreCount,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    }
}

/// Signal `signal_semaphore`/`fence` (acquire fake) or wait on
/// `wait_semaphores[..wait_count]` (present drain) with an empty queue
/// submit on `queue`. Returns `None` while the real submit or queue is not
/// captured yet, `Some(ret)` with the submit result otherwise.
///
/// # Safety
/// `queue` must be a valid queue handle; the semaphore/fence handles and
/// `wait_semaphores` must be valid Vulkan handles (or null/empty).
unsafe fn empty_submit(
    queue: VkQueue,
    wait_semaphores: *const VkSemaphore,
    wait_count: u32,
    signal_semaphore: VkSemaphore,
    fence: VkFence,
) -> Option<VkResult> {
    let submit = QUEUE_SUBMIT.load(Ordering::Acquire);
    if submit == 0 || queue.is_null() {
        return None;
    }
    // SAFETY: the captured pointer is the loader-resolved original for this
    // exact signature.
    let submit: NonNullPFN_vkQueueSubmit = unsafe { std::mem::transmute(submit) };
    let signal_count: u32 = u32::from(!signal_semaphore.is_null());
    let stage: VkPipelineStageFlags = VK_PIPELINE_STAGE_COLOR_ATTACHMENT_OUTPUT_BIT;
    // pWaitDstStageMask is an array with one entry per wait semaphore.
    let stages = std::vec![stage; wait_count as usize];

    let info = VkSubmitInfo {
        sType: VK_STRUCTURE_TYPE_SUBMIT_INFO,
        pNext: std::ptr::null(),
        waitSemaphoreCount: wait_count,
        pWaitSemaphores: wait_semaphores,
        pWaitDstStageMask: stages.as_ptr(),
        commandBufferCount: 0,
        pCommandBuffers: std::ptr::null(),
        signalSemaphoreCount: signal_count,
        pSignalSemaphores: &signal_semaphore,
    };

    // SAFETY: `queue` is the game's own present/graphics queue, `info` is a
    // spec-shaped empty VkSubmitInfo, and the handles come from the
    // caller's own acquire/present calls.
    Some(unsafe { submit(queue, 1, &info, fence) })
}
