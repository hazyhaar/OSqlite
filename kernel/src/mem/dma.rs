/// DMA-safe buffer allocator.
///
/// Wraps the physical page allocator. Guarantees:
/// - Physically contiguous memory
/// - Known physical address (for PRP entries)
/// - Cache coherence helpers (flush before device-read, invalidate after device-write)
use core::ptr;
use core::slice;

use super::phys::{PhysAddr, AllocError, PAGE_SIZE, PHYS_ALLOCATOR};

/// A DMA-safe buffer backed by physically contiguous pages.
///
/// Virtual pointers use the HHDM: virt = phys + hhdm_offset.
pub struct DmaBuf {
    phys: PhysAddr,
    len: usize,
    page_count: usize,
}

impl DmaBuf {
    /// Allocate a DMA buffer of at least `size` bytes.
    /// Actual allocation is rounded up to the next page boundary.
    pub fn alloc(size: usize) -> Result<Self, AllocError> {
        if size == 0 {
            return Err(AllocError::InvalidSize);
        }

        let page_count = (size + PAGE_SIZE - 1) / PAGE_SIZE;
        let phys = PHYS_ALLOCATOR.alloc_pages_contiguous(page_count, 1)?;

        // Zero the buffer
        unsafe {
            ptr::write_bytes(phys.as_ptr::<u8>(), 0, page_count * PAGE_SIZE);
        }

        Ok(Self {
            phys,
            len: size,
            page_count,
        })
    }

    /// Allocate a DMA buffer aligned to `align` pages.
    /// Useful for PRP lists which must be page-aligned.
    pub fn alloc_aligned(size: usize, page_align: usize) -> Result<Self, AllocError> {
        if size == 0 {
            return Err(AllocError::InvalidSize);
        }

        let page_count = (size + PAGE_SIZE - 1) / PAGE_SIZE;
        let phys = PHYS_ALLOCATOR.alloc_pages_contiguous(page_count, page_align)?;

        unsafe {
            ptr::write_bytes(phys.as_ptr::<u8>(), 0, page_count * PAGE_SIZE);
        }

        Ok(Self {
            phys,
            len: size,
            page_count,
        })
    }

    /// Physical base address of the buffer.
    #[inline]
    pub fn phys_addr(&self) -> PhysAddr {
        self.phys
    }

    /// Virtual address (phys + hhdm_offset).
    #[inline]
    pub fn as_ptr(&self) -> *const u8 {
        self.phys.as_ptr()
    }

    #[inline]
    pub fn as_mut_ptr(&self) -> *mut u8 {
        self.phys.as_ptr()
    }

    /// Usable length in bytes (may be less than allocated pages).
    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// View as a byte slice.
    pub fn as_slice(&self) -> &[u8] {
        unsafe { slice::from_raw_parts(self.as_ptr(), self.len) }
    }

    /// View as a mutable byte slice.
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        unsafe { slice::from_raw_parts_mut(self.as_mut_ptr(), self.len) }
    }

    /// Copy `data` into the DMA buffer at offset 0.
    pub fn copy_from_slice(&mut self, data: &[u8]) {
        assert!(data.len() <= self.len, "data exceeds DMA buffer capacity");
        unsafe {
            ptr::copy_nonoverlapping(data.as_ptr(), self.as_mut_ptr(), data.len());
        }
    }

    /// Copy from the DMA buffer into `dest`.
    pub fn copy_to_slice(&self, dest: &mut [u8], offset: usize, len: usize) {
        assert!(offset + len <= self.len, "read exceeds DMA buffer bounds");
        unsafe {
            ptr::copy_nonoverlapping(self.as_ptr().add(offset), dest.as_mut_ptr(), len);
        }
    }

    /// Flush CPU caches for this buffer.
    ///
    /// Call BEFORE the device reads from this buffer (e.g., NVMe write command).
    /// Ensures the device sees the data the CPU wrote.
    pub fn flush_cache(&self) {
        let start = self.as_ptr() as usize;
        let end = start + self.page_count * PAGE_SIZE;
        let mut addr = start;
        while addr < end {
            unsafe {
                core::arch::asm!(
                    "clflushopt [{}]",
                    in(reg) addr,
                    options(nostack, preserves_flags)
                );
            }
            addr += 64; // cache line size
        }
        // Ensure all flushes complete before returning
        unsafe {
            core::arch::asm!("sfence", options(nostack, preserves_flags));
        }
    }

    /// Invalidate CPU caches for this buffer.
    ///
    /// Call AFTER the device writes to this buffer (e.g., NVMe read command).
    /// Ensures the CPU sees the data the device wrote, not stale cache.
    pub fn invalidate_cache(&self) {
        // On x86, clflush/clflushopt both invalidate the line.
        // There is no "invalidate without writeback" instruction on x86.
        // clflushopt is the closest — it writes back dirty lines and invalidates.
        let start = self.as_ptr() as usize;
        let end = start + self.page_count * PAGE_SIZE;
        let mut addr = start;
        while addr < end {
            unsafe {
                core::arch::asm!(
                    "clflushopt [{}]",
                    in(reg) addr,
                    options(nostack, preserves_flags)
                );
            }
            addr += 64;
        }
        unsafe {
            core::arch::asm!("mfence", options(nostack, preserves_flags));
        }
    }
}

impl Drop for DmaBuf {
    fn drop(&mut self) {
        PHYS_ALLOCATOR.free_pages(self.phys, self.page_count);
    }
}

// DmaBuf is Send but NOT Sync — only one owner should access it at a time.
// The NVMe driver takes &mut DmaBuf or moves ownership during I/O.
unsafe impl Send for DmaBuf {}
