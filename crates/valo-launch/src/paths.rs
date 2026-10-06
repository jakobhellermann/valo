use std::path::{Path, PathBuf};
use std::sync::OnceLock;

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
        home
    })
}

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
