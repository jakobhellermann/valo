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

    if valo_home().is_none() {
        lock(&home)?;
    }

    point_latest_symlink(&home);

    HOME.set(home).expect("home initialized twice");

    Ok(())
}

pub fn get() -> &'static Path {
    HOME.get().expect("home used before init")
}

/// Try to point `/tmp/valo-latest` at this run's home
fn point_latest_symlink(home: &Path) {
    let temp_dir = std::env::temp_dir();
    let temp = temp_dir.join(format!("valo-latest.new-{}", std::process::id()));
    let latest = temp_dir.join("valo-latest");
    let Ok(target) = std::path::absolute(home) else {
        return;
    };
    // Atomically swap. Last one wins.
    if std::os::unix::fs::symlink(&target, &temp).is_ok()
        && std::fs::rename(&temp, &latest).is_err()
    {
        // rename failed, clean up symlink
        let _ = std::fs::remove_file(&temp);
    }
}

pub fn open() -> Result<Dir, std::io::Error> {
    Dir::open(get())
}

/// Mark the home as owned until this process exits,
/// in order to prevent cleanup sweeps.
fn lock(home: &Path) -> Result<()> {
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(home.join("lock"))?;
    file.try_lock()?;
    // Hold the lock until process exit; the fd must not close earlier.
    std::mem::forget(file);
    Ok(())
}
