// TODO(ai-review): generated, not yet audited
use core::ffi::CStr;
use core::fmt::Write;
use std::borrow::Cow;
use std::ffi::CString;
use std::io::IoSlice;
use std::sync::atomic::{AtomicI32, Ordering};

use rustix::fd::BorrowedFd;
use rustix::fs::{Mode, OFlags};
use rustix::io;

use super::StackWriter;

const BUF_SIZE: usize = 1024;

static STRUCTURED_FD: AtomicI32 = AtomicI32::new(-1);

pub fn init() {
    // Default /tmp/valo-{pid}.jsonl: each process run keeps its events.
    let path: Cow<'static, CStr> = match path_from_env() {
        Some(p) => p.into(),
        None => {
            let pid = std::process::id();
            Cow::Owned(CString::new(format!("/tmp/valo-{pid}.jsonl")).unwrap())
        }
    };
    let flags = OFlags::WRONLY | OFlags::CREATE | OFlags::TRUNC | OFlags::CLOEXEC;
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

fn path_from_env() -> Option<&'static CStr> {
    // SAFETY: Environment is not mutated?
    let raw = unsafe { libc::getenv(c"VALO_STRUCTURED_LOG".as_ptr()) };
    if raw.is_null() {
        return None;
    }
    // SAFETY: getenv returns a NUL-terminated C string.
    Some(unsafe { CStr::from_ptr(raw) })
}

pub fn structured_write(fields: impl FnOnce(&mut StackWriter)) {
    let raw_fd = STRUCTURED_FD.load(Ordering::Acquire);
    if raw_fd < 0 {
        return;
    }

    let mut w = StackWriter::new();
    let _ = w.write_str("{");
    fields(&mut w);
    let _ = w.write_str("}");
    if w.len < BUF_SIZE {
        w.buf[w.len] = b'\n';
        w.len += 1;
    } else {
        w.buf[BUF_SIZE - 1] = b'\n';
    }

    // SAFETY: raw_fd is a file descriptor we opened in init() and never
    // close. Writing to it from any thread/context is safe.
    let fd = unsafe { BorrowedFd::borrow_raw(raw_fd) };
    let _ = io::writev(fd, &[IoSlice::new(w.as_slice())]);
}

pub fn write_field(w: &mut StackWriter, key: &str, value: impl JsonField) {
    if w.len > 1 {
        let _ = w.write_str(",");
    }
    let _ = w.write_str("\"");
    let _ = w.write_str(key);
    let _ = w.write_str("\":");
    value.write_json(w);
}

pub trait JsonField {
    fn write_json(&self, w: &mut StackWriter);
}

impl<T: JsonField + ?Sized> JsonField for &T {
    fn write_json(&self, w: &mut StackWriter) {
        (**self).write_json(w);
    }
}

impl JsonField for &str {
    fn write_json(&self, w: &mut StackWriter) {
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
    fn write_json(&self, w: &mut StackWriter) {
        let _ = write!(w, "{}", self);
    }
}
impl JsonField for u32 {
    fn write_json(&self, w: &mut StackWriter) {
        let _ = write!(w, "{}", self);
    }
}
impl JsonField for u64 {
    fn write_json(&self, w: &mut StackWriter) {
        let _ = write!(w, "{}", self);
    }
}

impl JsonField for bool {
    fn write_json(&self, w: &mut StackWriter) {
        let _ = w.write_str(if *self { "true" } else { "false" });
    }
}

impl JsonField for crate::procfs_maps::MemoryMapPermissions {
    fn write_json(&self, w: &mut StackWriter) {
        let _ = w.write_str("\"");
        let _ = write!(w, "{}", self);
        let _ = w.write_str("\"");
    }
}
pub mod events;
