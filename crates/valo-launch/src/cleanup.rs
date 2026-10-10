// TODO(ai-review): generated, not yet audited
//! Removing savestate bulk data of finished and abandoned run homes.
//!
//! A home is owned while any process of its session lives: the creating
//! launcher flocks `<home>/lock` for its lifetime and the target inherits
//! the lock fd, so the kernel holds the lock until the last fd holder
//! exits. A lock that can be acquired therefore marks an abandoned home.
//!
//! Only the bulk data under `slots/` (CPU snapshot pages, GPU snapshots) is
//! removed. Logs are kept for post-mortem inspection; the remaining home
//! shell is tiny and vanishes with the tmpfs.

use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use crate::paths;

/// Delete the savestate data of leftover named homes
pub fn clean_stale_homes() {
    let temp = std::env::temp_dir();
    let Ok(entries) = fs::read_dir(&temp) else {
        return;
    };
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().into_string().ok() else {
            continue;
        };
        let Some(pid) = default_home_pid(&name) else {
            continue;
        };
        if Path::new(&format!("/proc/{pid}")).exists() {
            continue;
        }
        let home = temp.join(&name);
        if !home.is_dir() {
            continue;
        }
        if claim(&home) {
            remove_slot_data(&home);
        }
    }
}

/// Remove this run's savestate data.
pub fn remove_savestate_data() {
    remove_slot_data(paths::valo_home());
}

/// Extract the pid of a default-named home (`valo-<pid>`), `None` otherwise.
fn default_home_pid(name: &str) -> Option<u32> {
    let pid = name.strip_prefix("valo-")?;
    if pid.is_empty() || !pid.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    pid.parse().ok()
}

/// Try to acquire the home lock.
/// true -> unclaimed
fn claim(home: &Path) -> bool {
    let file = match fs::OpenOptions::new().write(true).open(home.join("lock")) {
        Ok(file) => file,
        Err(e) if e.kind() == ErrorKind::NotFound => return true,
        Err(_) => return false,
    };
    file.try_lock().is_ok()
}

fn remove_slot_data(home: &Path) {
    let slots = home.join("slots");
    match fs::remove_dir_all(&slots) {
        Ok(()) => eprintln!("[valo] removed savestate data {}", slots.display()),
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        Err(e) => eprintln!("[valo] warning: removing {} failed: {e}", slots.display()),
    }
}
