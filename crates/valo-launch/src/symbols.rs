// TODO(ai-review): generated, not yet audited
//! Resolve symbols of a stripped module from its separate debug file, for
//! hooks the lib installs at load base + symbol address.

use std::collections::HashSet;
use std::path::Path;

use anyhow::{Context, Result, bail};
use object::{Object, ObjectSymbol};
use valo_ipc::config::ResolvedModule;

/// Resolves `names` (mangled) of the module at `path`. The symbols come from
/// the file its `.gnu_debuglink` names, next to the module, whose build id
/// must match.
pub fn resolve_module(path: &Path, names: &[&str]) -> Result<ResolvedModule> {
    let data = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let module =
        object::File::parse(&*data).with_context(|| format!("parse {}", path.display()))?;
    let build_id = module
        .build_id()?
        .with_context(|| format!("{} has no build id", path.display()))?
        .to_vec();
    let (debug_name, _crc) = module
        .gnu_debuglink()?
        .with_context(|| format!("{} has no .gnu_debuglink", path.display()))?;
    let debug_path = path
        .parent()
        .context("module path has no parent")?
        .join(std::str::from_utf8(debug_name).context("non-utf8 debuglink")?);

    let debug_data =
        std::fs::read(&debug_path).with_context(|| format!("read {}", debug_path.display()))?;
    let debug = object::File::parse(&*debug_data)
        .with_context(|| format!("parse {}", debug_path.display()))?;
    if debug.build_id()? != Some(&build_id[..]) {
        bail!(
            "{} does not belong to {} (build id mismatch)",
            debug_path.display(),
            path.display()
        );
    }

    let wanted: HashSet<&str> = names.iter().copied().collect();
    let mut symbols = Vec::new();
    for symbol in debug.symbols() {
        if let Ok(name) = symbol.name()
            && wanted.contains(name)
        {
            symbols.push((name.to_string(), symbol.address()));
        }
    }
    let missing: Vec<&&str> = names
        .iter()
        .filter(|name| !symbols.iter().any(|(found, _)| found == *name))
        .collect();
    if !missing.is_empty() {
        bail!("{} lacks symbols {missing:?}", debug_path.display());
    }

    Ok(ResolvedModule {
        name: path
            .file_name()
            .context("module path has no file name")?
            .to_string_lossy()
            .into_owned(),
        build_id,
        symbols,
    })
}
