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

pub fn reallocate_areas_begin() {
    trace_step("restore.memory.reallocate.begin");
}

pub fn reallocate_areas_end() {
    trace_step("restore.memory.reallocate.end");
}

pub fn restore_area(area: &MemoryMap, skip: bool) {
    structured_write(|w| {
        write_field(w, "e", "restore.memory.area.begin");
        write_field(w, "ts_us", timestamp_us());
        write_field(w, "start", area.address.start);
        write_field(w, "end", area.address.end);
        write_field(w, "perms", area.perms);
        write_field(w, "path", area.pathname.as_str());
        write_field(w, "skip", skip);
    });
}

pub fn restore_area_done(area: &MemoryMap) {
    structured_write(|w| {
        write_field(w, "e", "restore.memory.area.end");
        write_field(w, "ts_us", timestamp_us());
        write_field(w, "start", area.address.start);
    });
}

pub fn checkpoint_begin(slot: SnapshotSlot) {
    trace_slot_event("checkpoint.begin", slot);
}

pub fn checkpoint_end(slot: SnapshotSlot) {
    trace_slot_event("checkpoint.end", slot);
}

pub fn restore_begin(slot: SnapshotSlot) {
    trace_slot_event("restore.begin", slot);
}

pub fn restore_end(slot: SnapshotSlot) {
    trace_slot_event("restore.end", slot);
}

pub fn trace_step(event: &'static str) {
    trace_event(event);
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

fn trace_event(event: &'static str) {
    trace_event_with(event, |_| {});
}

fn trace_slot_event(event: &'static str, slot: SnapshotSlot) {
    trace_event_with(event, |w| write_field(w, "slot", slot.0 as u64));
}

fn trace_event_with(event: &'static str, fields: impl FnOnce(&mut StackWriter)) {
    structured_write(|w| {
        write_field(w, "e", event);
        write_field(w, "ts_us", timestamp_us());
        fields(w);
    });
}

fn timestamp_us() -> u64 {
    let time = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    time.tv_sec as u64 * 1_000_000 + time.tv_nsec as u64 / 1_000
}
