use std::sync::atomic::Ordering;

use crate::global_state::GlobalState;

pub fn frame_boundary(draw: impl FnOnce()) {
    let global = GlobalState::get();
    global.frame_count.fetch_add(1, Ordering::SeqCst);
    draw();
}
