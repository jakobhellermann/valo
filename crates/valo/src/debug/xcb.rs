// TODO(ai-review): generated, not yet audited
//! xcb event decoding for the xcb hooks: response-type extraction and
//! human-readable event names.

use crate::hooks::xcb::xcb_generic_event_t;

/// `(0, "none")` for null events, otherwise the event's response type code
/// (SendEvent flag masked off) and a human-readable name.
pub(crate) fn describe_event(event: *mut xcb_generic_event_t) -> (u8, &'static str) {
    if event.is_null() {
        return (0, "none");
    }
    // SAFETY: every xcb event struct starts with the one-byte response_type
    // and is at least 32 bytes large; `event` is a non-null xcb event pointer
    // returned by libxcb, so reading its first byte is in bounds.
    let raw = unsafe { (event as *const u8).read() };
    let code = raw & 0x7f;
    (code, event_name(code))
}

/// Names for core X protocol event codes (`xcbproto` XCB_* event values).
fn event_name(code: u8) -> &'static str {
    match code {
        1 => "error/reply",
        2 => "key_press",
        3 => "key_release",
        4 => "button_press",
        5 => "button_release",
        6 => "motion_notify",
        7 => "enter_notify",
        8 => "leave_notify",
        9 => "focus_in",
        10 => "focus_out",
        11 => "keymap_notify",
        12 => "expose",
        13 => "graphics_exposure",
        14 => "no_exposure",
        15 => "visibility_notify",
        16 => "create_notify",
        17 => "destroy_notify",
        18 => "unmap_notify",
        19 => "map_notify",
        20 => "map_request",
        21 => "reparent_notify",
        22 => "configure_notify",
        23 => "configure_request",
        24 => "gravity_notify",
        25 => "resize_request",
        26 => "circulate_notify",
        27 => "circulate_request",
        28 => "property_notify",
        29 => "selection_clear",
        30 => "selection_request",
        31 => "selection_notify",
        32 => "colormap_notify",
        33 => "client_message",
        34 => "mapping_notify",
        35 => "ge_generic",
        _ => "unknown",
    }
}
