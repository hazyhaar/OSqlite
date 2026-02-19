/// Virtio split virtqueue implementation.
///
/// A virtqueue is the fundamental I/O mechanism for virtio devices.
/// It consists of three parts:
/// - Descriptor Table: array of buffer descriptors (addr, len, flags, next)
/// - Available Ring: guest → device (which descriptors are ready)
/// - Used Ring: device → guest (which descriptors are done)
///
/// This is shared memory between the guest (HeavenOS) and the host (QEMU).
use core::sync::atomic::{fence, Ordering};
use crate::mem::{PhysAddr, DmaBuf, AllocError};

/// Number of descriptors per queue (must be power of 2).
const QUEUE_SIZE: u16 = 256;

/// Virtqueue descriptor flags.
const VIRTQ_DESC_F_NEXT: u16 = 1;     // Buffer continues in next descriptor
const VIRTQ_DESC_F_WRITE: u16 = 2;    // Buffer is device-writable (for rx)

/// A single descriptor in the descriptor table.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct VirtqDesc {
    pub addr: u64,    // Physical address of buffer
    pub len: u32,     // Length of buffer
    pub flags: u16,   // VIRTQ_DESC_F_*
    pub next: u16,    // Next descriptor index (if NEXT flag set)
}

/// Available ring — guest writes here to offer buffers to the device.
#[repr(C)]
pub struct VirtqAvail {
    pub flags: u16,
    pub idx: u16,
    pub ring: [u16; QUEUE_SIZE as usize],
}

/// Used ring — device writes here when it's consumed a buffer.
#[repr(C)]
pub struct VirtqUsed {
    pub flags: u16,
    pub idx: u16,
    pub ring: [VirtqUsedElem; QUEUE_SIZE as usize],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct VirtqUsedElem {
    pub id: u32,      // Descriptor chain head index
    pub len: u32,     // Total bytes written by device
}

/// A complete virtqueue with descriptor table, available ring, and used ring.
pub struct Virtqueue {
    /// DMA buffer holding the descriptor table.
    desc_buf: DmaBuf,
    /// DMA buffer holding the available ring.
    avail_buf: DmaBuf,
    /// DMA buffer holding the used ring.
    used_buf: DmaBuf,
    /// Number of descriptors.
    size: u16,
    /// Next descriptor index to allocate.
    free_head: u16,
    /// Number of free descriptors.
    num_free: u16,
    /// Last used index we've seen.
    last_used_idx: u16,
}

impl Virtqueue {
    /// Allocate and initialize a new virtqueue.
    pub fn new() -> Result<Self, AllocError> {
        let desc_size = QUEUE_SIZE as usize * core::mem::size_of::<VirtqDesc>();
        let avail_size = 4 + QUEUE_SIZE as usize * 2; // flags + idx + ring entries
        let used_size = 4 + QUEUE_SIZE as usize * core::mem::size_of::<VirtqUsedElem>();

        let desc_buf = DmaBuf::alloc_aligned(desc_size, 1)?;
        let avail_buf = DmaBuf::alloc_aligned(avail_size, 1)?;
        let used_buf = DmaBuf::alloc_aligned(used_size, 1)?;

        // Initialize descriptor free list: each points to the next
        let descs = desc_buf.as_mut_ptr() as *mut VirtqDesc;
        for i in 0..QUEUE_SIZE {
            unsafe {
                let desc = &mut *descs.add(i as usize);
                desc.next = if i + 1 < QUEUE_SIZE { i + 1 } else { 0 };
                desc.flags = 0;
            }
        }

        Ok(Self {
            desc_buf,
            avail_buf,
            used_buf,
            size: QUEUE_SIZE,
            free_head: 0,
            num_free: QUEUE_SIZE,
            last_used_idx: 0,
        })
    }

    /// Physical addresses for device configuration.
    pub fn desc_phys(&self) -> PhysAddr { self.desc_buf.phys_addr() }
    pub fn avail_phys(&self) -> PhysAddr { self.avail_buf.phys_addr() }
    pub fn used_phys(&self) -> PhysAddr { self.used_buf.phys_addr() }
    pub fn size(&self) -> u16 { self.size }

    /// Add a buffer (single descriptor) to the available ring.
    /// `buf_phys`: physical address of the buffer
    /// `len`: buffer length
    /// `device_writable`: true if the device should write to this buffer (rx)
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
        let descs = self.desc_buf.as_mut_ptr() as *mut VirtqDesc;
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
        let avail = self.avail_buf.as_mut_ptr() as *mut VirtqAvail;
        unsafe {
            let avail = &mut *avail;
            let avail_idx = avail.idx;
            avail.ring[(avail_idx % self.size) as usize] = idx;
            fence(Ordering::Release);
            avail.idx = avail_idx.wrapping_add(1);
        }

        Some(idx)
    }

    /// Check if the device has returned any used buffers.
    /// Returns (descriptor_index, bytes_written) or None.
    pub fn poll_used(&mut self) -> Option<(u16, u32)> {
        let used = self.used_buf.as_ptr() as *const VirtqUsed;
        let used_idx = unsafe { core::ptr::read_volatile(&(*used).idx) };

        if self.last_used_idx == used_idx {
            return None;
        }

        fence(Ordering::Acquire);

        let entry = unsafe {
            let ring_idx = (self.last_used_idx % self.size) as usize;
            core::ptr::read_volatile(&(*used).ring[ring_idx])
        };

        self.last_used_idx = self.last_used_idx.wrapping_add(1);

        // Return descriptor to free list
        let descs = self.desc_buf.as_mut_ptr() as *mut VirtqDesc;
        unsafe {
            let desc = &mut *descs.add(entry.id as usize);
            desc.next = self.free_head;
        }
        self.free_head = entry.id as u16;
        self.num_free += 1;

        Some((entry.id as u16, entry.len))
    }
}
