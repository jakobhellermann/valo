// TODO(ai-review): generated, not yet audited
//! 2D shapes drawn over the game's frames: tessellated with epaint and rendered with Vulkan into
//! the swapchain image right before it is presented, so they are part of the presented frame
//! instead of a window of their own. The game side submits them through `valo_overlay_shapes`,
//! the lib adds toasts.

mod game;
mod paint;
mod renderer;
mod toast;

use std::sync::atomic::{AtomicBool, Ordering};

use ash::vk;
use valo_ipc::config::OverlayConfig;

pub(crate) use game::queue_frame;
pub(crate) use renderer::{
    on_create_device, on_create_swapchain, on_destroy_device, on_destroy_swapchain,
    on_get_device_queue, swapchain_usage,
};
pub(crate) use toast::{ToastKind, toast};

pub static OVERLAY: Overlay = Overlay {
    enabled: AtomicBool::new(false),
    savestate_toasts: AtomicBool::new(true),
};

/// The applied [`OverlayConfig`].
pub struct Overlay {
    enabled: AtomicBool,
    savestate_toasts: AtomicBool,
}

impl Overlay {
    pub fn set_config(&self, config: &OverlayConfig) {
        self.enabled.store(config.enabled, Ordering::Relaxed);
        self.savestate_toasts
            .store(config.savestate_toasts, Ordering::Relaxed);
    }

    pub fn config(&self) -> OverlayConfig {
        OverlayConfig {
            enabled: self.enabled.load(Ordering::Relaxed),
            savestate_toasts: self.savestate_toasts(),
        }
    }

    pub(crate) fn savestate_toasts(&self) -> bool {
        self.savestate_toasts.load(Ordering::Relaxed)
    }
}

/// Draws over the image `present` shows on `queue`, after the semaphores the present would wait
/// on. Returns the semaphore the present has to wait on instead, or None when nothing was drawn
/// and the present stays as it is.
pub(crate) fn before_present(
    queue: vk::Queue,
    swapchain: vk::SwapchainKHR,
    image_index: u32,
    wait: &[vk::Semaphore],
) -> Option<vk::Semaphore> {
    let extent = renderer::extent(queue, swapchain)?;
    let (width, height) = (extent.width, extent.height);
    let game = game::take_shapes(width, height);
    let toasts = toast::live();
    if game.is_empty() && toasts.is_empty() {
        return None;
    }
    let paint = paint::paint(width, height, |fonts| {
        let mut shapes = game;
        shapes.extend(toast::layout(fonts, width, height, &toasts));
        shapes
    });
    renderer::draw(queue, swapchain, image_index, wait, &paint)
}
