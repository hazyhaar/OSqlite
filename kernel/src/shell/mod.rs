/// HeavenOS interactive shell over serial console.
///
/// This is a minimal kernel shell for debugging and system interaction.
/// It reads from COM1, provides line editing (backspace, Ctrl-C, Ctrl-U),
/// and dispatches commands to built-in handlers.
///
/// Think of it as a Plan 9 `rc` that speaks to the kernel directly —
/// not a POSIX shell, not bash. Commands map to Styx namespace operations.
mod line;
mod commands;

use crate::{serial_print, serial_println};

use line::LineEditor;
use commands::dispatch;

const PROMPT: &str = "heaven% ";

/// Run the interactive shell. This function never returns.
pub fn run() -> ! {
    serial_println!();
    serial_println!("HeavenOS shell ready. Type 'help' for commands.");

    let mut editor = LineEditor::new();

    loop {
        serial_print!("{}", PROMPT);
        match editor.read_line() {
            Some(line) => {
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    dispatch(trimmed);
                }
            }
            None => {
                // Ctrl-C or similar — just print a new prompt
                serial_println!();
            }
        }
    }
}
