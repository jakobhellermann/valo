//! Helpers for async-signal-safe/allocation free code.

use std::fmt;

/// Formats an [`io::Error`] without allocating
pub struct SignalSafeFormat<'a, T>(pub &'a T);

impl fmt::Display for SignalSafeFormat<'_, std::io::Error> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0.raw_os_error() {
            Some(code) => match errno_name(code) {
                Some(name) => write!(f, "{name} (os error {code})"),
                None => write!(f, "os error {code}"),
            },
            None => self.0.fmt(f),
        }
    }
}
impl fmt::Display for SignalSafeFormat<'_, rustix::io::Errno> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let code = self.0.raw_os_error();
        match errno_name(code) {
            Some(name) => write!(f, "{name} (os error {code})"),
            None => write!(f, "os error {code}"),
        }
    }
}

fn errno_name(code: i32) -> Option<&'static str> {
    Some(match code {
        libc::EPERM => "EPERM",
        libc::ENOENT => "ENOENT",
        libc::EIO => "EIO",
        libc::ENOMEM => "ENOMEM",
        libc::EACCES => "EACCES",
        libc::EFAULT => "EFAULT",
        libc::EBUSY => "EBUSY",
        libc::EEXIST => "EEXIST",
        libc::EINVAL => "EINVAL",
        libc::EMFILE => "EMFILE",
        libc::ENOSPC => "ENOSPC",
        libc::ESPIPE => "ESPIPE",
        libc::EPIPE => "EPIPE",
        _ => return None,
    })
}
