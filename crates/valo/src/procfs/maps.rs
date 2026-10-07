use std::fmt::Write;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::ops::Range;
use std::os::fd::OwnedFd;
use std::str::FromStr;

type Result<T, E = std::io::Error> = std::result::Result<T, E>;

use rustix::fs::{MemfdFlags, Mode, OFlags, memfd_create};
use rustix::mm::{MapFlags, MprotectFlags};

use crate::PAGE_SIZE;
use crate::stackio::StackBufReader;

const STACK_BUF_SIZE: usize = 2048;
pub struct ProcMaps(StackBufReader<File, STACK_BUF_SIZE>);

#[repr(C)]
#[derive(Debug)]
pub struct MemoryMap {
    pub address: Range<u64>,
    pub perms: MemoryMapPermissions,
    pub offset: u64,
    pub dev: (i32, i32),
    pub inode: u64,
    pub path_kind: MMapPath,
    pub pathname: heapless::String<1024>,
}
impl std::fmt::Display for MemoryMap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let _page_idx = self.address.start as usize / PAGE_SIZE;
        let page_count = (self.address.end - self.address.start) as usize / PAGE_SIZE;
        write!(
            f,
            "{:x}-{:x} {} ({}) {:?}",
            self.address.start,
            self.address.end,
            self.perms,
            page_count,
            // self.offset,
            self.pathname
        )
    }
}
impl MemoryMap {
    pub fn page(&self) -> PageIndex {
        self.address.start as usize / PAGE_SIZE
    }
    pub fn page_count(&self) -> usize {
        ((self.address.end - self.address.start) as usize).div_ceil(PAGE_SIZE)
    }
    pub fn pages(&self) -> Range<PageIndex> {
        let page = self.page();
        let page_count = self.page_count();
        page..page + page_count
    }

    pub fn len(&self) -> usize {
        (self.address.end - self.address.start) as usize
    }

    fn map_file_path(&self) -> heapless::String<46> {
        let mut buf = heapless::String::<46>::new();
        write!(
            &mut buf,
            "/proc/self/map_files/{:x}-{:x}",
            self.address.start, self.address.end
        )
        .unwrap();
        buf
    }

    pub(crate) fn open_file(&self) -> Result<OwnedFd, rustix::io::Errno> {
        let fd = rustix::fs::open(self.pathname.as_str(), OFlags::RDONLY, Mode::empty());
        fd.or_else(|_| {
            let map_file = self.map_file_path();
            rustix::fs::open(map_file.as_str(), OFlags::RDONLY, Mode::empty())
        })
    }

    pub(crate) fn as_bytes(&self) -> &[u8] {
        // SAFETY: repr(C)
        unsafe { std::slice::from_raw_parts(std::ptr::from_ref(self).cast(), size_of::<Self>()) }
    }

    pub(crate) fn unmap(&self) -> Result<(), rustix::io::Errno> {
        // SAFETY:
        // - ptr is aligned to page size
        // - the range covered by len is mapped
        // - provenance: shrug
        unsafe {
            rustix::mm::munmap(
                std::ptr::with_exposed_provenance_mut(self.address.start as usize),
                self.len(),
            )
        }
    }
}

impl ProcMaps {
    pub fn new(file: File) -> Self {
        ProcMaps(StackBufReader::new(file))
    }

    pub fn new_self_copy() -> Result<Self> {
        let mut maps = File::open("/proc/self/maps")?;
        let memfd = memfd_create(c"valo-maps", MemfdFlags::CLOEXEC)?;
        let mut copy = File::from(memfd);
        std::io::copy(&mut maps, &mut copy)?;
        copy.seek(SeekFrom::Start(0))?;
        Ok(ProcMaps::new(copy))
    }

    #[allow(dead_code)]
    pub fn reset(&mut self) -> Result<()> {
        self.0.reset()
    }

    pub fn next(&mut self) -> Result<Option<MemoryMap>> {
        let Some(line) = self.0.read_until(b'\n')? else {
            return Ok(None);
        };
        let line = std::str::from_utf8(line).unwrap();
        let map = parse_map_line(line).unwrap_or_else(|| panic!("Failed to parse line: {line}"));

        Ok(Some(map))
    }
}

fn parse_map_line(line: &str) -> Option<MemoryMap> {
    let mut s = line.splitn(6, ' ');

    let address = s.next()?;
    let perms = s.next()?;
    let offset = s.next()?;
    let dev = s.next()?;
    let inode = s.next()?;
    let path = s.next()?.trim_start();

    let (start, end) = address.split_once('-')?;
    let start = u64::from_str_radix(start, 16).ok()?;
    let end = u64::from_str_radix(end, 16).ok()?;

    let (minor, major) = dev.split_once(':')?;
    let minor = i32::from_str_radix(minor, 16).ok()?;
    let major = i32::from_str_radix(major, 16).ok()?;

    let perms = perms.parse().ok()?;

    Some(MemoryMap {
        address: start..end,
        perms,
        offset: u64::from_str_radix(offset, 16).ok()?,
        dev: (minor, major),
        inode: inode.parse().ok()?,
        pathname: heapless::String::from_str(path).ok()?,
        path_kind: classify_path(path)?,
    })
}

#[allow(dead_code)]
#[derive(Debug)]
pub enum MMapPath {
    /// The file that is backing the mapping.
    Path,
    /// The process's heap.
    Heap,
    /// The initial process's (also known as the main thread's) stack.
    Stack,
    /// A thread's stack (where the `<tid>` is a thread ID).  It corresponds to the
    /// `/proc/<pid>/task/<tid>/` path.
    ///
    /// (since Linux 3.4)
    TStack(u32),
    /// The virtual dynamically linked shared object.
    Vdso,
    /// Shared kernel variables
    Vvar,
    /// obsolete virtual syscalls, succeeded by vdso
    Vsyscall,
    /// rollup memory mappings, from `/proc/<pid>/smaps_rollup`
    Rollup,
    /// An anonymous mapping as obtained via mmap(2).
    Anonymous,
    /// Shared memory segment.
    Vsys(i32),
    /// Some other pseudo-path
    Other,
    /// memfd:
    MemFd,
    /// /dev/shm
    Shm,
}

fn classify_path(path: &str) -> Option<MMapPath> {
    Some(match path.trim() {
        "" => MMapPath::Anonymous,
        "[heap]" => MMapPath::Heap,
        "[stack]" => MMapPath::Stack,
        "[vdso]" => MMapPath::Vdso,
        "[vvar]" => MMapPath::Vvar,
        "[vsyscall]" => MMapPath::Vsyscall,
        "[rollup]" => MMapPath::Rollup,
        x if x.starts_with("[stack:") => {
            let mut s = x[1..x.len() - 1].split(':');
            let tid = s.nth(1)?.parse().ok()?;
            MMapPath::TStack(tid)
        }
        x if x.starts_with('[') && x.ends_with(']') => MMapPath::Other,
        x if x.starts_with("/SYSV") => {
            MMapPath::Vsys(u32::from_str_radix(&x[5..13], 16).ok()? as i32)
        }
        x if x.starts_with("/memfd:") => MMapPath::MemFd,
        x if x.starts_with("/dev/shm/") => MMapPath::Shm,
        _ => MMapPath::Path,
    })
}

pub struct ProcPagemap(File);

pub type PageIndex = usize;

impl ProcPagemap {
    pub fn new(file: File) -> Self {
        Self(file)
    }

    pub fn new_self() -> Result<Self> {
        let file = File::open("/proc/self/pagemap")?;
        Ok(Self::new(file))
    }

    pub fn seek(&mut self, page: PageIndex) -> Result<()> {
        let position = page as u64 * size_of::<u64>() as u64;
        self.0.seek(SeekFrom::Start(position))?;
        Ok(())
    }

    pub fn read(&mut self, page_flags: &mut [PageFlags]) -> Result<()> {
        // SAFETY: purely relaxing alignment just page_flags
        let page_flags = unsafe {
            std::slice::from_raw_parts_mut(
                page_flags.as_mut_ptr().cast::<u8>(),
                page_flags.len() * size_of::<u64>(),
            )
        };
        self.0.read_exact(page_flags)?;
        Ok(())
    }

    pub fn read_single(&mut self) -> Result<PageFlags> {
        let mut flags = [PageFlags::default(); 1];
        self.read(&mut flags)?;
        Ok(flags[0])
    }

    #[allow(dead_code)]
    pub fn read_at(&mut self, page: usize, page_flags: &mut [PageFlags]) -> Result<()> {
        self.seek(page)?;
        self.read(page_flags)
    }

    pub fn read_at_single(&mut self, page: usize) -> Result<PageFlags> {
        self.seek(page)?;
        self.read_single()
    }

    #[allow(dead_code)]
    pub fn for_each_page_chunked_at<const CHUNK_SIZE: usize>(
        &mut self,
        page: usize,
        page_count: usize,
        f: impl Fn(PageFlags),
    ) -> Result<()> {
        self.seek(page)?;
        self.for_each_page_chunked::<CHUNK_SIZE>(page_count, f)
    }

    pub fn for_each_page_chunked<const CHUNK_SIZE: usize>(
        &mut self,
        page_count: usize,
        f: impl Fn(PageFlags),
    ) -> Result<()> {
        let mut remaining = page_count;

        while remaining > 0 {
            let mut chunk = [PageFlags::default(); CHUNK_SIZE];
            let n_chunk = remaining.min(chunk.len());
            let flags = &mut chunk[0..n_chunk];
            self.read(flags)?;

            for flag in flags {
                f(*flag);
            }

            remaining -= n_chunk;
        }

        Ok(())
    }
}

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct MemoryMapPermissions: u8 {
        /// No permissions
        const NONE = 0;
        /// Read permission
        const READ = 1 << 0;
        /// Write permission
        const WRITE = 1 << 1;
        /// Execute permission
        const EXECUTE = 1 << 2;
        /// Memory is shared with another process.
        ///
        /// Mutually exclusive with PRIVATE.
        const SHARED = 1 << 3;
        /// Memory is private (and copy-on-write)
        ///
        /// Mutually exclusive with SHARED.
        const PRIVATE = 1 << 4;
    }
}

impl From<MemoryMapPermissions> for MprotectFlags {
    fn from(perms: MemoryMapPermissions) -> Self {
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
}

impl From<MemoryMapPermissions> for MapFlags {
    fn from(perms: MemoryMapPermissions) -> Self {
        let mut flags = MapFlags::empty();
        if perms.contains(MemoryMapPermissions::SHARED) {
            flags.insert(MapFlags::SHARED);
        }
        if perms.contains(MemoryMapPermissions::PRIVATE) {
            flags.insert(MapFlags::PRIVATE);
        }
        flags
    }
}

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, Default, PartialEq)]
    pub struct PageFlags: u64 {
       /// Page frame number if present
       #[doc(hidden)]
       const PFN = genmask(54, 0);
       /// PTE is soft-dirty
       const SOFT_DIRTY = 1 << 55;
       /// Page is exclusively mapped
       const MMAP_EXCLUSIVE = 1 << 56;
       /// Page is file-page or shared-anon
       const FILE = 1 << 61;
       /// Page is swapped
       #[doc(hidden)]
       const SWAP = 1 << 62;
       /// Page is present
       const PRESENT = 1 << 63;
   }
}
const fn genmask(high: usize, low: usize) -> u64 {
    let mask_bits = u64::BITS as usize;
    (!0 - (1 << low) + 1) & (!0 >> (mask_bits - 1 - high))
}

impl MemoryMapPermissions {
    fn from_ascii_char(b: u8) -> Self {
        match b {
            b'r' => Self::READ,
            b'w' => Self::WRITE,
            b'x' => Self::EXECUTE,
            b's' => Self::SHARED,
            b'p' => Self::PRIVATE,
            _ => Self::NONE,
        }
    }
}

impl std::fmt::Display for MemoryMapPermissions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_char(if self.contains(Self::READ) { 'r' } else { '-' })?;
        f.write_char(if self.contains(Self::WRITE) { 'w' } else { '-' })?;
        f.write_char(if self.contains(Self::EXECUTE) {
            'x'
        } else {
            '-'
        })?;
        f.write_char(if self.contains(Self::SHARED) {
            's'
        } else if self.contains(Self::PRIVATE) {
            'p'
        } else {
            '-'
        })?;
        Ok(())
    }
}

impl FromStr for MemoryMapPermissions {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(s.bytes()
            .map(Self::from_ascii_char)
            .fold(Self::empty(), std::ops::BitOr::bitor))
    }
}
