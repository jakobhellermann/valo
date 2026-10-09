// TODO(ai-review): generated, not yet audited
//! Present-mode selection for the guest's swapchains.
//!
//! Blocking modes (FIFO) pace presents to the display refresh and would
//! cap a deterministic run at real time; they are rewritten to the first
//! non-blocking mode the surface supports. The supported-mode set is
//! cached from the guest's own surface query, which precedes every
//! swapchain creation.

use std::sync::Mutex;

use crate::debug;
use vulkan_headers::vulkan::vulkan::*;

// (surface, supported modes)
static SURFACE_MODES: Mutex<Vec<(usize, Vec<VkPresentModeKHR>)>> = Mutex::new(Vec::new());

pub(crate) fn record_surface_modes(surface: VkSurfaceKHR, modes: &[VkPresentModeKHR]) {
    if modes.is_empty() {
        return;
    }
    let surface_id = surface as usize;
    let modes = modes.to_vec();
    if let Ok(mut entries) = SURFACE_MODES.lock() {
        match entries.iter_mut().find(|(s, _)| *s == surface_id) {
            Some(entry) => entry.1 = modes,
            None => entries.push((surface_id, modes)),
        }
    }
}

pub(crate) fn rewrite_create_info(
    info: &VkSwapchainCreateInfoKHR,
) -> Option<VkSwapchainCreateInfoKHR> {
    if crate::determinism::time::CLOCK.config().uncontrolled_time {
        return None;
    }

    let requested = info.presentMode;
    if !blocks(requested) {
        return None;
    }

    let Some(supported) = supported_modes_of(info.surface) else {
        debug!(
            Hooks,
            "present mode: keep requested {requested} (surface modes unknown)"
        );
        return None;
    };

    let Some(want) = unblocked(&supported) else {
        debug!(
            Hooks,
            "present mode: keep requested {requested} (no non-blocking mode supported)"
        );
        return None;
    };

    let mut rewritten = *info;
    rewritten.presentMode = want;
    debug!(Hooks, "present mode: {requested} -> {want}");

    Some(rewritten)
}

fn supported_modes_of(surface: VkSurfaceKHR) -> Option<Vec<VkPresentModeKHR>> {
    let surface_id = surface as usize;
    SURFACE_MODES
        .lock()
        .ok()?
        .iter()
        .find(|(s, _)| *s == surface_id)
        .map(|(_, modes)| modes.clone())
}

fn unblocked(supported: &[VkPresentModeKHR]) -> Option<VkPresentModeKHR> {
    [VK_PRESENT_MODE_MAILBOX_KHR, VK_PRESENT_MODE_IMMEDIATE_KHR]
        .into_iter()
        .find(|want| supported.contains(want))
}

fn blocks(mode: VkPresentModeKHR) -> bool {
    mode == VK_PRESENT_MODE_FIFO_KHR || mode == VK_PRESENT_MODE_FIFO_RELAXED_KHR
}
