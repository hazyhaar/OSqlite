/// Global Descriptor Table (GDT) — required for x86_64 long mode.
///
/// Even though long mode ignores most segment fields, the CPU still
/// requires a valid GDT with at least a null descriptor, a kernel
/// code segment, and a kernel data segment.
use core::mem::size_of;

/// GDT entry (8 bytes).
#[repr(C, packed)]
#[derive(Clone, Copy)]
struct GdtEntry(u64);

impl GdtEntry {
    const fn null() -> Self {
        Self(0)
    }

    /// Kernel code segment: L=1 (long mode), DPL=0, type=execute/read.
    const fn kernel_code() -> Self {
        // Access byte: Present=1, DPL=00, S=1, Type=1010 (exec/read)
        // = 0b1001_1010 = 0x9A
        // Flags: G=0, L=1 (long mode), D=0, AVL=0 = 0b0010 = 0x2
        // For long mode: limit and base are ignored, but flags[5] (L) = 1
        //
        // Encoding: base=0, limit=0, access=0x9A, flags=0x20
        // Byte layout of 8-byte GDT entry:
        //   [0-1] limit low     = 0x0000
        //   [2-3] base low      = 0x0000
        //   [4]   base mid      = 0x00
        //   [5]   access        = 0x9A
        //   [6]   flags:limit_hi= 0x20
        //   [7]   base high     = 0x00
        Self(0x00_20_9A_00_0000_0000)
    }

    /// Kernel data segment: DPL=0, type=read/write.
    const fn kernel_data() -> Self {
        // Access byte: Present=1, DPL=00, S=1, Type=0010 (read/write)
        // = 0b1001_0010 = 0x92
        // Flags: G=0, L=0, D=0, AVL=0 = 0x00
        Self(0x00_00_92_00_0000_0000)
    }
}

/// Our GDT: null + kernel code + kernel data.
#[repr(C, align(16))]
struct Gdt {
    entries: [GdtEntry; 3],
}

/// GDTR pointer for lgdt instruction.
#[repr(C, packed)]
struct GdtPointer {
    limit: u16,
    base: u64,
}

static GDT: Gdt = Gdt {
    entries: [
        GdtEntry::null(),        // 0x00: null
        GdtEntry::kernel_code(), // 0x08: kernel CS
        GdtEntry::kernel_data(), // 0x10: kernel DS
    ],
};

/// Kernel code segment selector.
pub const KERNEL_CS: u16 = 0x08;
/// Kernel data segment selector.
pub const KERNEL_DS: u16 = 0x10;

/// Load the GDT and reload segment registers.
///
/// # Safety
/// Must be called exactly once, early in boot, before loading the IDT.
pub unsafe fn init() {
    let ptr = GdtPointer {
        limit: (size_of::<Gdt>() - 1) as u16,
        base: &GDT as *const _ as u64,
    };

    core::arch::asm!(
        // Load GDTR
        "lgdt [{ptr}]",

        // Reload CS via a far return: push new CS, push return address, retfq
        "push {cs}",
        "lea {tmp}, [rip + 2f]",
        "push {tmp}",
        "retfq",
        "2:",

        // Reload data segment registers
        "mov ds, {ds:x}",
        "mov es, {ds:x}",
        "mov fs, {ds:x}",
        "mov gs, {ds:x}",
        "mov ss, {ds:x}",

        ptr = in(reg) &ptr,
        cs = in(reg) KERNEL_CS as u64,
        ds = in(reg) KERNEL_DS as u16,
        tmp = lateout(reg) _,
        options(preserves_flags),
    );
}
