// TODO(ai-review): generated, not yet audited
//! Bind mounts that only the target sees, in a user and mount namespace of its own.

use std::ffi::{CStr, CString};
use std::fs::File;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::str::FromStr;

use anyhow::{Context, Result, anyhow, bail};
use rustix::fs::{Mode, OFlags};
use rustix::mount::mount_bind;
use rustix::process::{getgid, getuid};
use rustix::thread::{UnshareFlags, unshare_unsafe};

/// `SRC:DEST`: the target sees the contents of `SRC` at `DEST`.
#[derive(Debug, Clone)]
pub struct Bind {
    pub src: PathBuf,
    pub dest: PathBuf,
}

impl FromStr for Bind {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Bind> {
        let (src, dest) = s
            .split_once(':')
            .ok_or_else(|| anyhow!("expected SRC:DEST, got {s:?}"))?;
        if src.is_empty() || dest.is_empty() {
            bail!("expected SRC:DEST, got {s:?}");
        }
        Ok(Bind {
            src: src.into(),
            dest: dest.into(),
        })
    }
}

/// Makes the target run with `binds` mounted.
pub fn bind_mounts(cmd: &mut Command, binds: &[Bind]) -> Result<()> {
    if binds.is_empty() {
        return Ok(());
    }
    let mounts = binds
        .iter()
        .map(|bind| {
            let dest = resolve(&bind.dest)?;
            create_missing_src(bind)?;
            Ok((resolve(&bind.src)?, dest))
        })
        .collect::<Result<Vec<_>>>()?;
    for bind in binds {
        eprintln!(
            "[valo-launch] bind {} at {}",
            bind.src.display(),
            bind.dest.display()
        );
    }
    let uid_map = format!("{0} {0} 1", getuid().as_raw());
    let gid_map = format!("{0} {0} 1", getgid().as_raw());
    // SAFETY: the closure only issues syscalls on data formatted before the fork, no allocation.
    unsafe {
        cmd.pre_exec(move || {
            // SAFETY: the forked child is single threaded, no other thread shares its fd table.
            unshare_unsafe(UnshareFlags::NEWUSER | UnshareFlags::NEWNS)?;
            write_proc(c"/proc/self/setgroups", b"deny")?;
            write_proc(c"/proc/self/uid_map", uid_map.as_bytes())?;
            write_proc(c"/proc/self/gid_map", gid_map.as_bytes())?;
            for (src, dest) in &mounts {
                mount_bind(src.as_c_str(), dest.as_c_str())?;
            }
            Ok(())
        });
    }
    Ok(())
}

/// Creates `src` like `dest`, as an empty directory or file.
fn create_missing_src(bind: &Bind) -> Result<()> {
    let Bind { src, dest } = bind;
    if src
        .try_exists()
        .with_context(|| format!("bind path {}", src.display()))?
    {
        return Ok(());
    }
    eprintln!("[valo-launch] creating bind source {}", src.display());
    if dest.is_dir() {
        std::fs::create_dir_all(src)
    } else {
        if let Some(parent) = src.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        File::create_new(src).map(drop)
    }
    .with_context(|| format!("create {}", src.display()))
}

fn resolve(path: &Path) -> Result<CString> {
    let path = path
        .canonicalize()
        .with_context(|| format!("bind path {}", path.display()))?;
    Ok(CString::new(path.as_os_str().as_bytes())?)
}

fn write_proc(path: &CStr, contents: &[u8]) -> rustix::io::Result<()> {
    let fd = rustix::fs::open(path, OFlags::WRONLY | OFlags::CLOEXEC, Mode::empty())?;
    let written = rustix::io::write(&fd, contents)?;
    if written != contents.len() {
        return Err(rustix::io::Errno::IO);
    }
    Ok(())
}
