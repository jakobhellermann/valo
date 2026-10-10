use std::sync::atomic::Ordering;

use crate::global_state::GlobalState;
use crate::{ipc, thread_tracker, trace};

pub fn frame_boundary<T>(draw: impl FnOnce() -> T) -> T {
    let global = GlobalState::get();
    let frame = global.frame_count.fetch_add(1, Ordering::SeqCst);
    thread_tracker::claim_main();

    trace!(Frame, "frame boundary {frame}");

    let checkpoint_frame = global.checkpoint_frame.load(Ordering::Relaxed);
    let restore_frame = global.restore_frame.load(Ordering::Relaxed);
    if checkpoint_frame > 0 && frame == checkpoint_frame {
        crate::valo_checkpoint();
    } else if restore_frame > 0 && frame == restore_frame {
        crate::valo_restore();
    }

    ipc::handle_frame_boundary(frame);

    draw()
}
