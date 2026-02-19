/// x86_64 architecture support.
///
/// This module provides:
/// - Port I/O (in/out instructions)
/// - Serial console (COM1) for debug output
/// - CPU feature detection
/// - Interrupt descriptor table (IDT) skeleton
pub mod serial;
pub mod cpu;
pub mod idt;

/// Halt the CPU until the next interrupt.
#[inline(always)]
pub fn hlt() {
    unsafe { core::arch::asm!("hlt", options(nostack, nomem)); }
}

/// Disable interrupts.
#[inline(always)]
pub fn cli() {
    unsafe { core::arch::asm!("cli", options(nostack, nomem)); }
}

/// Enable interrupts.
#[inline(always)]
pub fn sti() {
    unsafe { core::arch::asm!("sti", options(nostack, nomem)); }
}

/// Write a byte to an I/O port.
#[inline(always)]
pub fn outb(port: u16, val: u8) {
    unsafe {
        core::arch::asm!(
            "out dx, al",
            in("dx") port,
            in("al") val,
            options(nostack, preserves_flags),
        );
    }
}

/// Read a byte from an I/O port.
#[inline(always)]
pub fn inb(port: u16) -> u8 {
    let val: u8;
    unsafe {
        core::arch::asm!(
            "in al, dx",
            in("dx") port,
            out("al") val,
            options(nostack, preserves_flags),
        );
    }
    val
}

/// Write a 32-bit value to an I/O port.
#[inline(always)]
pub fn outl(port: u16, val: u32) {
    unsafe {
        core::arch::asm!(
            "out dx, eax",
            in("dx") port,
            in("eax") val,
            options(nostack, preserves_flags),
        );
    }
}

/// Read a 32-bit value from an I/O port.
#[inline(always)]
pub fn inl(port: u16) -> u32 {
    let val: u32;
    unsafe {
        core::arch::asm!(
            "in eax, dx",
            in("dx") port,
            out("eax") val,
            options(nostack, preserves_flags),
        );
    }
    val
}
