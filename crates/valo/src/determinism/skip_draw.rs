// TODO(ai-review): generated, not yet audited
//! Skip presenting frames.
//! When active,
//! - acquire is not forwarded to the WSI
//! - present does an empty submit to handle wait semaphores
//!
//! Game rendering still happens as usual.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::debug;
use crate::global_state::GlobalState;
use vulkan_headers::vulkan::vulkan::*;

/// The game's graphics queue, captured at the first present
static QUEUE: AtomicUsize = AtomicUsize::new(0); // VkQueue
/// The real `vkQueueSubmit`, captured from the proc-addr chain.
static QUEUE_SUBMIT: AtomicUsize = AtomicUsize::new(0);

static ANNOUNCED: AtomicBool = AtomicBool::new(false);

pub(crate) fn active() -> bool {
    GlobalState::get().skip_draw.load(Ordering::Relaxed)
}

/// Capture the real `vkQueueSubmit`
pub(crate) fn capture_submit(proc: *const c_void) {
    if !proc.is_null() {
        QUEUE_SUBMIT.store(proc as usize, Ordering::Release);
    }
}

/// Capture the presenting queue.
pub(crate) fn capture_queue(queue: VkQueue) {
    QUEUE.store(queue as usize, Ordering::Release);
}

/// Decide and execute the acquire fake.
/// Returns `true` when the frame is skipped.
///
/// # Safety
/// `semaphore` and `fence` must each be null or a valid handle.
pub(crate) unsafe fn fake_acquire(
    semaphore: VkSemaphore,
    fence: VkFence,
    image_index: &mut u32,
) -> bool {
    if !active() {
        return false;
    }
    let queue = QUEUE.load(Ordering::Acquire) as VkQueue;
    // SAFETY: TODO: `QUEUE` may be stale after a device recreate, and the submit is not
    // externally synchronized against the game's own submits on it.
    let Some(ret) = (unsafe { empty_submit(queue, &[], semaphore, fence) }) else {
        return false; // queue not captured yet: render this one for real
    };
    if ret != VK_SUCCESS {
        return false; // signal failed: fall back to a real acquire
    }
    *image_index = 0;
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
/// `queue` and `wait_semaphores` must be valid handles.
pub(crate) unsafe fn drain_present(
    queue: VkQueue,
    wait_semaphores: &[VkSemaphore],
) -> Option<VkResult> {
    if wait_semaphores.is_empty() {
        return Some(VK_SUCCESS);
    }
    // SAFETY: the handles are valid per this fn's contract.
    unsafe {
        empty_submit(
            queue,
            wait_semaphores,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    }
}

/// Signal `signal_semaphore`/`fence` or wait on `wait_semaphores` with an
/// empty queue submit on `queue`. Returns `None` while the real submit or
/// queue is not captured yet.
///
/// # Safety
/// `queue` must be null or a valid queue, `wait_semaphores` valid handles,
/// `signal_semaphore` and `fence` null or valid handles.
unsafe fn empty_submit(
    queue: VkQueue,
    wait_semaphores: &[VkSemaphore],
    signal_semaphore: VkSemaphore,
    fence: VkFence,
) -> Option<VkResult> {
    let submit = QUEUE_SUBMIT.load(Ordering::Acquire);
    if submit == 0 || queue.is_null() {
        return None;
    }
    // SAFETY: `QUEUE_SUBMIT` only holds pointers the proc-addr chain returned
    // for `vkQueueSubmit`.
    let submit: NonNullPFN_vkQueueSubmit = unsafe { std::mem::transmute(submit) };
    let wait_count = wait_semaphores.len() as u32;
    let signal_count: u32 = u32::from(!signal_semaphore.is_null());
    let stage: VkPipelineStageFlags = VK_PIPELINE_STAGE_COLOR_ATTACHMENT_OUTPUT_BIT;
    // pWaitDstStageMask is an array with one entry per wait semaphore.
    let stages = std::vec![stage; wait_semaphores.len()];

    let info = VkSubmitInfo {
        sType: VK_STRUCTURE_TYPE_SUBMIT_INFO,
        pNext: std::ptr::null(),
        waitSemaphoreCount: wait_count,
        pWaitSemaphores: wait_semaphores.as_ptr(),
        pWaitDstStageMask: stages.as_ptr(),
        commandBufferCount: 0,
        pCommandBuffers: std::ptr::null(),
        signalSemaphoreCount: signal_count,
        pSignalSemaphores: &signal_semaphore,
    };

    // SAFETY: the handles are valid per this fn's contract, and every array in
    // `info` has its declared length and outlives the call.
    Some(unsafe { submit(queue, 1, &info, fence) })
}
