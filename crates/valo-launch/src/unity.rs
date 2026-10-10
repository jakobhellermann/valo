// TODO(ai-review): generated, not yet audited
//! Unity games: engine version and the player symbols the lib hooks.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use valo_ipc::config::ResolvedModule;
use valo_ipc::gamespecific::unity;

/// The `<game>_Data` directory next to a Unity player binary, if it exists.
pub fn data_dir(program: &Path) -> Option<PathBuf> {
    let stem = program.file_stem()?.to_str()?;
    let dir = program.parent()?.join(format!("{stem}_Data"));
    dir.is_dir().then_some(dir)
}

/// Engine version from the header of `globalgamemanagers`, e.g. `6000.0.61f1`.
pub fn version(data_dir: &Path) -> Result<String> {
    let path = data_dir.join("globalgamemanagers");
    let header = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
    let format = u32::from_be_bytes(header.get(8..12).context("short header")?.try_into()?);
    // serialized file format 22 moved the header fields to 64 bit
    let start = if format >= 22 { 0x30 } else { 0x14 };
    let rest = header.get(start..).context("short header")?;
    let end = rest
        .iter()
        .position(|&b| b == 0)
        .context("unterminated version")?;
    let version = std::str::from_utf8(&rest[..end]).context("non-utf8 version")?;
    if version.is_empty() {
        bail!("empty version in {}", path.display());
    }
    Ok(version.to_string())
}

/// Resolves the player symbols the lib hooks, from `UnityPlayer.so` next to `program`.
pub fn resolve_player(program: &Path) -> Result<ResolvedModule> {
    let player = program
        .parent()
        .context("program path has no parent")?
        .join(unity::MODULE);
    crate::symbols::resolve_module(&player, unity::SYMBOLS)
}
