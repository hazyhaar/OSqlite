/// Interrupt Descriptor Table (IDT) — minimal skeleton.
///
/// For Phase 1, we only handle:
/// - Division by zero (#DE)
/// - Page fault (#PF)
/// - General protection fault (#GP)
/// - Double fault (#DF)
///
/// NVMe interrupts will be added in Phase 2 (MSI-X).

/// IDT entry (16 bytes on x86_64).
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct IdtEntry {
    offset_low: u16,
    selector: u16,
    ist: u8,
    type_attr: u8,
    offset_mid: u16,
    offset_high: u32,
    _reserved: u32,
}

impl IdtEntry {
    pub const fn missing() -> Self {
        Self {
            offset_low: 0,
            selector: 0,
            ist: 0,
            type_attr: 0,
            offset_mid: 0,
            offset_high: 0,
            _reserved: 0,
        }
    }

    /// Create an interrupt gate entry.
    /// `handler`: address of the handler function
    /// `selector`: code segment selector (usually 0x08 for kernel CS)
    /// `ist`: interrupt stack table index (0 = no IST)
    pub fn new(handler: u64, selector: u16, ist: u8) -> Self {
        Self {
            offset_low: handler as u16,
            selector,
            ist,
            type_attr: 0x8E, // present, ring 0, interrupt gate
            offset_mid: (handler >> 16) as u16,
            offset_high: (handler >> 32) as u32,
            _reserved: 0,
        }
    }
}

/// The IDT — 256 entries.
#[repr(C, align(16))]
pub struct Idt {
    pub entries: [IdtEntry; 256],
}

impl Idt {
    pub const fn new() -> Self {
        Self {
            entries: [IdtEntry::missing(); 256],
        }
    }

    /// Load this IDT into the CPU.
    pub fn load(&'static self) {
        let ptr = IdtPointer {
            limit: (core::mem::size_of::<Self>() - 1) as u16,
            base: self as *const _ as u64,
        };

        unsafe {
            core::arch::asm!("lidt [{}]", in(reg) &ptr, options(nostack));
        }
    }
}

#[repr(C, packed)]
struct IdtPointer {
    limit: u16,
    base: u64,
}

pub static mut IDT: Idt = Idt::new();
