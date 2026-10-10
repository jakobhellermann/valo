use std::cmp::Ordering;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::mem::MaybeUninit;
use std::os::fd::OwnedFd;

use rustix::fs::{OFlags, SeekFrom as RSeekFrom};
use rustix::mm::{self, MapFlags, MprotectFlags, ProtFlags};

use crate::checkpoint::{AreaMeta, Header, PageFlag, Result, SnapshotSlot};
use crate::log::structured;
use crate::procfs::fds;
use crate::procfs::maps::{
    MMapPath, MemoryMap, MemoryMapPermissions, PageFlags, PageIndex, ProcMaps, ProcPagemap,
};
use crate::token::Frozen;
use crate::utils::{PAGE_SIZE, SignalSafeFormat, StackBufReader, align_page_down, align_page_up};
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
        let (snapshot_meta, snapshot_page_data) = super::open_snapshot_files(slot)?;

        let mut reader = SnapshotReader {
            snapshot_meta: StackBufReader::new(snapshot_meta),
            snapshot_page_data,
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
            Ok(PageFlag::from_u8(byte).ok_or(std::io::ErrorKind::InvalidData)?)
        } else {
            unreachable!("attempteed to read out-of-bounds pageflag")
        }
    }
}

pub fn read_all_areas(frozen: &Frozen, reader: &mut SnapshotReader) -> Result<()> {
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
        let order = reallocate_area(
            frozen,
            saved_area.as_mut().map(|x| &mut x.0),
            current_area.as_mut(),
        )?;

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
        read_area(frozen, &area, &meta, reader, &mut page_map)?;
        structured::events::restore_area_done(&area);
    }
    debug!(Restore, "finished reading areas");

    Ok(())
}

/// Open the backing file of a saved area: by path first, then by identity
/// through the live fd table (the only access path for unlinked files).
/// The restore side must resolve live: the fd snapshot table is ordinary
/// process memory and may already be half-rewound at this point of the
/// memory restore.
fn open_area_file(frozen: &Frozen, area: &MemoryMap, flags: OFlags) -> Result<OwnedFd> {
    area.open_file(flags)
        .or_else(|path_err| {
            fds::open_by_identity(frozen, area.dev, area.inode, flags)
                .ok()
                .flatten()
                .ok_or(path_err)
        })
        .inspect_err(|e| {
            warn!(
                Restore,
                "could not open backing file {}: {}",
                area.pathname,
                SignalSafeFormat(e)
            )
        })
        .map_err(std::io::Error::from)
}

fn area_fd(frozen: &Frozen, saved_area: &MemoryMap) -> Option<(OwnedFd, u64, u64)> {
    // SysV segments have no fd
    if matches!(saved_area.path_kind, MMapPath::Vsys(_)) {
        return None;
    }

    let fd = open_area_file(frozen, saved_area, OFlags::RDONLY).ok()?;
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
    frozen: &Frozen,
    saved_area: &MemoryMap,
    meta: &AreaMeta,
    reader: &mut SnapshotReader,
    current_pagemap: &mut ProcPagemap,
) -> Result<()> {
    // TODO: mprotect read

    let mut shared_fd = saved_area
        .perms
        .contains(MemoryMapPermissions::SHARED)
        .then(|| area_fd(frozen, saved_area))
        .flatten();
    structured::events::shared_fd_initial(saved_area.address.start, shared_fd.as_ref());

    debug!(Restore, "restore {}", saved_area);

    reader
        .snapshot_page_data
        .seek(SeekFrom::Start(meta.page_data_offset))?;

    let mut stats = Stats::default();
    let mut shared_seek_failed = false;
    let mut page_loads = PageLoadQueue::default();

    let mut orig_fd = None;

    let mut read_area_page = |page: PageIndex,
                              current_pageflag: PageFlags|
     -> Result<(), std::io::Error> {
        let addr = std::ptr::with_exposed_provenance_mut::<u8>(page * PAGE_SIZE);
        let page_relative = page - saved_area.page();

        let flag = match meta.uncommitted {
            true => PageFlag::No,
            false => reader.read_pageflag()?,
        };
        if flag != PageFlag::Full {
            page_loads.flush(frozen, &mut reader.snapshot_page_data)?;
        }

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

        if !saved_area.perms.contains(MemoryMapPermissions::READ) {
            page_mprotect_with(MprotectFlags::READ)?;
        }

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
                let orig_fd = orig_fd.get_or_insert_with(|| saved_area.open_file(OFlags::RDONLY));
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
        page_loads.push(frozen, &mut reader.snapshot_page_data, addr)?;

        Ok(())
    };

    current_pagemap.for_each_page(saved_area.pages(), &mut read_area_page)?;
    page_loads.flush(frozen, &mut reader.snapshot_page_data)?;

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

#[derive(Default)]
struct PageLoadQueue {
    start: *mut u8,
    pages: usize,
}

impl PageLoadQueue {
    fn push(&mut self, frozen: &Frozen, file: &mut File, addr: *mut u8) -> Result<()> {
        const MAX_PAGES: usize = 256;
        if self.pages > 0
            && (addr != self.start.wrapping_add(self.pages * PAGE_SIZE) || self.pages == MAX_PAGES)
        {
            self.flush(frozen, file)?;
        }
        if self.pages == 0 {
            self.start = addr;
        }
        self.pages += 1;
        Ok(())
    }

    fn flush(&mut self, _frozen: &Frozen, file: &mut File) -> Result<()> {
        if self.pages == 0 {
            return Ok(());
        }
        // SAFETY: each queued Full page is mapped and writable when pushed. The queue
        // contains consecutive pages, and Frozen guarantees other guest threads are suspended.
        let dst = unsafe { std::slice::from_raw_parts_mut(self.start, self.pages * PAGE_SIZE) };
        file.read_exact(dst)?;
        self.pages = 0;
        Ok(())
    }
}

fn is_zero_page() -> bool {
    false // TODO
}

// eq -> advance both
// greater -> advance current
// less -> advance saved
fn reallocate_area(
    frozen: &Frozen,
    mut saved_area: Option<&mut MemoryMap>,
    mut current_area: Option<&mut MemoryMap>,
) -> Result<Ordering> {
    let current_eq_saved = |saved_area: &mut MemoryMap, current_area: &mut MemoryMap| {
        // Not the same area, unmap. A file mapping is only the same if it maps the
        // same range of the same file: the rewound timeline may have mapped another
        // file with the same name (e.g. a fresh memfd), or another offset of it,
        // at the same address.
        let other_file = is_file_backed(&saved_area.path_kind)
            && (saved_area.dev != current_area.dev
                || saved_area.inode != current_area.inode
                || saved_area.offset != current_area.offset);
        if saved_area.pathname != current_area.pathname
            || saved_area.perms != current_area.perms
            || other_file
        {
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

        // The bracketed kernel sections, like libTAS' `name[0] == '['`.
        let is_special_section = match saved_area.path_kind {
            MMapPath::Anonymous
            | MMapPath::Vsys(_) // TODO?
            | MMapPath::Path
            | MMapPath::MemFd
            | MMapPath::Shm => false,
            MMapPath::Heap
            | MMapPath::Stack
            | MMapPath::TStack(_)
            | MMapPath::Vdso
            | MMapPath::Vvar
            | MMapPath::Vsyscall
            | MMapPath::Rollup
            | MMapPath::Other => true,
        };
        if saved_area.address.end == current_area.address.end || is_special_section {
            Ok(Ordering::Equal)
        } else if saved_area.address.end > current_area.address.end {
            trace!(Restore, "realloc eq: saved ends after current");
            // the rest of a file mapping continues further into the file
            saved_area.offset += current_area.address.end - saved_area.address.start;
            saved_area.address.start = current_area.address.end;
            Ok(Ordering::Greater)
        } else if saved_area.address.end < current_area.address.end {
            trace!(Restore, "realloc eq: saved ends before current");
            current_area.offset += saved_area.address.end - current_area.address.start;
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

        // The saved area must be allocated. File mappings must reference
        // their file again: pages the snapshot flagged `File` are not
        // stored — their content is assumed to equal the mapped file, so
        // an anonymous mapping would silently zero them. Shared mappings
        // additionally alias pages other processes may hold (a memfd the
        // GPU host imports as a dma-buf), so an anonymous copy detaches
        // every later write from them. Prefer a live descriptor of the same
        // inode (the file may have been deleted since the checkpoint; the
        // fd keepalives count), then open the path (libTAS does the same
        // for its area reallocation).
        let file_backed = is_file_backed(&saved_area.path_kind);
        let file_fd: Option<OwnedFd> = if file_backed {
            // Already logged; file pages will read as zeros.
            reopen_file_area(frozen, saved_area).ok()
        } else {
            None
        };
        if file_backed
            && file_fd.is_none()
            && saved_area.perms.contains(MemoryMapPermissions::SHARED)
        {
            error!(
                Restore,
                "shared file mapping {} re-created anonymously: writes no longer reach {}",
                saved_area,
                saved_area.pathname
            );
        }

        // SAFETY: the fixed address is free (the merge loop unmapped
        // overlapping current areas first) or FIXED_NOREPLACE fails
        // loudly instead of clobbering another mapping.
        unsafe {
            let flags = MapFlags::FIXED_NOREPLACE | saved_area.perms.into();
            let mapped = match &file_fd {
                Some(fd) => rustix::mm::mmap(
                    saved_area.address.start as *mut _,
                    saved_area.len(),
                    saved_area.perms.into(),
                    flags,
                    fd,
                    saved_area.offset,
                ),
                None => rustix::mm::mmap_anonymous(
                    saved_area.address.start as *mut _,
                    saved_area.len(),
                    saved_area.perms.into(),
                    flags,
                ),
            };
            mapped?;
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

fn is_file_backed(path_kind: &MMapPath) -> bool {
    matches!(path_kind, MMapPath::Path | MMapPath::MemFd | MMapPath::Shm)
}

/// Re-open the file backing a saved file mapping whose area no longer
/// exists at restore time, for the re-allocation in [`reallocate_area`].
/// Adds the libTAS shrunken-file warning to [`open_area_file`].
fn reopen_file_area(frozen: &Frozen, area: &MemoryMap) -> Result<OwnedFd> {
    // mmap refuses a writable MAP_SHARED mapping of a read-only description.
    let flags = if area
        .perms
        .contains(MemoryMapPermissions::SHARED | MemoryMapPermissions::WRITE)
    {
        OFlags::RDWR
    } else {
        OFlags::RDONLY
    };
    let fd = open_area_file(frozen, area, flags)?;

    // Like libTAS: warn when the file shrank below the mapped range — the
    // mapping is still created, but accessing beyond the end SIGBUSes.
    let stat = rustix::fs::fstat(&fd)?;
    if (stat.st_size as u64) < area.offset + area.len() as u64 {
        warn!(
            Restore,
            "file {} mapped at offset {} is smaller than the saved area ({} bytes now)",
            area.pathname,
            area.offset + area.len() as u64,
            stat.st_size
        );
    }
    Ok(fd)
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
