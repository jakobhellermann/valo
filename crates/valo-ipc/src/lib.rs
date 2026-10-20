// TOOD(ai-review): interface is reviewed, implementation not
//! Wire format and rendezvous for the valo control socket.
//!
//! See `docs/ipc.md` for the design. This crate exposes two role-typed
//! handles, [`ClientIPC`] and [`LibIPC`], each with `send`/`recv` typed to
//! its direction. The transport itself is symmetric — see
//! [`ClientIPC::establish`] / [`LibIPC::establish`].

pub mod config;
pub mod gamespecific;
pub mod kumquat;
pub mod messages;
mod raw;

pub type Result<T, E = std::io::Error> = std::result::Result<T, E>;

use std::os::fd::{BorrowedFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use crate::messages::{ClientHello, ClientToLib, LibToClient};

/// Bumped on any breaking change to the message enums or framing.
pub const PROTOCOL_VERSION: u32 = 2;

/// Hard cap on a single message's payload
pub const MAX_MESSAGE_SIZE: usize = 16 * 1024 * 1024;

/// Connection handle for the client side of the protocol. Sends
/// [`ClientToLib`], receives [`LibToClient`].
pub struct ClientIPC {
    stream: UnixStream,
}

impl ClientIPC {
    pub fn establish(path: &Path) -> Result<Self> {
        Ok(Self {
            stream: raw::establish_stream(path)?,
        })
    }

    /// Hello exchange on a fresh connection: version check, and the config
    /// the lib currently has.
    pub fn hello(&mut self) -> Result<crate::config::Config> {
        self.send(&ClientToLib::Hello(ClientHello::new()))?;
        let Some(LibToClient::Hello(peer)) = self.recv()? else {
            return Err(std::io::Error::other("lib closed before Hello"));
        };
        if peer.version != crate::PROTOCOL_VERSION {
            return Err(std::io::Error::other(format!(
                "lib protocol version {} does not match client {}",
                peer.version,
                crate::PROTOCOL_VERSION
            )));
        }
        Ok(peer.config)
    }

    /// The init exchange every session starts with: hello, then exactly one
    /// SetConfig — fire-and-forget, the lib applies it before anything
    /// runs — and with `use_gpu` the GPU host's control socket. The session
    /// starts held; a Detach sent here drains at the first frame boundary
    /// and frees the session.
    pub fn init(
        &mut self,
        config: &crate::config::Config,
        gpu_control: Option<BorrowedFd<'_>>,
    ) -> Result<()> {
        if config.use_gpu != gpu_control.is_some() {
            return Err(std::io::Error::other(
                "use_gpu requires the GPU control socket and vice versa",
            ));
        }
        self.hello()?;
        self.send(&ClientToLib::SetConfig(config.clone()))?;
        if let Some(control) = gpu_control {
            self.send_gpu_control(control)?;
        }
        Ok(())
    }

    /// Connect-only handshake for one-shot clients.
    /// Unlike [`ClientIPC::establish`] this never
    /// falls back to becoming the listener.
    pub fn connect(path: &Path, timeout: Duration) -> Result<Self> {
        Ok(Self {
            stream: raw::connect_stream(path, timeout)?,
        })
    }

    /// Wrap an already-accepted stream. For sessions that own the listener
    /// socket and accept (re)connections at frame boundaries.
    pub fn from_accepted(stream: UnixStream) -> Self {
        Self { stream }
    }

    pub fn send(&mut self, msg: &ClientToLib) -> Result<()> {
        raw::send(&mut self.stream, msg)
    }

    /// Returns `Ok(None)` if the peer hung up cleanly before any byte of
    /// the next length prefix arrived.
    pub fn recv(&mut self) -> Result<Option<LibToClient>> {
        raw::recv(&mut self.stream)
    }

    fn send_gpu_control(&mut self, control: BorrowedFd<'_>) -> Result<()> {
        self.send(&ClientToLib::GpuControl)?;
        raw::send_descriptor(&self.stream, control)
    }

    /// Pid of the peer process, as recorded by the kernel at its connect/listen.
    pub fn peer_pid(&self) -> Result<u32> {
        let cred = rustix::net::sockopt::socket_peercred(&self.stream)?;
        Ok(cred.pid.as_raw_nonzero().get().cast_unsigned())
    }

    /// Duplicate the underlying stream. Both read/write to the same socket.
    pub fn try_clone_for_reader(&self) -> Result<Self> {
        Ok(Self {
            stream: self.stream.try_clone()?,
        })
    }

    pub fn set_read_timeout(&self, timeout: Duration) -> Result<()> {
        self.stream.set_read_timeout(Some(timeout))
    }

    pub fn clear_read_timeout(&self) -> Result<()> {
        self.stream.set_read_timeout(None)
    }
}

/// Connection handle for the lib side of the protocol. Sends
/// [`LibToClient`], receives [`ClientToLib`].
pub struct LibIPC {
    stream: UnixStream,
}

impl LibIPC {
    pub fn establish(path: &Path) -> Result<Self> {
        Ok(Self {
            stream: raw::establish_stream(path)?,
        })
    }

    /// Wrap an already-accepted stream. For sessions that own the listener
    /// socket and accept (re)connections at frame boundaries.
    pub fn from_accepted(stream: UnixStream) -> Self {
        Self { stream }
    }

    pub fn set_read_timeout(&self, timeout: Duration) -> Result<()> {
        self.stream.set_read_timeout(Some(timeout))
    }

    pub fn clear_read_timeout(&self) -> Result<()> {
        self.stream.set_read_timeout(None)
    }

    pub fn send(&mut self, msg: &LibToClient) -> Result<()> {
        raw::send(&mut self.stream, msg)
    }

    /// Returns `Ok(None)` if the peer hung up cleanly before any byte of
    /// the next length prefix arrived.
    pub fn recv(&mut self) -> Result<Option<ClientToLib>> {
        raw::recv(&mut self.stream)
    }
    /// The GPU host's control socket, which must be the next message.
    pub fn recv_gpu_control(&mut self) -> Result<OwnedFd> {
        match self.recv()? {
            Some(ClientToLib::GpuControl) => self.recv_descriptor(),
            other => Err(std::io::Error::other(format!(
                "expected GpuControl, got {other:?}"
            ))),
        }
    }

    /// The descriptor that follows a received [`ClientToLib::GpuControl`].
    pub fn recv_descriptor(&mut self) -> Result<OwnedFd> {
        raw::recv_descriptor(&self.stream)
    }
}

#[cfg(test)]
mod tests {
    use super::config::Config;
    use super::messages::*;
    use super::*;
    use std::os::unix::net::UnixListener;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;
    use std::time::Duration;
    use tempfile::TempDir;

    /// One thread plays the client, the other the lib. Whichever binds
    /// first becomes listener; both end up paired regardless.
    #[test]
    fn establish_pair_can_handshake() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("valo-test.sock");
        let p1 = path.clone();
        let p2 = path.clone();

        let t_client = thread::spawn(move || -> Result<()> {
            let mut c = ClientIPC::establish(&p1)?;
            c.send(&ClientToLib::Hello(ClientHello::new()))?;
            let reply = c.recv()?.expect("eof");
            assert!(matches!(reply, LibToClient::Hello(_)));
            Ok(())
        });
        let t_lib = thread::spawn(move || -> Result<()> {
            let mut l = LibIPC::establish(&p2)?;
            let req = l.recv()?.expect("eof");
            assert!(matches!(req, ClientToLib::Hello(_)));
            l.send(&LibToClient::Hello(LibHello::new(Config::default())))?;
            Ok(())
        });

        t_client.join().unwrap().unwrap();
        t_lib.join().unwrap().unwrap();
    }

    #[test]
    fn gpu_control_descriptor_reaches_the_lib() {
        use std::io::{Read, Write};
        let (client_end, lib_end) = UnixStream::pair().unwrap();
        let mut client = ClientIPC::from_accepted(client_end);
        let mut lib = LibIPC::from_accepted(lib_end);
        let (mut a, b) = UnixStream::pair().unwrap();

        let config = Config {
            use_gpu: true,
            ..Config::default()
        };
        let t_client = thread::spawn(move || {
            client
                .init(&config, Some(std::os::fd::AsFd::as_fd(&b)))
                .unwrap();
            client.send(&ClientToLib::GetState).unwrap();
        });
        assert!(matches!(lib.recv().unwrap(), Some(ClientToLib::Hello(_))));
        lib.send(&LibToClient::Hello(LibHello::new(Config::default())))
            .unwrap();
        assert!(matches!(
            lib.recv().unwrap(),
            Some(ClientToLib::SetConfig(_))
        ));
        let mut received = UnixStream::from(lib.recv_gpu_control().unwrap());
        assert_eq!(lib.recv().unwrap(), Some(ClientToLib::GetState));
        a.write_all(b"x").unwrap();
        let mut byte = [0u8];
        received.read_exact(&mut byte).unwrap();
        assert_eq!(&byte, b"x");
        t_client.join().unwrap();
    }

    /// Stale socket file from a previous run should be cleaned up on the
    /// next rendezvous.
    /// Works before the listener ever accepts, i.e. also against a hung peer.
    #[test]
    fn peer_pid_of_unaccepted_listener() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("valo-test.sock");
        let _listener = UnixListener::bind(&path).unwrap();
        let conn = ClientIPC::connect(&path, Duration::from_secs(1)).unwrap();
        assert_eq!(conn.peer_pid().unwrap(), std::process::id());
    }

    #[test]
    fn establish_cleans_stale_socket() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("valo-test.sock");

        // Create a stale socket: bind a listener and let it drop. The path
        // persists but nothing listens.
        {
            let _ = UnixListener::bind(&path).unwrap();
        }
        assert!(path.exists());

        let p1 = path.clone();
        let p2 = path.clone();
        let t1 = thread::spawn(move || ClientIPC::establish(&p1));
        let t2 = thread::spawn(move || LibIPC::establish(&p2));
        let mut client = t1.join().unwrap().unwrap();
        let mut lib = t2.join().unwrap().unwrap();
        client.send(&ClientToLib::AdvanceFrames(1)).unwrap();
        let got = lib.recv().unwrap().unwrap();
        assert_eq!(got, ClientToLib::AdvanceFrames(1));
    }

    /// After a session ends and both sides drop their connections, a
    /// fresh pair of `establish()` calls should succeed without manual
    /// cleanup.
    #[test]
    fn establish_reconnect_after_drop() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("valo-test.sock");

        for _round in 0..3 {
            let p1 = path.clone();
            let p2 = path.clone();
            let t1 = thread::spawn(move || ClientIPC::establish(&p1));
            let t2 = thread::spawn(move || LibIPC::establish(&p2));
            let mut client = t1.join().unwrap().unwrap();
            let mut lib = t2.join().unwrap().unwrap();
            client.send(&ClientToLib::AdvanceFrames(1)).unwrap();
            let got = lib.recv().unwrap().unwrap();
            assert_eq!(got, ClientToLib::AdvanceFrames(1));
        }
    }

    /// Even when one peer arrives much later than the other, the first
    /// peer waits in accept() and the pair connects.
    #[test]
    fn establish_late_peer() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("valo-test.sock");
        let p1 = path.clone();
        let p2 = path.clone();
        let started = Arc::new(AtomicBool::new(false));
        let started1 = started.clone();

        let t1 = thread::spawn(move || -> Result<ClientIPC> {
            started1.store(true, Ordering::SeqCst);
            ClientIPC::establish(&p1)
        });
        while !started.load(Ordering::SeqCst) {
            thread::yield_now();
        }
        thread::sleep(Duration::from_millis(50));
        let t2 = thread::spawn(move || LibIPC::establish(&p2));
        let _c = t1.join().unwrap().unwrap();
        let _l = t2.join().unwrap().unwrap();
    }

    /// Round-trip every variant of both message enums via a real
    /// `ClientIPC` / `LibIPC` pair.
    #[test]
    fn roundtrip_all_variants() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("valo-test.sock");
        let p1 = path.clone();
        let p2 = path.clone();
        let t1 = thread::spawn(move || ClientIPC::establish(&p1));
        let t2 = thread::spawn(move || LibIPC::establish(&p2));
        let mut client = t1.join().unwrap().unwrap();
        let mut lib = t2.join().unwrap().unwrap();

        let client_msgs = vec![
            ClientToLib::Hello(ClientHello::new()),
            ClientToLib::AdvanceFrames(1),
            ClientToLib::Detach,
            ClientToLib::Checkpoint(0),
            ClientToLib::Checkpoint(7),
            ClientToLib::Restore(0),
            ClientToLib::Restore(7),
            ClientToLib::GetState,
            ClientToLib::Quit,
            ClientToLib::AdvanceFrames(42),
        ];
        for msg in &client_msgs {
            client.send(msg).unwrap();
            let got = lib.recv().unwrap().unwrap();
            assert_eq!(&got, msg);
        }

        let lib_msgs = vec![
            LibToClient::Hello(LibHello::new(Config::default())),
            LibToClient::FrameBoundary(FrameBoundary {
                frame: 42,
                game_window: Some(0x400001),
            }),
            LibToClient::State(State { frame: 100 }),
            LibToClient::CheckpointDone,
            LibToClient::RestoreSucceeded(RestoreSucceeded { frame: 7 }),
            LibToClient::Error(Error {
                message: "nope".to_string(),
            }),
        ];
        for msg in &lib_msgs {
            lib.send(msg).unwrap();
            let got = client.recv().unwrap().unwrap();
            assert_eq!(&got, msg);
        }
    }
}
