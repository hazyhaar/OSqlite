/// PCI enumeration for NVMe controllers.
///
/// NVMe controllers are PCI class 01h (Mass Storage), subclass 08h (NVM),
/// programming interface 02h (NVMe).
use crate::mem::PhysAddr;
use crate::drivers::pci::{pci_read32, pci_write32};

/// PCI device identification.
#[derive(Debug, Clone)]
pub struct PciDevice {
    pub bus: u8,
    pub device: u8,
    pub function: u8,
    pub vendor_id: u16,
    pub device_id: u16,
    pub class_code: u8,
    pub subclass: u8,
    pub prog_if: u8,
    pub bar0: u64,
}

/// Read BAR0 (Base Address Register 0) — may be 32-bit or 64-bit.
fn read_bar0(bus: u8, device: u8, func: u8) -> u64 {
    let bar0_low = pci_read32(bus, device, func, 0x10);

    // Check if it's a 64-bit BAR (bits 2:1 == 10b)
    if bar0_low & 0x06 == 0x04 {
        let bar0_high = pci_read32(bus, device, func, 0x14);
        let addr = ((bar0_high as u64) << 32) | ((bar0_low as u64) & !0xF);
        addr
    } else {
        (bar0_low & !0xF) as u64
    }
}

/// Enable bus mastering and memory space access for a PCI device.
fn enable_device(bus: u8, device: u8, func: u8) {
    let cmd = pci_read32(bus, device, func, 0x04);
    // Set bits: Memory Space (1), Bus Master (2)
    let new_cmd = cmd | 0x06;
    pci_write32(bus, device, func, 0x04, new_cmd);
}

/// Check if a PCI device is multi-function (Header Type bit 7).
fn is_multi_function(bus: u8, device: u8) -> bool {
    let header_type = pci_read32(bus, device, 0, 0x0C);
    // Header Type is at offset 0x0E within the config space (byte 2 of dword at 0x0C)
    ((header_type >> 16) & 0x80) != 0
}

/// Scan the PCI bus for NVMe controllers.
/// Returns the first NVMe controller found.
/// Checks all functions (0..7) on multi-function devices.
pub fn find_nvme_controller() -> Option<PciDevice> {
    for bus in 0..=255u16 {
        for device in 0..32u8 {
            let vendor_device = pci_read32(bus as u8, device, 0, 0x00);
            let vendor_id = (vendor_device & 0xFFFF) as u16;

            if vendor_id == 0xFFFF {
                continue; // No device
            }

            let max_func = if is_multi_function(bus as u8, device) { 8 } else { 1 };

            for func in 0..max_func {
                if func > 0 {
                    let vd = pci_read32(bus as u8, device, func, 0x00);
                    if (vd & 0xFFFF) as u16 == 0xFFFF {
                        continue;
                    }
                }

                let class_reg = pci_read32(bus as u8, device, func, 0x08);
                let class_code = ((class_reg >> 24) & 0xFF) as u8;
                let subclass = ((class_reg >> 16) & 0xFF) as u8;
                let prog_if = ((class_reg >> 8) & 0xFF) as u8;

                // NVMe: class 01h, subclass 08h, prog_if 02h
                if class_code == 0x01 && subclass == 0x08 && prog_if == 0x02 {
                    let vd = if func == 0 { vendor_device } else {
                        pci_read32(bus as u8, device, func, 0x00)
                    };
                    let vid = (vd & 0xFFFF) as u16;
                    let device_id = ((vd >> 16) & 0xFFFF) as u16;

                    // Enable the device before reading BAR
                    enable_device(bus as u8, device, func);

                    let bar0 = read_bar0(bus as u8, device, func);

                    return Some(PciDevice {
                        bus: bus as u8,
                        device,
                        function: func,
                        vendor_id: vid,
                        device_id,
                        class_code,
                        subclass,
                        prog_if,
                        bar0,
                    });
                }
            }
        }
    }

    None
}

/// Get the physical address of BAR0 for an NVMe controller.
/// The caller must map this address as uncacheable (UC) in the page tables.
pub fn nvme_bar0_phys(dev: &PciDevice) -> PhysAddr {
    PhysAddr::new(dev.bar0)
}
