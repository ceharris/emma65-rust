//! Bidirectional transport over a pair of OS pipes with non-blocking IO.
//!
//! Holds two pipes: one for inbound bytes (remote writes, we read) and one for
//! outbound bytes (we write, remote reads). Both ends are set nonblocking so
//! `try_recv` and `send` never block the CPU thread.
//!
//! This is an internal transport used to connect devices to in-process pipe
//! pairs (e.g. the default console attached to the emulator's own stdin/stdout,
//! or test harnesses that need synchronous byte-level access). For connecting
//! a device to an external child process, see [`PipeTransport`].

use std::fs::File;
use std::io::{self, Read, Write};
use std::os::unix::io::{FromRawFd, OwnedFd, RawFd};

use super::{Transport, TransportError, TransportEvent};

/// Bidirectional transport over a pair of OS pipes with non-blocking IO.
pub struct InternalPipeTransport {
    rx: File,
    tx: File,
    connected: bool,
    // Set on construction; consumed (once) by the first `try_recv_tagged` call.
    connect_event_pending: bool,
    // Set when `connected` transitions true -> false; consumed (once) by
    // `try_recv_tagged` after all buffered data has been drained.
    disconnect_event_pending: bool,
}

impl InternalPipeTransport {
    /// # Safety
    /// The caller must ensure the file descriptors are valid and exclusively owned.
    pub unsafe fn from_raw_fds(rx_fd: RawFd, tx_fd: RawFd) -> io::Result<Self> {
        let (rx, tx) = unsafe {
            let rx_owned = OwnedFd::from_raw_fd(rx_fd);
            let tx_owned = OwnedFd::from_raw_fd(tx_fd);
            (File::from(rx_owned), File::from(tx_owned))
        };
        set_nonblocking(&rx)?;
        set_nonblocking(&tx)?;
        Ok(Self { rx, tx, connected: true, connect_event_pending: true, disconnect_event_pending: false })
    }

    /// Creates a transport connected to this process's stdin (fd 0) and stdout (fd 1).
    pub fn stdio() -> io::Result<Self> {
        let rx_fd = dup_fd(0)?;
        let tx_fd = dup_fd(1)?;
        // SAFETY: dup_fd returns a fresh, exclusively owned descriptor each call.
        unsafe { Self::from_raw_fds(rx_fd, tx_fd) }
    }

    /// Splits this transport into its raw rx and tx file handles.
    pub fn into_split(self) -> (File, File) {
        (self.rx, self.tx)
    }

    /// Creates a matched local/remote pair of transports connected by cross-wired
    /// OS pipes. Useful for in-process testing.
    pub fn pair() -> io::Result<(Self, Self)> {
        let (a_rx, a_tx) = os_pipe()?;
        let (b_rx, b_tx) = os_pipe()?;
        let local = Self { rx: a_rx, tx: b_tx, connected: true, connect_event_pending: true, disconnect_event_pending: false };
        let remote = Self { rx: b_rx, tx: a_tx, connected: true, connect_event_pending: true, disconnect_event_pending: false };
        set_nonblocking(&local.rx)?;
        set_nonblocking(&local.tx)?;
        set_nonblocking(&remote.rx)?;
        set_nonblocking(&remote.tx)?;
        Ok((local, remote))
    }

    /// Marks this transport disconnected, queuing a `Disconnected` event to
    /// be delivered on the next `try_recv_tagged` call — unless it's already
    /// disconnected, in which case there's nothing new to report.
    fn mark_disconnected(&mut self) {
        if self.connected {
            self.connected = false;
            self.disconnect_event_pending = true;
        }
    }
}

impl Transport for InternalPipeTransport {
    fn try_recv(&mut self) -> Option<u8> {
        if !self.connected {
            return None;
        }
        let mut buf = [0u8; 1];
        match self.rx.read(&mut buf) {
            Ok(1) => Some(buf[0]),
            Ok(_) => {
                self.mark_disconnected();
                None
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => None,
            Err(_) => {
                self.mark_disconnected();
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
                self.mark_disconnected();
                Err(TransportError::Disconnected)
            }
            Err(e) => {
                self.mark_disconnected();
                Err(TransportError::Io(e))
            }
        }
    }

    fn is_connected(&self) -> bool {
        self.connected
    }

    fn try_recv_tagged(&mut self) -> Option<TransportEvent> {
        if self.connect_event_pending {
            self.connect_event_pending = false;
            return Some(TransportEvent::Connected(0));
        }
        if let Some(byte) = self.try_recv() {
            return Some(TransportEvent::Data(0, byte));
        }
        if self.disconnect_event_pending {
            self.disconnect_event_pending = false;
            return Some(TransportEvent::Disconnected(0));
        }
        None
    }

    fn shutdown(&mut self) {
        self.mark_disconnected();
    }
}

fn os_pipe() -> io::Result<(File, File)> {
    use std::os::unix::io::FromRawFd;
    let mut fds = [0i32; 2];
    let rc = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    let read_end = unsafe { File::from_raw_fd(fds[0]) };
    let write_end = unsafe { File::from_raw_fd(fds[1]) };
    Ok((read_end, write_end))
}

fn dup_fd(fd: RawFd) -> io::Result<RawFd> {
    let rc = unsafe { libc::dup(fd) };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(rc)
}

fn set_nonblocking(file: &File) -> io::Result<()> {
    use std::os::unix::io::AsRawFd;
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
        let (mut local, mut remote) = InternalPipeTransport::pair().unwrap();
        local.send(0x42).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1));
        assert_eq!(remote.try_recv(), Some(0x42));
    }

    #[test]
    fn try_recv_returns_none_when_empty() {
        let (_, mut remote) = InternalPipeTransport::pair().unwrap();
        assert_eq!(remote.try_recv(), None);
    }

    #[test]
    fn is_connected_initially_true() {
        let (local, _remote) = InternalPipeTransport::pair().unwrap();
        assert!(local.is_connected());
    }

    #[test]
    fn shutdown_marks_disconnected() {
        let (mut local, _remote) = InternalPipeTransport::pair().unwrap();
        local.shutdown();
        assert!(!local.is_connected());
    }

    #[test]
    fn try_recv_tagged_emits_connected_before_data() {
        let (mut local, mut remote) = InternalPipeTransport::pair().unwrap();

        assert_eq!(local.try_recv_tagged(), Some(TransportEvent::Connected(0)));

        remote.send(0x7A).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1));
        assert_eq!(local.try_recv_tagged(), Some(TransportEvent::Data(0, 0x7A)));
        assert_eq!(local.try_recv_tagged(), None);
    }

    #[test]
    fn try_recv_tagged_emits_disconnected_once() {
        let (mut local, _remote) = InternalPipeTransport::pair().unwrap();
        let _ = local.try_recv_tagged(); // consume Connected
        local.shutdown();

        assert_eq!(local.try_recv_tagged(), Some(TransportEvent::Disconnected(0)));
        assert_eq!(local.try_recv_tagged(), None);
    }
}
