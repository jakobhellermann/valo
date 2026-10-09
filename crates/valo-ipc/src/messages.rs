use serde::{Deserialize, Serialize};

use crate::config::Config;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    Lib,
    Client,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientHello {
    pub version: u32,
    pub role: Role,
}

impl ClientHello {
    pub const fn new() -> Self {
        Self {
            version: crate::PROTOCOL_VERSION,
            role: Role::Client,
        }
    }
}

impl Default for ClientHello {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibHello {
    pub version: u32,
    pub role: Role,
    /// The config the lib currently has (init or runtime SetConfig).
    pub config: Config,
}

impl LibHello {
    pub const fn new(config: Config) -> Self {
        Self {
            version: crate::PROTOCOL_VERSION,
            role: Role::Lib,
            config,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientToLib {
    Hello(ClientHello),
    /// Run N frames, then stop at the next boundary.
    AdvanceFrames(u64),
    /// Replace the runtime config.
    SetConfig(Config),
    /// held → free: frames run by default.
    Detach,
    /// Snapshot into the given savestate slot.
    Checkpoint(u32),
    /// Rewind to the snapshot in the given slot.
    Restore(u32),
    /// Full input state for feeding; applied at the next frame boundary.
    SetInputs(InputState),
    GetState,
    Quit,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    pub frame: u64,
}

/// Full keyboard state for input feeding. Sent as a whole replacement
/// per frame boundary; the lib diffs it against the previous state and
/// synthesizes xcb KeyPress/KeyRelease events for the changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputState {
    /// Modifier mask written into the events' `state` field (KeyButMask
    /// bits, computed by the client from its keymap).
    pub mods: u16,
    /// Held keycodes as a 256-bit bitmap (bit `kc` = keycode `kc`).
    pub keys: [u8; 32],
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameBoundary {
    pub frame: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Error {
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LibToClient {
    Hello(LibHello),
    FrameBoundary(FrameBoundary),
    State(State),
    /// Sent after a successful `Checkpoint`. The lib then loops back to
    /// recv another command (typically `Advance`).
    CheckpointDone,
    RestoreSucceeded,
    SetConfigDone,
    Error(Error),
}
