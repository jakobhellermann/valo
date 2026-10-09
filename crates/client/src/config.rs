// TODO(ai-review): generated, not yet audited
use clap::{Args, ValueEnum};
use valo_ipc::config::{
    Config, DeterminismConfig, FrameConfig, InputConfig, LogConfig, LogLevel, RandomnessConfig,
    SkipDrawMode, TimeConfig,
};

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

    /// Don't force any determinism
    #[arg(long, help_heading = "Runtime")]
    pub uncontrolled: bool,

    /// Don't force deterministic time
    #[arg(long, help_heading = "Runtime")]
    pub uncontrolled_time: bool,

    /// Don't force deterministic randomness sources
    #[arg(long, help_heading = "Runtime")]
    pub uncontrolled_randomness: bool,

    /// Let interactive input through to the game
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
            use_gpu: !self.software_rendering,
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
