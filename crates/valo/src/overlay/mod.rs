// TODO(ai-review): generated, not yet audited
//! 2D shapes drawn over the game's frames: tessellated with epaint and rendered with Vulkan into
//! the swapchain image right before it is presented, so they are part of the presented frame
//! instead of a window of their own. The game side submits them through `valo_overlay_shapes`,
//! the lib adds toasts.

mod game;
mod paint;
mod renderer;
mod toast;

use ash::vk;

pub(crate) use game::queue_frame;
pub use game::{config, set_config};
pub(crate) use renderer::{
    on_create_device, on_create_swapchain, on_destroy_device, on_destroy_swapchain,
    on_get_device_queue, swapchain_usage,
};
pub(crate) use toast::{ToastKind, toast};

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
