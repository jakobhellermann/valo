// TODO(ai-review): generated, not yet audited
//! One Kumquat GPU host and one completion pipe per `valo run` session.

use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::fs::FileTypeExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};
use std::{fs, io};

use anyhow::{Context, Result, bail};

use crate::paths;

static NEXT_SESSION: AtomicU32 = AtomicU32::new(0);

pub struct GpuSession {
    host: Child,
    result_read: Option<OwnedFd>,
    runtime_dir: PathBuf,
}

impl GpuSession {
    pub fn start() -> Result<Self> {
        let seq = NEXT_SESSION.fetch_add(1, Ordering::Relaxed);
        let home = paths::valo_home();
        let runtime_dir = home.join(format!("gpu-{seq}"));
        if runtime_dir.exists() {
            fs::remove_dir_all(&runtime_dir)
                .with_context(|| format!("remove stale {}", runtime_dir.display()))?;
        }
        fs::create_dir(&runtime_dir)
            .with_context(|| format!("create {}", runtime_dir.display()))?;
        let socket = runtime_dir.join("gpu.sock");
        let slots_base = home.join("slots");
        let log = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(runtime_dir.join("kumquat.log"))?;
        let (result_read, result_write) = new_pipe()?;
        let read_fd = result_read.as_raw_fd();
        let write_fd = result_write.as_raw_fd();

        let mut cmd = Command::new(crate::resolve_kumquat_bin()?);
        cmd.arg("--capset-names=gfxstream-vulkan")
            .arg(format!("--gpu-socket-path={}", socket.display()))
            .arg(format!("--snapshot-base={}", slots_base.display()))
            .arg(format!("--result-fd={write_fd}"))
            .env_remove("VK_DRIVER_FILES")
            .env_remove("VK_ICD_FILENAMES")
            .env_remove("VK_LOADER_DRIVERS_SELECT")
            .env_remove("VN_DEBUG")
            .env_remove("LD_PRELOAD")
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log));
        if std::env::var_os("GFXSTREAM_LOG_LEVEL").is_none() {
            cmd.env("GFXSTREAM_LOG_LEVEL", "info");
        }

        // SAFETY: this closure runs after fork and uses only fcntl/close;
        // the write end belongs to the Kumquat child after exec.
        let parent_pid = std::process::id() as libc::pid_t;
        unsafe {
            cmd.pre_exec(move || {
                // Kill the child when this process dies, so no kumquat
                // outlives its session even when Drop never runs.
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) < 0 {
                    return Err(io::Error::last_os_error());
                }
                // The parent may have died between fork and prctl.
                if libc::getppid() != parent_pid {
                    return Err(io::Error::from_raw_os_error(libc::ESRCH));
                }
                libc::close(read_fd);
                if libc::fcntl(write_fd, libc::F_SETFD, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let host = match cmd.spawn() {
            Ok(host) => host,
            Err(error) => {
                let _ = fs::remove_dir_all(&runtime_dir);
                return Err(error).context("failed to start kumquat");
            }
        };
        drop(result_write);
        let mut session = Self {
            host,
            result_read: Some(result_read),
            runtime_dir,
        };
        session.wait_ready(&socket)?;
        Ok(session)
    }

    pub fn configure_game(&self, cmd: &mut Command) {
        let read_fd = self.result_read.as_ref().unwrap().as_raw_fd();
        cmd.env("KUMQUAT_PID", self.host.id().to_string())
            .env("VALO_GPU_RESULT_FD", read_fd.to_string())
            .env(
                "VIRTGPU_KUMQUAT_SOCKET_PATH",
                self.runtime_dir.join("gpu.sock"),
            );
        // SAFETY: only fcntl is called after fork; the game's inherited read
        // end must survive exec so libvalo can use it before and after restore.
        unsafe {
            cmd.pre_exec(move || {
                if libc::fcntl(read_fd, libc::F_SETFD, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }

    pub fn game_started(&mut self) {
        self.result_read = None;
    }

    pub fn pid(&self) -> u32 {
        self.host.id()
    }

    pub fn log_path(&self) -> PathBuf {
        self.runtime_dir.join("kumquat.log")
    }

    pub fn exit_status(&mut self) -> Result<Option<ExitStatus>> {
        Ok(self.host.try_wait()?)
    }

    fn wait_ready(&mut self, socket: &Path) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(6);
        loop {
            if socket
                .metadata()
                .is_ok_and(|metadata| metadata.file_type().is_socket())
            {
                return Ok(());
            }
            if let Some(status) = self.host.try_wait()? {
                let debug_hint = if status.signal().is_some() {
                    format!("; inspect core dump: coredumpctl debug {}", self.host.id())
                } else {
                    String::new()
                };
                bail!(
                    "Kumquat (pid {}) exited during startup: {status}{debug_hint}; log: {}",
                    self.host.id(),
                    self.runtime_dir.join("kumquat.log").display()
                );
            }
            if Instant::now() >= deadline {
                bail!(
                    "Kumquat did not create {}; see {}",
                    socket.display(),
                    self.runtime_dir.join("kumquat.log").display()
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for GpuSession {
    fn drop(&mut self) {
        let _ = self.host.kill();
        let _ = self.host.wait();
    }
}

fn new_pipe() -> Result<(OwnedFd, OwnedFd)> {
    rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC)
        .map_err(std::io::Error::from)
        .context("create GPU result pipe")
}
