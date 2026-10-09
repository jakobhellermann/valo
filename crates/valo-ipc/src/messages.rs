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
    /// The input state from the next frame boundary on.
    SetInputs(InputState),
    GetState,
    Quit,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    pub frame: u64,
}

/// The full keyboard state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputState {
    /// X modifier mask (KeyButMask) of the key events.
    pub mods: u16,
    /// Held keycodes, bit `n` for keycode `n`.
    pub keys: [u8; 32],
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameBoundary {
    pub frame: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreSucceeded {
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
    /// The lib is held in the restored `frame` and waits for commands, as after a `FrameBoundary`.
    RestoreSucceeded(RestoreSucceeded),
    SetConfigDone,
    Error(Error),
}
