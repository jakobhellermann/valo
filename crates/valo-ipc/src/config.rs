// TODO(ai-review): generated, not yet audited
//! The lib's runtime config.
//!
//! Sent as a full replacement — with `ClientToLib::Init` before anything
//! runs, then via `ClientToLib::SetConfig` at frame boundaries. The
//! `LibHello` handshake echo exposes the config the lib currently has, so
//! clients can learn the state and patch and re-set it. Settings fixed for
//! the session travel in `Init` only.

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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum RestoreRedraw {
    #[default]
    None,
    /// Before the rewind, so the frame shows while the restore runs
    Early,
    /// After the rewind
    Late,
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

    pub fn frametime(&self) -> Duration {
        Duration::from_nanos(self.denom.get() as u64 * 1_000_000_000 / self.num.get() as u64)
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RandomnessConfig {
    pub uncontrolled_randomness: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DeterminismConfig {
    pub time: TimeConfig,
    pub randomness: RandomnessConfig,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OverlayConfig {
    /// Draw the shapes the game submits through `valo_overlay_shapes` over its frames.
    pub enabled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub frames: FrameConfig,
    pub log: LogConfig,
    pub determinism: DeterminismConfig,
    /// Which frames to skip/draw
    pub skip_draw: SkipDrawMode,
    /// When a restore paints back the window pixels stored with the checkpoint.
    pub restore_redraw: RestoreRedraw,
    pub input: InputConfig,
    pub overlay: OverlayConfig,
}

/// Symbols of a loaded module, resolved by the client from its symbol table
/// (for stripped modules from the separate debug file).
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ResolvedModule {
    /// File name of the module, e.g. `UnityPlayer.so`.
    pub name: String,
    /// GNU build id the symbols belong to; the lib rejects a loaded module
    /// with a different one.
    pub build_id: Vec<u8>,
    /// Mangled name and address relative to the module's load base.
    pub symbols: Vec<(String, u64)>,
}
