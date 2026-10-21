//! Savestate toasts derived from the IPC traffic between client and lib.

use valo_ipc::messages::{ClientToLib, LibToClient};

use crate::overlay::{Overlay, ToastKind};

pub struct Hud {
    overlay: Overlay,
    pending_slot: Option<u32>,
}

impl Hud {
    pub fn new(overlay: Overlay) -> Self {
        Self {
            overlay,
            pending_slot: None,
        }
    }

    pub fn sent(&mut self, msg: &ClientToLib) {
        if let ClientToLib::Checkpoint(slot) | ClientToLib::Restore(slot) = msg {
            self.pending_slot = Some(*slot);
        }
    }

    pub fn received(&mut self, msg: &LibToClient) {
        match msg {
            LibToClient::CheckpointDone => self.toast_op("saved"),
            LibToClient::RestoreSucceeded(_) => self.toast_op("loaded"),
            LibToClient::Error(e) => self.overlay.toast(ToastKind::Error, &e.message),
            _ => {}
        }
    }

    fn toast_op(&mut self, verb: &str) {
        let text = match self.pending_slot.take() {
            Some(slot) => format!("{verb} {slot}"),
            None => verb.to_string(),
        };
        self.overlay.toast(ToastKind::Info, text);
    }
}
