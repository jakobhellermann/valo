use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::cleanup;

/// Create and resolve the per-run valo directory
pub fn valo_home() -> &'static Path {
    static HOME: OnceLock<PathBuf> = OnceLock::new();
    HOME.get_or_init(|| {
        let home = std::env::var_os("VALO_HOME")
            .map(PathBuf::from)
            .filter(|h| !h.as_os_str().is_empty())
            .unwrap_or_else(|| std::env::temp_dir().join(format!("valo-{}", std::process::id())));
        std::fs::create_dir_all(&home)
            .unwrap_or_else(|e| panic!("create valo home {}: {e}", home.display()));
        lock(&home);
        cleanup::clean_stale_homes();
        home
    })
}

/// Raw fd of the home lock, held for this process's lifetime. The target
/// inherits it across exec (see `command::configure_target`), so the lock
/// marks the home as owned until the whole session is gone.
pub fn lock_fd() -> Option<i32> {
    LOCK_FD.get().copied()
}

/// Pages file prepared for the next checkpoint into a new slot: `<home>/slots/<SPARE_PAGES>`.
pub const SPARE_PAGES: &str = "spare-pages";

/// Socket path for IPC with the target: `<home>/ipc.sock`.
pub fn ipc_sock() -> &'static PathBuf {
    static SOCK: OnceLock<PathBuf> = OnceLock::new();
    SOCK.get_or_init(|| valo_home().join("ipc.sock"))
}

/// Log path of the target: `$VALO_LOG` or `<home>/valo.log`
// TODO: deduplicate with valo/log
pub fn log_path() -> PathBuf {
    std::env::var_os("VALO_LOG")
        .map(PathBuf::from)
        .unwrap_or_else(|| valo_home().join("valo.log"))
}

static LOCK_FD: OnceLock<i32> = OnceLock::new();

fn lock(home: &Path) {
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(home.join("lock"))
        .unwrap_or_else(|e| panic!("open {} lock: {e}", home.display()));
    if file.try_lock().is_err() {
        let e = std::io::Error::last_os_error();
        panic!(
            "valo home {} is locked by a live session: {e}",
            home.display()
        );
    }
    // Leak the fd deliberately: the kernel releases the lock when the
    // process (and every fd-inheriting child) exits.
    let fd = std::os::unix::io::AsRawFd::as_raw_fd(&file);
    std::mem::forget(file);
    LOCK_FD.set(fd).expect("home lock initialized twice");
}
