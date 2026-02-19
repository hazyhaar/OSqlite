/// 8259 PIC (Programmable Interrupt Controller) — remap and mask.
///
/// The legacy PIC maps IRQ 0-7 to interrupts 8-15, which collides with
/// CPU exceptions. We remap IRQs to 32-47, then mask all of them since
/// we don't use hardware IRQs yet (NVMe uses polling, not MSI-X).

const PIC1_CMD: u16 = 0x20;
const PIC1_DATA: u16 = 0x21;
const PIC2_CMD: u16 = 0xA0;
const PIC2_DATA: u16 = 0xA1;

const ICW1_INIT: u8 = 0x11; // initialization + ICW4 needed
const ICW4_8086: u8 = 0x01; // 8086 mode

const EOI: u8 = 0x20;

/// Remap the PIC so IRQs don't collide with CPU exceptions,
/// then mask all IRQs.
///
/// # Safety
/// Must be called during early boot.
pub unsafe fn init() {
    use super::{outb, inb};

    // Save masks
    let mask1 = inb(PIC1_DATA);
    let mask2 = inb(PIC2_DATA);

    // ICW1: start initialization sequence
    outb(PIC1_CMD, ICW1_INIT);
    io_wait();
    outb(PIC2_CMD, ICW1_INIT);
    io_wait();

    // ICW2: vector offsets
    outb(PIC1_DATA, 32); // IRQ 0-7  → INT 32-39
    io_wait();
    outb(PIC2_DATA, 40); // IRQ 8-15 → INT 40-47
    io_wait();

    // ICW3: tell PICs about each other
    outb(PIC1_DATA, 4); // slave on IRQ2
    io_wait();
    outb(PIC2_DATA, 2); // cascade identity
    io_wait();

    // ICW4: 8086 mode
    outb(PIC1_DATA, ICW4_8086);
    io_wait();
    outb(PIC2_DATA, ICW4_8086);
    io_wait();

    // Mask ALL IRQs (we use polling, not interrupts, for now)
    outb(PIC1_DATA, 0xFF);
    outb(PIC2_DATA, 0xFF);

    let _ = (mask1, mask2); // original masks preserved if needed later
}

/// Send End-of-Interrupt to both PICs.
pub fn send_eoi_both() {
    super::outb(PIC2_CMD, EOI);
    super::outb(PIC1_CMD, EOI);
}

/// Small I/O delay for PIC initialization.
fn io_wait() {
    // Writing to port 0x80 is a common way to add a small delay
    super::outb(0x80, 0);
}
