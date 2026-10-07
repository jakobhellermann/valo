// TODO(ai-review): generated, not yet audited
use std::os::fd::{AsRawFd, OwnedFd};

use rustix::io::FdFlags;

use crate::Result;
use crate::checkpoint::{PageFlag, SnapshotSlot};
use crate::procfs::fds::{OpenFd, ProcFds};
use crate::procfs::maps::{self, MemoryMap, PageFlags, ProcPagemap};
use crate::utils::TrackRun;

use super::{EventWriter, structured_write, write_field};

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

pub fn shared_fd_initial(start: u64, shared_fd: Option<&(OwnedFd, u64, u64)>) {
    structured_write(|w| {
        write_field(w, "e", "shared_fd_initial");
        write_field(w, "start", start);
        write_field(w, "available", shared_fd.is_some());
        if let Some((fd, next_hole, next_data)) = shared_fd {
            write_field(w, "fd", fd.as_raw_fd() as u64);
            write_field(w, "next_hole", *next_hole);
            write_field(w, "next_data", *next_data);
        }
    });
}

pub fn checkpoint_saved_page_run(
    area: &MemoryMap,
) -> TrackRun<PageFlag, impl Fn(usize, usize, PageFlag)> {
    TrackRun::<PageFlag, _>::new(|start, length, flag| {
        save_pages(area.address.start, start as u64, length as u64, flag)
    })
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

pub fn checkpoint_mem_page_run(
    area: &MemoryMap,
) -> TrackRun<maps::PageFlags, impl Fn(usize, usize, maps::PageFlags)> {
    pagemap_run("save_pagemap_pages", area.address.start)
}

fn pagemap_run(
    event: &'static str,
    start: u64,
) -> TrackRun<maps::PageFlags, impl Fn(usize, usize, maps::PageFlags)> {
    TrackRun::new(move |index, count, flags| {
        pagemap_pages(event, start, index as u64, count as u64, flags);
    })
}

fn pagemap_pages(event: &'static str, start: u64, index: u64, count: u64, flags: maps::PageFlags) {
    structured_write(|w| {
        write_field(w, "e", event);
        write_field(w, "start", start);
        write_field(w, "index", index);
        write_field(w, "count", count);
        write_field(w, "bits", pagemap_log_bits(flags));
    });
}

fn current_pagemap_error(start: u64) {
    structured_write(|w| {
        write_field(w, "e", "current_pagemap_error");
        write_field(w, "start", start);
    });
}

pub fn log_current_area(area: &MemoryMap, pagemap: &mut ProcPagemap) {
    current_area(area);
    let mut run = pagemap_run("current_pagemap_pages", area.address.start);
    let result = (|| -> crate::Result<()> {
        pagemap.seek(area.page())?;
        let mut remaining = area.page_count();
        while remaining > 0 {
            let mut chunk = [PageFlags::default(); 256];
            let count = remaining.min(chunk.len());
            pagemap.read(&mut chunk[..count])?;
            for flags in &chunk[..count] {
                run.record(*flags);
            }
            remaining -= count;
        }
        Ok(())
    })();
    drop(run);
    if result.is_err() {
        current_pagemap_error(area.address.start);
    }
}

// Linux pagemap bit 58 marks a guard region (since Linux 6.15).
const PAGEMAP_GUARD_REGION: u64 = 1 << 58;

/// Compact structured-log format for pagemap runs (`bits` in JSON):
/// bit 0 = present, 1 = file/shared-anon, 2 = swapped, 3 = guard region,
/// 4 = soft-dirty, 5 = exclusively mapped. PFN and swap location are omitted.
fn pagemap_log_bits(flags: PageFlags) -> u8 {
    u8::from(flags.contains(PageFlags::PRESENT))
        | (u8::from(flags.contains(PageFlags::FILE)) << 1)
        | (u8::from(flags.contains(PageFlags::SWAP)) << 2)
        | (u8::from(flags.bits() & PAGEMAP_GUARD_REGION != 0) << 3)
        | (u8::from(flags.contains(PageFlags::SOFT_DIRTY)) << 4)
        | (u8::from(flags.contains(PageFlags::MMAP_EXCLUSIVE)) << 5)
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

pub fn fd(event: &'static str, entry: &OpenFd) {
    structured_write(|w| {
        write_field(w, "e", event);
        write_field(w, "fd", entry.fd as u32);
        write_field(w, "kind", entry.kind);
        write_field(w, "cloexec", entry.fd_flags.contains(FdFlags::CLOEXEC));
        write_field(w, "status", entry.status_flags.bits() as u64);
        write_field(w, "offset", entry.offset);
        write_field(w, "size", entry.size);
        write_field(w, "dev", entry.dev);
        write_field(w, "ino", entry.ino);
        write_field(w, "link", entry.link);
    });
}

pub fn log_fd_list(event: &'static str) -> Result<()> {
    let mut fds = ProcFds::new_self()?;
    while let Some(entry) = fds.next()? {
        fd(event, &entry);
    }
    Ok(())
}

pub fn trace_step(event: &'static str) {
    trace_event(event);
}

pub fn saved_area(area: &MemoryMap, skip: bool) {
    structured_write(|w| {
        write_field(w, "e", "saved_area");
        write_field(w, "start", area.address.start);
        write_field(w, "end", area.address.end);
        write_field(w, "perms", area.perms);
        write_field(w, "path", area.pathname.as_str());
        write_field(w, "skip", skip);
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
    trace_event_with(event, |w| write_field(w, "slot", slot.0));
}

fn trace_event_with(event: &'static str, fields: impl FnOnce(&mut EventWriter)) {
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
