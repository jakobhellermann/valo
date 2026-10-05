use std::sync::atomic::{AtomicU64, Ordering};

use valo_ipc::messages::{ClientToLib, Error, FrameBoundary, LibToClient, State};

use crate::global_state::GlobalState;
use crate::{error, info, ipc, trace};

static BLOCK_ADVANCE_UNTIL: AtomicU64 = AtomicU64::new(0);

pub fn frame_boundary<T>(draw: impl FnOnce() -> T) -> T {
    let global = GlobalState::get();
    let frame = global.frame_count.fetch_add(1, Ordering::SeqCst);

    trace!(Frame, "frame boundary {frame}");

    if global.checkpoint_frame > 0 && frame == global.checkpoint_frame {
        crate::valo_checkpoint();
    } else if global.restore_frame > 0 && frame == global.restore_frame {
        crate::valo_restore();
    }

    handle_ipc(frame);

    draw()
}

fn handle_ipc(frame: u64) {
    if ipc::async_mode() {
        ipc::handle_async(frame);
        return;
    }

    if frame < BLOCK_ADVANCE_UNTIL.load(Ordering::SeqCst) {
        return;
    }
    BLOCK_ADVANCE_UNTIL.store(0, Ordering::SeqCst);

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
            ClientToLib::AdvanceFrames(count) => match count {
                0 => {
                    if let Err(e) = conn.send(&LibToClient::State(State { frame })) {
                        error!(Ipc, "send State: {e}");
                        return;
                    }
                }
                1 => return,
                n => {
                    if let Some(target) = frame.checked_add(n) {
                        trace!(Ipc, "received AdvanceFrames({n})");
                        BLOCK_ADVANCE_UNTIL.store(target, Ordering::SeqCst);
                        return;
                    }
                    if let Err(e) = conn.send(&LibToClient::Error(Error {
                        message: "advance frame count overflow".into(),
                    })) {
                        error!(Ipc, "send advance error: {e}");
                        return;
                    }
                }
            },
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
