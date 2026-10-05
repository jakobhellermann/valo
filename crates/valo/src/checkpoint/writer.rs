#![cfg_attr(not(feature = "structured-log"), allow(unused))]
use std::fs::File;
use std::io::{Seek, SeekFrom, Write};
use std::os::fd::OwnedFd;

use rustix::fs::SeekFrom as RSeekFrom;

use crate::checkpoint::threads::SavedThreads;
use crate::checkpoint::{AreaMeta, Header, PageFlag, SnapshotSlot};
use crate::global_state::{GlobalState, ReservedMemory};
use crate::log::structured;
use crate::procfs::maps::{
    MMapPath, MemoryMap, MemoryMapPermissions, PageFlags, PageIndex, ProcMaps, ProcPagemap,
};
use crate::utils::{PAGE_SIZE, align_page_down, align_page_up};

use crate::{Result, debug, info, warn};

pub struct SnapshotWriter {
    // todo: stack buffered?

    // Header, [MemoryMap Area] x n_areas
    pub snapshot_meta: File,
    // PageFlag x n_page_flags
    pub snapshot_pages: File,
}

impl SnapshotWriter {
    pub fn new(slot: SnapshotSlot) -> Result<SnapshotWriter> {
        Ok(SnapshotWriter {
            snapshot_meta: File::create(super::snapshot_path(slot, "meta").as_str())?,
            snapshot_pages: File::create(super::snapshot_path(slot, "pages").as_str())?,
        })
    }
}

#[allow(clippy::undocumented_unsafe_blocks)]
pub fn write_all_areas(writer: &mut SnapshotWriter, threads: SavedThreads) -> Result<()> {
    // TODO proc.clear_refs(ClearRefs::SoftDirty)?;

    let mut maps = ProcMaps::new_self_copy()?;
    let mut page_map = ProcPagemap::new_self()?;

    let mut header = Header {
        area_count: 0,
        threads,
    };
    let header_pos = writer.snapshot_meta.stream_position()?;
    writer
        .snapshot_meta
        .seek_relative(size_of::<Header>() as i64)?;

    while let Some(area) = maps.next()? {
        header.area_count += 1;

        write_area(writer, &area, &mut page_map)?;
    }

    writer.snapshot_meta.seek(SeekFrom::Start(header_pos))?;
    writer.snapshot_meta.write_all(header.as_bytes())?;

    Ok(())
}

#[derive(Default)]
struct AreaStats {
    unmapped: usize,
    zero_or_file: usize,
    full: usize,

    modified: usize,
}

// Write the [MemoryMap, AreaMeta] into the writer.
// Returns whether individual pages need to be saved.
fn write_area_meta(
    writer: &mut SnapshotWriter,
    area: &MemoryMap,
    page_map: &mut ProcPagemap,
) -> Result<AreaMeta> {
    // TODO: mprotect read?
    // TODO: "fill deleted fd"

    let page_data_offset = writer.snapshot_pages.stream_position()?;
    let uncommitted = area_is_uncommmited(area, page_map)?;

    // TODO: more skip logic?
    let skip = skip(area);
    writer.snapshot_meta.write_all(area.as_bytes())?;
    let meta = AreaMeta {
        skip,
        uncommitted,
        page_data_offset,
    };
    writer.snapshot_meta.write_all(meta.as_bytes())?;

    Ok(meta)
}

// File descriptor of the shared area, whith hold information.
// Returns offset of the next hold and next data.
fn area_fd(area: &MemoryMap) -> Option<(OwnedFd, u64, u64)> {
    let fd = area
        .open_file()
        .inspect_err(|e| {
            warn!(
                Checkpoint,
                "Could not open shared map file for save {}: err={}",
                area.pathname,
                e.raw_os_error()
            )
        })
        .ok()?;

    let mut next_off_hole = rustix::fs::seek(&fd, RSeekFrom::Hole(area.offset))
        .inspect_err(|e| {
            warn!(
                Checkpoint,
                "Could not seek shared map file for save {}: err={}",
                area.pathname,
                e.raw_os_error()
            )
        })
        .ok()?;

    let mut next_off_data = rustix::fs::seek(&fd, RSeekFrom::Data(area.offset)).unwrap_or(u64::MAX);
    next_off_hole = align_page_up(next_off_hole);
    next_off_data = align_page_down(next_off_data);

    if next_off_data == 0 && next_off_hole == 0 {
        // TODO if shared_fd != area.fd
        info!(Checkpoint, "shared fd hold and data are both 0");
        return None;
    }

    Some((fd, next_off_hole, next_off_data))
}

fn write_area(
    writer: &mut SnapshotWriter,
    area: &MemoryMap,
    page_map: &mut ProcPagemap,
) -> Result<()> {
    let meta = write_area_meta(writer, area, page_map)?;
    if meta.skip || meta.uncommitted {
        debug!(Checkpoint, "Skip   {}", area);
        structured::events::save_area(area, true);

        return Ok(());
    }
    structured::events::save_area(area, false);

    debug!(Checkpoint, "Saving {}", area);

    let mut stats = AreaStats::default();
    let mut page_saves = PageSaveQueue::default();

    #[cfg(feature = "structured-log")]
    let (mut log_saved_flags, mut log_page_flags) = (
        structured::events::checkpoint_saved_page_run(area),
        structured::events::checkpoint_mem_page_run(area),
    );

    let mut save_page_flags = |snapshot_meta: &mut File, flag: PageFlag| {
        #[cfg(feature = "structured-log")]
        log_saved_flags.record(flag);

        // todo buffer
        snapshot_meta.write_all(&[flag as u8])
    };

    let mut shared_fd = area
        .perms
        .contains(MemoryMapPermissions::SHARED)
        .then(|| area_fd(area))
        .flatten();
    structured::events::shared_fd_initial(area.address.start, shared_fd.as_ref());

    let mut write_area_page = |page: PageIndex,
                               page_flags: PageFlags|
     -> Result<(), std::io::Error> {
        let addr = std::ptr::with_exposed_provenance(page * PAGE_SIZE);

        #[cfg(feature = "structured-log")]
        log_page_flags.record(page_flags);

        /*if page_flags.contains(PageFlags::SWAP) {
            todo!("swap: {page_flags:?}");
        }*/

        let page_present = page_flags.contains(PageFlags::PRESENT);
        let page_file = page_flags.contains(PageFlags::FILE);
        let page_softdirty = page_flags.contains(PageFlags::SOFT_DIRTY);
        let page_guard_region = page_flags.bits() & (1 << 58) != 0;

        if page_guard_region {
            save_page_flags(&mut writer.snapshot_meta, PageFlag::Guard)?;
            return Ok(());
        }

        if area.perms.contains(MemoryMapPermissions::PRIVATE) {
            if let MMapPath::Anonymous = area.path_kind
                && !page_present
            {
                stats.unmapped += 1;
                save_page_flags(&mut writer.snapshot_meta, PageFlag::No)?;
                return Ok(());
            }

            // TODO: is zero page

            if let MMapPath::Path = &area.path_kind
                && (!page_present || page_file)
            {
                stats.zero_or_file += 1;
                // debug assert eq
                save_page_flags(&mut writer.snapshot_meta, PageFlag::File)?;
                return Ok(());
            }
        }

        if area.perms.contains(MemoryMapPermissions::SHARED) {
            let page_i = page - area.page();
            let current_off = area.offset + page_i as u64 * PAGE_SIZE as u64;

            if let Some((ref fd, ref mut next_off_hole, ref mut next_off_data)) = shared_fd
                && current_off > *next_off_data
                && current_off > *next_off_hole
            {
                // update data and hole
                *next_off_data =
                    rustix::fs::seek(fd, RSeekFrom::Data(current_off)).unwrap_or(u64::MAX);
                match rustix::fs::seek(fd, RSeekFrom::Hole(current_off)) {
                    Err(_) => {
                        warn!(
                            Checkpoint,
                            "Shared map file {} could not be queried for hole information while saving",
                            area.pathname
                        );
                        shared_fd = None;
                    }
                    Ok(hole) => {
                        *next_off_data = align_page_down(*next_off_data);
                        *next_off_hole = align_page_up(hole);
                    }
                }
            }

            if let Some((_, next_off_hole, next_off_data)) = shared_fd.as_ref() {
                if current_off == *next_off_data || current_off < *next_off_hole {
                    // current page is data
                } else if current_off == *next_off_hole || current_off < *next_off_data {
                    save_page_flags(&mut writer.snapshot_meta, PageFlag::No)?;
                    stats.unmapped += 1;

                    return Ok(());
                } else {
                    todo!();
                }
            }

            if !page_present && !matches!(area.path_kind, MMapPath::MemFd) {
                stats.zero_or_file += 1;
                save_page_flags(&mut writer.snapshot_meta, PageFlag::File)?;
                return Ok(());
            }
            if is_zero_page() {
                stats.zero_or_file += 1;
                save_page_flags(&mut writer.snapshot_meta, PageFlag::Zero)?;
                return Ok(());
            }
        }

        if page_softdirty {
            stats.modified += 1;
        }

        #[allow(clippy::overly_complex_bool_expr)]
        if !page_softdirty && false {
            todo!();
        } else {
            page_saves.push(&mut writer.snapshot_pages, addr)?;

            stats.full += 1;
            save_page_flags(&mut writer.snapshot_meta, PageFlag::Full)?;
            Ok(())
        }
    };

    page_map.for_each_page(area.pages(), &mut write_area_page)?;
    page_saves.flush(&mut writer.snapshot_pages)?;

    debug!(
        Checkpoint,
        "stats: unmapped={} zero_or_file={} full={} softdirty={}",
        stats.unmapped,
        stats.zero_or_file,
        stats.full,
        stats.modified,
    );

    Ok(())
}

#[derive(Default)]
struct PageSaveQueue {
    start: *const u8,
    pages: usize,
}

impl PageSaveQueue {
    fn push(&mut self, file: &mut File, addr: *const u8) -> Result<()> {
        const MAX_PAGES: usize = 256;
        if self.pages > 0
            && (addr != self.start.wrapping_add(self.pages * PAGE_SIZE) || self.pages == MAX_PAGES)
        {
            self.flush(file)?;
        }
        if self.pages == 0 {
            self.start = addr;
        }
        self.pages += 1;
        Ok(())
    }

    fn flush(&mut self, file: &mut File) -> Result<()> {
        if self.pages == 0 {
            return Ok(());
        }
        // SAFETY: each queued Full page is readable when pushed, the queue only
        // contains consecutive pages, and other guest threads are suspended.
        let src = unsafe { std::slice::from_raw_parts(self.start, self.pages * PAGE_SIZE) };
        file.write_all(src)?;
        self.pages = 0;
        Ok(())
    }
}

fn area_is_uncommmited(area: &MemoryMap, page_map: &mut ProcPagemap) -> Result<bool> {
    if area.perms.contains(MemoryMapPermissions::READ)
        || area.perms.contains(MemoryMapPermissions::WRITE)
    {
        return Ok(false);
    }

    if !area.perms.contains(MemoryMapPermissions::PRIVATE) {
        return Ok(false);
    }

    let MMapPath::Anonymous = area.path_kind else {
        return Ok(false);
    };

    let page_flags = page_map.read_at_single(area.page())?;

    let present = page_flags.contains(PageFlags::PRESENT);
    Ok(!present)
}

#[allow(clippy::needless_bool)]
fn skip(area: &MemoryMap) -> bool {
    if area.address.end >= 0x7f00000000 && area.address.start == 0xffffe000 {
        return true;
    }
    if area.address.end >= 0x7f00000000 && area.address.start == 0xffffffffff600000 {
        return true;
    }
    let size = area.address.end - area.address.start;

    if size == 0 {
        return true;
    }

    // When built with -C instrument-coverage, the LLVM profile counters live in
    // writable mapped pages of libvalo.so. Snapshotting them would cause restore
    // to wipe counter increments made during the signal handler that performs
    // the restore itself. Skip the area covering those pages so the counters
    // accumulate across restores.
    if covers_coverage_counters(area) {
        return true;
    }

    match area.path_kind {
        // vectors
        MMapPath::Vsyscall | MMapPath::Vdso | MMapPath::Vvar => return true,
        // todo: memfd => false,
        MMapPath::Anonymous => {
            let reserved_addr = GlobalState::get().reserved as usize as u64;
            let reserved_len = size_of::<ReservedMemory>().next_multiple_of(PAGE_SIZE) as u64;

            if area.address.start == reserved_addr && size == reserved_len {
                return true;
            }

            return false;
        }
        _ => {}
    }

    if area.perms.contains(MemoryMapPermissions::WRITE) {
        return false;
    }

    true
}

#[cfg(coverage)]
fn covers_coverage_counters(area: &MemoryMap) -> bool {
    unsafe extern "C" {
        // Stubs in compiler-rt's profiling runtime that return the bounds of the
        // __llvm_prf_cnts section. They are tiny `mov rax, &section; ret` thunks —
        // they must be called, not interpreted as the addresses themselves.
        fn __llvm_profile_begin_counters() -> *const u8;
        fn __llvm_profile_end_counters() -> *const u8;
    }
    // SAFETY: simple address-returning thunks, no preconditions
    let (begin, end) = unsafe {
        (
            __llvm_profile_begin_counters() as u64,
            __llvm_profile_end_counters() as u64,
        )
    };
    area.address.start < end && begin < area.address.end
}

#[cfg(not(coverage))]
fn covers_coverage_counters(_area: &MemoryMap) -> bool {
    false
}

fn is_zero_page() -> bool {
    false // TODO
}
