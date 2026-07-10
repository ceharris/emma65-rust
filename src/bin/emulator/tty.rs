//! Best-effort raw-mode handling for the process's controlling terminal.
//!
//! Used only when the emulator's console is attached directly to this process's own
//! stdin/stdout (the default, no-config layout): with no external terminal program to put
//! the line discipline into raw mode, the emulator has to do it itself so that input reaches
//! the emulated console byte-by-byte instead of line-buffered and echoed by the kernel.

use std::io::{self, IsTerminal};

use nix::sys::termios::{self, SetArg, Termios};

/// Restores the terminal's original mode when dropped.
pub struct RawModeGuard {
    original: Termios,
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = termios::tcsetattr(io::stdin(), SetArg::TCSANOW, &self.original);
    }
}

/// Puts stdin into raw mode, returning a guard that restores the original mode on drop.
///
/// Does nothing (returns `None`) if stdin isn't a terminal, or if the termios calls fail;
/// in either case the console still works, just without host-terminal raw mode.
pub fn enter_raw_mode() -> Option<RawModeGuard> {
    if !io::stdin().is_terminal() {
        return None;
    }
    let original = match termios::tcgetattr(io::stdin()) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("warning: failed to read terminal mode: {e}");
            return None;
        }
    };
    let mut raw = original.clone();
    termios::cfmakeraw(&mut raw);
    if let Err(e) = termios::tcsetattr(io::stdin(), SetArg::TCSANOW, &raw) {
        eprintln!("warning: failed to set terminal to raw mode: {e}");
        return None;
    }
    Some(RawModeGuard { original })
}
