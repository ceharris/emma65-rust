//! Bidirectional transport over a pair of OS pipes with non-blocking IO.
//!
//! Holds two pipes: one for inbound bytes (remote writes, we read) and one for
//! outbound bytes (we write, remote reads). Both ends are set non-blocking so
//! `try_recv` and `send` never block the CPU thread.

use std::io::{self, Read, Write};
use std::os::unix::io::{FromRawFd, OwnedFd, RawFd};
use std::fs::File;

use super::{Transport, TransportError};

/// Bidirectional transport over a pair of OS pipes with non-blocking IO.
pub struct PipeTransport {
    // Read end of the inbound pipe (we read from this).
    rx: File,
    // Write end of the outbound pipe (we write to this).
    tx: File,
    connected: bool,
}

impl PipeTransport {
    /// Creates a `PipeTransport` from two already-opened OS file descriptors.
    ///
    /// `rx_fd` must be readable; `tx_fd` must be writable. Both are set non-blocking.
    ///
    /// # Safety
    /// The caller must ensure the file descriptors are valid and exclusively owned.
    pub unsafe fn from_raw_fds(rx_fd: std::os::unix::io::RawFd, tx_fd: std::os::unix::io::RawFd) -> io::Result<Self> {
        let (rx, tx) = unsafe {
            let rx_owned = OwnedFd::from_raw_fd(rx_fd);
            let tx_owned = OwnedFd::from_raw_fd(tx_fd);
            (File::from(rx_owned), File::from(tx_owned))
        };
        set_nonblocking(&rx)?;
        set_nonblocking(&tx)?;
        Ok(Self { rx, tx, connected: true })
    }

    /// Creates a `PipeTransport` connected to the process's own stdin and stdout.
    ///
    /// Duplicates fd 0 and fd 1 so this transport owns independent descriptors: dropping it
    /// (or its underlying files) closes only the duplicates, leaving the process's real
    /// stdin/stdout usable for anything else (e.g. `println!`).
    pub fn stdio() -> io::Result<Self> {
        let rx_fd = dup_fd(0)?;
        let tx_fd = dup_fd(1)?;
        // SAFETY: dup_fd returns a fresh, exclusively-owned descriptor each call.
        unsafe { Self::from_raw_fds(rx_fd, tx_fd) }
    }

    /// Consumes this transport and returns the underlying `(rx, tx)` files.
    ///
    /// Both files remain non-blocking. The caller takes ownership and is
    /// responsible for all further I/O. The `connected` flag is discarded.
    pub fn into_split(self) -> (File, File) {
        (self.rx, self.tx)
    }

    /// Creates a connected `PipeTransport` pair backed by two OS pipe(2) calls.
    ///
    /// Returns `(local, remote)` — both ends are `PipeTransport`s. The local end
    /// reads from one pipe and writes to the other; the remote end is the mirror.
    pub fn pair() -> io::Result<(Self, Self)> {
        let (a_rx, a_tx) = os_pipe()?;
        let (b_rx, b_tx) = os_pipe()?;
        // local reads from a, writes to b; remote reads from b, writes to a
        let local = Self { rx: a_rx, tx: b_tx, connected: true };
        let remote = Self { rx: b_rx, tx: a_tx, connected: true };
        set_nonblocking(&local.rx)?;
        set_nonblocking(&local.tx)?;
        set_nonblocking(&remote.rx)?;
        set_nonblocking(&remote.tx)?;
        Ok((local, remote))
    }
}

impl Transport for PipeTransport {
    fn try_recv(&mut self) -> Option<u8> {
        if !self.connected {
            return None;
        }
        let mut buf = [0u8; 1];
        match self.rx.read(&mut buf) {
            Ok(1) => Some(buf[0]),
            Ok(_) => {
                self.connected = false;
                None
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => None,
            Err(_) => {
                self.connected = false;
                None
            }
        }
    }

    fn send(&mut self, byte: u8) -> Result<(), TransportError> {
        if !self.connected {
            return Err(TransportError::Disconnected);
        }
        match self.tx.write_all(&[byte]) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => Err(TransportError::Full),
            Err(e) if e.kind() == io::ErrorKind::BrokenPipe => {
                self.connected = false;
                Err(TransportError::Disconnected)
            }
            Err(e) => {
                self.connected = false;
                Err(TransportError::Io(e))
            }
        }
    }

    fn is_connected(&self) -> bool {
        self.connected
    }

    fn connection_id(&self) -> u64 {
        0
    }

    fn shutdown(&mut self) {
        self.connected = false;
    }
}

fn os_pipe() -> io::Result<(File, File)> {
    use std::os::unix::io::FromRawFd;
    let mut fds = [0i32; 2];
    // SAFETY: pipe2 fills fds with valid file descriptors on success.
    let rc = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    let read_end = unsafe { File::from_raw_fd(fds[0]) };
    let write_end = unsafe { File::from_raw_fd(fds[1]) };
    Ok((read_end, write_end))
}

fn dup_fd(fd: RawFd) -> io::Result<RawFd> {
    // SAFETY: dup() with a valid fd either returns a new, exclusively-owned descriptor or -1.
    let rc = unsafe { libc::dup(fd) };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(rc)
}

fn set_nonblocking(file: &File) -> io::Result<()> {
    use std::os::unix::io::AsRawFd;
    // SAFETY: fcntl with F_GETFL/F_SETFL on a valid fd.
    unsafe {
        let flags = libc::fcntl(file.as_raw_fd(), libc::F_GETFL);
        if flags < 0 {
            return Err(io::Error::last_os_error());
        }
        let rc = libc::fcntl(file.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK);
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn send_recv_round_trip() {
        let (mut local, mut remote) = PipeTransport::pair().unwrap();
        local.send(0x42).unwrap();
        // Give the kernel a moment to move the byte across the pipe.
        std::thread::sleep(std::time::Duration::from_millis(1));
        assert_eq!(remote.try_recv(), Some(0x42));
    }

    #[test]
    fn try_recv_returns_none_when_empty() {
        let (_, mut remote) = PipeTransport::pair().unwrap();
        assert_eq!(remote.try_recv(), None);
    }

    #[test]
    fn is_connected_initially_true() {
        let (local, _remote) = PipeTransport::pair().unwrap();
        assert!(local.is_connected());
    }

    #[test]
    fn shutdown_marks_disconnected() {
        let (mut local, _remote) = PipeTransport::pair().unwrap();
        local.shutdown();
        assert!(!local.is_connected());
    }
}
