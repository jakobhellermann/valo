// TODO(ai-review): generated, not yet audited
//! The lib's runtime config.
//!
//! Sent as a full replacement via `ClientToLib::SetConfig` — at init
//! (applied before anything runs) and as a runtime command at frame
//! boundaries (acked with `LibToClient::SetConfigDone`). The `LibHello`
//! handshake echo exposes the config the lib currently has, so clients can
//! learn the state and patch and re-set it. `use_gpu` is fixed at init.

use std::num::NonZeroU32;
use std::time::{Duration, SystemTime};

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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum SkipDrawMode {
    /// Normal playback
    #[default]
    Off,
    /// Every frame skips
    All,
    /// Render one frame per `min_render_interval_ms`
    Interval { min_render_interval_ms: u32 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Framerate {
    pub num: NonZeroU32,
    pub denom: NonZeroU32,
}

impl Framerate {
    pub const fn integer(fps: u32) -> Framerate {
        Framerate {
            num: NonZeroU32::new(fps).unwrap(),
            denom: NonZeroU32::MIN,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TimeConfig {
    pub uncontrolled_time: bool,
    pub framerate: Framerate,
    /// `CLOCK_REALTIME` at process start.
    pub realtime_start: SystemTime,
}

impl Default for TimeConfig {
    fn default() -> Self {
        Self {
            uncontrolled_time: Default::default(),
            framerate: Framerate::integer(60),
            realtime_start: SystemTime::UNIX_EPOCH + Duration::from_secs(1),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct InputConfig {
    /// The game reads real X input instead of the `SetInputs` states.
    pub uncontrolled_input: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub frames: FrameConfig,
    pub log: LogConfig,
    /// GPU checkpoint orchestration. Must match what the client set up for
    /// the target (kumquat host, renderer selection).
    pub use_gpu: bool,
    pub time: TimeConfig,
    /// Which frames to skip/draw
    pub skip_draw: SkipDrawMode,
    pub input: InputConfig,
}
