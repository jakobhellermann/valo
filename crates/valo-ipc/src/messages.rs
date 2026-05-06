use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    Lib,
    Client,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    pub version: u32,
    pub role: Role,
}

impl Hello {
    pub const CLIENT: Hello = Hello {
        version: crate::PROTOCOL_VERSION,
        role: Role::Client,
    };
    pub const LIB: Hello = Hello {
        version: crate::PROTOCOL_VERSION,
        role: Role::Lib,
    };
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientToLib {
    Hello(Hello),
    /// Run the game until the next frame boundary.
    Advance,
    Checkpoint,
    Restore,
    GetState,
    Quit,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    pub frame: u64,
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
    Hello(Hello),
    FrameBoundary(FrameBoundary),
    State(State),
    /// Sent after a successful `Checkpoint`. The lib then loops back to
    /// recv another command (typically `Advance`).
    CheckpointDone,
    RestoreSucceeded,
    Error(Error),
}
