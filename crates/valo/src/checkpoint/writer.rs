use std::fs::File;
use std::io::{Seek, SeekFrom, Write};

use crate::Result;
use crate::checkpoint::{AreaMeta, Header, PageFlag};
use crate::global_state::{GlobalState, ReservedMemory};
use crate::procfs_maps::{
    MMapPath, MemoryMap, MemoryMapPermissions, PageFlags, ProcMaps, ProcPagemap,
};
use crate::utils::PAGE_SIZE;

pub struct SnapshotMetaWriter {
    // todo: stack buffered?
    pub snapshot_meta: File,
    pub snapshot_pages: File,
}

impl SnapshotMetaWriter {
    pub fn new() -> Result<SnapshotMetaWriter> {
        Ok(SnapshotMetaWriter {
            snapshot_meta: File::create(super::SNAPSHOT_PATH_META)?,
            snapshot_pages: File::create(super::SNAPSHOT_PATH_PAGEDATA)?,
        })
    }
}

pub fn write_all_areas() -> Result<()> {
    let mut writer = SnapshotMetaWriter::new()?;

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

        // mprotect read

        let page_data_offset = writer.snapshot_pages.stream_position()?;
        let uncommitted = area_is_uncommmited(&area, &mut page_map)?;

        // todo more skip?
        let skip = skip(&area);
        writer.snapshot_meta.write_all(unsafe {
            std::slice::from_raw_parts(
                std::ptr::from_ref(&area).cast(),
                std::mem::size_of::<MemoryMap>(),
            )
        })?;
        let meta = AreaMeta {
            skip,
            uncommitted,
            page_data_offset,
        };
        writer.snapshot_meta.write_all(unsafe {
            std::slice::from_raw_parts(
                std::ptr::from_ref(&meta).cast(),
                std::mem::size_of::<AreaMeta>(),
            )
        })?;

        eprint!("{}", area);
        eprintln!(" skipped={}", skip);

        if skip || uncommitted {
            // mprotect?
            continue;
        }

        debug_assert!(area.address.start % PAGE_SIZE as u64 == 0);

        if area.perms.contains(MemoryMapPermissions::SHARED) {
            todo!()
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
            let addr = page * PAGE_SIZE;
            let page_flags = page_map.read_single()?; // TODO buffer

            if page_flags.contains(PageFlags::SWAP) {
                todo!();
            }

            let page_present = page_flags.contains(PageFlags::PRESENT);
            let page_file = page_flags.contains(PageFlags::FILE);
            let page_softdirty = page_flags.contains(PageFlags::SOFT_DIRTY);
            let page_guard_region = page_flags.bits() & (1 << 58) != 0;

            if page_guard_region {
                save_page_flags(&mut writer.snapshot_meta, PageFlag::Guard)?;
                todo!();
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
                todo!();
            }

            if page_softdirty {
                stats.modified += 1;
            }

            #[allow(clippy::overly_complex_bool_expr)]
            if !page_softdirty && false {
                todo!();
            } else {
                // todo: queue + bigger reads
                let page_data = unsafe { std::slice::from_raw_parts(addr as *const _, PAGE_SIZE) };
                writer.snapshot_pages.write_all(page_data)?;

                stats.full += 1;
                save_page_flags(&mut writer.snapshot_meta, PageFlag::Full)?;
                continue;
            }
        }

        debug_assert_eq!(flags_written, area.page_count());

        if false {
            eprintln!(
                "  unmapped={} zero_or_file={} full={} softdirty={}",
                stats.unmapped, stats.zero_or_file, stats.full, stats.modified,
            );
        }
    }

    writer.snapshot_meta.seek(SeekFrom::Start(header_pos))?;
    writer.snapshot_meta.write_all(unsafe {
        std::slice::from_raw_parts(std::ptr::from_ref(&header).cast(), size_of::<Header>())
    })?;

    // postcard::to_io(&maps, snapshot_area)?;

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

    match area.path_kind {
        // vectors
        MMapPath::Vsyscall | MMapPath::Vdso | MMapPath::Vvar => return true,
        // todo: memfd => false,
        MMapPath::Anonymous => {
            let reserved_addr = GlobalState::get().reserved as usize as u64;

            if area.address.start == reserved_addr && size == size_of::<ReservedMemory>() as u64 {
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
