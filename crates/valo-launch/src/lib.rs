pub mod gpu_session;

pub mod bind;
pub mod cleanup;
pub mod command;
pub mod paths;
pub mod setup_lib;
pub mod symbols;
pub mod unity;

use std::ffi::OsString;
use std::fs::File;
use std::io::ErrorKind;
use std::os::fd::BorrowedFd;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use valo_ipc::ClientIPC;
use valo_ipc::config::Config;

/// Resolves the modules from the socket peer's exe: behind wrappers, the spawned program is not the game.
pub fn init_game(
    conn: &mut ClientIPC,
    config: &Config,
    gpu_control: Option<BorrowedFd<'_>>,
) -> Result<u32> {
    let pid = conn.peer_pid().context("game pid from socket peer")?;
    let exe = std::fs::read_link(format!("/proc/{pid}/exe"))
        .with_context(|| format!("resolve exe of game pid {pid}"))?;
    eprintln!("[valo-launch] attached to pid {pid}: {}", exe.display());
    conn.init(config, unity::resolve_modules(&exe)?, gpu_control)?;
    Ok(pid)
}

fn exe_dir() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("current_exe failed")?;
    exe.parent()
        .map(Path::to_path_buf)
        .with_context(|| format!("binary {} has no parent dir", exe.display()))
}

fn non_empty_env(var: &str) -> Option<OsString> {
    std::env::var_os(var).filter(|v| !v.is_empty())
}

/// Resolves the libvalo path, falling back to `libvalo.so` next to the current binary.
pub fn resolve_lib(explicit: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(p) = explicit {
        if !p.exists() {
            bail!("--lib path does not exist: {}", p.display());
        }
        return Ok(p);
    }
    let candidate = exe_dir()?.join("libvalo.so");
    if candidate.exists() {
        return Ok(candidate);
    }
    bail!(
        "libvalo.so not found next to the binary at {}",
        candidate.display()
    )
}

pub fn resolve_kumquat_bin() -> Result<OsString> {
    if let Some(p) = non_empty_env("VALO_KUMQUAT_BIN") {
        return Ok(p);
    }
    let candidate = exe_dir()?.join("kumquat");
    let bundled = candidate
        .try_exists()
        .with_context(|| format!("stat {}", candidate.display()))?;
    Ok(match bundled {
        true => candidate.into_os_string(),
        false => OsString::from("kumquat"),
    })
}

pub fn is_wsl_gpu() -> bool {
    Path::new("/dev/dxg").exists()
}

/// Whether kumquat can share host visible memory with the game, or why not.
pub fn host_visible_memory_sharing() -> Result<(), &'static str> {
    if is_wsl_gpu() {
        return Err("the WSL GPU (/dev/dxg) cannot import guest memory");
    }
    if File::open("/dev/udmabuf").is_err() {
        return Err("/dev/udmabuf is not readable");
    }
    Ok(())
}

pub fn resolve_gfxstream_icd() -> Result<Option<PathBuf>> {
    resolve_icd("VALO_GFXSTREAM_ICD", "gfxstream_vk_icd")
}

/// The dozen ICD, bundled next to the gfxstream ICD.
pub fn resolve_dozen_icd() -> Result<Option<PathBuf>> {
    resolve_icd("VALO_DOZEN_ICD", "dzn_icd")
}

/// Library path with zink's libGLX_mesa and libgallium, bundled in lib/zink.
pub fn resolve_zink_library_path() -> Result<Option<OsString>> {
    if let Some(p) = non_empty_env("VALO_ZINK_LIBRARY_PATH") {
        return Ok(Some(p));
    }
    let dir = exe_dir()?.join("../lib/zink");
    let bundled = dir
        .try_exists()
        .with_context(|| format!("stat {}", dir.display()))?;
    Ok(bundled.then(|| dir.into_os_string()))
}

fn resolve_icd(var: &str, prefix: &str) -> Result<Option<PathBuf>> {
    if let Some(p) = non_empty_env(var) {
        return Ok(Some(PathBuf::from(p)));
    }
    let dir = exe_dir()?.join("../share/vulkan/icd.d");
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("read {}", dir.display())),
    };
    let mut hits = Vec::new();
    for entry in entries {
        let path = entry
            .with_context(|| format!("read {}", dir.display()))?
            .path();
        let is_icd = path.file_name().is_some_and(|n| {
            let n = n.as_encoded_bytes();
            n.starts_with(prefix.as_bytes()) && n.ends_with(b".json")
        });
        if is_icd {
            hits.push(path);
        }
    }
    match <[PathBuf; 1]>::try_from(hits) {
        Ok([icd]) => Ok(Some(icd)),
        Err(hits) if hits.is_empty() => Ok(None),
        Err(hits) => bail!("multiple {prefix} ICDs in {}: {hits:?}", dir.display()),
    }
}
