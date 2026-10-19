// TOOD(ai-review)
//! Length-prefixed postcard framing over a generic `Read`/`Write`.

use std::io::{ErrorKind, IoSlice, IoSliceMut, Read, Write};
use std::mem::MaybeUninit;
use std::os::fd::{BorrowedFd, OwnedFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::time::{Duration, Instant};
use std::{fs, thread};

use serde::{Deserialize, Serialize};

use crate::{MAX_MESSAGE_SIZE, Result};

pub(crate) fn send<W: Write, M: Serialize>(w: &mut W, msg: &M) -> Result<()> {
    let len = postcard::experimental::serialized_size(msg)
        .map_err(|e| std::io::Error::new(ErrorKind::InvalidData, format!("postcard size: {e}")))?;
    if len > MAX_MESSAGE_SIZE {
        return Err(std::io::Error::new(
            ErrorKind::InvalidData,
            format!("payload {len} exceeds MAX_MESSAGE_SIZE {MAX_MESSAGE_SIZE}"),
        ));
    }
    w.write_all(&(len as u32).to_le_bytes())?;
    // Prevent to_io from mapping errors to SerializeBufferFull
    let mut catching = ErrorCatching {
        inner: &mut *w,
        err: None,
    };
    if let Err(e) = postcard::to_io(msg, &mut catching) {
        if let Some(io_err) = catching.err.take() {
            return Err(io_err);
        }
        return Err(std::io::Error::new(
            ErrorKind::InvalidData,
            format!("postcard encode: {e}"),
        ));
    }
    Ok(())
}

/// Records the first I/O error the writer produces
struct ErrorCatching<'a, W: Write> {
    inner: &'a mut W,
    err: Option<std::io::Error>,
}

impl<W: Write> Write for ErrorCatching<'_, W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.inner.write(buf).inspect_err(|e| self.record(e))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush().inspect_err(|e| self.record(e))
    }
}

impl<W: Write> ErrorCatching<'_, W> {
    fn record(&mut self, e: &std::io::Error) {
        self.err
            .get_or_insert_with(|| std::io::Error::new(e.kind(), e.to_string()));
    }
}

pub(crate) fn recv<R: Read, M: for<'de> Deserialize<'de>>(r: &mut R) -> Result<Option<M>> {
    let Some(buf) = read_frame(r)? else {
        return Ok(None);
    };
    let msg = postcard::from_bytes(&buf).map_err(|e| {
        std::io::Error::new(ErrorKind::InvalidData, format!("postcard decode: {e}"))
    })?;
    Ok(Some(msg))
}

/// Read exactly one frame: u32 little-endian length, then `length` bytes.
/// Returns `Ok(None)` on clean EOF before any byte of the length prefix is
/// read; otherwise propagates the I/O error or yields the payload bytes.
// TODO: remove allocation
fn read_frame<R: Read>(r: &mut R) -> Result<Option<Vec<u8>>> {
    let mut len_buf = [0u8; 4];
    match read_full_or_eof(r, &mut len_buf)? {
        ReadFull::Eof => return Ok(None),
        ReadFull::Ok => {}
    }
    let len = u32::from_le_bytes(len_buf) as usize;
    if len > MAX_MESSAGE_SIZE {
        return Err(std::io::Error::new(
            ErrorKind::InvalidData,
            format!("frame length {len} exceeds MAX_MESSAGE_SIZE"),
        ));
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    Ok(Some(buf))
}

enum ReadFull {
    Ok,
    Eof,
}

fn read_full_or_eof<R: Read>(r: &mut R, buf: &mut [u8]) -> Result<ReadFull> {
    let mut filled = 0;
    while filled < buf.len() {
        match r.read(&mut buf[filled..]) {
            Ok(0) => {
                if filled == 0 {
                    return Ok(ReadFull::Eof);
                }
                return Err(std::io::Error::new(
                    ErrorKind::UnexpectedEof,
                    "EOF in middle of length prefix",
                ));
            }
            Ok(n) => filled += n,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(ReadFull::Ok)
}

// ---------- descriptors ----------

/// Send `fd` attached to a single byte, outside the framed message stream.
pub(crate) fn send_descriptor(stream: &UnixStream, fd: BorrowedFd<'_>) -> Result<()> {
    use rustix::net::{SendAncillaryBuffer, SendAncillaryMessage, SendFlags, sendmsg};
    let mut space = [MaybeUninit::<u8>::uninit(); rustix::cmsg_space!(ScmRights(1))];
    let mut ancillary = SendAncillaryBuffer::new(&mut space);
    let fds = [fd];
    if !ancillary.push(SendAncillaryMessage::ScmRights(&fds)) {
        return Err(std::io::Error::other(
            "descriptor does not fit the ancillary buffer",
        ));
    }
    let sent = sendmsg(
        stream,
        &[IoSlice::new(&[0])],
        &mut ancillary,
        SendFlags::NOSIGNAL,
    )?;
    if sent != 1 {
        return Err(std::io::Error::new(
            ErrorKind::WriteZero,
            "descriptor byte not sent",
        ));
    }
    Ok(())
}

/// Receive the descriptor [`send_descriptor`] attached to its byte.
pub(crate) fn recv_descriptor(stream: &UnixStream) -> Result<OwnedFd> {
    use rustix::net::{RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, recvmsg};
    let mut space = [MaybeUninit::<u8>::uninit(); rustix::cmsg_space!(ScmRights(1))];
    let mut ancillary = RecvAncillaryBuffer::new(&mut space);
    let mut byte = [0u8];
    let received = recvmsg(
        stream,
        &mut [IoSliceMut::new(&mut byte)],
        &mut ancillary,
        RecvFlags::CMSG_CLOEXEC,
    )?;
    if received.bytes != 1 {
        return Err(std::io::Error::new(
            ErrorKind::UnexpectedEof,
            "peer closed before the descriptor byte",
        ));
    }
    let mut fds = Vec::new();
    for message in ancillary.drain() {
        if let RecvAncillaryMessage::ScmRights(received) = message {
            fds.extend(received);
        }
    }
    match <[OwnedFd; 1]>::try_from(fds) {
        Ok([fd]) => Ok(fd),
        Err(fds) => Err(std::io::Error::new(
            ErrorKind::InvalidData,
            format!("expected one descriptor, got {}", fds.len()),
        )),
    }
}

// ---------- rendezvous ----------

/// Connect to a peer that already owns the listener socket, retrying only
/// while the socket file does not exist yet (peer still starting up).
/// Never binds or removes the socket file.
pub(crate) fn connect_stream(path: &Path, timeout: Duration) -> Result<UnixStream> {
    let deadline = Instant::now() + timeout;
    let mut delay = Duration::from_millis(5);
    loop {
        match UnixStream::connect(path) {
            Ok(stream) => return Ok(stream),
            Err(e)
                if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::ConnectionRefused)
                    && Instant::now() < deadline =>
            {
                // NotFound: the lib has not bound the path yet.
                // ConnectionRefused: a stale socket file from the init
                // rendezvous, which the lib rebinds during its init — both
                // are transient.
                thread::sleep(delay);
                delay = (delay * 2).min(Duration::from_millis(100));
            }
            Err(e) => return Err(e),
        }
    }
}

pub(crate) fn establish_stream(path: &Path) -> Result<UnixStream> {
    let mut backoff = Backoff::new();
    loop {
        match UnixStream::connect(path) {
            Ok(stream) => return Ok(stream),
            Err(e) if e.kind() == ErrorKind::NotFound => {
                // No socket file yet; try to be the listener.
            }
            Err(e) if e.kind() == ErrorKind::ConnectionRefused => {
                // Stale socket from a dead peer. Remove it and try to bind.
                // Ignore the result: a concurrent peer may have already
                // unlinked it, in which case our bind will succeed anyway.
                let _ = fs::remove_file(path);
            }
            Err(e) => {
                backoff.sleep();
                debug_assert!(
                    e.kind() != ErrorKind::NotFound && e.kind() != ErrorKind::ConnectionRefused
                );
                continue;
            }
        }

        match UnixListener::bind(path) {
            Ok(listener) => {
                let (stream, _addr) = listener.accept()?;
                // Once we've accepted, drop the listener so the path is
                // free for future re-use after we exit. Note: dropping the
                // listener does not unlink the socket file; we leave that
                // to the next peer's stale-socket cleanup.
                drop(listener);
                return Ok(stream);
            }
            Err(e) if e.kind() == ErrorKind::AddrInUse => {
                // A peer just won the bind race. Loop back and connect.
                continue;
            }
            Err(e) => {
                backoff.sleep();
                let _ = e;
                continue;
            }
        }
    }
}

struct Backoff {
    delay: Duration,
}

impl Backoff {
    fn new() -> Self {
        Self {
            delay: Duration::from_millis(10),
        }
    }
    fn sleep(&mut self) {
        thread::sleep(self.delay);
        let next = (self.delay * 2).min(Duration::from_secs(1));
        self.delay = next;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn read_frame_clean_eof_returns_none() {
        let mut empty = Cursor::new(Vec::<u8>::new());
        assert!(read_frame(&mut empty).unwrap().is_none());
    }

    #[test]
    fn read_frame_partial_prefix_is_unexpected_eof() {
        let mut partial = Cursor::new(vec![1u8, 2, 3]);
        let err = read_frame(&mut partial).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::UnexpectedEof);
    }

    #[test]
    fn read_frame_rejects_oversize() {
        let mut bad = Vec::new();
        bad.extend_from_slice(&((MAX_MESSAGE_SIZE as u32 + 1).to_le_bytes()));
        let mut cur = Cursor::new(bad);
        let err = read_frame(&mut cur).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidData);
    }

    #[test]
    fn read_frame_handles_split_reads() {
        // Drip yields one byte at a time; ensures read_frame's loop
        // copes with short reads.
        struct Drip<'a> {
            inner: &'a [u8],
        }
        impl Read for Drip<'_> {
            fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
                if self.inner.is_empty() || buf.is_empty() {
                    return Ok(0);
                }
                buf[0] = self.inner[0];
                self.inner = &self.inner[1..];
                Ok(1)
            }
        }
        let mut framed = Vec::new();
        framed.extend_from_slice(&5u32.to_le_bytes());
        framed.extend_from_slice(b"hello");
        let mut drip = Drip { inner: &framed };
        let got = read_frame(&mut drip).unwrap().unwrap();
        assert_eq!(got, b"hello");
    }

    #[test]
    fn roundtrip_via_cursor() {
        // A simple end-to-end via Cursor — no ValoIPC wrapper.
        let mut buf = Vec::new();
        send(&mut buf, &("hello".to_string(), 7u32)).unwrap();
        let mut cur = Cursor::new(buf);
        let got: (String, u32) = recv(&mut cur).unwrap().unwrap();
        assert_eq!(got, ("hello".to_string(), 7));
    }
}
