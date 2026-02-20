/// Virtio-net driver — paravirtualized NIC for QEMU (legacy mode).
///
/// This driver targets legacy virtio (device ID 0x1000) with port I/O
/// transport. All register access goes through x86 in/out instructions
/// at the I/O port base from BAR0.
///
/// Legacy register layout at BAR0 (I/O port base):
///   0x00  Device Features      (RO, 32-bit)
///   0x04  Driver Features      (WO, 32-bit)
///   0x08  Queue Address (PFN)  (RW, 32-bit) — phys/4096
///   0x0C  Queue Size           (RO, 16-bit)
///   0x0E  Queue Select         (RW, 16-bit)
///   0x10  Queue Notify         (WO, 16-bit)
///   0x12  Device Status        (RW, 8-bit)
///   0x13  ISR Status           (RO, 8-bit)
///   0x14+ Device-specific config (MAC at 0x14..0x19)
use alloc::vec::Vec;
use spin::Mutex;

use crate::arch::x86_64::{inb, inl, inw, outb, outl, outw};
use crate::drivers::pci::{pci_read32, pci_write32};
use crate::mem::DmaBuf;
use super::virtqueue::Virtqueue;

/// Legacy virtio register offsets from I/O port base.
mod regs {
    pub const DEVICE_FEATURES: u16 = 0x00;  // 32-bit RO
    pub const DRIVER_FEATURES: u16 = 0x04;  // 32-bit WO
    pub const QUEUE_ADDRESS: u16   = 0x08;  // 32-bit RW (PFN)
    pub const QUEUE_SIZE: u16      = 0x0C;  // 16-bit RO
    pub const QUEUE_SELECT: u16    = 0x0E;  // 16-bit RW
    pub const QUEUE_NOTIFY: u16    = 0x10;  // 16-bit WO
    pub const DEVICE_STATUS: u16   = 0x12;  // 8-bit RW
    pub const ISR_STATUS: u16      = 0x13;  // 8-bit RO
    pub const MAC_BASE: u16        = 0x14;  // device-specific: 6 bytes
}

/// Virtio device status bits.
const STATUS_ACKNOWLEDGE: u8 = 1;
const STATUS_DRIVER: u8 = 2;
const STATUS_DRIVER_OK: u8 = 4;

/// Virtio-net feature bits.
const VIRTIO_NET_F_MAC: u32 = 1 << 5;

/// Virtio-net header prepended to every packet (legacy, 10 bytes).
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct VirtioNetHeader {
    pub flags: u8,
    pub gso_type: u8,
    pub hdr_len: u16,
    pub gso_size: u16,
    pub csum_start: u16,
    pub csum_offset: u16,
}

const NET_HDR_SIZE: usize = core::mem::size_of::<VirtioNetHeader>(); // 10 bytes

/// Maximum Ethernet frame + virtio header.
const RX_BUF_SIZE: usize = 1514 + NET_HDR_SIZE;

/// Number of pre-allocated receive buffers.
const RX_POOL_SIZE: usize = 64;

/// Virtio-net driver (legacy, port I/O).
pub struct VirtioNet {
    /// I/O port base from PCI BAR0.
    iobase: u16,
    /// Receive virtqueue (queue 0).
    rx_queue: Virtqueue,
    /// Transmit virtqueue (queue 1).
    tx_queue: Virtqueue,
    /// MAC address.
    mac: [u8; 6],
    /// Pre-allocated receive buffers, indexed by descriptor index.
    rx_buffers: Vec<DmaBuf>,
    /// In-flight TX buffers awaiting device completion.
    tx_inflight: Vec<Option<DmaBuf>>,
}

unsafe impl Send for VirtioNet {}

impl VirtioNet {
    /// Initialize the virtio-net device at the given I/O port base.
    ///
    /// # Safety
    /// `iobase` must be the I/O port address from BAR0 of a legacy
    /// virtio-net PCI device (vendor 0x1AF4, device 0x1000, subsys 1).
    pub unsafe fn new(iobase: u16) -> Result<Self, VirtioNetError> {
        // 1. Reset device (write 0 to status)
        outb(iobase + regs::DEVICE_STATUS, 0);

        // 2. Acknowledge: we see the device
        outb(iobase + regs::DEVICE_STATUS, STATUS_ACKNOWLEDGE);

        // 3. Driver: we know how to drive it
        outb(iobase + regs::DEVICE_STATUS, STATUS_ACKNOWLEDGE | STATUS_DRIVER);

        // 4. Negotiate features (we want MAC address)
        let device_features = inl(iobase + regs::DEVICE_FEATURES);
        let our_features = device_features & VIRTIO_NET_F_MAC;
        outl(iobase + regs::DRIVER_FEATURES, our_features);
        // Legacy virtio has no FEATURES_OK step — features are just set.

        // 5. Read MAC address from device-specific config
        let mut mac = [0u8; 6];
        for i in 0..6 {
            mac[i] = inb(iobase + regs::MAC_BASE + i as u16);
        }

        // 6. Set up virtqueues
        let rx_queue = Self::setup_queue(iobase, 0)?;
        let tx_queue = Self::setup_queue(iobase, 1)?;

        let mut driver = Self {
            iobase,
            rx_queue,
            tx_queue,
            mac,
            rx_buffers: Vec::new(),
            tx_inflight: Vec::new(),
        };

        // 7. Pre-allocate and post receive buffers
        driver.fill_rx_pool()?;

        // 8. Driver OK — device is live
        outb(
            iobase + regs::DEVICE_STATUS,
            STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_DRIVER_OK,
        );

        Ok(driver)
    }

    /// Configure a virtqueue for legacy virtio.
    unsafe fn setup_queue(iobase: u16, queue_idx: u16) -> Result<Virtqueue, VirtioNetError> {
        // Select queue
        outw(iobase + regs::QUEUE_SELECT, queue_idx);

        // Read queue size from device
        let size = inw(iobase + regs::QUEUE_SIZE);
        if size == 0 {
            return Err(VirtioNetError::QueueNotAvailable);
        }

        // Allocate virtqueue with device-reported size
        let vq = Virtqueue::new(size).map_err(|_| VirtioNetError::OutOfMemory)?;

        // Tell device where the queue is: write PFN to Queue Address register
        outl(iobase + regs::QUEUE_ADDRESS, vq.pfn());

        Ok(vq)
    }

    /// Pre-allocate receive buffers and post them to the rx queue.
    fn fill_rx_pool(&mut self) -> Result<(), VirtioNetError> {
        for _ in 0..RX_POOL_SIZE {
            let buf = DmaBuf::alloc(RX_BUF_SIZE).map_err(|_| VirtioNetError::OutOfMemory)?;
            let phys = buf.phys_addr();
            self.rx_queue.add_buf(phys, RX_BUF_SIZE as u32, true);
            self.rx_buffers.push(buf);
        }
        // Notify device that rx queue has buffers
        self.notify_queue(0);
        Ok(())
    }

    /// Reclaim completed TX buffers from the device.
    fn reclaim_tx_buffers(&mut self) {
        while let Some((desc_idx, _len)) = self.tx_queue.poll_used() {
            let idx = desc_idx as usize;
            if idx < self.tx_inflight.len() {
                self.tx_inflight[idx] = None;
            }
        }
    }

    /// Transmit an Ethernet frame. Prepends the virtio-net header.
    pub fn transmit(&mut self, frame: &[u8]) -> Result<(), VirtioNetError> {
        self.reclaim_tx_buffers();

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
            Some(desc_idx) => {
                // Notify device: tx is queue 1
                self.notify_queue(1);
                // Track buffer until device returns it
                let idx = desc_idx as usize;
                if idx >= self.tx_inflight.len() {
                    self.tx_inflight.resize_with(idx + 1, || None);
                }
                self.tx_inflight[idx] = Some(buf);
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

            let frame = buf.as_slice()[NET_HDR_SIZE..len as usize].to_vec();

            // Re-post the buffer for future receives
            let phys = buf.phys_addr();
            self.rx_queue.add_buf(phys, RX_BUF_SIZE as u32, true);
            // Notify device that rx queue has a new buffer
            self.notify_queue(0);

            Some(frame)
        } else {
            None
        }
    }

    /// Get the MAC address.
    pub fn mac(&self) -> [u8; 6] {
        self.mac
    }

    /// Notify the device that a queue has new buffers.
    /// For legacy virtio: write the queue index to the Queue Notify register.
    #[inline]
    fn notify_queue(&self, queue_idx: u16) {
        outw(self.iobase + regs::QUEUE_NOTIFY, queue_idx);
    }
}

/// Check if a PCI device is multi-function (Header Type bit 7).
fn is_multi_function(bus: u8, device: u8) -> bool {
    let header_type = pci_read32(bus, device, 0, 0x0C);
    ((header_type >> 16) & 0x80) != 0
}

/// Scan PCI for a legacy virtio-net device.
/// Legacy virtio: vendor 0x1AF4, device 0x1000, subsystem ID 1 (network).
/// Checks all functions (0..7) on multi-function devices.
pub fn find_virtio_net() -> Option<VirtioNetPciInfo> {
    for bus in 0..=255u16 {
        for device in 0..32u8 {
            let vendor_device = pci_read32(bus as u8, device, 0, 0x00);
            let vendor_id = (vendor_device & 0xFFFF) as u16;

            if vendor_id == 0xFFFF {
                continue;
            }

            let max_func = if is_multi_function(bus as u8, device) { 8 } else { 1 };

            for func in 0..max_func {
                let vd = if func == 0 { vendor_device } else {
                    let vd = pci_read32(bus as u8, device, func, 0x00);
                    if (vd & 0xFFFF) as u16 == 0xFFFF { continue; }
                    vd
                };
                let vid = (vd & 0xFFFF) as u16;
                let did = ((vd >> 16) & 0xFFFF) as u16;

                if vid != 0x1AF4 || did != 0x1000 {
                    continue;
                }

                // Check subsystem ID to confirm it's a network device (subsys 1)
                let subsys = pci_read32(bus as u8, device, func, 0x2C);
                let subsys_id = ((subsys >> 16) & 0xFFFF) as u16;
                if subsys_id != 1 {
                    continue;
                }

                // Enable bus mastering + I/O space access
                let cmd = pci_read32(bus as u8, device, func, 0x04);
                pci_write32(bus as u8, device, func, 0x04, cmd | 0x05);

                // Read BAR0 — for legacy virtio this is an I/O port BAR
                let bar0_raw = pci_read32(bus as u8, device, func, 0x10);
                let iobase = (bar0_raw & !0x3) as u16;

                return Some(VirtioNetPciInfo {
                    bus: bus as u8,
                    device,
                    device_id: did,
                    iobase,
                });
            }
        }
    }
    None
}

#[derive(Debug)]
pub struct VirtioNetPciInfo {
    pub bus: u8,
    pub device: u8,
    pub device_id: u16,
    pub iobase: u16,
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

/// Global virtio-net driver instance.
pub static VIRTIO_NET: Mutex<Option<VirtioNet>> = Mutex::new(None);
