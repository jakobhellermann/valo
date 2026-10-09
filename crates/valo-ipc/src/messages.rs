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
    pub keys: KeyBitset,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct KeyBitset([u8; 32]);

impl KeyBitset {
    pub fn is_held(&self, keycode: u8) -> bool {
        self.0[keycode as usize / 8] & (1 << (keycode % 8)) != 0
    }

    pub fn set(&mut self, keycode: u8, held: bool) {
        let bit = 1 << (keycode % 8);
        let byte = &mut self.0[keycode as usize / 8];
        if held {
            *byte |= bit;
        } else {
            *byte &= !bit;
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_bitset_sets_bit_n_for_keycode_n() {
        let mut keys = KeyBitset::default();
        keys.set(9, true);
        keys.set(255, true);
        keys.set(38, true);
        keys.set(38, false);

        let mut expected = [0u8; 32];
        expected[1] = 0b10;
        expected[31] = 0b1000_0000;
        assert_eq!(keys, KeyBitset(expected));
        assert_eq!(
            (0..=u8::MAX)
                .filter(|&k| keys.is_held(k))
                .collect::<Vec<_>>(),
            vec![9, 255]
        );
    }
}
