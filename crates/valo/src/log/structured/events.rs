// TODO(ai-review): generated, not yet audited
use crate::checkpoint::{PageFlag, SnapshotSlot};
use crate::procfs_maps::MemoryMap;

use super::{StackWriter, structured_write, write_field};

pub fn init() {
    structured_write(|w| {
        write_field(w, "e", "init");
        write_field(w, "pid", std::process::id() as u64);
    });
}

pub fn save_area(area: &MemoryMap, skip: bool) {
    structured_write(|w| {
        write_field(w, "e", "save_area");
        write_field(w, "start", area.address.start);
        write_field(w, "end", area.address.end);
        write_field(w, "perms", area.perms);
        write_field(w, "path", area.pathname.as_str());
        write_field(w, "skip", skip);
    });
}

pub fn save_pages(start: u64, index: u64, count: u64, flag: PageFlag) {
    structured_write(|w| {
        write_field(w, "e", "save_pages");
        write_field(w, "start", start);
        write_field(w, "index", index);
        write_field(w, "count", count);
        write_field(w, "flag", flag as u8 as u64);
    });
}

pub fn restore_area(area: &MemoryMap, skip: bool) {
    structured_write(|w| {
        write_field(w, "e", "restore_area");
        write_field(w, "start", area.address.start);
        write_field(w, "end", area.address.end);
        write_field(w, "perms", area.perms);
        write_field(w, "path", area.pathname.as_str());
        write_field(w, "skip", skip);
    });
}

pub fn checkpoint_begin(slot: SnapshotSlot) {
    structured_write(|w| {
        write_field(w, "e", "checkpoint_begin");
        write_field(w, "slot", slot.0 as u64);
    });
}

pub fn restore_begin(slot: SnapshotSlot) {
    structured_write(|w| {
        write_field(w, "e", "restore_begin");
        write_field(w, "slot", slot.0 as u64);
    });
}

pub fn saved_area(area: &MemoryMap) {
    structured_write(|w| {
        write_field(w, "e", "saved_area");
        write_field(w, "start", area.address.start);
        write_field(w, "end", area.address.end);
        write_field(w, "perms", area.perms);
        write_field(w, "path", area.pathname.as_str());
    });
}

pub fn current_area(area: &MemoryMap) {
    structured_write(|w| {
        write_field(w, "e", "current_area");
        write_field(w, "start", area.address.start);
        write_field(w, "end", area.address.end);
        write_field(w, "perms", area.perms);
        write_field(w, "path", area.pathname.as_str());
    });
}
