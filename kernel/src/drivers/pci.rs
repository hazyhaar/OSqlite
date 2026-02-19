/// Shared PCI configuration space access via port I/O (0xCF8/0xCFC).
///
/// Both the NVMe and virtio drivers need PCI config reads/writes.
/// This module centralises them to avoid code duplication.
use crate::arch::x86_64::{outl, inl};

/// Read a 32-bit value from PCI configuration space.
pub fn pci_read32(bus: u8, device: u8, func: u8, offset: u8) -> u32 {
    let addr: u32 = 0x8000_0000
        | ((bus as u32) << 16)
        | ((device as u32) << 11)
        | ((func as u32) << 8)
        | ((offset as u32) & 0xFC);
    outl(0xCF8, addr);
    inl(0xCFC)
}

/// Write a 32-bit value to PCI configuration space.
pub fn pci_write32(bus: u8, device: u8, func: u8, offset: u8, val: u32) {
    let addr: u32 = 0x8000_0000
        | ((bus as u32) << 16)
        | ((device as u32) << 11)
        | ((func as u32) << 8)
        | ((offset as u32) & 0xFC);
    outl(0xCF8, addr);
    outl(0xCFC, val);
}
