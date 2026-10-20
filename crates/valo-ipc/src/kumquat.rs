//! Message types of Kumquat's control connection (kumquat/server/src/control.rs). Every message
//! starts with a little-endian `u32` type; snapshot and restore requests carry the snapshot
//! directory as payload.

pub const SNAPSHOT: u32 = 0x1;
/// Answered with RESP_OK, one REMAP per replaced blob memory, then END.
pub const RESTORE: u32 = 0x2;
/// Answered with RESP_OK, one RESOURCE per blob memory a client holds, then END.
pub const LIST: u32 = 0x3;
pub const READY: u32 = 0x100;
pub const RESP_OK: u32 = 0x101;
/// The payload is the UTF-8 error.
pub const RESP_ERR: u32 = 0x102;
/// `u32` resource id, `u64` device and `u64` inode of the blob memory.
pub const RESOURCE: u32 = 0x103;
/// `u32` resource id; the new blob memory is the attached descriptor.
pub const REMAP: u32 = 0x104;
pub const END: u32 = 0x105;
