use std::cmp::Ordering;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::mem::MaybeUninit;
use std::os::fd::OwnedFd;

use rustix::fs::SeekFrom as RSeekFrom;
use rustix::mm::{self, MprotectFlags, ProtFlags};

use crate::checkpoint::{AreaMeta, Header, PageFlag, Result, SnapshotSlot};
use crate::log::structured;
use crate::procfs::maps::{
    MMapPath, MemoryMap, MemoryMapPermissions, PageFlags, PageIndex, ProcMaps, ProcPagemap,
};
use crate::stackio::StackBufReader;
use crate::utils::{PAGE_SIZE, align_page_down, align_page_up};
use crate::{debug, error, info, trace, warn};

/// Buffer for the meta file reads
const META_BUF: usize = 8192;

pub struct SnapshotReader {
    // Header, [MemoryMap Area] x n_areas, PageFlag x n_page_flags
    snapshot_meta: StackBufReader<File, META_BUF>,
    // PageFlag x n_page_flags
    snapshot_page_data: File,

    flags_remaining: usize,
    pub header: Header,
}

impl SnapshotReader {
    pub fn new(slot: SnapshotSlot) -> Result<SnapshotReader> {
        let mut reader = SnapshotReader {
            snapshot_meta: StackBufReader::new(File::open(
                super::snapshot_path(slot, "meta").as_str(),
            )?),
            snapshot_page_data: File::open(super::snapshot_path(slot, "pages").as_str())?,
            flags_remaining: 0,
            header: Header {
                area_count: 0,
                threads: super::threads::SavedThreads::new(),
            },
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
        self.flags_remaining = 0;
        self.snapshot_meta.seek_start(size_of::<Header>() as u64)?;
        Ok(())
    }

    pub fn read(&mut self) -> Result<(MemoryMap, AreaMeta)> {
        if self.flags_remaining > 0 {
            self.snapshot_meta
                .skip(self.flags_remaining * size_of::<PageFlag>())?;
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

            let mut byte = 0_u8;
            self.snapshot_meta
                .read_exact(std::slice::from_mut(&mut byte))?;
            // SAFETY: assume savestate is untampered
            Ok(unsafe { PageFlag::from_u8_unchecked(byte) })
        } else {
            unreachable!("attempteed to read out-of-bounds pageflag")
        }
    }
}

pub fn read_all_areas(reader: &mut SnapshotReader) -> Result<()> {
    debug!(Restore, "Restoring {} areas", reader.header.area_count);

    let mut maps = ProcMaps::new_self_copy()?;
    let mut page_map = ProcPagemap::new_self()?;

    #[cfg(feature = "structured-log")]
    {
        for (area, meta) in (0..reader.header.area_count).map(|_| reader.read().unwrap()) {
            structured::events::saved_area(&area, meta.skip || meta.uncommitted);
        }
        reader.restart()?;

        while let Some(area) = maps.next()? {
            structured::events::log_current_area(&area, &mut page_map);
        }
        maps.reset()?;
    }

    let mut saved_maps = (0..reader.header.area_count).map(|_| reader.read().unwrap());

    let mut saved_area = saved_maps.next();
    let mut current_area = maps.next()?;

    structured::events::reallocate_areas_begin();
    while saved_area.is_some() || current_area.is_some() {
        let order = reallocate_area(saved_area.as_mut().map(|x| &mut x.0), current_area.as_mut())?;

        match order {
            Ordering::Less => saved_area = saved_maps.next(),
            Ordering::Equal => {
                saved_area = saved_maps.next();
                current_area = maps.next()?;
            }
            Ordering::Greater => current_area = maps.next()?,
        }
    }
    structured::events::reallocate_areas_end();
    debug!(Restore, "finished reallocating areas");

    reader.restart()?;
    for _ in 0..reader.header.area_count {
        let (area, meta) = reader.read()?;

        if meta.skip || meta.uncommitted {
            // TODO: check still uncommitted
            structured::events::restore_area(&area, true);
            structured::events::restore_area_done(&area);
            continue;
        }

        structured::events::restore_area(&area, false);
        read_area(&area, &meta, reader, &mut page_map)?;
        structured::events::restore_area_done(&area);
    }
    debug!(Restore, "finished reading areas");

    Ok(())
}

fn area_fd(saved_area: &MemoryMap) -> Option<(OwnedFd, u64, u64)> {
    let fd = saved_area
        .open_file()
        .inspect_err(|e| {
            warn!(
                Checkpoint,
                "Could not open shared map file for restore {}: err={}",
                saved_area.pathname,
                e.raw_os_error()
            );
        })
        .ok()?;
    let mut next_off_hole = rustix::fs::seek(&fd, RSeekFrom::Hole(saved_area.offset)).ok()?;
    let mut next_off_data =
        rustix::fs::seek(&fd, RSeekFrom::Data(saved_area.offset)).unwrap_or(u64::MAX);
    next_off_hole = align_page_up(next_off_hole);
    next_off_data = align_page_down(next_off_data);

    Some((fd, next_off_hole, next_off_data))
}

#[derive(Default)]
struct Stats {
    full: usize,
    skip: usize,
    zero_or_file: usize,
}

fn read_area(
    saved_area: &MemoryMap,
    meta: &AreaMeta,
    reader: &mut SnapshotReader,
    current_pagemap: &mut ProcPagemap,
) -> Result<()> {
    // TODO: mprotect read

    let mut shared_fd = saved_area
        .perms
        .contains(MemoryMapPermissions::SHARED)
        .then(|| area_fd(saved_area))
        .flatten();
    structured::events::shared_fd_initial(saved_area.address.start, shared_fd.as_ref());

    debug!(Restore, "restore {}", saved_area);

    current_pagemap.seek(saved_area.page())?;
    reader
        .snapshot_page_data
        .seek(SeekFrom::Start(meta.page_data_offset))?;

    let mut stats = Stats::default();
    let mut shared_seek_failed = false;

    let mut orig_fd = None;

    let mut read_area_page = |page: PageIndex| -> Result<(), std::io::Error> {
        let addr = std::ptr::with_exposed_provenance_mut::<u8>(page * PAGE_SIZE);
        let current_pageflag = current_pagemap.read_at_single(page)?; // TODO buffer
        let page_relative = page - saved_area.page();

        let flag = match meta.uncommitted {
            true => PageFlag::No,
            false => reader.read_pageflag()?,
        };

        let page_present = current_pageflag.contains(PageFlags::PRESENT);
        let page_file = current_pageflag.contains(PageFlags::FILE);
        let _page_softdirty = current_pageflag.contains(PageFlags::SOFT_DIRTY);
        let page_guard_region = current_pageflag.bits() & (1 << 58) != 0;

        let page_mprotect_with = |extra_flags: MprotectFlags| {
            let perms = extra_flags | saved_area.perms.into();
            // SAFETY: todo
            unsafe { mm::mprotect(addr.cast(), PAGE_SIZE, perms) }
        };
        // SAFETY: todo
        let page_memset0 = || unsafe { libc::memset(addr.cast(), 0, PAGE_SIZE) };

        if let PageFlag::Guard = flag {
            stats.skip += 1;
            if !page_guard_region {
                // SAFETY: page_addr is page-aligned by construction
                unsafe { set_guard_region(addr, true)? };
            }
            return Ok(());
        } else if page_guard_region {
            // SAFETY: page_addr is page-aligned by construction
            unsafe { set_guard_region(addr, false)? };
        }

        page_mprotect_with(MprotectFlags::READ)?;

        // Page was unmapped in checkpoint
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
                if let &mut Some((ref shared_fd, ref mut next_off_hole, ref mut next_off_data)) =
                    &mut shared_fd
                    && !shared_seek_failed
                {
                    let current_offset = saved_area.offset + (page_relative * PAGE_SIZE) as u64;

                    // Advance hole/data
                    if current_offset > *next_off_data && current_offset > *next_off_hole {
                        *next_off_data =
                            rustix::fs::seek(shared_fd, RSeekFrom::Data(current_offset))
                                .unwrap_or(u64::MAX);
                        match rustix::fs::seek(shared_fd, RSeekFrom::Hole(current_offset)) {
                            Ok(hole) => {
                                *next_off_data = align_page_down(*next_off_data);
                                *next_off_hole = align_page_up(hole);
                            }
                            Err(_) => {
                                warn!(
                                    Checkpoint,
                                    "shared map file {} could not be queried for hole information during restore",
                                    saved_area.pathname
                                );
                                shared_seek_failed = true;
                            }
                        }
                    }

                    if shared_seek_failed {
                        // do nothing
                    } else if current_offset == *next_off_data || current_offset < *next_off_hole {
                        // Current page has data, saved page is hole -> write 0
                        // TODO: fallocate the hole
                        if !saved_area.perms.contains(MemoryMapPermissions::WRITE) {
                            page_mprotect_with(MprotectFlags::WRITE)?;
                        }
                        page_memset0();
                        stats.zero_or_file += 1;
                    } else if current_offset == *next_off_hole || current_offset < *next_off_data {
                        // Hole in current and savestate
                        stats.skip += 1;
                    } else {
                        warn!(
                            Checkpoint,
                            "shared file seek neither data nor hole at offset {current_offset}"
                        );
                    }
                } else {
                    warn!(
                        Checkpoint,
                        "shared map file {} was not found even if it existed when the \
                         savestate was made",
                        saved_area.pathname
                    );
                }
            }
            return Ok(());
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
            return Ok(());
        }

        if let PageFlag::File = flag {
            if page_present && !page_file {
                let orig_fd = orig_fd.get_or_insert_with(|| saved_area.open_file());
                let orig_fd = match orig_fd {
                    Err(e) => {
                        warn!(
                            Checkpoint,
                            "Could not open {} to recover file mapped page: {}",
                            saved_area.pathname,
                            e
                        );
                        return Ok(());
                    }
                    Ok(fd) => fd,
                };

                if !saved_area.perms.contains(MemoryMapPermissions::WRITE) {
                    page_mprotect_with(MprotectFlags::WRITE)?;
                }

                let file_offset = saved_area.offset + (page_relative * PAGE_SIZE) as u64;
                // SAFETY: page_addr is a mapped page and was made writable above.
                let page = unsafe { std::slice::from_raw_parts_mut(addr, PAGE_SIZE) };

                read_all(orig_fd, page, file_offset).inspect_err(|e| {
                    warn!(
                        Checkpoint,
                        "Could not read {} to recover file mapped page: {}", saved_area.pathname, e
                    );
                })?;
                stats.zero_or_file += 1;
            } else {
                stats.skip += 1;
            }
            return Ok(());
        }

        if !saved_area.perms.contains(MemoryMapPermissions::WRITE) {
            page_mprotect_with(MprotectFlags::WRITE)?;
        }

        // todo: base page

        stats.full += 1;
        queue_page_load(reader, addr, flag)?;

        Ok(())
    };

    for page in saved_area.pages() {
        read_area_page(page)?;
    }

    if !saved_area.perms.contains(MemoryMapPermissions::WRITE)
        || !saved_area.perms.contains(MemoryMapPermissions::READ)
    {
        // SAFETY: saved area range is page aligned valid for reads
        unsafe {
            mm::mprotect(
                saved_area.address.start as *mut _,
                saved_area.len(),
                saved_area.perms.into(),
            )?;
        }
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
                todo!("stack page size changed");
            }
            if let MMapPath::Heap = saved_area.path_kind {
                // SAFETY: ideally transparent to the process
                let ret = unsafe { libc::brk(saved_area.address.end as *mut _) };
                if ret < 0 {
                    error!(Checkpoint, "failed to resize heap");
                    return Ok(Ordering::Equal);
                }

                info!(
                    Checkpoint,
                    "heap resized from {current_area_size} to {saved_area_size}"
                );
            }
        }

        if saved_area.perms != current_area.perms {
            // mprotect
            todo!("page protections changed");
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
                saved_area.perms.into(),
                saved_area.perms.into(),
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

impl From<MemoryMapPermissions> for ProtFlags {
    fn from(perms: MemoryMapPermissions) -> Self {
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
}

/// Read exactly `buf.len()` bytes from `fd` at `offset`.
fn read_all(fd: &OwnedFd, buf: &mut [u8], offset: u64) -> Result<(), rustix::io::Errno> {
    let mut done = 0;
    while done < buf.len() {
        let read = rustix::io::pread(fd, &mut buf[done..], offset + done as u64)?;
        if read == 0 {
            // Unexpected EOF: the mapped region extends beyond the file.
            return Err(rustix::io::Errno::NXIO);
        }
        done += read;
    }
    Ok(())
}

/// Install or remove a madvise guard region on a single page.
///
/// # Safety
/// - `page_addr` must be page-aligned.
unsafe fn set_guard_region(page_addr: *mut u8, guarded: bool) -> Result<(), std::io::Error> {
    // Values from /usr/include/bits/mman-linux.h; libc 0.2 lacks the constants.
    const MADV_GUARD_INSTALL: i32 = 102;
    const MADV_GUARD_REMOVE: i32 = 103;

    let advice = match guarded {
        true => MADV_GUARD_INSTALL,
        false => MADV_GUARD_REMOVE,
    };

    // SAFETY: the caller upholds page-alignment
    // madvise itself never dereferences the range and the kernel rejects unaligned or unmapped
    // ranges with EINVAL/ENOMEM.
    // TODO: validate this is correct
    if unsafe { libc::madvise(page_addr.cast(), PAGE_SIZE, advice) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}
