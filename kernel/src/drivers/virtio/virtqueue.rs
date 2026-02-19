/// Virtio split virtqueue implementation (legacy layout).
///
/// In legacy virtio (device ID 0x1000), descriptors + available ring +
/// padding + used ring live in a single contiguous physical allocation.
/// The device is given the Page Frame Number (PFN = phys_addr / 4096)
/// via the Queue Address register, and derives all three addresses from
/// the standard layout.
///
/// Layout:
///   [descriptors: 16 * queue_size]
///   [available ring: 6 + 2 * queue_size]
///   [padding to 4096 boundary]
///   [used ring: 6 + 8 * queue_size]
use core::sync::atomic::{fence, Ordering};
use crate::mem::{PhysAddr, DmaBuf, AllocError};

/// Virtqueue descriptor flags.
const VIRTQ_DESC_F_WRITE: u16 = 2; // Buffer is device-writable (for rx)

/// A single descriptor in the descriptor table (16 bytes).
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct VirtqDesc {
    pub addr: u64,  // Physical address of buffer
    pub len: u32,   // Length of buffer
    pub flags: u16, // VIRTQ_DESC_F_*
    pub next: u16,  // Next descriptor index (if NEXT flag set)
}

/// Available ring header — guest writes here to offer buffers to the device.
#[repr(C)]
pub struct VirtqAvailHdr {
    pub flags: u16,
    pub idx: u16,
    // ring: [u16; queue_size] follows
    // used_event: u16 follows (after ring)
}

/// Used ring element.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct VirtqUsedElem {
    pub id: u32,  // Descriptor chain head index
    pub len: u32, // Total bytes written by device
}

/// Used ring header — device writes here when it's consumed a buffer.
#[repr(C)]
pub struct VirtqUsedHdr {
    pub flags: u16,
    pub idx: u16,
    // ring: [VirtqUsedElem; queue_size] follows
    // avail_event: u16 follows (after ring)
}

/// A complete virtqueue backed by a single contiguous DMA allocation
/// with legacy layout.
pub struct Virtqueue {
    /// Single DMA buffer holding the entire virtqueue (desc + avail + used).
    buf: DmaBuf,
    /// Queue size (number of descriptors).
    size: u16,
    /// Byte offset of the available ring within the buffer.
    avail_offset: usize,
    /// Byte offset of the used ring within the buffer.
    used_offset: usize,
    /// Next descriptor index to allocate.
    free_head: u16,
    /// Number of free descriptors.
    num_free: u16,
    /// Last used index we've seen.
    last_used_idx: u16,
}

impl Virtqueue {
    /// Compute the total size and offsets for a legacy virtqueue.
    /// Returns (total_bytes, avail_offset, used_offset).
    fn legacy_layout(queue_size: u16) -> (usize, usize, usize) {
        let qs = queue_size as usize;
        let desc_size = 16 * qs;
        // avail ring: flags(2) + idx(2) + ring(2*N) + used_event(2)
        let avail_size = 6 + 2 * qs;
        let avail_offset = desc_size;
        // Used ring must start at the next 4096-byte boundary
        let used_offset = align_up(desc_size + avail_size, 4096);
        // used ring: flags(2) + idx(2) + ring(8*N) + avail_event(2)
        let used_size = 6 + 8 * qs;
        let total = used_offset + used_size;
        (total, avail_offset, used_offset)
    }

    /// Allocate and initialize a new virtqueue with legacy layout.
    ///
    /// `queue_size` must be the value read from the device's Queue Size
    /// register — it dictates how many descriptors the device expects.
    /// The entire allocation must be page-aligned (PFN = phys/4096).
    pub fn new(queue_size: u16) -> Result<Self, AllocError> {
        let (total_size, avail_offset, used_offset) = Self::legacy_layout(queue_size);

        // Allocate page-aligned contiguous buffer.
        // page_align=1 means "align to 1 page" = 4096 bytes, which is
        // what legacy virtio requires (the PFN must be exact).
        let buf = DmaBuf::alloc_aligned(total_size, 1)?;
        // DmaBuf::alloc_aligned already zeroes the buffer.

        // Initialize descriptor free list: each descriptor's `next` points
        // to the following one, forming a singly-linked free list.
        let descs = buf.as_mut_ptr() as *mut VirtqDesc;
        for i in 0..queue_size {
            unsafe {
                let desc = &mut *descs.add(i as usize);
                desc.next = if i + 1 < queue_size { i + 1 } else { 0 };
                desc.flags = 0;
            }
        }

        Ok(Self {
            buf,
            size: queue_size,
            avail_offset,
            used_offset,
            free_head: 0,
            num_free: queue_size,
            last_used_idx: 0,
        })
    }

    /// Physical address of the start (= descriptor table).
    pub fn phys_addr(&self) -> PhysAddr {
        self.buf.phys_addr()
    }

    /// Page Frame Number for the legacy Queue Address register.
    /// PFN = physical_address / 4096.
    pub fn pfn(&self) -> u32 {
        (self.buf.phys_addr().as_u64() / 4096) as u32
    }

    pub fn size(&self) -> u16 {
        self.size
    }

    // ---- Internal pointer helpers ----

    fn avail_hdr_ptr(&self) -> *mut VirtqAvailHdr {
        unsafe { self.buf.as_mut_ptr().add(self.avail_offset) as *mut VirtqAvailHdr }
    }

    fn avail_ring_ptr(&self) -> *mut u16 {
        // Ring array starts right after the 4-byte VirtqAvailHdr.
        unsafe { self.buf.as_mut_ptr().add(self.avail_offset + 4) as *mut u16 }
    }

    fn used_hdr_ptr(&self) -> *const VirtqUsedHdr {
        unsafe { self.buf.as_ptr().add(self.used_offset) as *const VirtqUsedHdr }
    }

    fn used_ring_ptr(&self) -> *const VirtqUsedElem {
        // Ring array starts right after the 4-byte VirtqUsedHdr.
        unsafe { self.buf.as_ptr().add(self.used_offset + 4) as *const VirtqUsedElem }
    }

    /// Add a buffer (single descriptor) to the available ring.
    ///
    /// Returns the descriptor index, or None if the queue is full.
    pub fn add_buf(
        &mut self,
        buf_phys: PhysAddr,
        len: u32,
        device_writable: bool,
    ) -> Option<u16> {
        if self.num_free == 0 {
            return None;
        }

        let idx = self.free_head;

        // Set up the descriptor
        let descs = self.buf.as_mut_ptr() as *mut VirtqDesc;
        unsafe {
            let desc = &mut *descs.add(idx as usize);
            self.free_head = desc.next;
            desc.addr = buf_phys.as_u64();
            desc.len = len;
            desc.flags = if device_writable { VIRTQ_DESC_F_WRITE } else { 0 };
            desc.next = 0;
        }
        self.num_free -= 1;

        // Add to available ring
        unsafe {
            let avail = &mut *self.avail_hdr_ptr();
            let avail_idx = avail.idx;
            let ring = self.avail_ring_ptr();
            core::ptr::write_volatile(ring.add((avail_idx % self.size) as usize), idx);
            // Ensure the descriptor and ring entry are visible before we
            // update the index (which the device watches).
            fence(Ordering::Release);
            core::ptr::write_volatile(&mut avail.idx as *mut u16, avail_idx.wrapping_add(1));
        }

        Some(idx)
    }

    /// Check if the device has returned any used buffers.
    /// Returns (descriptor_index, bytes_written) or None.
    pub fn poll_used(&mut self) -> Option<(u16, u32)> {
        let used_idx = unsafe { core::ptr::read_volatile(&(*self.used_hdr_ptr()).idx) };

        if self.last_used_idx == used_idx {
            return None;
        }

        // Ensure we read the ring entry after seeing the updated index.
        fence(Ordering::Acquire);

        let ring_idx = (self.last_used_idx % self.size) as usize;
        let entry = unsafe { core::ptr::read_volatile(self.used_ring_ptr().add(ring_idx)) };

        self.last_used_idx = self.last_used_idx.wrapping_add(1);

        // Return descriptor to free list
        let descs = self.buf.as_mut_ptr() as *mut VirtqDesc;
        unsafe {
            let desc = &mut *descs.add(entry.id as usize);
            desc.next = self.free_head;
        }
        self.free_head = entry.id as u16;
        self.num_free += 1;

        Some((entry.id as u16, entry.len))
    }
}

fn align_up(val: usize, align: usize) -> usize {
    (val + align - 1) & !(align - 1)
}
