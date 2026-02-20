/// Global Descriptor Table (GDT) with Task State Segment (TSS).
///
/// Long mode requires a valid GDT with at least null, kernel CS, and
/// kernel DS descriptors. We also include a TSS with IST entries so
/// the double-fault handler can use a dedicated stack, preventing
/// triple faults on stack overflow.
use core::cell::UnsafeCell;
use core::mem::size_of;
use core::sync::atomic::{AtomicU64, Ordering};

/// IST1 stack top — set during guard page setup, used by double fault handler.
pub static IST1_STACK_TOP: AtomicU64 = AtomicU64::new(0);

/// Guard page virtual address — used by page fault handler to detect overflow.
pub static GUARD_PAGE_ADDR: AtomicU64 = AtomicU64::new(0);

/// Kernel stack top — exported for page fault diagnostics.
pub static KERNEL_STACK_TOP: AtomicU64 = AtomicU64::new(0);

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

/// Task State Segment for long mode.
/// We only use the IST entries for alternate stacks on exceptions.
#[repr(C, packed)]
struct Tss {
    _reserved0: u32,
    rsp0: u64,
    rsp1: u64,
    rsp2: u64,
    _reserved1: u64,
    ist1: u64,
    ist2: u64,
    ist3: u64,
    ist4: u64,
    ist5: u64,
    ist6: u64,
    ist7: u64,
    _reserved2: u64,
    _reserved3: u16,
    iopb: u16,
}

static_assertions::const_assert_eq!(size_of::<Tss>(), 104);

/// Wrapper for single-core init-once statics that replaces `static mut`.
/// Safety: only written during single-threaded boot, read-only afterwards.
#[repr(transparent)]
struct SyncUnsafeCell<T>(UnsafeCell<T>);
unsafe impl<T> Sync for SyncUnsafeCell<T> {}

impl<T> SyncUnsafeCell<T> {
    const fn new(val: T) -> Self {
        Self(UnsafeCell::new(val))
    }
    /// # Safety
    /// Caller must ensure no concurrent access (single-threaded boot).
    unsafe fn get_mut(&self) -> &mut T {
        &mut *self.0.get()
    }
    fn as_ptr(&self) -> *const T {
        self.0.get()
    }
}

/// Static TSS — zeroed initially, IST1 set during boot.
static TSS: SyncUnsafeCell<Tss> = SyncUnsafeCell::new(Tss {
    _reserved0: 0,
    rsp0: 0,
    rsp1: 0,
    rsp2: 0,
    _reserved1: 0,
    ist1: 0,
    ist2: 0,
    ist3: 0,
    ist4: 0,
    ist5: 0,
    ist6: 0,
    ist7: 0,
    _reserved2: 0,
    _reserved3: 0,
    iopb: size_of::<Tss>() as u16, // No I/O permission bitmap
});

/// GDT layout: null + kernel code + kernel data + TSS (16 bytes = 2 entries)
/// TSS descriptor in long mode is 16 bytes, occupying entries 3 and 4.
#[repr(C, align(16))]
struct Gdt {
    entries: [GdtEntry; 5],
}

/// GDTR pointer for lgdt instruction.
#[repr(C, packed)]
struct GdtPointer {
    limit: u16,
    base: u64,
}

/// Static GDT — TSS entries filled dynamically during init.
static GDT: SyncUnsafeCell<Gdt> = SyncUnsafeCell::new(Gdt {
    entries: [
        GdtEntry::null(),        // 0x00: null
        GdtEntry::kernel_code(), // 0x08: kernel CS
        GdtEntry::kernel_data(), // 0x10: kernel DS
        GdtEntry::null(),        // 0x18: TSS low (set in init)
        GdtEntry::null(),        // 0x20: TSS high (set in init)
    ],
});

/// Kernel code segment selector.
pub const KERNEL_CS: u16 = 0x08;
/// Kernel data segment selector.
pub const KERNEL_DS: u16 = 0x10;
/// TSS segment selector.
pub const TSS_SEL: u16 = 0x18;

/// Build the two 64-bit words for a TSS descriptor in long mode.
///
/// A system descriptor (TSS) in long mode is 16 bytes (128 bits):
///   [low word]  standard 8-byte descriptor encoding
///   [high word] upper 32 bits of base address + reserved
fn tss_descriptor(base: u64, limit: u32) -> (u64, u64) {
    let base_low = (base & 0xFFFF) as u64;
    let base_mid = ((base >> 16) & 0xFF) as u64;
    let base_high_byte = ((base >> 24) & 0xFF) as u64;
    let base_upper = ((base >> 32) & 0xFFFF_FFFF) as u64;

    let limit_low = (limit & 0xFFFF) as u64;
    let limit_high = ((limit >> 16) & 0xF) as u64;

    // Type = 0x9 (available 64-bit TSS), DPL=0, Present=1
    let access: u64 = 0x89;

    let low = limit_low
        | (base_low << 16)
        | (base_mid << 32)
        | (access << 40)
        | (limit_high << 48)
        | (base_high_byte << 56);

    let high = base_upper;

    (low, high)
}

/// Load the GDT (with TSS) and reload segment registers.
///
/// # Safety
/// Must be called exactly once, early in boot, before loading the IDT.
pub unsafe fn init() {
    let tss_addr = TSS.as_ptr() as u64;
    let tss_limit = (size_of::<Tss>() - 1) as u32;
    let (tss_low, tss_high) = tss_descriptor(tss_addr, tss_limit);

    // Fill TSS descriptor entries in the GDT
    let gdt = GDT.get_mut();
    gdt.entries[3] = GdtEntry(tss_low);
    gdt.entries[4] = GdtEntry(tss_high);

    let ptr = GdtPointer {
        limit: (size_of::<Gdt>() - 1) as u16,
        base: GDT.as_ptr() as u64,
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

    // Load the TSS
    core::arch::asm!(
        "ltr {sel:x}",
        sel = in(reg) TSS_SEL,
        options(nostack, preserves_flags),
    );
}

/// Set up the IST1 stack for the double-fault handler.
///
/// # Safety
/// Must be called after the physical allocator is initialized and before
/// any code that could cause a double fault.
pub unsafe fn set_ist1(stack_top: u64) {
    TSS.get_mut().ist1 = stack_top;
    IST1_STACK_TOP.store(stack_top, Ordering::Release);
}
