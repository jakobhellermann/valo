//! Message types of Kumquat's control connection (kumquat/server/src/control.rs). Every message
//! starts with a little-endian `u32` type; requests carry the snapshot directory as payload.

pub const SNAPSHOT: u32 = 0x1;
pub const RESTORE: u32 = 0x2;
pub const READY: u32 = 0x100;
pub const RESP_OK: u32 = 0x101;
/// The payload is the UTF-8 error.
pub const RESP_ERR: u32 = 0x102;
