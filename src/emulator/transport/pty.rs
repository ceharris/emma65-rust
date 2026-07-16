//! Transport over a pseudo-terminal (PTY).
//!
//! Opens a PTY pair on construction. A Tokio task owns the master side fd via
//! [`AsyncFd`] for proper non-blocking epoll integration. The sync side
//! communicates via bounded `crossbeam` channels. The slave device path is
//! available for external processes to connect to. An optional stable symlink
//! may be created at a caller-supplied path so that external programs (e.g.
//! terminal emulators) can find the port by a predictable name; the symlink
//! is removed automatically when the transport is shut down or dropped.

use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::OwnedFd;
use std::os::unix::io::{AsRawFd, BorrowedFd, FromRawFd};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use crossbeam_channel::{Receiver, Sender};
use nix::fcntl::{fcntl, FcntlArg, OFlag};
use nix::pty::{openpty, OpenptyResult};
use nix::sys::termios::{self, cfmakeraw, SetArg};
use nix::unistd;
use tokio::io::unix::AsyncFd;
use tokio::sync::oneshot;

use super::{ChannelBridge, Transport, TransportError, TransportEvent};

/// Transport over a pseudo-terminal (PTY).
pub struct PtyTransport {
    bridge: ChannelBridge<TransportEvent>,
    slave_path: Option<String>,
    _slave: OwnedFd,
    // Reflects whether an external process is currently confirmed connected
    // (i.e. has actually read a byte); shared with the Tokio task. Note this
    // no longer gates `send()` — see its doc comment.
    client_connected: Arc<AtomicBool>,
    symlink_path: Option<PathBuf>,
}

impl PtyTransport {
    pub fn open(symlink_path: Option<&Path>) -> std::io::Result<Self> {
        let OpenptyResult { master, slave } = openpty(None, None)
            .map_err(|e| std::io::Error::from_raw_os_error(e as i32))?;

        // Put the slave into raw mode. Default termios is cooked (interactive
        // terminal) mode: ICANON line-buffers input until a newline, ECHO
        // mirrors master writes straight back to master reads (so the
        // emulator would see its own output as if it were peripheral input),
        // ISIG turns control bytes into signals, and OPOST rewrites output
        // bytes (e.g. \n -> \r\n). None of that is appropriate for a raw
        // binary byte-stream transport.
        let mut term = termios::tcgetattr(&slave)
            .map_err(|e| std::io::Error::from_raw_os_error(e as i32))?;
        cfmakeraw(&mut term);
        termios::tcsetattr(&slave, SetArg::TCSANOW, &term)
            .map_err(|e| std::io::Error::from_raw_os_error(e as i32))?;

        let slave_path = unsafe {
            tty_name(BorrowedFd::borrow_raw(slave.as_raw_fd()))
        };

        let symlink_path = if let (Some(link), Some(target)) = (symlink_path, &slave_path) {
            if link.symlink_metadata().map(|m| m.file_type().is_symlink()).unwrap_or(false) {
                std::fs::remove_file(link)?;
            }
            std::os::unix::fs::symlink(target, link)?;
            Some(link.to_path_buf())
        } else {
            None
        };

        let raw = master.as_raw_fd();
        let flags = fcntl(raw, FcntlArg::F_GETFL)
            .map_err(|e| std::io::Error::from_raw_os_error(e as i32))?;
        let new_flags = OFlag::from_bits_truncate(flags) | OFlag::O_NONBLOCK;
        fcntl(raw, FcntlArg::F_SETFL(new_flags))
            .map_err(|e| std::io::Error::from_raw_os_error(e as i32))?;

        let (bridge, in_tx, out_rx, shutdown_rx) = ChannelBridge::<TransportEvent>::new();
        let client_connected = Arc::new(AtomicBool::new(false));
        let connection_counter = Arc::new(AtomicU64::new(0));

        let master_std = unsafe { File::from_raw_fd(raw) };
        std::mem::forget(master);
        let async_fd = AsyncFd::new(master_std)?;

        tokio::spawn(run_pty_task(
            async_fd,
            in_tx,
            out_rx,
            shutdown_rx,
            Arc::clone(&client_connected),
            Arc::clone(&connection_counter),
        ));

        Ok(Self { bridge, slave_path, _slave: slave, client_connected, symlink_path })
    }

    pub fn slave_path(&self) -> Option<&str> {
        self.slave_path.as_deref()
    }
}

impl Transport for PtyTransport {
    fn try_recv(&mut self) -> Option<u8> {
        loop {
            match self.bridge.try_recv()? {
                TransportEvent::Data(_, byte) => return Some(byte),
                TransportEvent::Connected(_) | TransportEvent::Disconnected(_) => continue,
            }
        }
    }

    /// Sends a byte to the PTY master.
    ///
    /// Unlike the previous implementation, this does not gate on whether an
    /// external process has the slave open yet: PTY masters accept writes
    /// regardless of whether anyone has opened the slave side (the kernel
    /// buffers the output). Dropping writes here would silently discard the
    /// protocol layer's initial state dump, which is now sent proactively
    /// as soon as `Connected` fires — before we can know a real reader
    /// exists. `is_connected()` still reflects true external attachment.
    fn send(&mut self, byte: u8) -> Result<(), TransportError> {
        self.bridge.send(byte)
    }

    fn is_connected(&self) -> bool {
        self.client_connected.load(Ordering::Acquire)
    }

    fn try_recv_tagged(&mut self) -> Option<TransportEvent> {
        self.bridge.try_recv()
    }

    fn shutdown(&mut self) {
        self.bridge.shutdown();
        if let Some(ref link) = self.symlink_path.take() {
            let _ = std::fs::remove_file(link);
        }
    }
}

impl Drop for PtyTransport {
    fn drop(&mut self) {
        if let Some(ref link) = self.symlink_path {
            let _ = std::fs::remove_file(link);
        }
    }
}

/// Tokio task: bridges the async PTY master to sync crossbeam channels.
///
/// Emits `Connected(0)` once at startup (the transport is ready to accept
/// writes immediately — see `Transport::send`'s doc comment), `Data(0, _)`
/// for each byte read, and `Disconnected(0)` once when the task exits.
/// `client_connected`/`connection_counter` still track real external
/// attach/detach (via first successful read / EIO) for `is_connected()` and
/// `connection_id()`, independent of the event stream above.
async fn run_pty_task(
    async_fd: AsyncFd<File>,
    in_tx: Sender<TransportEvent>,
    out_rx: Receiver<u8>,
    mut shutdown_rx: oneshot::Receiver<()>,
    client_connected: Arc<AtomicBool>,
    connection_counter: Arc<AtomicU64>,
) {
    if in_tx.send(TransportEvent::Connected(0)).is_err() {
        return;
    }

    let mut buf = [0u8; 1];
    loop {
        tokio::select! {
            _ = &mut shutdown_rx => break,

            result = async_fd.readable() => {
                let mut guard = match result { Ok(g) => g, Err(_) => break };
                match guard.try_io(|inner| inner.get_ref().read(&mut buf)) {
                    Ok(Ok(1)) => {
                        if !client_connected.load(Ordering::Acquire) {
                            connection_counter.fetch_add(1, Ordering::Release);
                            client_connected.store(true, Ordering::Release);
                        }
                        if in_tx.send(TransportEvent::Data(0, buf[0])).is_err() {
                            break;
                        }
                    }
                    Ok(Ok(_)) => {}
                    Ok(Err(e)) if e.raw_os_error() == Some(nix::libc::EIO) => {
                        while out_rx.try_recv().is_ok() {}
                        client_connected.store(false, Ordering::Release);
                    }
                    Ok(Err(_)) => break,
                    Err(_) => { guard.clear_ready(); }
                }
            }

            _ = drain_outbound(async_fd.get_ref(), &out_rx) => {}
        }
    }
    client_connected.store(false, Ordering::Release);
    let _ = in_tx.send(TransportEvent::Disconnected(0));
}

async fn drain_outbound(mut file: &File, out_rx: &Receiver<u8>) {
    while let Ok(byte) = out_rx.try_recv() {
        if file.write_all(&[byte]).is_err() {
            return;
        }
    }
    tokio::task::yield_now().await;
}

fn tty_name(fd: BorrowedFd<'_>) -> Option<String> {
    match unistd::ttyname(fd) {
        Ok(path) => path.to_str().map(String::from),
        Err(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn open_send_recv() {
        let mut transport = PtyTransport::open(None).unwrap();
        assert!(transport.slave_path().is_some());

        let slave_path = transport.slave_path().unwrap().to_owned();
        let slave_file = std::fs::OpenOptions::new().write(true).open(&slave_path).unwrap();
        use std::io::Write;
        { let mut f = slave_file; f.write_all(&[0xBB]).unwrap(); }

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert_eq!(transport.try_recv(), Some(0xBB));
    }

    #[tokio::test]
    async fn shutdown_marks_disconnected() {
        let mut transport = PtyTransport::open(None).unwrap();
        transport.shutdown();
        assert!(!transport.is_connected());
    }

    #[tokio::test]
    async fn send_before_any_external_attach_succeeds() {
        let mut transport = PtyTransport::open(None).unwrap();

        // No external process connected yet; send must not be silently dropped.
        assert!(!transport.is_connected());
        assert!(transport.send(0xFF).is_ok());
    }

    #[tokio::test]
    async fn is_connected_reflects_client_state() {
        let transport = PtyTransport::open(None).unwrap();
        let slave_path = transport.slave_path().unwrap().to_owned();

        assert!(!transport.is_connected());

        let slave = std::fs::OpenOptions::new().write(true).open(&slave_path).unwrap();
        use std::io::Write;
        { let mut f = slave; f.write_all(&[0x01]).unwrap(); }

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(transport.is_connected());
    }

    #[tokio::test]
    async fn open_with_symlink_creates_and_removes_link() {
        let link_path = PathBuf::from("/tmp/emma65_test_pty_link");
        let _ = std::fs::remove_file(&link_path);

        let transport = PtyTransport::open(Some(&link_path)).unwrap();
        assert!(link_path.exists());

        drop(transport);
        assert!(!link_path.exists());
    }

    #[tokio::test]
    async fn try_recv_tagged_emits_connected_on_creation() {
        let mut transport = PtyTransport::open(None).unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;

        assert_eq!(transport.try_recv_tagged(), Some(TransportEvent::Connected(0)));
    }

    #[tokio::test]
    async fn initial_dump_reaches_client_that_attaches_later() {
        let mut transport = PtyTransport::open(None).unwrap();
        let slave_path = transport.slave_path().unwrap().to_owned();

        // Give the spawned task a chance to run before checking for its
        // startup event — `open()` is synchronous and returns before the
        // task has been polled even once.
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;

        // Consume the Connected event and write a "dump" before anyone
        // has opened the slave side.
        assert_eq!(transport.try_recv_tagged(), Some(TransportEvent::Connected(0)));
        transport.send(0xD0).unwrap();
        transport.send(0xD1).unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        // Now a reader attaches and should still see the buffered bytes.
        let mut slave = std::fs::OpenOptions::new().read(true).open(&slave_path).unwrap();
        use std::io::Read;
        let mut buf = [0u8; 2];
        slave.read_exact(&mut buf).unwrap();
        assert_eq!(buf, [0xD0, 0xD1]);
    }

    #[tokio::test]
    async fn send_is_not_echoed_back_as_received_data() {
        let mut transport = PtyTransport::open(None).unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        let _ = transport.try_recv_tagged(); // consume Connected

        transport.send(0xAB).unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        // In cooked mode this would incorrectly come back as received data.
        assert_eq!(transport.try_recv(), None);
    }

}
