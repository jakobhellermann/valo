use std::cmp::Ordering;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::mem::MaybeUninit;

use crate::checkpoint::{AreaMeta, Header, PageFlag, Result};
use crate::procfs_maps::{
    MMapPath, MemoryMap, MemoryMapPermissions, PageFlags, ProcMaps, ProcPagemap,
};
use crate::utils::PAGE_SIZE;

pub struct SnapshotMetaReader {
    snapshot_meta: File,
    pub snapshot_page_data: File,
    flags_remaining: usize,
    pub header: Header,
}

impl SnapshotMetaReader {
    pub fn new() -> Result<SnapshotMetaReader> {
        let mut reader = SnapshotMetaReader {
            snapshot_meta: File::open(super::SNAPSHOT_PATH_META)?,
            snapshot_page_data: File::open(super::SNAPSHOT_PATH_PAGEDATA)?,
            flags_remaining: 0,
            header: Header { area_count: 0 },
        };
        // SAFETY: assume layout
        reader.header = unsafe { reader.read_data::<Header>()? };
        Ok(reader)
    }
    // SAFETY:
    // - A (POD) value of `T` mut be stored at the current position
    unsafe fn read_data<T>(&mut self) -> Result<T> {
        let mut data = MaybeUninit::<T>::zeroed();
        // SAFETY: zeroed
        let buf = unsafe { data.as_bytes_mut().assume_init_mut() };
        self.snapshot_meta.read_exact(buf)?;
        // SAFETY: read-exact read full, assume precondition
        let data = unsafe { data.assume_init() };
        Ok(data)
    }

    pub fn restart(&mut self) -> Result<()> {
        self.snapshot_meta
            .seek(SeekFrom::Start(size_of::<Header>() as u64))?;
        Ok(())
    }

    pub fn read(&mut self) -> Result<(MemoryMap, AreaMeta)> {
        if self.flags_remaining > 0 {
            self.snapshot_meta
                .seek_relative(self.flags_remaining as i64 * size_of::<PageFlag>() as i64)?;
        }

        // SAFETY: assume layout
        let map = unsafe { self.read_data::<MemoryMap>()? };
        // SAFETY: assume layout
        let meta = unsafe { self.read_data::<AreaMeta>()? };

        self.flags_remaining = if meta.skip || meta.uncommitted {
            0
        } else {
            map.page_count()
        };

        Ok((map, meta))
    }

    pub fn read_pageflag(&mut self) -> Result<PageFlag> {
        if self.flags_remaining > 0 {
            self.flags_remaining -= 1;

            // SAFETY: assume layout, PageFlag is just u8
            unsafe { self.read_data::<PageFlag>() }
        } else {
            todo!()
        }
    }
}

pub fn read_all_areas() -> Result<()> {
    let mut reader = SnapshotMetaReader::new()?;
    let saved_maps = (0..reader.header.area_count).map(|_| reader.read());

    /*let mut snapshot_area_vec = std::fs::read(SNAPSHOT_PATH_META)?;
    let mut snapshot_pagedata = BufReader::new(File::open(SNAPSHOT_PATH_PAGEDATA)?);

    let mut snapshot_area = snapshot_area_vec.as_mut_slice();
    let mut saved_maps = Vec::new();
    while !snapshot_area.is_empty() {
        let map;
        let meta;
        (map, snapshot_area) = snapshot_area.split_at_mut(size_of::<MemoryMap>());
        (meta, snapshot_area) = snapshot_area.split_at_mut(size_of::<AreaMeta>());
        let map = unsafe { &mut *map.as_mut_ptr().cast::<MemoryMap>() };
        let meta = unsafe { &mut *meta.as_mut_ptr().cast::<AreaMeta>() };

        let page_count = map.page_count();
        let mut _page_flags = Vec::new();

        if !(meta.skip || meta.uncommitted) {
            let (page_flags, rest) = snapshot_area.split_at_mut(page_count as usize);
            let page_flags = unsafe {
                std::slice::from_raw_parts(page_flags.as_ptr().cast::<PageFlag>(), page_flags.len())
            };
            _page_flags = page_flags.to_vec();
            snapshot_area = rest;
        }

        saved_maps.push((map, meta, _page_flags));
    }*/

    let mut maps = ProcMaps::new_self_copy()?;
    let mut page_map = ProcPagemap::new_self()?;

    /*eprintln!("Saved: ");
    for (area, meta, flags) in &saved_maps {
        eprintln!("{}", area);
    }
    eprintln!();
    eprintln!("Current: ");
    while let Some(area) = maps.next()? {
       eprintln!("{}", area);
    }
    eprintln!();
    maps.reset()?;*/

    let mut saved_maps_iter = saved_maps.map(Result::unwrap);

    let mut _saved_area = saved_maps_iter.next();
    let mut _current_area = maps.next()?;

    structured::events::reallocate_areas_begin();

    while _saved_area.is_some() || _current_area.is_some() {
        let (saved_area, current_area) = match (_saved_area.as_mut(), _current_area.as_mut()) {
            (Some(a), Some(b)) => (a, b),
            _ => todo!(),
        };

        let order = reallocate_area(&mut saved_area.0, current_area)?;

        match order {
            Ordering::Less => _saved_area = saved_maps_iter.next(),
            Ordering::Equal => {
                _saved_area = saved_maps_iter.next();
                _current_area = maps.next()?;
            }
            Ordering::Greater => _current_area = maps.next()?,
        }
    }
    structured::events::reallocate_areas_end();

    reader.restart()?;

    for _ in 0..reader.header.area_count {
        let (area, meta) = reader.read()?;

        read_area(&area, &meta, &mut reader, &mut page_map)?;
    }

    Ok(())
}

#[allow(clippy::undocumented_unsafe_blocks)]
fn read_area(
    saved_area: &MemoryMap,
    meta: &AreaMeta,
    reader: &mut SnapshotMetaReader,
    current_pagemap: &mut ProcPagemap,
) -> Result<()> {
    // todo: uncommitted

    if meta.skip {
        return Ok(());
    }

    if meta.uncommitted {
        // TODO: check current is uncommitted
        return Ok(());
    }

    // mprotect read

    let page_idx = saved_area.page();
    current_pagemap.seek(page_idx)?;
    let page_count = saved_area.page_count();

    if saved_area.perms.contains(MemoryMapPermissions::SHARED) {
        todo!();
    }

    // eprintln!("restore {}", saved_area);

    reader
        .snapshot_page_data
        .seek(SeekFrom::Start(meta.page_data_offset))?;

    struct Stats {
        full: usize,
    }
    let mut stats = Stats { full: 0 };

    for page_idx in page_idx..page_idx + page_count {
        let current_pageflag = current_pagemap.read_at_single(page_idx)?; // TODO buffer

        let page_addr = page_idx * PAGE_SIZE;

        let flag = if meta.uncommitted {
            PageFlag::No
        } else {
            reader.read_pageflag()?
        };

        if current_pageflag.contains(PageFlags::SWAP) {
            todo!();
        }

        let page_present = current_pageflag.contains(PageFlags::PRESENT);
        let _page_file = current_pageflag.contains(PageFlags::FILE);
        let _page_softdirty = current_pageflag.contains(PageFlags::SOFT_DIRTY);
        let page_guard_region = current_pageflag.bits() & (1 << 58) != 0;

        if let PageFlag::Guard = flag {
            todo!();
        } else if page_guard_region {
            todo!()
        }

        if let PageFlag::No = flag {
            if saved_area.perms.contains(MemoryMapPermissions::PRIVATE) {
                if page_present && !is_zero_page() {
                    // mprotect + memset
                    todo!();
                }
            } else {
                todo!();
            }
            continue;
        }

        if let PageFlag::Zero = flag {
            // todo soft-dirty
            if !is_zero_page() {
                // mprotect + memset 0
                todo!();
            }
        }

        if let PageFlag::File = flag {
            todo!();
        }

        // todo: mprotect write
        // todo: base page
        {
            // queue page load
            if let PageFlag::Full = flag {
                stats.full += 1;
                let mut dst =
                    unsafe { std::slice::from_raw_parts_mut(page_addr as *mut u8, PAGE_SIZE) };
                std::io::copy(
                    &mut (&mut reader.snapshot_page_data).take(PAGE_SIZE as u64),
                    &mut dst,
                )?;
            } else {
                unreachable!("{:?}", flag);
            }
        }

        // todo recover permission
    }
    if false {
        eprintln!("  full={}", stats.full);
    }

    Ok(())
}

fn is_zero_page() -> bool {
    false // TODO
}

fn reallocate_area(saved_area: &mut MemoryMap, current_area: &mut MemoryMap) -> Result<Ordering> {
    /*eprintln!("Saved/Current:");
    eprintln!("{}", saved_area);
    eprintln!("{}", current_area);
    eprintln!();*/

    //eprintln!("{:?} -> {:?}", saved_area, current_area);

    // same start addr
    if saved_area.address.start == current_area.address.start {
        // not the same area, unmap
        if saved_area.pathname != current_area.pathname || saved_area.perms != current_area.perms {
            // unmap
            todo!();
        }
        let saved_area_size = saved_area.address.end - saved_area.address.start;
        let current_area_size = current_area.address.end - current_area.address.start;

        let copy_size = std::cmp::max(saved_area_size, current_area_size);
        if saved_area_size != current_area_size {
            if let MMapPath::Stack = saved_area.path_kind {
                todo!();
            }
            if let MMapPath::Heap = saved_area.path_kind {
                todo!();
            }
            todo!();
        }

        if saved_area.perms != current_area.perms {
            // mprotect
            todo!();
        }

        let is_special_section = match saved_area.path_kind {
                MMapPath::Anonymous |
                MMapPath::Vsys(_)| // TODO?
                MMapPath::Path => false,
                _ => true,
            };
        if saved_area.address.end == current_area.address.end || is_special_section {
            Ok(Ordering::Equal)
        } else if saved_area.address.end > current_area.address.end {
            saved_area.address.start = current_area.address.end;
            saved_area.address.end -= copy_size;
            Ok(Ordering::Greater)
        } else if saved_area.address.end < current_area.address.end {
            current_area.address.start = saved_area.address.end;
            current_area.address.end -= copy_size;
            Ok(Ordering::Greater)
        } else {
            unreachable!()
        }
    } else {
        // not same start addr

        // todo: or saved is none
        if saved_area.address.start > current_area.address.start {
            eprintln!("[realloc] unmap");
            current_area.unmap()?;
            return Ok(Ordering::Greater);
        }

        // todo: or current is none
        if saved_area.address.start < current_area.address.start {
            // todo: if current not none
            if saved_area.address.end > current_area.address.start {
                // overlap
                // unmap
                todo!();
                // return Ok(Ordering::Greater);
            }

            // must be allocated
            if let MMapPath::Path = saved_area.path_kind {
                todo!("realloc path {}", saved_area.pathname);
            }

            // create memory map
            todo!();
        }

        unreachable!()
    }
}
