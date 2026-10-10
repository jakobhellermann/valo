pub mod gpu_session;

pub mod command;
pub mod paths;

use std::path::PathBuf;

use anyhow::{Context, Result, bail};

/// Resolves the libvalo path, falling back to `libvalo.so` next to the current binary.
pub fn resolve_lib(explicit: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(p) = explicit {
        if !p.exists() {
            bail!("--lib path does not exist: {}", p.display());
        }
        return Ok(p);
    }
    let exe = std::env::current_exe().context("current_exe failed")?;
    let dir = exe
        .parent()
        .with_context(|| format!("binary {} has no parent dir", exe.display()))?;
    let candidate = dir.join("libvalo.so");
    if candidate.exists() {
        return Ok(candidate);
    }
    bail!(
        "libvalo.so not found next to the binary at {}",
        candidate.display()
    )
}
