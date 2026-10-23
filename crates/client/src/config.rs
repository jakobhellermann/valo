// TODO(ai-review): generated, not yet audited
use std::ffi::OsString;
use std::process::Command;

use clap::{Args, ValueEnum};
use valo_ipc::config::{
    Config, DeterminismConfig, FrameConfig, InputConfig, LogConfig, LogLevel, RandomnessConfig,
    SkipDrawMode, TimeConfig,
};
use valo_launch::bind::{Bind, bind_mounts};

#[derive(Args)]
#[command(next_help_heading = "Auto checkpoints")]
pub(crate) struct ConfigArgs {
    /// Frame at which the lib checkpoints
    #[arg(long)]
    pub(crate) checkpoint_frame: Option<u64>,

    /// Frame at which the lib restores
    #[arg(long)]
    pub(crate) restore_frame: Option<u64>,

    /// Restore count at which the lib exits
    #[arg(long)]
    pub(crate) max_restores: Option<u64>,

    #[arg(long, value_enum, help_heading = "Runtime")]
    pub(crate) log_level: Option<CliLogLevel>,

    /// Disable GPU checkpoints and render in software
    #[arg(long, help_heading = "Runtime")]
    pub(crate) software_rendering: bool,

    /// File name of the executable valo attaches to, if behind a wrapper
    #[arg(long = "target", value_name = "NAME", help_heading = "Runtime")]
    pub(crate) attach_target: Option<OsString>,

    /// Show the target the contents of SRC at DEST, e.g. to keep its saves away from the real ones
    #[arg(long, value_name = "SRC:DEST", help_heading = "Runtime")]
    pub(crate) bind: Vec<Bind>,

    /// Don't force any determinism
    #[arg(long, help_heading = "Runtime")]
    pub uncontrolled: bool,

    /// Don't force deterministic time
    #[arg(long, help_heading = "Runtime")]
    pub uncontrolled_time: bool,

    /// Don't force deterministic randomness sources
    #[arg(long, help_heading = "Runtime")]
    pub uncontrolled_randomness: bool,

    #[arg(long, help_heading = "Runtime")]
    pub uncontrolled_input: bool,
}

#[derive(ValueEnum, Clone, Copy)]
pub(crate) enum CliLogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl From<CliLogLevel> for LogLevel {
    fn from(level: CliLogLevel) -> Self {
        match level {
            CliLogLevel::Error => LogLevel::Error,
            CliLogLevel::Warn => LogLevel::Warn,
            CliLogLevel::Info => LogLevel::Info,
            CliLogLevel::Debug => LogLevel::Debug,
            CliLogLevel::Trace => LogLevel::Trace,
        }
    }
}

impl ConfigArgs {
    /// Applies the settings that reach the target through its command rather than the IPC config.
    pub(crate) fn apply(&self, cmd: &mut Command) -> anyhow::Result<()> {
        bind_mounts(cmd, &self.bind)?;
        if let Some(target) = &self.attach_target {
            cmd.env("VALO_TARGET", target);
        }
        Ok(())
    }

    pub(crate) fn config(&self) -> Config {
        Config {
            frames: FrameConfig {
                checkpoint_frame: self.checkpoint_frame.unwrap_or(0),
                restore_frame: self.restore_frame.unwrap_or(0),
                max_restores: self.max_restores.unwrap_or(0),
            },
            log: LogConfig {
                level: self.log_level.map_or(LogLevel::Trace, Into::into),
            },
            restore_redraw: Default::default(),
            overlay: Default::default(),
            determinism: DeterminismConfig {
                time: TimeConfig {
                    uncontrolled_time: self.uncontrolled_time || self.uncontrolled,
                    ..TimeConfig::default()
                },
                randomness: RandomnessConfig {
                    uncontrolled_randomness: self.uncontrolled_randomness || self.uncontrolled,
                },
            },
            skip_draw: SkipDrawMode::Off,
            input: InputConfig {
                uncontrolled_input: self.uncontrolled_input || self.uncontrolled,
            },
        }
    }
}
