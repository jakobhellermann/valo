//! Home of all runtime files.

use std::fs::Dir;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::Result;

static HOME: OnceLock<PathBuf> = OnceLock::new();

pub fn valo_home() -> Option<PathBuf> {
    std::env::var_os("VALO_HOME").map(PathBuf::from)
}

/// Resolve and create the VALO_HOME directory.
pub fn init() -> Result<()> {
    let home = match valo_home() {
        Some(home) => home,
        None => std::env::temp_dir().join(format!("valo-{}", std::process::id())),
    };

    std::fs::create_dir_all(&home).map_err(|e| {
        std::io::Error::other(format!(
            "failed to create VALO_HOME {}: {e}",
            home.display()
        ))
    })?;

    HOME.set(home).expect("home initialized twice");

    Ok(())
}

pub fn get() -> &'static Path {
    HOME.get().expect("home used before init")
}

pub fn open() -> Result<Dir, std::io::Error> {
    Dir::open(get())
}
