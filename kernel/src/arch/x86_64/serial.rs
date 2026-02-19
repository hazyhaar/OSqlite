/// Serial port driver (COM1, 0x3F8) — bidirectional.
///
/// Output: debug logging via serial_println!
/// Input: interactive shell via read_byte / try_read_byte
use core::fmt;
use spin::Mutex;

const COM1: u16 = 0x3F8;

pub static SERIAL: Mutex<Serial> = Mutex::new(Serial::new(COM1));

pub struct Serial {
    port: u16,
}

impl Serial {
    pub const fn new(port: u16) -> Self {
        Self { port }
    }

    /// Initialize the serial port (8N1, 115200 baud).
    pub fn init(&self) {
        super::outb(self.port + 1, 0x00); // Disable interrupts
        super::outb(self.port + 3, 0x80); // Enable DLAB (set baud rate divisor)
        super::outb(self.port + 0, 0x01); // 115200 baud (divisor 1, low byte)
        super::outb(self.port + 1, 0x00); // (divisor 1, high byte)
        super::outb(self.port + 3, 0x03); // 8 bits, no parity, one stop bit
        super::outb(self.port + 2, 0xC7); // Enable FIFO, clear, 14-byte threshold
        super::outb(self.port + 4, 0x0B); // IRQs enabled, RTS/DSR set
    }

    // ---- Output ----

    /// Check if the transmit buffer is empty.
    fn is_transmit_empty(&self) -> bool {
        super::inb(self.port + 5) & 0x20 != 0
    }

    /// Write a single byte, waiting for the transmit buffer.
    pub fn write_byte(&self, byte: u8) {
        while !self.is_transmit_empty() {
            core::hint::spin_loop();
        }
        super::outb(self.port, byte);
    }

    /// Write a string.
    pub fn write_str_raw(&self, s: &str) {
        for byte in s.bytes() {
            if byte == b'\n' {
                self.write_byte(b'\r');
            }
            self.write_byte(byte);
        }
    }

    // ---- Input ----

    /// Check if a byte is available to read (LSR bit 0 = Data Ready).
    pub fn has_data(&self) -> bool {
        super::inb(self.port + 5) & 0x01 != 0
    }

    /// Read a byte, blocking until one is available.
    pub fn read_byte(&self) -> u8 {
        while !self.has_data() {
            core::hint::spin_loop();
        }
        super::inb(self.port)
    }

    /// Try to read a byte without blocking. Returns None if no data available.
    pub fn try_read_byte(&self) -> Option<u8> {
        if self.has_data() {
            Some(super::inb(self.port))
        } else {
            None
        }
    }
}

impl fmt::Write for Serial {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.write_str_raw(s);
        Ok(())
    }
}

/// Print to serial console.
#[macro_export]
macro_rules! serial_print {
    ($($arg:tt)*) => {
        {
            use core::fmt::Write;
            let mut serial = $crate::arch::x86_64::serial::SERIAL.lock();
            let _ = write!(serial, $($arg)*);
        }
    };
}

/// Print to serial console with a newline.
#[macro_export]
macro_rules! serial_println {
    () => ($crate::serial_print!("\n"));
    ($($arg:tt)*) => {
        $crate::serial_print!("{}\n", format_args!($($arg)*))
    };
}
