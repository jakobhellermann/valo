// TODO(ai-review): generated, not yet audited
use core::fmt::Write;
use std::io::IoSlice;
use std::path::PathBuf;
use std::sync::atomic::{AtomicI32, Ordering};

use rustix::fd::BorrowedFd;
use rustix::fs::{Mode, OFlags};
use rustix::io;

use crate::home;

/// Chunk size of the streaming event writer.
const BUF_SIZE: usize = 1024;

static STRUCTURED_FD: AtomicI32 = AtomicI32::new(-1);

pub fn init() {
    let path = path_from_env().unwrap_or_else(|| home::get().join("valo-struct.jsonl"));
    // APPEND: the fd sync rewinds the log fd's offset on restore.
    let flags = OFlags::WRONLY | OFlags::CREATE | OFlags::TRUNC | OFlags::APPEND | OFlags::CLOEXEC;
    let mode = Mode::RUSR | Mode::WUSR | Mode::RGRP | Mode::ROTH;
    match rustix::fs::open(path, flags, mode) {
        Ok(fd) => {
            let raw = rustix::fd::IntoRawFd::into_raw_fd(fd);
            STRUCTURED_FD.store(raw, Ordering::Release);
        }
        Err(_) => {
            STRUCTURED_FD.store(-2, Ordering::Release);
        }
    }
}

fn path_from_env() -> Option<PathBuf> {
    std::env::var_os("VALO_STRUCTURED_LOG").map(PathBuf::from)
}

/// Streaming writer for one JSON event
pub struct EventWriter<'fd> {
    fd: BorrowedFd<'fd>,
    buf: [u8; BUF_SIZE],
    len: usize,
}

impl EventWriter<'_> {
    fn flush(&mut self) {
        let _ = io::writev(self.fd, &[IoSlice::new(&self.buf[..self.len])]);
        self.len = 0;
    }
}

impl core::fmt::Write for EventWriter<'_> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let mut rest = s.as_bytes();
        while !rest.is_empty() {
            let n = rest.len().min(BUF_SIZE - self.len);
            self.buf[self.len..self.len + n].copy_from_slice(&rest[..n]);
            self.len += n;
            rest = &rest[n..];
            if self.len == BUF_SIZE {
                self.flush();
            }
        }
        Ok(())
    }
}

/// Writes one JSON object as a line. Async-signal-safe.
pub fn structured_write(fields: impl FnOnce(&mut EventWriter)) {
    let raw_fd = STRUCTURED_FD.load(Ordering::Acquire);
    if raw_fd < 0 {
        return;
    }

    // SAFETY: raw_fd is a file descriptor we opened in init() and never
    // close. Writing to it from any thread/context is safe.
    let fd = unsafe { BorrowedFd::borrow_raw(raw_fd) };
    let mut w = EventWriter {
        fd,
        buf: [0; BUF_SIZE],
        len: 0,
    };
    let _ = w.write_str("{");
    fields(&mut w);
    let _ = w.write_str("}\n");
    w.flush();
}

pub fn write_field(w: &mut EventWriter, key: &str, value: impl JsonField) {
    if w.len > 1 {
        let _ = w.write_str(",");
    }
    let _ = w.write_str("\"");
    let _ = w.write_str(key);
    let _ = w.write_str("\":");
    value.write_json(w);
}

pub trait JsonField {
    fn write_json(&self, w: &mut EventWriter);
}

impl<T: JsonField + ?Sized> JsonField for &T {
    fn write_json(&self, w: &mut EventWriter) {
        (**self).write_json(w);
    }
}

impl JsonField for &str {
    fn write_json(&self, w: &mut EventWriter) {
        let _ = w.write_str("\"");
        for c in self.chars() {
            match c {
                '"' => {
                    let _ = w.write_str("\\\"");
                }
                '\\' => {
                    let _ = w.write_str("\\\\");
                }
                '\n' => {
                    let _ = w.write_str("\\n");
                }
                '\r' => {
                    let _ = w.write_str("\\r");
                }
                '\t' => {
                    let _ = w.write_str("\\t");
                }
                c if (c as u32) < 0x20 => {
                    let _ = write!(w, "\\u{:04x}", c as u32);
                }
                c => {
                    let _ = w.write_str(c.encode_utf8(&mut [0u8; 4]));
                }
            }
        }
        let _ = w.write_str("\"");
    }
}

impl JsonField for u8 {
    fn write_json(&self, w: &mut EventWriter) {
        let _ = write!(w, "{}", self);
    }
}
impl JsonField for u32 {
    fn write_json(&self, w: &mut EventWriter) {
        let _ = write!(w, "{}", self);
    }
}
impl JsonField for u64 {
    fn write_json(&self, w: &mut EventWriter) {
        let _ = write!(w, "{}", self);
    }
}

impl JsonField for bool {
    fn write_json(&self, w: &mut EventWriter) {
        let _ = w.write_str(if *self { "true" } else { "false" });
    }
}

impl<T: JsonField> JsonField for Option<T> {
    fn write_json(&self, w: &mut EventWriter) {
        match self {
            Some(value) => value.write_json(w),
            None => {
                let _ = w.write_str("null");
            }
        }
    }
}

impl JsonField for crate::procfs::maps::MemoryMapPermissions {
    fn write_json(&self, w: &mut EventWriter) {
        let _ = w.write_str("\"");
        let _ = write!(w, "{}", self);
        let _ = w.write_str("\"");
    }
}

impl JsonField for crate::procfs::fds::FdKind {
    fn write_json(&self, w: &mut EventWriter) {
        let _ = w.write_str("\"");
        let _ = write!(w, "{}", self);
        let _ = w.write_str("\"");
    }
}

pub mod events;

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsFd;

    #[test]
    fn streams_long_fields_without_truncation() {
        use rustix::fs::{MemfdFlags, SeekFrom, memfd_create};

        // The memfd below mutates the fd table; the fd tests enumerate it
        // against a Frozen assumption, so every fd-churning test serializes
        // on the same lock.
        let _fds = crate::utils::FD_TABLE_TEST_LOCK.lock().unwrap();

        let memfd = memfd_create(c"valo-event-writer-test", MemfdFlags::CLOEXEC).unwrap();
        let mut w = EventWriter {
            fd: memfd.as_fd(),
            buf: [0; BUF_SIZE],
            len: 0,
        };
        let long = "x".repeat(BUF_SIZE * 3 + 17);
        let _ = w.write_str("{");
        let _ = w.write_str(&long);
        let _ = w.write_str("}\n");
        w.flush();

        rustix::fs::seek(&memfd, SeekFrom::Start(0)).unwrap();
        let mut buf = vec![0; BUF_SIZE * 3 + 64];
        let n = rustix::io::read(&memfd, &mut buf).unwrap();
        buf.truncate(n);
        assert_eq!(buf, format!("{{{}}}\n", long).as_bytes());
    }
}
