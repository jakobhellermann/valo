// TODO(ai-review): generated, not yet audited
//! The lib's runtime config.
//!
//! Sent as a full replacement via `ClientToLib::SetConfig` — at init
//! (applied before anything runs) and as a runtime command at frame
//! boundaries (acked with `LibToClient::SetConfigDone`). The `LibHello`
//! handshake echo exposes the config the lib currently has, so clients can
//! learn the state and patch and re-set it. `use_gpu` is fixed at init.

use serde::{Deserialize, Serialize};

/// Ordered from least to most verbose.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
    #[default]
    Trace,
}

#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LogConfig {
    pub level: LogLevel,
}

#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct FrameConfig {
    /// 0 = disabled
    pub checkpoint_frame: u64,
    /// 0 = disabled
    pub restore_frame: u64,
    /// 0 = unlimited
    pub max_restores: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Framerate {
    pub num: u64,
    pub denom: u64,
}

impl Default for Framerate {
    fn default() -> Self {
        Self { num: 60, denom: 1 }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TimeConfig {
    // Lets time run controlled
    pub uncontrolled_time: bool,
    /// Simulated frame rate
    pub framerate: Framerate,
}

#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub frames: FrameConfig,
    pub log: LogConfig,
    /// GPU checkpoint orchestration. Must match what the client set up for
    /// the target (kumquat host, renderer selection).
    pub use_gpu: bool,
    /// Deterministic simulated time.
    pub time: TimeConfig,
    /// skip_draw: skip the swapchain image pipeline entirely (fake
    /// acquire, drained present, libTAS `skipping_draw` parity). Session
    /// state — applied at frame boundaries, survives restores unchanged.
    pub skip_draw: bool,
}
