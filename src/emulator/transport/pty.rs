//! Transport over a pseudo-terminal (PTY).
//!
//! Opens a PTY pair on construction. A Tokio task owns the master side fd via
//! [`AsyncFd`] for proper non-blocking epoll integration. Inbound bytes flow
//! through a [`ChannelRelay<u8>`](ChannelRelay) (transport relay redesign
//! plan, §3/§4.1): the Tokio task pushes each byte it reads into a plain
//! `crossbeam_channel`, and the relay's own OS thread relays those into an
//! `rtrb` ring for the caller to drain. Outbound bytes go the other way: the
//! transport pushes into an `rtrb::Producer<u8>` (never blocking; overflow is
//! counted via `TransportReporter`, not surfaced as an error, per §1.5), and
//! the same Tokio task drains the matching `Consumer<u8>` and writes to the
//! master fd. The slave device path is available for external processes to
//! connect to. An optional stable symlink may be created at a caller-supplied
//! path so that external programs (e.g. terminal emulators) can find the port
//! by a predictable name; the symlink is removed automatically when the
//! transport is shut down or dropped.

use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::OwnedFd;
use std::os::unix::io::{AsRawFd, BorrowedFd, FromRawFd};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crossbeam_channel::{Sender, bounded};
use nix::fcntl::{FcntlArg, OFlag, fcntl};
use nix::pty::{OpenptyResult, openpty};
use nix::sys::termios::{self, SetArg, cfmakeraw};
use nix::unistd;
use rtrb::{Consumer, Producer, PushError, RingBuffer};
use tokio::io::unix::AsyncFd;
use tokio::sync::{Notify, oneshot};

use super::{CHANNEL_CAPACITY, ChannelRelay, Transport, TransportError, TransportReporter};

/// Transport over a pseudo-terminal (PTY).
pub struct PtyTransport {
    /// Outbound ring producer; see the module doc for the outbound path.
    outbound: Producer<u8>,
    outbound_notify: Arc<Notify>,
    /// One-shot signal sent to the Tokio task to request shutdown.
    shutdown_tx: Option<oneshot::Sender<()>>,
    slave_path: Option<String>,
    _slave: OwnedFd,
    // Reflects whether an external process is currently confirmed connected
    // (i.e. has actually read a byte); shared with the Tokio task. Note this
    // no longer gates `send()` — see its doc comment.
    client_connected: Arc<AtomicBool>,
    symlink_path: Option<PathBuf>,
    /// Clone of the reporter supplied to `open`, for `send()`'s own
    /// `note_outbound_drop` call (§2.1).
    reporter: TransportReporter,
}

impl PtyTransport {
    /// Opens a PTY pair, using the crate's default channel capacity for both
    /// the inbound relay's ring and the outbound ring.
    pub fn open(
        symlink_path: Option<&Path>,
        reporter: TransportReporter,
    ) -> std::io::Result<(Self, ChannelRelay<u8>)> {
        Self::open_with_capacity(symlink_path, reporter, CHANNEL_CAPACITY)
    }

    /// Same as [`open`](Self::open), with the inbound/outbound ring capacity
    /// parameterized. `pub(crate)` and used only by
    /// `outbound_overflow_increments_drop_counter_and_is_reported` below, to
    /// force a deterministic ring overflow (capacity 1) rather than relying
    /// on timing — see the "Resolved during design review" log in
    /// `doc/transport-relay-redesign-plan.md` §7 for why the naive,
    /// timing-based version of this test was rejected.
    pub(crate) fn open_with_capacity(
        symlink_path: Option<&Path>,
        reporter: TransportReporter,
        capacity: usize,
    ) -> std::io::Result<(Self, ChannelRelay<u8>)> {
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

        let (in_tx, in_rx) = bounded::<u8>(capacity);
        let relay = ChannelRelay::spawn(in_rx, capacity);

        let (outbound_producer, outbound_consumer) = RingBuffer::new(capacity);
        let outbound_notify = Arc::new(Notify::new());
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let client_connected = Arc::new(AtomicBool::new(false));

        let master_std = unsafe { File::from_raw_fd(raw) };
        std::mem::forget(master);
        let async_fd = AsyncFd::new(master_std)?;

        tokio::spawn(run_pty_task(
            async_fd,
            in_tx,
            outbound_consumer,
            Arc::clone(&outbound_notify),
            shutdown_rx,
            Arc::clone(&client_connected),
            reporter.clone(),
        ));

        let transport = Self {
            outbound: outbound_producer,
            outbound_notify,
            shutdown_tx: Some(shutdown_tx),
            slave_path,
            _slave: slave,
            client_connected,
            symlink_path,
            reporter,
        };
        Ok((transport, relay))
    }

    pub fn slave_path(&self) -> Option<&str> {
        self.slave_path.as_deref()
    }
}

impl Transport for PtyTransport {
    /// Superseded by the [`ChannelRelay<u8>`](ChannelRelay) returned
    /// alongside this transport from `open`/`open_with_capacity` — inbound
    /// bytes flow through that relay now, not through this method. Retained
    /// only because `Transport::try_recv` isn't removed from the trait until
    /// every device migrates to draining a relay directly (transport relay
    /// redesign plan §10, checklist item 12). Until then, a device still
    /// calling this method on a `PtyTransport` will not receive PTY input.
    fn try_recv(&mut self) -> Option<u8> {
        None
    }

    /// Sends a byte to the PTY master.
    ///
    /// Unlike the previous implementation, this does not gate on whether an
    /// external process has the slave open yet: PTY masters accept writes
    /// regardless of whether anyone has opened the slave side (the kernel
    /// buffers the output). `is_connected()` still reflects true external
    /// attachment, independently of this. Never blocks and never errors
    /// (§1.5/§2): outbound ring overflow is counted via `TransportReporter`,
    /// not surfaced here.
    fn send(&mut self, byte: u8) {
        match self.outbound.push(byte) {
            Ok(()) => self.outbound_notify.notify_one(),
            Err(PushError::Full(_)) => self.reporter.note_outbound_drop(),
        }
    }

    fn is_connected(&self) -> bool {
        self.client_connected.load(Ordering::Acquire)
    }

    fn shutdown(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
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

/// Tokio task: bridges the async PTY master to the sync side.
///
/// Reads bytes from the master fd and pushes them into `in_tx` (consumed by
/// the `ChannelRelay<u8>` returned from `open`); drains `outbound` (the
/// transport's `send()` target) and writes those bytes to the master fd;
/// reports outbound/inbound drop counts on a 1-second interval (§1.5).
/// `client_connected` tracks real external attach/detach (via first
/// successful read / EIO) for `is_connected()`, independent of the relay.
/// Each attach/detach edge is also reported via `reporter.report_connected`/
/// `report_disconnected` (§5.1), guarded so a spurious detach isn't reported
/// on task exit when no client was ever attached.
async fn run_pty_task(
    async_fd: AsyncFd<File>,
    in_tx: Sender<u8>,
    mut outbound: Consumer<u8>,
    outbound_notify: Arc<Notify>,
    mut shutdown_rx: oneshot::Receiver<()>,
    client_connected: Arc<AtomicBool>,
    reporter: TransportReporter,
) {
    let mut report_interval = tokio::time::interval(Duration::from_secs(1));
    report_interval.tick().await; // first tick fires immediately; skip it

    let mut buf = [0u8; 1];
    loop {
        tokio::select! {
            _ = &mut shutdown_rx => break,

            result = async_fd.readable() => {
                let mut guard = match result { Ok(g) => g, Err(_) => break };
                match guard.try_io(|inner| inner.get_ref().read(&mut buf)) {
                    Ok(Ok(1)) => {
                        if !client_connected.swap(true, Ordering::Release) {
                            reporter.report_connected(None);
                        }
                        if in_tx.send(buf[0]).is_err() {
                            break;
                        }
                    }
                    Ok(Ok(_)) => {}
                    Ok(Err(e)) if e.raw_os_error() == Some(nix::libc::EIO) => {
                        while outbound.pop().is_ok() {}
                        if client_connected.swap(false, Ordering::Release) {
                            reporter.report_disconnected(None, "EIO".to_string());
                        }
                    }
                    Ok(Err(_)) => break,
                    Err(_) => { guard.clear_ready(); }
                }
            }

            _ = drain_outbound(async_fd.get_ref(), &mut outbound, &outbound_notify, &reporter) => {}

            _ = report_interval.tick() => {
                reporter.report_counts();
            }
        }
    }
    // Guarded (`swap`, not `store`): this runs unconditionally on every task
    // exit, including shutdown before any client ever attached, which must
    // not be reported as a disconnect (§5.1) — only a genuine false→true→false
    // transition should.
    if client_connected.swap(false, Ordering::Release) {
        reporter.report_disconnected(None, "shutdown".to_string());
    }
    drop(in_tx);
}

async fn drain_outbound(
    mut file: &File,
    outbound: &mut Consumer<u8>,
    notify: &Notify,
    reporter: &TransportReporter,
) {
    notify.notified().await;
    while let Ok(byte) = outbound.pop() {
        if let Err(e) = file.write_all(&[byte]) {
            reporter.report_error(TransportError::Io(e));
            return;
        }
    }
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
    use crate::emulator::{DeviceEvent, DeviceId, device_event_channel};

    /// Shuts `transport` down (stopping the background Tokio task and its
    /// OS resources, not just the relay) and drops it plus `relay`.
    /// `ChannelRelay::drop`'s internal stop signal means this is safe and
    /// prompt even here, inside an `async fn` test on a Tokio worker — it no
    /// longer depends on that worker being free to poll `run_pty_task` to
    /// completion first (see the shutdown contract in the module doc).
    fn close(mut transport: PtyTransport, relay: ChannelRelay<u8>) {
        transport.shutdown();
        drop((transport, relay));
    }

    #[tokio::test]
    async fn open_send_recv() {
        let (transport, mut relay) = PtyTransport::open(None, TransportReporter::pending(None)).unwrap();
        assert!(transport.slave_path().is_some());

        let slave_path = transport.slave_path().unwrap().to_owned();
        let slave_file = std::fs::OpenOptions::new().write(true).open(&slave_path).unwrap();
        { let mut f = slave_file; f.write_all(&[0xBB]).unwrap(); }

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let mut got = Vec::new();
        relay.drain_into(|b| got.push(b));
        assert_eq!(got, vec![0xBB]);

        close(transport, relay);
    }

    #[tokio::test]
    async fn shutdown_marks_disconnected() {
        let (mut transport, relay) = PtyTransport::open(None, TransportReporter::pending(None)).unwrap();
        transport.shutdown();
        assert!(!transport.is_connected());

        close(transport, relay);
    }

    #[tokio::test]
    async fn send_before_any_external_attach_succeeds() {
        let (mut transport, relay) = PtyTransport::open(None, TransportReporter::pending(None)).unwrap();

        // No external process connected yet; send must not panic or block.
        assert!(!transport.is_connected());
        transport.send(0xFF);

        close(transport, relay);
    }

    #[tokio::test]
    async fn is_connected_reflects_client_state() {
        let (transport, relay) = PtyTransport::open(None, TransportReporter::pending(None)).unwrap();
        let slave_path = transport.slave_path().unwrap().to_owned();

        assert!(!transport.is_connected());

        let slave = std::fs::OpenOptions::new().write(true).open(&slave_path).unwrap();
        { let mut f = slave; f.write_all(&[0x01]).unwrap(); }

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(transport.is_connected());

        close(transport, relay);
    }

    #[tokio::test]
    async fn open_with_symlink_creates_and_removes_link() {
        let link_path = PathBuf::from("/tmp/emma65_test_pty_link");
        let _ = std::fs::remove_file(&link_path);

        let (transport, relay) = PtyTransport::open(Some(&link_path), TransportReporter::pending(None)).unwrap();
        assert!(link_path.exists());

        close(transport, relay);
        assert!(!link_path.exists());
    }

    #[tokio::test]
    async fn initial_dump_reaches_client_that_attaches_later() {
        let (mut transport, relay) = PtyTransport::open(None, TransportReporter::pending(None)).unwrap();
        let slave_path = transport.slave_path().unwrap().to_owned();

        // Give the spawned task a chance to run before checking for its
        // startup event — `open()` is synchronous and returns before the
        // task has been polled even once.
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;

        transport.send(0xD0);
        transport.send(0xD1);
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        // Now a reader attaches and should still see the buffered bytes.
        let mut slave = std::fs::OpenOptions::new().read(true).open(&slave_path).unwrap();
        let mut buf = [0u8; 2];
        slave.read_exact(&mut buf).unwrap();
        assert_eq!(buf, [0xD0, 0xD1]);

        close(transport, relay);
    }

    #[tokio::test]
    async fn send_is_not_echoed_back_as_received_data() {
        let (mut transport, mut relay) = PtyTransport::open(None, TransportReporter::pending(None)).unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;

        transport.send(0xAB);
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        // In cooked mode this would incorrectly come back as received data.
        let mut got = Vec::new();
        relay.drain_into(|b| got.push(b));
        assert!(got.is_empty());

        close(transport, relay);
    }

    #[tokio::test]
    async fn outbound_overflow_increments_drop_counter_and_is_reported() {
        let (sender, mut receiver) = device_event_channel();
        let reporter = TransportReporter::new(DeviceId(99), Some(sender));
        let (mut transport, relay) = PtyTransport::open_with_capacity(None, reporter.clone(), 1).unwrap();

        // Capacity 1, two sends back-to-back with no `.await` in between —
        // the spawned Tokio task can't be scheduled to drain in between, so
        // the second send is guaranteed to see the ring still full.
        transport.send(0x01);
        transport.send(0x02);

        reporter.report_counts();

        match receiver.try_recv() {
            Ok(DeviceEvent::OutboundBytesDropped { device, count }) => {
                assert_eq!(device, DeviceId(99));
                assert!(count >= 1);
            }
            other => panic!("unexpected event: {other:?}"),
        }

        close(transport, relay);
    }
}
