/// Physical page allocator — bitmap-based.
///
/// Tracks 4 KiB pages via a bitmap. Supports allocation of contiguous
/// runs of pages with alignment constraints (required for DMA buffers
/// and PRP lists).
use core::fmt;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;

/// Higher-Half Direct Map offset, set once at boot from Limine's HHDM response.
/// All physical memory is linearly mapped at virtual address (phys + HHDM_OFFSET).
static HHDM_OFFSET: AtomicU64 = AtomicU64::new(0);

/// Set the HHDM offset. Must be called once during early boot before any
/// PhysAddr::as_ptr() calls.
pub fn set_hhdm_offset(offset: u64) {
    HHDM_OFFSET.store(offset, Ordering::Relaxed);
}

/// Get the current HHDM offset.
pub fn hhdm_offset() -> u64 {
    HHDM_OFFSET.load(Ordering::Relaxed)
}

/// A physical address. Transparent wrapper for clarity.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct PhysAddr(pub u64);

impl PhysAddr {
    pub const fn new(addr: u64) -> Self {
        Self(addr)
    }

    pub const fn as_u64(self) -> u64 {
        self.0
    }

    /// Convert to a virtual pointer via the HHDM (Higher-Half Direct Map).
    /// virt = phys + hhdm_offset.
    pub fn as_ptr<T>(self) -> *mut T {
        let offset = HHDM_OFFSET.load(Ordering::Relaxed);
        (self.0 + offset) as *mut T
    }
}

impl fmt::Debug for PhysAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PhysAddr({:#x})", self.0)
    }
}

#[derive(Debug)]
pub enum AllocError {
    OutOfMemory,
    InvalidAlignment,
    InvalidSize,
}

impl fmt::Display for AllocError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AllocError::OutOfMemory => write!(f, "out of physical memory"),
            AllocError::InvalidAlignment => write!(f, "invalid alignment"),
            AllocError::InvalidSize => write!(f, "invalid size"),
        }
    }
}

pub const PAGE_SIZE: usize = 4096;

/// Maximum supported physical memory: 4 GiB = 1M pages.
/// Bitmap = 1M bits = 128 KiB. Stored inline.
const MAX_PAGES: usize = 1024 * 1024;
const BITMAP_WORDS: usize = MAX_PAGES / 64;

pub struct PhysPageAllocator {
    inner: Mutex<AllocatorInner>,
}

struct AllocatorInner {
    bitmap: [u64; BITMAP_WORDS],
    total_pages: usize,
    free_pages: usize,
}

impl PhysPageAllocator {
    pub const fn new() -> Self {
        Self {
            inner: Mutex::new(AllocatorInner {
                bitmap: [0xFFFF_FFFF_FFFF_FFFF; BITMAP_WORDS], // all marked used
                total_pages: 0,
                free_pages: 0,
            }),
        }
    }

    /// Initialize the allocator with a memory map.
    /// `regions` is a list of (base_addr, length) pairs of usable RAM.
    /// The allocator marks these regions as free.
    pub fn init(&self, regions: &[(u64, u64)]) {
        let mut inner = self.inner.lock();

        // Start with everything marked as used (all bits = 1).
        // Then free the usable regions.
        for &(base, length) in regions {
            let start_page = (base as usize + PAGE_SIZE - 1) / PAGE_SIZE; // round up
            let end_page = ((base + length) as usize) / PAGE_SIZE; // round down

            for page in start_page..end_page.min(MAX_PAGES) {
                let word = page / 64;
                let bit = page % 64;
                if inner.bitmap[word] & (1 << bit) != 0 {
                    inner.bitmap[word] &= !(1 << bit); // 0 = free
                    inner.free_pages += 1;
                }
            }
        }

        // Calculate total pages from the highest usable address
        let max_addr = regions
            .iter()
            .map(|&(base, len)| base + len)
            .max()
            .unwrap_or(0);
        inner.total_pages = (max_addr as usize / PAGE_SIZE).min(MAX_PAGES);
    }

    /// Mark a range of pages as used (e.g., kernel image, MMIO regions).
    pub fn mark_used(&self, base: PhysAddr, count: usize) {
        let mut inner = self.inner.lock();
        let start_page = base.as_u64() as usize / PAGE_SIZE;
        for page in start_page..start_page + count {
            if page < MAX_PAGES {
                let word = page / 64;
                let bit = page % 64;
                if inner.bitmap[word] & (1 << bit) == 0 {
                    inner.bitmap[word] |= 1 << bit;
                    inner.free_pages -= 1;
                }
            }
        }
    }

    /// Allocate a single page. Returns its physical address.
    pub fn alloc_page(&self) -> Result<PhysAddr, AllocError> {
        self.alloc_pages_contiguous(1, 1)
    }

    /// Allocate `count` physically contiguous pages, aligned to `align` pages.
    /// `align` must be a power of two.
    pub fn alloc_pages_contiguous(
        &self,
        count: usize,
        align: usize,
    ) -> Result<PhysAddr, AllocError> {
        if count == 0 {
            return Err(AllocError::InvalidSize);
        }
        if !align.is_power_of_two() {
            return Err(AllocError::InvalidAlignment);
        }

        let mut inner = self.inner.lock();
        if inner.free_pages < count {
            return Err(AllocError::OutOfMemory);
        }

        // Linear scan for a contiguous run of `count` free pages
        // starting at an `align`-aligned page index.
        let mut candidate = 0usize;
        while candidate + count <= inner.total_pages {
            // Align the candidate
            let aligned = (candidate + align - 1) & !(align - 1);
            if aligned + count > inner.total_pages {
                break;
            }

            let mut found = true;
            for i in 0..count {
                let page = aligned + i;
                let word = page / 64;
                let bit = page % 64;
                if inner.bitmap[word] & (1 << bit) != 0 {
                    // Page is used — skip past it
                    candidate = page + 1;
                    found = false;
                    break;
                }
            }

            if found {
                // Mark all pages as used
                for i in 0..count {
                    let page = aligned + i;
                    let word = page / 64;
                    let bit = page % 64;
                    inner.bitmap[word] |= 1 << bit;
                }
                inner.free_pages -= count;
                return Ok(PhysAddr::new((aligned * PAGE_SIZE) as u64));
            }
        }

        Err(AllocError::OutOfMemory)
    }

    /// Free `count` pages starting at `base`.
    pub fn free_pages(&self, base: PhysAddr, count: usize) {
        let mut inner = self.inner.lock();
        let start_page = base.as_u64() as usize / PAGE_SIZE;

        for i in 0..count {
            let page = start_page + i;
            if page < MAX_PAGES {
                let word = page / 64;
                let bit = page % 64;
                if inner.bitmap[word] & (1 << bit) == 0 {
                    // Already free — silently ignore to prevent double-free corruption
                    continue;
                }
                inner.bitmap[word] &= !(1 << bit);
                inner.free_pages += 1;
            }
        }
    }

    /// Number of free pages remaining.
    pub fn free_count(&self) -> usize {
        self.inner.lock().free_pages
    }

    /// Total tracked pages.
    pub fn total_count(&self) -> usize {
        self.inner.lock().total_pages
    }
}

/// Global physical page allocator instance.
pub static PHYS_ALLOCATOR: PhysPageAllocator = PhysPageAllocator::new();
