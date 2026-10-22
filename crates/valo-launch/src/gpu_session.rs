// TODO(ai-review): generated, not yet audited
//! One Kumquat GPU host and its control connection per `valo run` session.

use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};
use std::{fs, io};

use anyhow::{Context, Result, bail};

use crate::paths;

static NEXT_SESSION: AtomicU32 = AtomicU32::new(0);

pub struct GpuSession {
    host: Child,
    /// The lib's end of Kumquat's control connection; handed over at init.
    control: OwnedFd,
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
        let log = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(runtime_dir.join("kumquat.log"))?;
        let (control, host_control) = control_socketpair()?;
        let control_fd = control.as_raw_fd();
        let host_control_fd = host_control.as_raw_fd();

        let mut renderer_features = vec!["VulkanSnapshots:enabled"];
        if let Err(reason) = crate::host_visible_memory_sharing() {
            eprintln!("[valo-launch] copying host visible GPU memory: {reason}");
            renderer_features.push("VirtioGpuNext:disabled");
        }

        let mut cmd = Command::new(crate::resolve_kumquat_bin()?);
        cmd.arg("--capset-names=gfxstream-vulkan")
            .arg(format!("--gpu-socket-path={}", socket.display()))
            .arg(format!("--control-fd={host_control_fd}"))
            .arg(format!(
                "--renderer-features={}",
                renderer_features.join(",")
            ))
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
        if let Some(icd) = crate::resolve_dozen_icd()? {
            cmd.env("VK_DRIVER_FILES", icd);
        }

        // SAFETY: this closure runs after fork and uses only prctl/getppid/close/fcntl;
        // the host's end of the control socket belongs to the Kumquat child after exec.
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
                libc::close(control_fd);
                if libc::fcntl(host_control_fd, libc::F_SETFD, 0) < 0 {
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
        drop(host_control);
        let mut session = Self {
            host,
            control,
            runtime_dir,
        };
        session.wait_ready()?;
        Ok(session)
    }

    pub fn configure_game(&self, cmd: &mut Command) {
        cmd.env(
            "VIRTGPU_KUMQUAT_SOCKET_PATH",
            self.runtime_dir.join("gpu.sock"),
        );
    }

    /// The control socket to hand to the lib during its init.
    pub fn control_for_lib(&self) -> BorrowedFd<'_> {
        self.control.as_fd()
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

    pub fn abort(&mut self) -> Result<ExitStatus> {
        let pid = rustix::process::Pid::from_child(&self.host);
        rustix::process::kill_process(pid, rustix::process::Signal::ABORT)
            .with_context(|| format!("SIGABRT kumquat (pid {pid})"))?;
        Ok(self.host.wait()?)
    }

    /// Waits for Kumquat to report that its GPU socket accepts connections.
    fn wait_ready(&mut self) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(6);
        loop {
            let mut poll_fds = [rustix::event::PollFd::new(
                &self.control,
                rustix::event::PollFlags::IN,
            )];
            let slice = rustix::time::Timespec {
                tv_sec: 0,
                tv_nsec: 10_000_000,
            };
            let ready = match rustix::event::poll(&mut poll_fds, Some(&slice)) {
                Err(rustix::io::Errno::INTR) => continue,
                ready => ready.map_err(io::Error::from)?,
            };
            if ready > 0 {
                let mut message = [0u8; 4];
                let len =
                    rustix::net::recv(&self.control, &mut message, rustix::net::RecvFlags::empty())
                        .map_err(io::Error::from)?
                        .0;
                if len == 4 && u32::from_le_bytes(message) == valo_ipc::kumquat::READY {
                    return Ok(());
                }
                if len != 0 {
                    bail!("Kumquat sent an unexpected first control message ({len} bytes)");
                }
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
                    "Kumquat did not report ready; see {}",
                    self.runtime_dir.join("kumquat.log").display()
                );
            }
        }
    }
}

impl Drop for GpuSession {
    fn drop(&mut self) {
        let _ = self.host.kill();
        let _ = self.host.wait();
    }
}

fn control_socketpair() -> Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0; 2];
    // SAFETY: socketpair writes two fresh descriptors into `fds` on success.
    let ret = unsafe {
        libc::socketpair(
            libc::AF_UNIX,
            libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC,
            0,
            fds.as_mut_ptr(),
        )
    };
    if ret < 0 {
        return Err(io::Error::last_os_error()).context("create Kumquat control socketpair");
    }
    // SAFETY: both descriptors are owned from here on.
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}
