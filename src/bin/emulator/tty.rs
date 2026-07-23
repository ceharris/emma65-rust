//! Best-effort raw-mode handling for the process's controlling terminal.
//!
//! Used when the emulator's console is attached directly to this process's own stdin/stdout:
//! with no external terminal program to put the line discipline into raw mode, the emulator
//! has to do it itself so that input reaches the emulated console byte-by-byte instead of
//! line-buffered and echoed by the kernel.

use std::io::{self, IsTerminal};
use std::os::fd::AsRawFd;
use std::panic;

/// Restores the terminal's original mode when dropped.
pub struct RawModeGuard {
    original: libc::termios,
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        unsafe { libc::tcsetattr(io::stdin().as_raw_fd(), libc::TCSANOW, &self.original) };
    }
}

/// Puts stdin into raw mode, returning a guard that restores the original mode on drop.
///
/// Also installs a panic hook that restores the terminal before the panic message is printed,
/// so that the message is readable. The previous hook is chained and still runs afterward.
///
/// Does nothing (returns `None`) if stdin isn't a terminal, or if the termios calls fail;
/// in either case the console still works, just without host-terminal raw mode.
pub fn enter_raw_mode() -> Option<RawModeGuard> {
    if !io::stdin().is_terminal() {
        return None;
    }
    let original = unsafe {
        let mut t = std::mem::zeroed::<libc::termios>();
        if libc::tcgetattr(io::stdin().as_raw_fd(), &mut t) != 0 {
            eprintln!("warning: failed to read terminal mode: {}", io::Error::last_os_error());
            return None;
        }
        t
    };
    let mut raw = original;
    unsafe {
        libc::cfmakeraw(&mut raw);
        if libc::tcsetattr(io::stdin().as_raw_fd(), libc::TCSANOW, &raw) != 0 {
            eprintln!("warning: failed to set terminal to raw mode: {}", io::Error::last_os_error());
            return None;
        }
    }

    // The default panic hook prints its message before stack unwinding begins, so the terminal
    // would still be in raw mode and the output would be garbled. Chain a hook that restores
    // the terminal first. The restore is idempotent, so it is harmless if Drop runs afterward.
    let prev_hook = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        unsafe { libc::tcsetattr(io::stdin().as_raw_fd(), libc::TCSANOW, &original) };
        prev_hook(info);
    }));

    Some(RawModeGuard { original })
}
