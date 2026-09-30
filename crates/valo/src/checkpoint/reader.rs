use std::cmp::Ordering;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::mem::MaybeUninit;

use rustix::mm::{self, MapFlags, MprotectFlags, ProtFlags};

use crate::checkpoint::{AreaMeta, Header, PageFlag, Result, SnapshotSlot};
use crate::procfs_maps::{
    MMapPath, MemoryMap, MemoryMapPermissions, PageFlags, ProcMaps, ProcPagemap,
};
use crate::utils::PAGE_SIZE;
use crate::{debug, trace, warn};

pub struct SnapshotReader {
    snapshot_meta: File,
    pub snapshot_page_data: File,
    flags_remaining: usize,
    pub header: Header,
}

impl SnapshotReader {
    pub fn new(slot: SnapshotSlot) -> Result<SnapshotReader> {
        let mut reader = SnapshotReader {
            snapshot_meta: File::open(super::snapshot_path(slot, "meta").as_str())?,
            snapshot_page_data: File::open(super::snapshot_path(slot, "pages").as_str())?,
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

pub fn read_all_areas(reader: &mut SnapshotReader) -> Result<()> {
    debug!(Restore, "Restoring {} areas", reader.header.area_count);

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

    /*debug!("Saved: ");
    for (area, meta, flags) in &saved_maps {
        debug!("{}", area);
    }
    debug!();
    debug!("Current: ");
    while let Some(area) = maps.next()? {
       debug!("{}", area);
    }
    debug!();
    maps.reset()?;*/

    let mut saved_maps_iter = saved_maps.map(Result::unwrap);

    let mut _saved_area = saved_maps_iter.next();
    let mut _current_area = maps.next()?;

    structured::events::reallocate_areas_begin();

    while _saved_area.is_some() || _current_area.is_some() {
        let order = reallocate_area(
            _saved_area.as_mut().map(|x| &mut x.0),
            _current_area.as_mut(),
        )?;

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

    debug!(Restore, "finished reallocating areas");
    reader.restart()?;

    for _ in 0..reader.header.area_count {
        let (area, meta) = reader.read()?;

        read_area(&area, &meta, reader, &mut page_map)?;
    }
    debug!(Restore, "finished reading areas");

    Ok(())
}

#[allow(clippy::undocumented_unsafe_blocks)]
fn read_area(
    saved_area: &MemoryMap,
    meta: &AreaMeta,
    reader: &mut SnapshotReader,
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

    // todo: mprotect read

    let page_idx = saved_area.page();
    current_pagemap.seek(page_idx)?;
    let page_count = saved_area.page_count();

    debug!(Restore, "restore {}", saved_area);

    let shared_fd;

    if saved_area.perms.contains(MemoryMapPermissions::SHARED) {
        let fd = saved_area.open_file().ok();
        shared_fd = fd.and_then(|fd| {
            let mut next_off_hole =
                rustix::fs::seek(&fd, rustix::fs::SeekFrom::Hole(saved_area.offset)).ok()?;
            let mut next_off_data =
                rustix::fs::seek(&fd, rustix::fs::SeekFrom::Data(saved_area.offset))
                    .unwrap_or(u64::MAX);
            next_off_hole &= !(PAGE_SIZE as u64 - 1);
            next_off_data &= !(PAGE_SIZE as u64 - 1);
            Some((fd, next_off_hole, next_off_data))
        });
        if shared_fd.is_none() {
            warn!(
                Checkpoint,
                "Could not open shared map file for restore {}", saved_area.pathname
            );
        }
    }

    reader
        .snapshot_page_data
        .seek(SeekFrom::Start(meta.page_data_offset))?;

    struct Stats {
        full: usize,
        skip: usize,
        zero_or_file: usize,
    }
    let mut stats = Stats {
        full: 0,
        skip: 0,
        zero_or_file: 0,
    };

    for page_idx in page_idx..page_idx + page_count {
        let current_pageflag = current_pagemap.read_at_single(page_idx)?; // TODO buffer

        let page_addr = std::ptr::with_exposed_provenance_mut::<u8>(page_idx * PAGE_SIZE);

        let page_mprotect_with = |extra_flags: MprotectFlags| unsafe {
            // SAFETY: todo
            mm::mprotect(
                page_addr.cast(),
                PAGE_SIZE,
                permission_to_mprotect(saved_area.perms) | extra_flags,
            )
        };
        let page_memset0 = || {
            // SAFETY: todo
            let page = unsafe { std::slice::from_raw_parts_mut(page_addr, PAGE_SIZE) };
            page.fill(0);
        };

        let flag = if meta.uncommitted {
            PageFlag::No
        } else {
            reader.read_pageflag()?
        };

        /*if current_pageflag.contains(PageFlags::SWAP) {
            todo!();
        }*/

        let page_present = current_pageflag.contains(PageFlags::PRESENT);
        let page_file = current_pageflag.contains(PageFlags::FILE);
        let _page_softdirty = current_pageflag.contains(PageFlags::SOFT_DIRTY);
        let page_guard_region = current_pageflag.bits() & (1 << 58) != 0;

        if let PageFlag::Guard = flag {
            stats.skip += 1;
            if !page_guard_region {
                todo!();
            }
            continue;
        } else if page_guard_region {
            todo!()
        }

        page_mprotect_with(MprotectFlags::READ)?;

        if let PageFlag::No = flag {
            if saved_area.perms.contains(MemoryMapPermissions::PRIVATE) {
                if page_present && !is_zero_page() {
                    if !saved_area.perms.contains(MemoryMapPermissions::WRITE) {
                        page_mprotect_with(MprotectFlags::WRITE)?;
                    }
                    page_memset0();
                    stats.zero_or_file += 1;
                } else {
                    stats.skip += 1;
                }
            } else {
                todo!();
            }
            continue;
        }

        if let PageFlag::Zero = flag {
            // todo incremental savestates

            if !is_zero_page() {
                if !saved_area.perms.contains(MemoryMapPermissions::WRITE) {
                    page_mprotect_with(MprotectFlags::WRITE)?;
                }
                page_memset0();
                stats.zero_or_file += 1;
            } else {
                stats.skip += 1;
            }
            continue;
        }

        if let PageFlag::File = flag {
            if page_present && !page_file {
                todo!()
            } else {
                stats.skip += 1;
            }
            continue;
        }

        if !saved_area.perms.contains(MemoryMapPermissions::WRITE) {
            page_mprotect_with(MprotectFlags::WRITE)?;
        }

        // todo: base page

        {
            stats.full += 1;
            queue_page_load(reader, page_addr, flag)?;
        }

        // todo recover permission
    }
    debug!(
        Restore,
        "stats: full={} zero_or_file={} skip={}", stats.full, stats.zero_or_file, stats.skip,
    );

    Ok(())
}

fn queue_page_load(
    reader: &mut SnapshotReader,
    page_addr: *mut u8,
    flag: PageFlag,
) -> Result<(), std::io::Error> {
    if let PageFlag::Full = flag {
        // SAFETY: todo
        let mut dst = unsafe { std::slice::from_raw_parts_mut(page_addr, PAGE_SIZE) };
        std::io::copy(
            &mut (&mut reader.snapshot_page_data).take(PAGE_SIZE as u64),
            &mut dst,
        )?;
    } else {
        unreachable!("{:?}", flag);
    }
    Ok(())
}

fn is_zero_page() -> bool {
    false // TODO
}

// eq -> advance both
// greater -> advance current
// less -> advance saved
fn reallocate_area(
    mut saved_area: Option<&mut MemoryMap>,
    mut current_area: Option<&mut MemoryMap>,
) -> Result<Ordering> {
    /*debug!("\nSaved/Current:");
    debug!(
        "{}",
        saved_area
            .as_ref()
            .map(|x| x.to_string())
            .unwrap_or_else(|| "<none>".into())
    );
    debug!(
        "{}",
        current_area
            .as_ref()
            .map(|x| x.to_string())
            .unwrap_or_else(|| "<none>".into())
    );*/

    // debug!("{:?} -> {:?}", saved_area, current_area);

    let current_eq_saved = |saved_area: &mut MemoryMap, current_area: &mut MemoryMap| {
        // not the same area, unmap
        if saved_area.pathname != current_area.pathname || saved_area.perms != current_area.perms {
            trace!(Restore, "realloc: unmapping {}", current_area);
            current_area.unmap()?;
            return Ok(Ordering::Greater);
        }
        let saved_area_size = saved_area.address.end - saved_area.address.start;
        let current_area_size = current_area.address.end - current_area.address.start;

        if saved_area_size != current_area_size {
            if let MMapPath::Stack = saved_area.path_kind {
                todo!();
            }
            if let MMapPath::Heap = saved_area.path_kind {
                todo!();
            }
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
            trace!(Restore, "realloc eq: saved ends after current");
            saved_area.address.start = current_area.address.end;
            Ok(Ordering::Greater)
        } else if saved_area.address.end < current_area.address.end {
            trace!(Restore, "realloc eq: saved ends before current");
            current_area.address.start = saved_area.address.end;
            Ok(Ordering::Less)
        } else {
            unreachable!()
        }
    };
    let current_before_saved = |current_area: &MemoryMap| {
        trace!(Restore, "realloc: current before saved, unmapping");
        current_area.unmap()?;
        Ok(Ordering::Greater)
    };
    let current_after_saved = |saved_area: &MemoryMap| {
        trace!(Restore, "realloc: current after saved");

        // must be allocated
        if let MMapPath::Path = saved_area.path_kind {
            todo!("realloc path {}", saved_area.pathname);
        }

        /*if let MMapPath::Anonymous = saved_area.path_kind {
            todo!("anon {}", saved_area.pathname);
        } else {
            todo!("non-anon {}", saved_area.pathname);
        }*/

        // SAFETY: todo
        unsafe {
            // TODO: fd if necessary
            rustix::mm::mmap_anonymous(
                saved_area.address.start as *mut _,
                saved_area.len(),
                permission_to_prot(saved_area.perms),
                permission_to_mapflags(saved_area.perms),
            )?;

            // TODO close fd if necessary

            Ok(Ordering::Less)
        }
    };

    if let Some(saved_area) = saved_area.as_deref_mut()
        && let Some(current_area) = current_area.as_deref_mut()
        && saved_area.address.start == current_area.address.start
    {
        return current_eq_saved(saved_area, current_area);
    }

    if saved_area.is_none() {
        return current_before_saved(current_area.unwrap());
    }
    if let Some(saved_area) = saved_area.as_deref()
        && let Some(current_area) = current_area.as_deref()
        && saved_area.address.start > current_area.address.start
    {
        return current_before_saved(current_area);
    }

    if current_area.is_none() {
        return current_after_saved(saved_area.unwrap());
    }
    if let Some(ref mut saved_area) = saved_area
        && let Some(ref mut current_area) = current_area
        && saved_area.address.start < current_area.address.start
    {
        if saved_area.address.end > current_area.address.start {
            trace!(Restore, "realloc: regions are overlapping, must unmap");
            current_area.unmap()?;
            return Ok(Ordering::Greater);
        }

        return current_after_saved(saved_area);
    }

    unreachable!()
}

fn permission_to_prot(perms: MemoryMapPermissions) -> ProtFlags {
    let mut prot = ProtFlags::empty();
    if perms.contains(MemoryMapPermissions::READ) {
        prot.insert(ProtFlags::READ);
    }
    if perms.contains(MemoryMapPermissions::WRITE) {
        prot.insert(ProtFlags::WRITE);
    }
    if perms.contains(MemoryMapPermissions::EXECUTE) {
        prot.insert(ProtFlags::EXEC);
    }
    prot
}

fn permission_to_mapflags(perms: MemoryMapPermissions) -> MapFlags {
    let mut flags = MapFlags::empty();
    if perms.contains(MemoryMapPermissions::SHARED) {
        flags.insert(MapFlags::SHARED);
    }
    if perms.contains(MemoryMapPermissions::PRIVATE) {
        flags.insert(MapFlags::PRIVATE);
    }
    flags
}

fn permission_to_mprotect(perms: MemoryMapPermissions) -> MprotectFlags {
    let mut prot = MprotectFlags::empty();
    if perms.contains(MemoryMapPermissions::READ) {
        prot.insert(MprotectFlags::READ);
    }
    if perms.contains(MemoryMapPermissions::WRITE) {
        prot.insert(MprotectFlags::WRITE);
    }
    if perms.contains(MemoryMapPermissions::EXECUTE) {
        prot.insert(MprotectFlags::EXEC);
    }
    prot
}
