use std::fs::File;
use std::io::{Seek, SeekFrom, Write};
use std::os::fd::OwnedFd;

use crate::checkpoint::{AreaMeta, Header, PageFlag, SnapshotSlot};
use crate::global_state::{GlobalState, ReservedMemory};
use crate::procfs_maps::{
    MMapPath, MemoryMap, MemoryMapPermissions, PageFlags, ProcMaps, ProcPagemap,
};
use crate::utils::PAGE_SIZE;
use crate::{Result, debug, warn};

pub struct SnapshotWriter {
    // todo: stack buffered?
    pub snapshot_meta: File,
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
pub fn write_all_areas(writer: &mut SnapshotWriter) -> Result<()> {
    let mut maps = File::open("/proc/self/maps")?;
    let mut copy = File::create("/tmp/test-a").unwrap();
    std::io::copy(&mut maps, &mut copy)?;
    let mut maps = File::open("/proc/self/maps")?;
    let mut copy = File::create("/tmp/test-b").unwrap();
    std::io::copy(&mut maps, &mut copy)?;

    // TODO proc.clear_refs(ClearRefs::SoftDirty)?;

    let mut maps = ProcMaps::new_self_copy()?;
    let mut page_map = ProcPagemap::new_self()?;

    let mut header = Header { area_count: 0 };
    let header_pos = {
        let pos = writer.snapshot_meta.stream_position()?;
        writer
            .snapshot_meta
            .seek_relative(size_of::<Header>() as i64)?;
        pos
    };

    while let Some(area) = maps.next()? {
        header.area_count += 1;

        write_area(writer, &area, &mut page_map)?;
    }

    writer.snapshot_meta.seek(SeekFrom::Start(header_pos))?;
    writer.snapshot_meta.write_all(unsafe {
        std::slice::from_raw_parts(std::ptr::from_ref(&header).cast(), size_of::<Header>())
    })?;

    // postcard::to_io(&maps, snapshot_area)?;

    Ok(())
}

#[allow(clippy::undocumented_unsafe_blocks)]
fn write_area(
    writer: &mut SnapshotWriter,
    area: &MemoryMap,
    page_map: &mut ProcPagemap,
) -> Result<()> {
    // mprotect read

    let page_data_offset = writer.snapshot_pages.stream_position()?;
    let uncommitted = area_is_uncommmited(area, page_map)?;

    // todo more skip?
    let skip = skip(area);
    writer.snapshot_meta.write_all(area.as_bytes())?;
    let meta = AreaMeta {
        skip,
        uncommitted,
        page_data_offset,
    };
    writer.snapshot_meta.write_all(meta.as_bytes())?;

    if skip || uncommitted {
        debug!(Checkpoint, "Skip   {}", area);
        // mprotect?
        return Ok(());
    }
    debug!(Checkpoint, "Saving {}", area);

    let mut shared_fd: Option<(OwnedFd, u64, u64)> = None;

    if area.perms.contains(MemoryMapPermissions::SHARED) {
        let fd = area.open_file().ok();

        if let Some(fd) = fd {
            match rustix::fs::seek(&fd, rustix::fs::SeekFrom::Hole(area.offset)) {
                Err(_) => std::mem::drop(fd),
                #[allow(unused)]
                Ok(mut next_off_hole) => {
                    let mut next_off_data =
                        rustix::fs::seek(&fd, rustix::fs::SeekFrom::Data(area.offset))
                            .unwrap_or(u64::MAX);
                    next_off_hole &= !(PAGE_SIZE as u64 - 1);
                    next_off_data &= !(PAGE_SIZE as u64 - 1);
                    shared_fd = Some((fd, next_off_hole, next_off_data));
                }
            }
        } else {
            warn!(
                Restore,
                "Could not open shared map file for save {}", area.pathname
            )
        }
    }

    #[derive(Debug)]
    struct Stats {
        unmapped: usize,
        zero_or_file: usize,
        full: usize,

        modified: usize,
    }
    let mut stats = Stats {
        unmapped: 0,
        zero_or_file: 0,
        full: 0,
        modified: 0,
    };

    let mut flags_written = 0;
    let mut save_page_flags = |snapshot_meta: &mut File, flag: PageFlag| {
        // todo buffer
        flags_written += 1;
        snapshot_meta.write_all(&[flag as u8])
        // snapshot_meta.write_all(&[])
    };

    page_map.seek(area.page())?;

    for page in area.pages() {
        let addr = std::ptr::with_exposed_provenance(page * PAGE_SIZE);
        let page_flags = page_map.read_single()?; // TODO buffer

        /*if page_flags.contains(PageFlags::SWAP) {
            todo!("swap: {page_flags:?}");
        }*/

        let page_present = page_flags.contains(PageFlags::PRESENT);
        let page_file = page_flags.contains(PageFlags::FILE);
        let page_softdirty = page_flags.contains(PageFlags::SOFT_DIRTY);
        let page_guard_region = page_flags.bits() & (1 << 58) != 0;

        if page_guard_region {
            save_page_flags(&mut writer.snapshot_meta, PageFlag::Guard)?;
            continue;
        }

        if area.perms.contains(MemoryMapPermissions::PRIVATE) {
            if let MMapPath::Anonymous = area.path_kind
                && !page_present
            {
                stats.unmapped += 1;
                save_page_flags(&mut writer.snapshot_meta, PageFlag::No)?;
                continue;
            }

            // TODO: is zero page

            if let MMapPath::Path = &area.path_kind
                && (!page_present || page_file)
            {
                stats.zero_or_file += 1;
                // debug assert eq
                save_page_flags(&mut writer.snapshot_meta, PageFlag::File)?;
                continue;
            }
        }

        if area.perms.contains(MemoryMapPermissions::SHARED) {
            if let Some((ref shared_fd, mut next_off_hole, mut next_off_data)) = shared_fd {
                let current_off = area.offset;
                if current_off > next_off_data && current_off > next_off_hole {
                    next_off_data =
                        rustix::fs::seek(shared_fd, rustix::fs::SeekFrom::Data(current_off))?;
                    next_off_data &= !(PAGE_SIZE as u64 - 1);
                    next_off_hole =
                        rustix::fs::seek(shared_fd, rustix::fs::SeekFrom::Hole(current_off))?;
                    next_off_hole &= !(PAGE_SIZE as u64 - 1);
                }

                if current_off == next_off_data || current_off < next_off_hole {
                    // current page is data
                } else if current_off == next_off_hole || current_off < next_off_data {
                    save_page_flags(&mut writer.snapshot_meta, PageFlag::No)?;
                    stats.unmapped += 1;
                } else {
                    todo!();
                }
            }

            if !page_present {
                stats.zero_or_file += 1;
                save_page_flags(&mut writer.snapshot_meta, PageFlag::File)?;
                continue;
            }
            if is_zero_page() {
                stats.zero_or_file += 1;
                save_page_flags(&mut writer.snapshot_meta, PageFlag::Zero)?;
                continue;
            }
        }

        if page_softdirty {
            stats.modified += 1;
        }

        #[allow(clippy::overly_complex_bool_expr)]
        if !page_softdirty && false {
            todo!();
        } else {
            // todo: queue + bigger reads
            let page_data = unsafe { std::slice::from_raw_parts(addr, PAGE_SIZE) };
            writer.snapshot_pages.write_all(page_data)?;

            stats.full += 1;
            save_page_flags(&mut writer.snapshot_meta, PageFlag::Full)?;
            continue;
        }
    }

    debug_assert_eq!(flags_written, area.page_count());

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
