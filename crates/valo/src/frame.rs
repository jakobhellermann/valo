use std::sync::atomic::Ordering;

use crate::global_state::GlobalState;
use crate::info;

pub fn frame_boundary(draw: impl FnOnce()) {
    let global = GlobalState::get();
    let frame = global.frame_count.fetch_add(1, Ordering::SeqCst);

    info!(Frame, "frame boundary {frame}");

    if global.checkpoint_frame > 0 && frame == global.checkpoint_frame {
        crate::valo_checkpoint();
    } else if global.restore_frame > 0 && frame == global.restore_frame {
        crate::valo_restore();
    }

    draw();
}
