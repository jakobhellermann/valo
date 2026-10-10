pub mod gpu_session;

pub mod cleanup;
pub mod command;
pub mod paths;

use std::ffi::OsString;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

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

pub fn resolve_gfxstream_icd() -> Result<Option<PathBuf>> {
    if let Some(p) = non_empty_env("VALO_GFXSTREAM_ICD") {
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
            n.starts_with(b"gfxstream_vk_icd") && n.ends_with(b".json")
        });
        if is_icd {
            hits.push(path);
        }
    }
    match <[PathBuf; 1]>::try_from(hits) {
        Ok([icd]) => Ok(Some(icd)),
        Err(hits) if hits.is_empty() => Ok(None),
        Err(hits) => bail!("multiple gfxstream ICDs in {}: {hits:?}", dir.display()),
    }
}
