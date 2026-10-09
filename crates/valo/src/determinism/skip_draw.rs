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
//! agent-docs/explicit-sync.md) is skipped. Nothing is presented while
//! active, so the window keeps the last presented frame; rendering into
//! image 0 behind the compositor's back can tear the displayed frame — the
//! known tradeoff of the libTAS approach. "Render every k-th frame" is a
//! client concern: toggle the mode around frame boundaries (never mid-frame
//! — IPC is only serviced at boundaries, which is what makes the mode safe).

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::debug;
use vulkan_headers::vulkan::vulkan::*;

/// The game's graphics queue, captured at the first present (the acquire
/// fake needs a queue handle, but acquire itself carries none).
static QUEUE: AtomicUsize = AtomicUsize::new(0);
/// The real `vkQueueSubmit`, captured from the proc-addr chain.
static QUEUE_SUBMIT: AtomicUsize = AtomicUsize::new(0);

static ANNOUNCED: AtomicBool = AtomicBool::new(false);

/// skip_draw is session state in the runtime config (client-owned,
/// applied at frame boundaries via `GlobalState::apply_config` — which is
/// what makes the mode safe: acquire and present of a frame always see the
/// same value, and IPC only turns it over between frames).
pub(crate) fn active() -> bool {
    crate::global_state::GlobalState::get()
        .skip_draw
        .load(Ordering::Relaxed)
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
    if !active() {
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
