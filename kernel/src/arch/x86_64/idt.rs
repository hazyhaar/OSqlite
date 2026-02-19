/// Interrupt Descriptor Table (IDT) with exception handlers.
///
/// Handles critical CPU exceptions so the kernel doesn't triple-fault:
/// - #DE (0)  Division by zero
/// - #DB (1)  Debug
/// - #NMI (2) Non-maskable interrupt
/// - #BP (3)  Breakpoint
/// - #OF (4)  Overflow
/// - #BR (5)  Bound range exceeded
/// - #UD (6)  Invalid opcode
/// - #NM (7)  Device not available
/// - #DF (8)  Double fault (uses IST1 if available)
/// - #GP (13) General protection fault
/// - #PF (14) Page fault
use super::gdt;

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
            type_attr: 0, // NOT present
            offset_mid: 0,
            offset_high: 0,
            _reserved: 0,
        }
    }

    /// Create a present interrupt gate entry (DPL=0).
    pub fn interrupt_gate(handler: u64) -> Self {
        Self {
            offset_low: handler as u16,
            selector: gdt::KERNEL_CS,
            ist: 0,
            type_attr: 0x8E, // present | interrupt gate | DPL=0
            offset_mid: (handler >> 16) as u16,
            offset_high: (handler >> 32) as u32,
            _reserved: 0,
        }
    }

    /// Create a trap gate entry (DPL=0, interrupts stay enabled).
    pub fn trap_gate(handler: u64) -> Self {
        Self {
            offset_low: handler as u16,
            selector: gdt::KERNEL_CS,
            ist: 0,
            type_attr: 0x8F, // present | trap gate | DPL=0
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

    /// Load this IDT into the CPU via LIDT.
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

static mut IDT: Idt = Idt::new();

/// Initialize the IDT with exception handlers and load it.
///
/// # Safety
/// Must be called after GDT init. Called once during boot.
pub unsafe fn init() {
    let idt = &raw mut IDT;

    // CPU exceptions
    (*idt).entries[0]  = IdtEntry::interrupt_gate(isr_de as *const () as u64);
    (*idt).entries[1]  = IdtEntry::interrupt_gate(isr_db as *const () as u64);
    (*idt).entries[2]  = IdtEntry::interrupt_gate(isr_nmi as *const () as u64);
    (*idt).entries[3]  = IdtEntry::trap_gate(isr_bp as *const () as u64);
    (*idt).entries[4]  = IdtEntry::interrupt_gate(isr_of as *const () as u64);
    (*idt).entries[5]  = IdtEntry::interrupt_gate(isr_br as *const () as u64);
    (*idt).entries[6]  = IdtEntry::interrupt_gate(isr_ud as *const () as u64);
    (*idt).entries[7]  = IdtEntry::interrupt_gate(isr_nm as *const () as u64);
    (*idt).entries[8]  = IdtEntry::interrupt_gate(isr_df as *const () as u64);
    (*idt).entries[13] = IdtEntry::interrupt_gate(isr_gp as *const () as u64);
    (*idt).entries[14] = IdtEntry::interrupt_gate(isr_pf as *const () as u64);

    // PIC IRQs (remapped to 32-47) — spurious handler for all
    for i in 32..48 {
        (*idt).entries[i] = IdtEntry::interrupt_gate(isr_irq_stub as *const () as u64);
    }

    // Safety: IDT is initialized above and lives for 'static lifetime
    (*idt).load();
}

// ---- Exception frame passed by the CPU on interrupt ----

/// Interrupt stack frame pushed by the CPU before our handler runs.
#[repr(C)]
pub struct InterruptFrame {
    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub ss: u64,
}

// ---- Exception handlers (naked asm stubs that call Rust functions) ----

// For exceptions WITHOUT error code: CPU pushes RIP, CS, RFLAGS, RSP, SS.
// For exceptions WITH error code: CPU also pushes an error code before RIP.
//
// We use extern "x86-interrupt" calling convention which handles all of this.

extern "x86-interrupt" fn isr_de(frame: InterruptFrame) {
    exception_handler("Division by zero (#DE)", &frame, None);
}

extern "x86-interrupt" fn isr_db(frame: InterruptFrame) {
    exception_handler("Debug (#DB)", &frame, None);
}

extern "x86-interrupt" fn isr_nmi(frame: InterruptFrame) {
    exception_handler("Non-maskable interrupt (#NMI)", &frame, None);
}

extern "x86-interrupt" fn isr_bp(frame: InterruptFrame) {
    // Breakpoint — don't halt, just log
    crate::serial_println!("[int] Breakpoint at {:#x}", frame.rip);
}

extern "x86-interrupt" fn isr_of(frame: InterruptFrame) {
    exception_handler("Overflow (#OF)", &frame, None);
}

extern "x86-interrupt" fn isr_br(frame: InterruptFrame) {
    exception_handler("Bound range exceeded (#BR)", &frame, None);
}

extern "x86-interrupt" fn isr_ud(frame: InterruptFrame) {
    exception_handler("Invalid opcode (#UD)", &frame, None);
}

extern "x86-interrupt" fn isr_nm(frame: InterruptFrame) {
    exception_handler("Device not available (#NM)", &frame, None);
}

extern "x86-interrupt" fn isr_df(frame: InterruptFrame, error_code: u64) {
    exception_handler("Double fault (#DF)", &frame, Some(error_code));
    // Double fault is unrecoverable
    loop { crate::arch::x86_64::hlt(); }
}

extern "x86-interrupt" fn isr_gp(frame: InterruptFrame, error_code: u64) {
    exception_handler("General protection fault (#GP)", &frame, Some(error_code));
}

extern "x86-interrupt" fn isr_pf(frame: InterruptFrame, error_code: u64) {
    // Read CR2 for the faulting address
    let cr2: u64;
    unsafe { core::arch::asm!("mov {}, cr2", out(reg) cr2, options(nostack, nomem)); }
    crate::serial_println!("!!! PAGE FAULT !!!");
    crate::serial_println!("  Address: {:#x}", cr2);
    crate::serial_println!("  Error:   {:#x}", error_code);
    crate::serial_println!("  RIP:     {:#x}", frame.rip);
    crate::serial_println!("  CS:      {:#x}", frame.cs);
    crate::serial_println!("  RFLAGS:  {:#x}", frame.rflags);
    crate::serial_println!("  RSP:     {:#x}", frame.rsp);
    loop { crate::arch::x86_64::hlt(); }
}

extern "x86-interrupt" fn isr_irq_stub(_frame: InterruptFrame) {
    // Send EOI to PIC (both master and slave for safety)
    super::pic::send_eoi_both();
}

/// Common exception reporting.
fn exception_handler(name: &str, frame: &InterruptFrame, error_code: Option<u64>) {
    crate::serial_println!("!!! CPU EXCEPTION: {} !!!", name);
    if let Some(code) = error_code {
        crate::serial_println!("  Error code: {:#x}", code);
    }
    crate::serial_println!("  RIP:     {:#x}", frame.rip);
    crate::serial_println!("  CS:      {:#x}", frame.cs);
    crate::serial_println!("  RFLAGS:  {:#x}", frame.rflags);
    crate::serial_println!("  RSP:     {:#x}", frame.rsp);
    loop { crate::arch::x86_64::hlt(); }
}
