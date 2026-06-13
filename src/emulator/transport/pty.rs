use std::fs::File;
use std::os::unix::io::{AsRawFd, BorrowedFd, FromRawFd};

use crossbeam_channel::{bounded, Receiver, Sender, TryRecvError};
use nix::fcntl::{fcntl, FcntlArg, OFlag};
use nix::pty::{openpty, OpenptyResult};
use nix::unistd;
use tokio::fs::File as TokioFile;
use tokio::io::{AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::sync::oneshot;

use super::{Transport, TransportError};

const CHANNEL_CAPACITY: usize = 256;

/// Transport over a pseudo-terminal (PTY).
///
/// Opens a PTY pair on construction. A Tokio task owns the master side fd;
/// the sync side communicates via bounded `crossbeam` channels. The slave
/// device path is available for external processes to connect to.
pub struct PtyTransport {
    rx: Receiver<u8>,
    tx: Sender<u8>,
    /// Signals the Tokio task to shut down.
    shutdown_tx: Option<oneshot::Sender<()>>,
    /// Path to the slave side of the PTY (e.g. `/dev/pts/N`).
    slave_path: Option<String>,
    connected: bool,
}

impl PtyTransport {
    /// Opens a new PTY pair and starts the Tokio IO task on the master side.
    pub fn open() -> std::io::Result<Self> {
        let OpenptyResult { master, slave } = openpty(None, None)
            .map_err(|e| std::io::Error::from_raw_os_error(e as i32))?;

        // SAFETY: slave is valid for the duration of this call.
        let slave_path = unsafe {
            tty_name(BorrowedFd::borrow_raw(slave.as_raw_fd()))
        };

        // Set master non-blocking so tokio can drive it without blocking the thread.
        let raw = master.as_raw_fd();
        let flags = fcntl(raw, FcntlArg::F_GETFL)
            .map_err(|e| std::io::Error::from_raw_os_error(e as i32))?;
        let new_flags = OFlag::from_bits_truncate(flags) | OFlag::O_NONBLOCK;
        fcntl(raw, FcntlArg::F_SETFL(new_flags))
            .map_err(|e| std::io::Error::from_raw_os_error(e as i32))?;

        let (in_tx, in_rx) = bounded::<u8>(CHANNEL_CAPACITY);
        let (out_tx, out_rx) = bounded::<u8>(CHANNEL_CAPACITY);
        let (shutdown_tx, shutdown_rx) = oneshot::channel();

        // Convert the master OwnedFd into a tokio::fs::File for async IO.
        // SAFETY: we own master; forget prevents double-close.
        let master_std = unsafe { File::from_raw_fd(raw) };
        std::mem::forget(master);
        let master_async = TokioFile::from_std(master_std);
        // Split so reader and writer can be polled independently in tokio::select!.
        let (reader, writer) = tokio::io::split(master_async);

        tokio::spawn(run_pty_task(reader, writer, in_tx, out_rx, shutdown_rx));

        Ok(Self {
            rx: in_rx,
            tx: out_tx,
            shutdown_tx: Some(shutdown_tx),
            slave_path,
            connected: true,
        })
    }

    /// Returns the path of the slave PTY device (e.g. `/dev/pts/3`), if available.
    pub fn slave_path(&self) -> Option<&str> {
        self.slave_path.as_deref()
    }
}

impl Transport for PtyTransport {
    fn try_recv(&mut self) -> Option<u8> {
        match self.rx.try_recv() {
            Ok(b) => Some(b),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                self.connected = false;
                None
            }
        }
    }

    fn send(&mut self, byte: u8) -> Result<(), TransportError> {
        if !self.connected {
            return Err(TransportError::Disconnected);
        }
        match self.tx.try_send(byte) {
            Ok(()) => Ok(()),
            Err(crossbeam_channel::TrySendError::Full(_)) => Err(TransportError::Full),
            Err(crossbeam_channel::TrySendError::Disconnected(_)) => {
                self.connected = false;
                Err(TransportError::Disconnected)
            }
        }
    }

    fn is_connected(&self) -> bool {
        self.connected
    }

    fn shutdown(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        self.connected = false;
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
        let mut transport = PtyTransport::open().unwrap();
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
        let mut transport = PtyTransport::open().unwrap();
        transport.shutdown();
        assert!(!transport.is_connected());
    }
}

/// Returns the slave PTY device name, or `None` on error.
fn tty_name(fd: BorrowedFd<'_>) -> Option<String> {
    match unistd::ttyname(fd) {
        Ok(path) => path.to_str().map(String::from),
        Err(_) => None,
    }
}
