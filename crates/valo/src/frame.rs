use std::sync::atomic::Ordering;

use valo_ipc::messages::{ClientToLib, FrameBoundary, LibToClient, State};

use crate::global_state::GlobalState;
use crate::{error, info, ipc, trace};

pub fn frame_boundary(draw: impl FnOnce()) {
    let global = GlobalState::get();
    let frame = global.frame_count.fetch_add(1, Ordering::SeqCst);

    trace!(Frame, "frame boundary {frame}");

    if global.checkpoint_frame > 0 && frame == global.checkpoint_frame {
        crate::valo_checkpoint();
    } else if global.restore_frame > 0 && frame == global.restore_frame {
        crate::valo_restore();
    }

    handle_ipc(frame);

    draw();
}

fn handle_ipc(frame: u64) {
    let Some(mut conn) = ipc::lock() else {
        return;
    };

    if let Err(e) = conn.send(&LibToClient::FrameBoundary(FrameBoundary { frame })) {
        error!(Ipc, "send FrameBoundary: {e}");
        return;
    }

    loop {
        let msg = match conn.recv() {
            Ok(Some(m)) => m,
            Ok(None) => {
                error!(Ipc, "client hung up at frame {frame}");
                return;
            }
            Err(e) => {
                error!(Ipc, "recv: {e}");
                return;
            }
        };
        match msg {
            ClientToLib::Advance => {
                trace!(Ipc, "received Advance");
                return;
            }
            ClientToLib::Checkpoint => {
                info!(Ipc, "received Checkpoint");
                drop(conn);
                crate::valo_checkpoint();
                let Some(c) = ipc::lock() else { return };
                conn = c;
                // valo_checkpoint returns twice, on successful save
                // and after a restore. Discriminate via restore_in_progress
                let response = match GlobalState::get().restore_in_progress() {
                    true => LibToClient::RestoreSucceeded,
                    false => LibToClient::CheckpointDone,
                };
                if let Err(e) = conn.send(&response) {
                    error!(Ipc, "send {response:?}: {e}");
                    return;
                }
            }
            ClientToLib::Restore => {
                info!(Ipc, "received Restore");
                drop(conn);
                // On success, valo_restore jumps into the saved ucontext
                // and never returns here. We only fall through on error.
                crate::valo_restore();
                error!(Ipc, "did not restore snapshot");
                return;
            }
            ClientToLib::GetState => {
                info!(Ipc, "received GetState");
                let state = State { frame };
                if let Err(e) = conn.send(&LibToClient::State(state)) {
                    error!(Ipc, "send State: {e}");
                    return;
                }
            }
            ClientToLib::Quit => {
                info!(Ipc, "quit received, exiting");
                std::process::exit(0);
            }
            ClientToLib::Hello(_) => {
                error!(Ipc, "unexpected Hello mid-session");
            }
        }
    }
}
