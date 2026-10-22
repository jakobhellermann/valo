//! Explains a target that does not connect: which processes below the launched command loaded
//! libvalo, against the `VALO_TARGET` they were checked for.

use std::ffi::OsStr;
use std::fmt::Write;
use std::path::PathBuf;

#[derive(Debug)]
pub(crate) struct Process {
    pid: u32,
    ppid: u32,
    exe: Option<PathBuf>,
    loads_valo: bool,
}

pub(crate) fn explain(root: u32, target: Option<&OsStr>, processes: &[Process]) -> String {
    let mut loading: Vec<&Process> = descendants(root, processes)
        .into_iter()
        .filter(|process| process.loads_valo)
        .collect();
    loading.sort_by_key(|process| process.pid);

    let mut msg = String::from("no target connected yet");
    if loading.is_empty() {
        let _ = write!(msg, "; no process below pid {root} has libvalo.so loaded");
        return msg;
    }
    match target {
        Some(target) => {
            let _ = write!(
                msg,
                "; VALO_TARGET={} must equal the file name of one of these executables:",
                target.display()
            );
        }
        None => msg.push_str("; VALO_TARGET is unset, the first of these attaches:"),
    }
    for process in loading {
        let name = process
            .exe
            .as_deref()
            .and_then(|exe| exe.file_name())
            .unwrap_or(OsStr::new("?"));
        let _ = write!(msg, "\n  {} (pid {})", name.display(), process.pid);
    }
    msg
}

fn descendants(root: u32, processes: &[Process]) -> Vec<&Process> {
    let mut found: Vec<&Process> = Vec::new();
    let mut parents = vec![root];
    while let Some(parent) = parents.pop() {
        for process in processes.iter().filter(|process| process.ppid == parent) {
            found.push(process);
            parents.push(process.pid);
        }
    }
    found
}

/// Processes that vanish or deny access while scanning are skipped.
pub(crate) fn scan() -> Vec<Process> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| entry.file_name().to_str()?.parse().ok())
        .filter_map(read_process)
        .collect()
}

fn read_process(pid: u32) -> Option<Process> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let ppid = parse_ppid(&stat)?;
    let exe = std::fs::read_link(format!("/proc/{pid}/exe")).ok();
    let loads_valo = std::fs::read_to_string(format!("/proc/{pid}/maps"))
        .is_ok_and(|maps| maps.lines().any(|line| line.ends_with("/libvalo.so")));
    Some(Process {
        pid,
        ppid,
        exe,
        loads_valo,
    })
}

/// The comm field in parentheses may itself contain spaces and parentheses.
fn parse_ppid(stat: &str) -> Option<u32> {
    let (_, rest) = stat.rsplit_once(')')?;
    rest.split_whitespace().nth(1)?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process(pid: u32, ppid: u32, exe: &str, loads_valo: bool) -> Process {
        Process {
            pid,
            ppid,
            exe: Some(PathBuf::from(exe)),
            loads_valo,
        }
    }

    #[test]
    fn lists_preloaded_descendants_against_the_target() {
        let processes = [
            process(10, 1, "/usr/bin/valo", false),
            process(11, 10, "/nix/store/x-bash/bin/bash", true),
            process(12, 11, "/usr/bin/bwrap", false),
            process(13, 12, "/games/Hollow Knight/hollow_knight.x86_64", true),
            process(20, 1, "/usr/bin/unrelated", true),
        ];
        assert_eq!(
            explain(10, Some(OsStr::new("Hollow Knight")), &processes),
            "no target connected yet; VALO_TARGET=Hollow Knight must equal the file name of one \
             of these executables:\n  bash (pid 11)\n  hollow_knight.x86_64 (pid 13)"
        );
    }

    #[test]
    fn reports_when_nothing_below_loads_valo() {
        let processes = [process(11, 10, "/bin/sh", false)];
        assert_eq!(
            explain(10, None, &processes),
            "no target connected yet; no process below pid 10 has libvalo.so loaded"
        );
    }

    #[test]
    fn ppid_follows_a_comm_with_parentheses() {
        assert_eq!(parse_ppid("42 (a) b (c)) S 7 42 42 0"), Some(7));
    }

    #[test]
    fn scan_finds_this_process() {
        let me = std::process::id();
        assert!(scan().iter().any(|process| process.pid == me));
    }
}
