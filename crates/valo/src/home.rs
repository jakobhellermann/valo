//! Home of all runtime files.

use std::fs::{Dir, OpenOptions};
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

    let from_client = valo_home().is_some() && std::env::var_os("VALO_SOCK").is_some();
    prepare(&home, from_client)?;

    if valo_home().is_none() {
        lock(&home)?;
    }

    point_latest_symlink(&home);

    HOME.set(home).expect("home initialized twice");

    Ok(())
}

/// A client creates the home before spawning the target and shares files in it with the target.
fn prepare(home: &Path, from_client: bool) -> Result<()> {
    if from_client {
        if home.is_dir() {
            return Ok(());
        }
        return Err(std::io::Error::other(format!(
            "VALO_HOME {} does not exist for this process, although a client is attached \
             (VALO_SOCK is set)",
            home.display()
        )));
    }
    std::fs::create_dir_all(home).map_err(|e| {
        std::io::Error::other(format!(
            "failed to create VALO_HOME {}: {e}",
            home.display()
        ))
    })
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

/// Mark as owned until this process exits to prevent cleanup.
fn lock(home: &Path) -> Result<()> {
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(home.join("lock"))?;
    file.try_lock()?;
    std::mem::forget(file);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_home_must_exist() {
        let home =
            std::env::temp_dir().join(format!("valo-home-test-missing-{}", std::process::id()));
        let err = prepare(&home, true).unwrap_err();
        assert!(err.to_string().contains("valo-home-test-missing"), "{err}");
        assert!(!home.exists());
    }

    #[test]
    fn own_home_is_created() {
        let home = std::env::temp_dir().join(format!("valo-home-test-own-{}", std::process::id()));
        prepare(&home, false).unwrap();
        assert!(home.is_dir());
        prepare(&home, true).unwrap();
        std::fs::remove_dir(&home).unwrap();
    }
}
