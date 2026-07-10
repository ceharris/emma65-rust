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
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crossbeam_channel::{Receiver, Sender};
use nix::fcntl::{fcntl, FcntlArg, OFlag};
use nix::pty::{openpty, OpenptyResult};
use nix::unistd;
use tokio::io::unix::AsyncFd;
use tokio::sync::oneshot;

use super::{ChannelBridge, Transport, TransportError};

/// Transport over a pseudo-terminal (PTY).
pub struct PtyTransport {
    bridge: ChannelBridge,
    // Path to the slave side of the PTY (e.g. `/dev/pts/N`).
    slave_path: Option<String>,
    // Keeps the slave fd open so the devpts node remains valid for the transport's lifetime.
    _slave: OwnedFd,
    // Reflects whether an external process is currently connected; shared with the Tokio task.
    client_connected: Arc<AtomicBool>,
    // Incremented each time an external process opens the slave; shared with the Tokio task.
    connection_counter: Arc<AtomicU64>,
    // Optional stable symlink pointing to the slave device node.
    symlink_path: Option<PathBuf>,
}

impl PtyTransport {
    /// Opens a new PTY pair and starts the Tokio IO task on the master side.
    ///
    /// If `symlink_path` is `Some(path)`, a symlink is created at `path` pointing to
    /// the slave device node, giving external programs a predictable port name.
    /// If a symlink already exists at `path`, it is removed before creating the new one.
    /// The symlink is removed when the transport is shut down or dropped.
    pub fn open(symlink_path: Option<&Path>) -> std::io::Result<Self> {
        let OpenptyResult { master, slave } = openpty(None, None)
            .map_err(|e| std::io::Error::from_raw_os_error(e as i32))?;

        // SAFETY: slave is valid for the duration of this call.
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

        // Set master non-blocking for AsyncFd/epoll integration.
        let raw = master.as_raw_fd();
        let flags = fcntl(raw, FcntlArg::F_GETFL)
            .map_err(|e| std::io::Error::from_raw_os_error(e as i32))?;
        let new_flags = OFlag::from_bits_truncate(flags) | OFlag::O_NONBLOCK;
        fcntl(raw, FcntlArg::F_SETFL(new_flags))
            .map_err(|e| std::io::Error::from_raw_os_error(e as i32))?;

        let (bridge, in_tx, out_rx, shutdown_rx) = ChannelBridge::new();
        let client_connected = Arc::new(AtomicBool::new(false));
        let connection_counter = Arc::new(AtomicU64::new(0));

        // SAFETY: we own master; forget prevents double-close.
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

        Ok(Self { bridge, slave_path, _slave: slave, client_connected, connection_counter, symlink_path })
    }

    /// Returns the path of the slave PTY device (e.g. `/dev/pts/3`), if available.
    pub fn slave_path(&self) -> Option<&str> {
        self.slave_path.as_deref()
    }
}

impl Transport for PtyTransport {
    fn try_recv(&mut self) -> Option<u8> { self.bridge.try_recv() }

    /// Sends a byte. Returns `Ok(())` silently if no external process has the slave open.
    fn send(&mut self, byte: u8) -> Result<(), TransportError> {
        if !self.client_connected.load(Ordering::Acquire) {
            return Ok(());
        }
        self.bridge.send(byte)
    }

    fn is_connected(&self) -> bool {
        self.client_connected.load(Ordering::Acquire)
    }

    fn connection_id(&self) -> u64 {
        self.connection_counter.load(Ordering::Acquire)
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
/// Sets `client_connected` to `true` on the first successful read (an external
/// process opened the slave), and back to `false` on EIO (all external openers
/// have closed). Outbound bytes are dropped silently while disconnected.
async fn run_pty_task(
    async_fd: AsyncFd<File>,
    in_tx: Sender<u8>,
    out_rx: Receiver<u8>,
    mut shutdown_rx: oneshot::Receiver<()>,
    client_connected: Arc<AtomicBool>,
    connection_counter: Arc<AtomicU64>,
) {
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
                        if in_tx.send(buf[0]).is_err() {
                            break;
                        }
                    }
                    Ok(Ok(_)) => {}
                    Ok(Err(e)) if e.raw_os_error() == Some(nix::libc::EIO) => {
                        // All external slave openers closed; drain stale outbound bytes.
                        while out_rx.try_recv().is_ok() {}
                        client_connected.store(false, Ordering::Release);
                    }
                    Ok(Err(_)) => break,
                    Err(_) => { guard.clear_ready(); } // WouldBlock — wait for next epoll event
                }
            }

            _ = drain_outbound(async_fd.get_ref(), &out_rx), if client_connected.load(Ordering::Acquire) => {}
        }
    }
    client_connected.store(false, Ordering::Release);
}

/// Flushes all pending outbound bytes from `out_rx` into the PTY master fd.
async fn drain_outbound(mut file: &File, out_rx: &Receiver<u8>) {
    while let Ok(byte) = out_rx.try_recv() {
        if file.write_all(&[byte]).is_err() {
            return;
        }
    }
    tokio::task::yield_now().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn open_send_recv() {
        let mut transport = PtyTransport::open(None).unwrap();
        assert!(transport.slave_path().is_some());

        // Write directly to the slave side to simulate external input.
        let slave_path = transport.slave_path().unwrap().to_owned();
        let slave_file = std::fs::OpenOptions::new()
            .write(true)
            .open(&slave_path)
            .unwrap();
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
    async fn send_while_no_client() {
        let mut transport = PtyTransport::open(None).unwrap();

        // No external process connected; send should succeed silently.
        assert!(!transport.is_connected());
        assert!(transport.send(0xFF).is_ok());
    }

    #[tokio::test]
    async fn is_connected_reflects_client_state() {
        let transport = PtyTransport::open(None).unwrap();
        let slave_path = transport.slave_path().unwrap().to_owned();

        assert!(!transport.is_connected());

        // Open slave and write a byte to trigger client_connected = true.
        let slave = std::fs::OpenOptions::new()
            .write(true)
            .open(&slave_path)
            .unwrap();
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
        assert!(link_path.exists(), "symlink should exist after open");

        drop(transport);
        assert!(!link_path.exists(), "symlink should be removed after drop");
    }
}

/// Returns the slave PTY device name, or `None` on error.
fn tty_name(fd: BorrowedFd<'_>) -> Option<String> {
    match unistd::ttyname(fd) {
        Ok(path) => path.to_str().map(String::from),
        Err(_) => None,
    }
}