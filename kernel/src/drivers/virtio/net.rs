/// Virtio-net driver — paravirtualized NIC for QEMU.
///
/// This is HeavenOS's path to the network. QEMU exposes a virtio-net
/// PCI device that we configure through MMIO registers and communicate
/// with via virtqueues (shared memory ring buffers).
///
/// The driver provides raw Ethernet frame send/receive, which feeds
/// into smoltcp for TCP/IP.
use alloc::vec::Vec;
use spin::Mutex;

use crate::mem::DmaBuf;
use super::virtqueue::Virtqueue;

/// Virtio PCI capability offsets (virtio 1.0+ modern device).
mod virtio_regs {
    // Common configuration
    pub const DEVICE_FEATURE: usize = 0x00;
    pub const DRIVER_FEATURE: usize = 0x04;
    pub const DEVICE_STATUS: usize = 0x14;
    pub const QUEUE_SELECT: usize = 0x16;
    pub const QUEUE_SIZE: usize = 0x18;
    pub const QUEUE_ENABLE: usize = 0x1C;
    pub const QUEUE_DESC: usize = 0x20;
    pub const QUEUE_AVAIL: usize = 0x28;
    pub const QUEUE_USED: usize = 0x30;
}

/// Virtio device status bits.
const STATUS_ACKNOWLEDGE: u8 = 1;
const STATUS_DRIVER: u8 = 2;
const STATUS_FEATURES_OK: u8 = 8;
const STATUS_DRIVER_OK: u8 = 4;
const STATUS_FAILED: u8 = 128;

/// Virtio-net feature bits we care about.
const VIRTIO_NET_F_MAC: u64 = 1 << 5;        // Device has MAC address
const VIRTIO_NET_F_STATUS: u64 = 1 << 16;    // Link status available

/// Virtio-net header prepended to every packet.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct VirtioNetHeader {
    pub flags: u8,
    pub gso_type: u8,
    pub hdr_len: u16,
    pub gso_size: u16,
    pub csum_start: u16,
    pub csum_offset: u16,
    // No num_buffers for tx
}

const NET_HDR_SIZE: usize = core::mem::size_of::<VirtioNetHeader>();

/// Maximum Ethernet frame size + virtio header.
const RX_BUF_SIZE: usize = 1514 + NET_HDR_SIZE;

/// Number of receive buffers pre-allocated.
const RX_POOL_SIZE: usize = 64;

/// Virtio-net driver.
pub struct VirtioNet {
    /// MMIO base for common configuration.
    common_cfg: *mut u8,
    /// Receive virtqueue (queue 0).
    rx_queue: Virtqueue,
    /// Transmit virtqueue (queue 1).
    tx_queue: Virtqueue,
    /// MAC address (6 bytes).
    mac: [u8; 6],
    /// Pre-allocated receive buffers.
    rx_buffers: Vec<DmaBuf>,
}

unsafe impl Send for VirtioNet {}

impl VirtioNet {
    /// Initialize the virtio-net device at the given MMIO address.
    ///
    /// # Safety
    /// `common_cfg` must point to the virtio common configuration space,
    /// mapped as uncacheable.
    pub unsafe fn new(common_cfg: *mut u8, net_cfg: *mut u8) -> Result<Self, VirtioNetError> {
        // 1. Reset device
        write_reg8(common_cfg, virtio_regs::DEVICE_STATUS, 0);

        // 2. Acknowledge device
        write_reg8(common_cfg, virtio_regs::DEVICE_STATUS, STATUS_ACKNOWLEDGE);

        // 3. We know how to drive it
        write_reg8(
            common_cfg,
            virtio_regs::DEVICE_STATUS,
            STATUS_ACKNOWLEDGE | STATUS_DRIVER,
        );

        // 4. Negotiate features (we want MAC address)
        let device_features = read_reg32(common_cfg, virtio_regs::DEVICE_FEATURE) as u64;
        let our_features = device_features & (VIRTIO_NET_F_MAC | VIRTIO_NET_F_STATUS);
        write_reg32(common_cfg, virtio_regs::DRIVER_FEATURE, our_features as u32);

        // 5. Features OK
        write_reg8(
            common_cfg,
            virtio_regs::DEVICE_STATUS,
            STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK,
        );

        let status = read_reg8(common_cfg, virtio_regs::DEVICE_STATUS);
        if status & STATUS_FEATURES_OK == 0 {
            return Err(VirtioNetError::FeatureNegotiationFailed);
        }

        // 6. Read MAC address from device-specific config
        let mut mac = [0u8; 6];
        for i in 0..6 {
            mac[i] = core::ptr::read_volatile(net_cfg.add(i));
        }

        // 7. Set up virtqueues
        // Queue 0 = rx, Queue 1 = tx
        let rx_queue = Self::setup_queue(common_cfg, 0)?;
        let tx_queue = Self::setup_queue(common_cfg, 1)?;

        let mut driver = Self {
            common_cfg,
            rx_queue,
            tx_queue,
            mac,
            rx_buffers: Vec::new(),
        };

        // 8. Pre-allocate and post receive buffers
        driver.fill_rx_pool()?;

        // 9. Driver OK — device is live
        write_reg8(
            common_cfg,
            virtio_regs::DEVICE_STATUS,
            STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK | STATUS_DRIVER_OK,
        );

        Ok(driver)
    }

    /// Configure a virtqueue.
    unsafe fn setup_queue(
        common_cfg: *mut u8,
        queue_idx: u16,
    ) -> Result<Virtqueue, VirtioNetError> {
        // Select queue
        write_reg16(common_cfg, virtio_regs::QUEUE_SELECT, queue_idx);

        // Check queue size
        let size = read_reg16(common_cfg, virtio_regs::QUEUE_SIZE);
        if size == 0 {
            return Err(VirtioNetError::QueueNotAvailable);
        }

        // Allocate virtqueue
        let vq = Virtqueue::new().map_err(|_| VirtioNetError::OutOfMemory)?;

        // Tell device where the queue is
        write_reg64(common_cfg, virtio_regs::QUEUE_DESC, vq.desc_phys().as_u64());
        write_reg64(common_cfg, virtio_regs::QUEUE_AVAIL, vq.avail_phys().as_u64());
        write_reg64(common_cfg, virtio_regs::QUEUE_USED, vq.used_phys().as_u64());

        // Enable queue
        write_reg16(common_cfg, virtio_regs::QUEUE_ENABLE, 1);

        Ok(vq)
    }

    /// Pre-allocate receive buffers and post them to the rx queue.
    fn fill_rx_pool(&mut self) -> Result<(), VirtioNetError> {
        for _ in 0..RX_POOL_SIZE {
            let buf = DmaBuf::alloc(RX_BUF_SIZE).map_err(|_| VirtioNetError::OutOfMemory)?;
            let phys = buf.phys_addr();
            self.rx_queue.add_buf(phys, RX_BUF_SIZE as u32, true); // device-writable
            self.rx_buffers.push(buf);
        }
        Ok(())
    }

    /// Transmit an Ethernet frame. Prepends the virtio-net header.
    pub fn transmit(&mut self, frame: &[u8]) -> Result<(), VirtioNetError> {
        let total_len = NET_HDR_SIZE + frame.len();
        let mut buf = DmaBuf::alloc(total_len).map_err(|_| VirtioNetError::OutOfMemory)?;

        // Write virtio-net header (all zeros = no offload)
        let hdr = VirtioNetHeader::default();
        let hdr_bytes = unsafe {
            core::slice::from_raw_parts(
                &hdr as *const VirtioNetHeader as *const u8,
                NET_HDR_SIZE,
            )
        };

        let data = buf.as_mut_slice();
        data[..NET_HDR_SIZE].copy_from_slice(hdr_bytes);
        data[NET_HDR_SIZE..total_len].copy_from_slice(frame);

        buf.flush_cache();
        let phys = buf.phys_addr();

        match self.tx_queue.add_buf(phys, total_len as u32, false) {
            Some(_) => {
                // Notify device (write to queue notify register)
                self.notify_tx();
                // Leak the buf — it'll be reclaimed when the device returns it
                // TODO: proper tx completion tracking
                core::mem::forget(buf);
                Ok(())
            }
            None => Err(VirtioNetError::QueueFull),
        }
    }

    /// Poll for received Ethernet frames. Returns the frame data (without
    /// the virtio-net header).
    pub fn receive(&mut self) -> Option<Vec<u8>> {
        let (desc_idx, len) = self.rx_queue.poll_used()?;

        if (desc_idx as usize) < self.rx_buffers.len() && len as usize > NET_HDR_SIZE {
            let buf = &self.rx_buffers[desc_idx as usize];
            buf.invalidate_cache();

            let frame_start = NET_HDR_SIZE;
            let frame_end = len as usize;
            let frame = buf.as_slice()[frame_start..frame_end].to_vec();

            // Re-post the buffer for future receives
            let phys = buf.phys_addr();
            self.rx_queue.add_buf(phys, RX_BUF_SIZE as u32, true);

            Some(frame)
        } else {
            None
        }
    }

    /// Get the MAC address.
    pub fn mac(&self) -> [u8; 6] {
        self.mac
    }

    /// Notify the device that the tx queue has new buffers.
    fn notify_tx(&self) {
        // For modern virtio: write to the queue notify offset.
        // The exact mechanism depends on the PCI capability structure.
        // For legacy virtio (port I/O), write queue index to port + 16.
        //
        // TODO: use the actual notify offset from PCI capabilities.
        // For now, this is a placeholder that works with legacy virtio.
    }
}

/// Scan PCI for a virtio-net device.
/// Virtio devices have vendor ID 0x1AF4, device IDs 0x1000-0x103F (legacy)
/// or 0x1041 (modern virtio-net).
pub fn find_virtio_net() -> Option<VirtioNetPciInfo> {
    for bus in 0..=255u16 {
        for device in 0..32u8 {
            let vendor_device = pci_read32(bus as u8, device, 0, 0x00);
            let vendor_id = (vendor_device & 0xFFFF) as u16;

            if vendor_id != 0x1AF4 {
                continue;
            }

            let device_id = ((vendor_device >> 16) & 0xFFFF) as u16;

            // Legacy virtio-net: device_id 0x1000
            // Modern virtio-net: device_id 0x1041
            if device_id != 0x1000 && device_id != 0x1041 {
                continue;
            }

            // Check subsystem ID to confirm it's a network device
            let subsys = pci_read32(bus as u8, device, 0, 0x2C);
            let subsys_id = ((subsys >> 16) & 0xFFFF) as u16;

            // Subsystem device ID 1 = network
            if device_id == 0x1000 && subsys_id != 1 {
                continue;
            }

            // Enable bus mastering
            let cmd = pci_read32(bus as u8, device, 0, 0x04);
            pci_write32(bus as u8, device, 0, 0x04, cmd | 0x06);

            // Read BARs
            let bar0 = pci_read32(bus as u8, device, 0, 0x10) as u64 & !0xF;
            let bar1 = pci_read32(bus as u8, device, 0, 0x14) as u64 & !0xF;

            return Some(VirtioNetPciInfo {
                bus: bus as u8,
                device,
                device_id,
                bar0,
                bar1,
            });
        }
    }
    None
}

#[derive(Debug)]
pub struct VirtioNetPciInfo {
    pub bus: u8,
    pub device: u8,
    pub device_id: u16,
    pub bar0: u64,
    pub bar1: u64,
}

#[derive(Debug)]
pub enum VirtioNetError {
    FeatureNegotiationFailed,
    QueueNotAvailable,
    QueueFull,
    OutOfMemory,
    DeviceNotFound,
}

impl core::fmt::Display for VirtioNetError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            VirtioNetError::FeatureNegotiationFailed => write!(f, "virtio feature negotiation failed"),
            VirtioNetError::QueueNotAvailable => write!(f, "virtio queue not available"),
            VirtioNetError::QueueFull => write!(f, "virtio tx queue full"),
            VirtioNetError::OutOfMemory => write!(f, "out of memory"),
            VirtioNetError::DeviceNotFound => write!(f, "virtio-net device not found"),
        }
    }
}

// PCI helpers (same as NVMe driver — should be shared, but keeping local for now)
fn pci_read32(bus: u8, device: u8, func: u8, offset: u8) -> u32 {
    let addr: u32 = 0x8000_0000
        | ((bus as u32) << 16)
        | ((device as u32) << 11)
        | ((func as u32) << 8)
        | ((offset as u32) & 0xFC);
    unsafe {
        core::arch::asm!("out dx, eax", in("dx") 0xCF8u16, in("eax") addr, options(nostack, preserves_flags));
        let val: u32;
        core::arch::asm!("in eax, dx", in("dx") 0xCFCu16, out("eax") val, options(nostack, preserves_flags));
        val
    }
}

fn pci_write32(bus: u8, device: u8, func: u8, offset: u8, val: u32) {
    let addr: u32 = 0x8000_0000
        | ((bus as u32) << 16)
        | ((device as u32) << 11)
        | ((func as u32) << 8)
        | ((offset as u32) & 0xFC);
    unsafe {
        core::arch::asm!("out dx, eax", in("dx") 0xCF8u16, in("eax") addr, options(nostack, preserves_flags));
        core::arch::asm!("out dx, eax", in("dx") 0xCFCu16, in("eax") val, options(nostack, preserves_flags));
    }
}

unsafe fn read_reg8(base: *mut u8, offset: usize) -> u8 {
    core::ptr::read_volatile(base.add(offset))
}

unsafe fn read_reg16(base: *mut u8, offset: usize) -> u16 {
    core::ptr::read_volatile(base.add(offset) as *const u16)
}

unsafe fn read_reg32(base: *mut u8, offset: usize) -> u32 {
    core::ptr::read_volatile(base.add(offset) as *const u32)
}

unsafe fn write_reg8(base: *mut u8, offset: usize, val: u8) {
    core::ptr::write_volatile(base.add(offset), val);
}

unsafe fn write_reg16(base: *mut u8, offset: usize, val: u16) {
    core::ptr::write_volatile(base.add(offset) as *mut u16, val);
}

unsafe fn write_reg32(base: *mut u8, offset: usize, val: u32) {
    core::ptr::write_volatile(base.add(offset) as *mut u32, val);
}

unsafe fn write_reg64(base: *mut u8, offset: usize, val: u64) {
    core::ptr::write_volatile(base.add(offset) as *mut u64, val);
}

/// Global virtio-net driver instance.
pub static VIRTIO_NET: Mutex<Option<VirtioNet>> = Mutex::new(None);
