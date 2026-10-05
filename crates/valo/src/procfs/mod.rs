pub mod fds;
pub mod maps;

use std::fs::File;
use std::io::Write;

pub fn clear_soft_dirty() -> Result<(), std::io::Error> {
    File::options()
        .write(true)
        .open("/proc/self/clear_refs")?
        .write_all(b"4\n")
}
