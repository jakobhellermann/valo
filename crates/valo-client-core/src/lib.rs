// TODO(ai-review): generated, not yet audited
//! Session handling shared by valo's clients.

use std::ffi::{OsStr, OsString};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, ExitStatus};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use valo_ipc::ClientIPC;
use valo_ipc::config::Config;
use valo_ipc::messages::{ClientToLib, LibToClient};
use valo_launch::gpu_session::GpuSession;
use valo_launch::paths;

mod stray_target;

const EXPLAIN_MISSING_TARGET_AFTER: Duration = Duration::from_secs(5);

/// `program` with libvalo and `extra_preloads` in front of the caller's `LD_PRELOAD`.
pub fn target_command(
    program: &Path,
    args: &[String],
    lib: &Path,
    extra_preloads: &[&Path],
) -> Command {
    let mut cmd = Command::new(program);
    cmd.args(args);
    cmd.env("LD_PRELOAD", ld_preload(lib, extra_preloads));
    cmd
}

pub fn ld_preload(lib: &Path, extra_preloads: &[&Path]) -> OsString {
    let mut value = lib.as_os_str().to_owned();
    for preload in extra_preloads {
        value.push(":");
        value.push(preload);
    }
    if let Some(existing) = std::env::var_os("LD_PRELOAD").filter(|v| !v.is_empty()) {
        value.push(":");
        value.push(existing);
    }
    value
}

/// A target running under libvalo.
///
/// Dropping a session that was not released kills the target and removes its
/// savestate data.
pub struct Session {
    child: Child,
    gpu: Option<GpuSession>,
    sock: PathBuf,
    /// `VALO_TARGET` as the target sees it.
    target: Option<OsString>,
    conn: Option<ClientIPC>,
    game_pid: Option<u32>,
    exited: Option<ExitStatus>,
    released: bool,
    spare: SparePages,
}

/// The lib refused a command; the session goes on.
#[derive(Debug)]
pub struct LibError(pub String);

impl Session {
    /// Spawns a target that the kernel kills when this process dies.
    pub fn spawn(cmd: Command, sock: &Path, use_gpu: bool) -> Result<Session> {
        Self::spawn_with(cmd, sock, use_gpu, true)
    }

    pub fn spawn_with(
        mut cmd: Command,
        sock: &Path,
        use_gpu: bool,
        parent_death_kills: bool,
    ) -> Result<Session> {
        claim_sock(sock)?;
        let gpu = if use_gpu {
            let gpu = GpuSession::start().context("start kumquat session")?;
            gpu.configure_game(&mut cmd);
            Some(gpu)
        } else {
            None
        };
        valo_launch::command::configure_target(&mut cmd, use_gpu)?;
        cmd.env("VALO_SOCK", sock);
        let target = target_env(&cmd);
        if parent_death_kills {
            // SAFETY: prctl is async-signal-safe and touches no state shared with the parent.
            unsafe {
                cmd.pre_exec(|| {
                    libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                    Ok(())
                });
            }
        }
        let child = cmd
            .spawn()
            .with_context(|| format!("spawning {}", cmd.get_program().to_string_lossy()))?;
        Ok(Session {
            child,
            gpu,
            sock: sock.to_owned(),
            target,
            conn: None,
            game_pid: None,
            exited: None,
            released: false,
            spare: SparePages::start(paths::valo_home().join("slots")),
        })
    }

    /// Connects to the target's lib and sends `config`; returns the game's pid,
    /// or `None` when the target exited first. Connecting again replaces the
    /// connection, e.g. when gdb reruns the inferior.
    pub fn connect(&mut self, config: &Config) -> Result<Option<u32>> {
        let Some(mut conn) =
            establish_or_child_exit(&self.sock, self.target.as_deref(), &mut self.child)?
        else {
            return Ok(None);
        };
        let control = self.gpu.as_ref().map(GpuSession::control_for_lib);
        let pid = valo_launch::init_game(&mut conn, config, control)?;
        self.conn = Some(conn);
        self.game_pid = Some(pid);
        Ok(Some(pid))
    }

    pub fn conn(&mut self) -> &mut ClientIPC {
        self.conn
            .as_mut()
            .expect("Session::connect succeeded before use")
    }

    /// The game's pid, which differs from the spawned process behind a wrapper.
    pub fn game_pid(&self) -> Option<u32> {
        self.game_pid
    }

    pub fn child_pid(&self) -> u32 {
        self.child.id()
    }

    pub fn gpu(&mut self) -> Option<&mut GpuSession> {
        self.gpu.as_mut()
    }

    pub fn send(&mut self, msg: &ClientToLib) -> Result<()> {
        Ok(self.conn().send(msg)?)
    }

    pub fn recv(&mut self) -> Result<Option<LibToClient>> {
        Ok(self.conn().recv()?)
    }

    pub fn checkpoint(&mut self, slot: u32) -> Result<Result<(), LibError>> {
        self.send(&ClientToLib::Checkpoint(slot))?;
        match self.recv()? {
            Some(LibToClient::CheckpointDone) => Ok(Ok(())),
            Some(LibToClient::Error(e)) => Ok(Err(LibError(e.message))),
            Some(other) => bail!("unexpected reply to checkpoint: {other:?}"),
            None => bail!("lib hung up waiting for the checkpoint"),
        }
    }

    /// The target exits at its next frame boundary.
    pub fn quit(&mut self) -> Result<()> {
        self.send(&ClientToLib::Quit)
    }

    /// The target stops pausing at frame boundaries.
    pub fn detach(&mut self) -> Result<()> {
        self.send(&ClientToLib::Detach)
    }

    /// Closes the connection; the target stays held until another client connects.
    pub fn disconnect(&mut self) {
        self.conn = None;
    }

    /// The target keeps running without this session and keeps its savestate data.
    pub fn release(mut self) {
        self.released = true;
    }

    /// Waits for the target to exit, killing it after `grace`, then removes its
    /// savestate data.
    pub fn wait(&mut self, grace: Duration) -> Result<ExitStatus> {
        let status = match self.exited {
            Some(status) => status,
            None => wait_or_kill(&mut self.child, grace)?,
        };
        self.exited = Some(status);
        self.remove_files();
        Ok(status)
    }

    /// [`Session::wait`], mapped to the exit code a launcher reports: the
    /// target's own, 128 + signal, or 125 when the GPU host died under a
    /// successful target.
    pub fn finish(&mut self, grace: Duration) -> Result<ExitCode> {
        let status = self.wait(grace)?;
        eprintln!("[valo] target (pid {}) exited: {status}", self.child.id());
        let host_died = match self.gpu.as_mut() {
            Some(gpu) => report_kumquat_exit(gpu)?,
            None => false,
        };
        Ok(if host_died && status.success() {
            ExitCode::from(125)
        } else if let Some(code) = status.code() {
            ExitCode::from(u8::try_from(code).unwrap_or(1))
        } else if let Some(sig) = status.signal() {
            ExitCode::from(u8::try_from(128 + sig).unwrap_or(255))
        } else {
            ExitCode::from(1)
        })
    }

    /// Ends a hung session with core dumps of the game and the GPU host; exit code 125.
    pub fn abort_hung(&mut self, game_pid: u32) -> Result<ExitCode> {
        eprintln!("[valo] core dump: coredumpctl debug {game_pid}");
        eprintln!("[valo] valo log: {}", paths::log_path().display());
        let pid = rustix::process::Pid::from_raw(game_pid.cast_signed()).context("game pid")?;
        if let Err(e) = rustix::process::kill_process(pid, rustix::process::Signal::ABORT) {
            eprintln!("[valo] abort game pid {pid}: {e}");
        }
        if let Some(gpu) = self.gpu.as_mut() {
            if gpu.exit_status()?.is_none() {
                gpu.abort()?;
            }
            report_kumquat_exit(gpu)?;
        }
        self.wait(Duration::from_secs(10))?;
        Ok(ExitCode::from(125))
    }

    pub fn try_wait(&mut self) -> Result<Option<ExitStatus>> {
        if self.exited.is_none() {
            self.exited = self.child.try_wait()?;
        }
        Ok(self.exited)
    }
}

impl Session {
    fn remove_files(&mut self) {
        self.spare.stop();
        let _ = std::fs::remove_file(&self.sock);
        valo_launch::cleanup::remove_savestate_data();
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        if self.exited.is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        self.remove_files();
    }
}

/// Prints the per-thread kernel wait state of a game that stopped answering.
pub fn report_hang(pid: u32) -> Result<()> {
    if !Path::new(&format!("/proc/{pid}")).exists() {
        eprintln!("[valo] game pid {pid} is gone");
        return Ok(());
    }
    eprintln!("[valo] hang: game pid {pid} stopped answering");
    // CPU time spent across the window tells a slow frame from a deadlock.
    let before = thread_cpu_ticks(pid)?;
    std::thread::sleep(HANG_CPU_WINDOW);
    let after = thread_cpu_ticks(pid)?;
    let tick_ms = 1000 / rustix::param::clock_ticks_per_second();
    for (tid, ticks) in after {
        let busy_ms = before
            .iter()
            .find(|(before_tid, _)| *before_tid == tid)
            .map(|(_, before_ticks)| (ticks - before_ticks) * tick_ms);
        let read = |file: &str| {
            std::fs::read_to_string(format!("/proc/{pid}/task/{tid}/{file}")).unwrap_or_default()
        };
        eprintln!(
            "[valo]   tid {tid} ({}) wchan={} cpu={}",
            read("comm").trim(),
            read("wchan").trim(),
            busy_ms.map_or("new".to_string(), |ms| format!(
                "{ms}ms/{}ms",
                HANG_CPU_WINDOW.as_millis()
            )),
        );
    }
    Ok(())
}

const HANG_CPU_WINDOW: Duration = Duration::from_secs(1);

/// User plus system clock ticks of each thread of `pid`; threads that exit while reading are skipped.
fn thread_cpu_ticks(pid: u32) -> Result<Vec<(u32, u64)>> {
    let mut threads = Vec::new();
    for entry in std::fs::read_dir(format!("/proc/{pid}/task"))? {
        let tid: u32 = entry?.file_name().to_string_lossy().parse()?;
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/task/{tid}/stat")) else {
            continue;
        };
        if let Some(ticks) = cpu_ticks(&stat) {
            threads.push((tid, ticks));
        }
    }
    Ok(threads)
}

/// utime + stime of a `/proc/.../stat` line; comm may contain spaces and parentheses.
fn cpu_ticks(stat: &str) -> Option<u64> {
    let (_, rest) = stat.rsplit_once(')')?;
    let mut fields = rest.split_whitespace().skip(11);
    let utime: u64 = fields.next()?.parse().ok()?;
    let stime: u64 = fields.next()?.parse().ok()?;
    Some(utime + stime)
}

/// Returns whether the GPU host has exited, reporting how.
fn report_kumquat_exit(gpu: &mut GpuSession) -> Result<bool> {
    let Some(status) = gpu.exit_status()? else {
        return Ok(false);
    };
    let pid = gpu.pid();
    if status.signal() == Some(rustix::process::Signal::ABORT.as_raw()) {
        eprintln!("[valo] kumquat core dump: coredumpctl debug {pid}");
    } else {
        eprintln!("[valo] kumquat (pid {pid}) exited: {status}");
    }
    eprintln!("[valo] kumquat log: {}", gpu.log_path().display());
    Ok(true)
}

fn claim_sock(sock: &Path) -> Result<()> {
    if !sock.exists() {
        return Ok(());
    }
    if ClientIPC::connect(sock, Duration::from_millis(200)).is_ok() {
        bail!(
            "a live session owns {}; stop it or point VALO_HOME elsewhere",
            sock.display()
        );
    }
    std::fs::remove_file(sock).with_context(|| format!("removing stale {}", sock.display()))
}

fn establish_or_child_exit(
    sock: &Path,
    target: Option<&OsStr>,
    child: &mut Child,
) -> Result<Option<ClientIPC>> {
    let sock = sock.to_owned();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(ClientIPC::establish(&sock));
    });
    let start = Instant::now();
    let mut explained = false;
    loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(conn) => return Ok(Some(conn.context("ipc establish")?)),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if child.try_wait().context("try_wait target")?.is_some() {
                    return Ok(None);
                }
                if !explained && start.elapsed() >= EXPLAIN_MISSING_TARGET_AFTER {
                    explained = true;
                    let processes = stray_target::scan();
                    eprintln!(
                        "[valo-client] {}",
                        stray_target::explain(child.id(), target, &processes)
                    );
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                bail!("ipc establish thread died without result")
            }
        }
    }
}

fn target_env(cmd: &Command) -> Option<OsString> {
    match cmd.get_envs().find(|(key, _)| *key == "VALO_TARGET") {
        Some((_, value)) => value.map(OsStr::to_owned),
        None => std::env::var_os("VALO_TARGET"),
    }
}

fn wait_or_kill(child: &mut Child, grace: Duration) -> Result<ExitStatus> {
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait().context("try_wait target")? {
            return Ok(status);
        }
        if start.elapsed() >= grace {
            eprintln!("[valo] target did not exit within {grace:?}, killing");
            let _ = child.kill();
            return child.wait().context("wait after kill");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Keeps a pages file of the largest savestate's size ready, so a checkpoint
/// into a new slot finds its page cache allocated.
struct SparePages {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl SparePages {
    fn start(slots: PathBuf) -> SparePages {
        let stop = Arc::new(AtomicBool::new(false));
        let thread = std::thread::spawn({
            let stop = Arc::clone(&stop);
            move || {
                while !stop.load(Ordering::Relaxed) {
                    if let Err(e) = refill_spare_pages(&slots) {
                        eprintln!("[valo] stopped preparing spare savestate pages: {e:#}");
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(200));
                }
            }
        });
        SparePages {
            stop,
            thread: Some(thread),
        }
    }

    fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn refill_spare_pages(slots: &Path) -> Result<()> {
    let entries = match std::fs::read_dir(slots) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e).with_context(|| format!("list {}", slots.display())),
    };
    let mut largest = 0;
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        match std::fs::metadata(entry.path().join("pages")) {
            Ok(meta) => largest = largest.max(meta.len()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    if largest == 0 {
        return Ok(());
    }
    // headroom for a savestate that grew since
    let size = (largest + largest / 10).next_multiple_of(2 << 20);
    let spare = slots.join(paths::SPARE_PAGES);
    match std::fs::metadata(&spare) {
        Ok(meta) if meta.len() >= size => return Ok(()),
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    // replaced rather than grown: the lib may take the spare at any moment
    let tmp = slots.join(format!("{}.tmp", paths::SPARE_PAGES));
    let file = std::fs::File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?;
    rustix::fs::fallocate(&file, rustix::fs::FallocateFlags::empty(), 0, size)
        .with_context(|| format!("allocate {size} bytes for {}", tmp.display()))?;
    std::fs::rename(&tmp, &spare).with_context(|| format!("rename to {}", spare.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_ticks_follow_a_comm_with_parentheses() {
        let stat = "42 (a) b (c)) S 1 42 42 0 -1 4194304 10 0 0 0 7 5 0 0 20 0 1 0";
        assert_eq!(cpu_ticks(stat), Some(12));
    }
}
