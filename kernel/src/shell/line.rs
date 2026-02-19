/// Line editor for the serial console.
///
/// Supports:
/// - Printable ASCII input
/// - Backspace / DEL (0x08 / 0x7F) — delete character before cursor
/// - Enter (0x0D) — submit line
/// - Ctrl-C (0x03) — cancel current line
/// - Ctrl-U (0x15) — clear line
/// - Ctrl-L (0x0C) — redraw line
use crate::arch::x86_64::serial::SERIAL;

const MAX_LINE: usize = 256;

pub struct LineEditor {
    buf: [u8; MAX_LINE],
    len: usize,
}

impl LineEditor {
    pub fn new() -> Self {
        Self {
            buf: [0u8; MAX_LINE],
            len: 0,
        }
    }

    /// Read a line from serial input. Returns the line content on Enter,
    /// or None on Ctrl-C.
    pub fn read_line(&mut self) -> Option<&str> {
        self.len = 0;

        loop {
            let byte = SERIAL.lock().read_byte();

            match byte {
                // Enter (CR)
                b'\r' | b'\n' => {
                    // Echo newline
                    let serial = SERIAL.lock();
                    serial.write_byte(b'\r');
                    serial.write_byte(b'\n');
                    drop(serial);

                    // Return the line as a str
                    let s = core::str::from_utf8(&self.buf[..self.len]).unwrap_or("");
                    return Some(s);
                }

                // Ctrl-C — cancel
                0x03 => {
                    let serial = SERIAL.lock();
                    serial.write_byte(b'^');
                    serial.write_byte(b'C');
                    serial.write_byte(b'\r');
                    serial.write_byte(b'\n');
                    drop(serial);

                    self.len = 0;
                    return None;
                }

                // Ctrl-U — clear line
                0x15 => {
                    self.erase_line();
                    self.len = 0;
                }

                // Ctrl-L — redraw
                0x0C => {
                    self.erase_line();
                    self.redraw();
                }

                // Backspace or DEL
                0x08 | 0x7F => {
                    if self.len > 0 {
                        self.len -= 1;
                        // Erase character on terminal: backspace, space, backspace
                        let serial = SERIAL.lock();
                        serial.write_byte(0x08);
                        serial.write_byte(b' ');
                        serial.write_byte(0x08);
                    }
                }

                // Escape sequences (arrow keys etc.) — consume and ignore
                0x1B => {
                    // Read the rest of the escape sequence without holding the lock
                    // across blocking reads (which would deadlock).
                    let maybe_bracket = SERIAL.lock().try_read_byte();
                    if let Some(b'[') = maybe_bracket {
                        // CSI sequence — read until a letter or ~ (max 8 bytes to prevent hang)
                        for _ in 0..8 {
                            let c = SERIAL.lock().read_byte();
                            if c.is_ascii_alphabetic() || c == b'~' {
                                break;
                            }
                        }
                    }
                }

                // Printable ASCII
                0x20..=0x7E => {
                    if self.len < MAX_LINE - 1 {
                        self.buf[self.len] = byte;
                        self.len += 1;
                        // Echo the character
                        SERIAL.lock().write_byte(byte);
                    }
                }

                // Ignore everything else (control chars, high bytes)
                _ => {}
            }
        }
    }

    /// Erase the current line on the terminal.
    fn erase_line(&self) {
        let serial = SERIAL.lock();
        // Move cursor back to start of input
        for _ in 0..self.len {
            serial.write_byte(0x08);
        }
        // Overwrite with spaces
        for _ in 0..self.len {
            serial.write_byte(b' ');
        }
        // Move cursor back again
        for _ in 0..self.len {
            serial.write_byte(0x08);
        }
    }

    /// Redraw the current line content.
    fn redraw(&self) {
        let serial = SERIAL.lock();
        for i in 0..self.len {
            serial.write_byte(self.buf[i]);
        }
    }
}
