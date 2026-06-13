use std::fs::File;
use std::os::unix::io::{AsRawFd, BorrowedFd, FromRawFd};
use std::path::{Path, PathBuf};

use crossbeam_channel::{Receiver, Sender};
use nix::fcntl::{fcntl, FcntlArg, OFlag};
use nix::pty::{openpty, OpenptyResult};
use nix::unistd;
use tokio::fs::File as TokioFile;
use tokio::io::{AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::sync::oneshot;

use super::{ChannelBridge, Transport, TransportError};

/// Transport over a pseudo-terminal (PTY).
///
/// Opens a PTY pair on construction. A Tokio task owns the master side fd;
/// the sync side communicates via bounded `crossbeam` channels. The slave
/// device path is available for external processes to connect to. An optional
/// stable symlink may be created at a caller-supplied path so that external
/// programs (e.g. terminal emulators) can find the port by a predictable name;
/// the symlink is removed automatically when the transport is shut down or dropped.
pub struct PtyTransport {
    bridge: ChannelBridge,
    /// Path to the slave side of the PTY (e.g. `/dev/pts/N`).
    slave_path: Option<String>,
    /// Optional stable symlink pointing to the slave device node.
    symlink_path: Option<PathBuf>,
}

impl PtyTransport {
    /// Opens a new PTY pair and starts the Tokio IO task on the master side.
    ///
    /// If `symlink_path` is `Some(path)`, a symlink is created at `path` pointing to
    /// the slave device node, giving external programs a predictable port name.
    /// The symlink is removed when the transport is shut down or dropped.
    pub fn open(symlink_path: Option<&Path>) -> std::io::Result<Self> {
        let OpenptyResult { master, slave } = openpty(None, None)
            .map_err(|e| std::io::Error::from_raw_os_error(e as i32))?;

        // SAFETY: slave is valid for the duration of this call.
        let slave_path = unsafe {
            tty_name(BorrowedFd::borrow_raw(slave.as_raw_fd()))
        };

        let symlink_path = if let (Some(link), Some(target)) = (symlink_path, &slave_path) {
            std::os::unix::fs::symlink(target, link)?;
            Some(link.to_path_buf())
        } else {
            None
        };

        // Set master non-blocking so tokio can drive it without blocking the thread.
        let raw = master.as_raw_fd();
        let flags = fcntl(raw, FcntlArg::F_GETFL)
            .map_err(|e| std::io::Error::from_raw_os_error(e as i32))?;
        let new_flags = OFlag::from_bits_truncate(flags) | OFlag::O_NONBLOCK;
        fcntl(raw, FcntlArg::F_SETFL(new_flags))
            .map_err(|e| std::io::Error::from_raw_os_error(e as i32))?;

        let (bridge, in_tx, out_rx, shutdown_rx) = ChannelBridge::new();

        // Convert the master OwnedFd into a tokio::fs::File for async IO.
        // SAFETY: we own master; forget prevents double-close.
        let master_std = unsafe { File::from_raw_fd(raw) };
        std::mem::forget(master);
        let master_async = TokioFile::from_std(master_std);
        // Split so reader and writer can be polled independently in tokio::select!.
        let (reader, writer) = tokio::io::split(master_async);

        tokio::spawn(run_pty_task(reader, writer, in_tx, out_rx, shutdown_rx));

        Ok(Self { bridge, slave_path, symlink_path })
    }

    /// Returns the path of the slave PTY device (e.g. `/dev/pts/3`), if available.
    pub fn slave_path(&self) -> Option<&str> {
        self.slave_path.as_deref()
    }
}

impl Transport for PtyTransport {
    fn try_recv(&mut self) -> Option<u8> { self.bridge.try_recv() }
    fn send(&mut self, byte: u8) -> Result<(), TransportError> { self.bridge.send(byte) }
    fn is_connected(&self) -> bool { self.bridge.is_connected() }

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
async fn run_pty_task(
    mut reader: ReadHalf<TokioFile>,
    mut writer: WriteHalf<TokioFile>,
    in_tx: Sender<u8>,
    out_rx: Receiver<u8>,
    mut shutdown_rx: oneshot::Receiver<()>,
) {
    let mut buf = [0u8; 1];

    loop {
        tokio::select! {
            _ = &mut shutdown_rx => break,

            result = reader.read(&mut buf) => {
                match result {
                    Ok(1) => {
                        if in_tx.send(buf[0]).is_err() {
                            break;
                        }
                    }
                    _ => break,
                }
            }

            _ = drain_outbound(&mut writer, &out_rx) => {}
        }
    }
}

/// Flushes all pending outbound bytes from `out_rx` into `writer`.
async fn drain_outbound(writer: &mut WriteHalf<TokioFile>, out_rx: &Receiver<u8>) {
    while let Ok(byte) = out_rx.try_recv() {
        if writer.write_all(&[byte]).await.is_err() {
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
        assert!(transport.is_connected());
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
