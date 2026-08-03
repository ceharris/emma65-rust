//! Transport that connects a device to a pair of pipes.
//!
//! This is an internal transport mechanism used to connect devices to in-process pipe
//! pairs (e.g. the default console attached to the emulator's own stdin/stdout,
//! or test harnesses that need synchronous byte-level access). For connecting
//! a device to an external child process, see [`PipeTransport`](super::pipe::PipeTransport).
//!
//! Unlike every other transport in this module, this one can collapse the
//! inbound read and its push into the relay ring onto a single blocking OS
//! thread, with no `crossbeam_channel` hop in between: reads here are already
//! blocking OS calls on a plain thread, not tokio-mediated. That thread blocks
//! in `libc::poll()` on both the real `rx` fd and the read end of a private
//! self-pipe used solely to interrupt that wait on `shutdown()`/`Drop` — a
//! plain blocking read has no other portable way to be interrupted, unlike
//! every other [`ChannelRelay`] producer thread ([`ChannelRelay::spawn`]),
//! which selects against a `crossbeam_channel` stop signal it can wait on
//! directly. See the [module documentation](crate::emulator::transport) for
//! the general shutdown contract and why `ChannelRelay::from_parts`-built
//! relays (this transport's only consumer) need this extra machinery.
//!
//! `pair()` is asymmetric: `local` owns a background relay thread as above,
//! paired with an externally-returned `ChannelRelay<u8>`; `remote` stays a
//! bare transport with no relay or background thread at all, reading its own
//! `rx` directly and synchronously via `try_recv()` on whatever thread calls
//! it. Nothing in production ever drains a relay for `remote` — it's either
//! used directly as a synchronous test double, or split into raw file handles
//! via [`into_split`](InternalPipeTransport::into_split) for direct fd-level
//! I/O (e.g. the debugger's terminal bridge) — so giving it a relay thread
//! would be pure waste at best and race `into_split`'s own raw reads at
//! worst.

use std::fs::File;
use std::io::{self, Read, Write};
use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use crossbeam_channel::{Receiver, Sender, bounded};
use rtrb::{Producer, RingBuffer};

use super::{CHANNEL_CAPACITY, ChannelRelay, Transport, TransportError, TransportReporter, push_and_park};

/// Bidirectional transport over a pair of OS pipes. See the module
/// documentation for the asymmetry between `local` (relay-thread-backed) and
/// `remote` (bare, direct-read) instances produced by [`pair`](Self::pair).
pub struct InternalPipeTransport {
    tx: File,
    reporter: TransportReporter,
    rx_mode: RxMode,
}

/// Distinguishes an end whose `rx` is read directly and synchronously by
/// whichever thread calls [`Transport::try_recv`] (`remote`, or any instance
/// built with no relay) from one whose `rx` has been handed off to a
/// background relay thread draining into an externally-owned
/// [`ChannelRelay<u8>`](ChannelRelay) (`local`, `stdio`, `from_raw_fds`).
enum RxMode {
    /// No relay, no background thread — `rx` is owned directly and read
    /// on-demand, non-blocking.
    Direct { rx: File, connected: Arc<AtomicBool> },
    /// A background thread owns `rx` (moved into its closure) and drains it
    /// into the ring backing an externally-returned `ChannelRelay<u8>`.
    /// `try_recv` is a permanent no-op for this mode — real data only flows
    /// through that relay now.
    Relayed {
        connected: Arc<AtomicBool>,
        /// Signals the relay thread's `push_and_park` retry loop to abandon
        /// a park-on-full-ring wait. `None` once `shutdown` has already sent it.
        stop_tx: Option<Sender<()>>,
        /// Write end of the self-pipe that interrupts the relay thread's
        /// `libc::poll()` wait. `None` once `shutdown` has already used it.
        interrupt_w: Option<File>,
    },
}

impl InternalPipeTransport {
    /// # Safety
    /// The caller must ensure the file descriptors are valid and exclusively owned.
    pub unsafe fn from_raw_fds(
        rx_fd: RawFd,
        tx_fd: RawFd,
        reporter: TransportReporter,
    ) -> io::Result<(Self, ChannelRelay<u8>)> {
        unsafe { Self::from_raw_fds_with_capacity(rx_fd, tx_fd, reporter, CHANNEL_CAPACITY) }
    }

    unsafe fn from_raw_fds_with_capacity(
        rx_fd: RawFd,
        tx_fd: RawFd,
        reporter: TransportReporter,
        capacity: usize,
    ) -> io::Result<(Self, ChannelRelay<u8>)> {
        let (rx, tx) = unsafe {
            let rx_owned = OwnedFd::from_raw_fd(rx_fd);
            let tx_owned = OwnedFd::from_raw_fd(tx_fd);
            (File::from(rx_owned), File::from(tx_owned))
        };
        set_nonblocking(&tx)?;
        Self::with_relay(rx, tx, reporter, capacity)
    }

    /// Creates a transport connected to this process's stdin (fd 0) and stdout (fd 1).
    pub fn stdio(reporter: TransportReporter) -> io::Result<(Self, ChannelRelay<u8>)> {
        let rx_fd = dup_fd(0)?;
        let tx_fd = dup_fd(1)?;
        // SAFETY: dup_fd returns a fresh, exclusively owned descriptor each call.
        unsafe { Self::from_raw_fds(rx_fd, tx_fd, reporter) }
    }

    /// Splits this transport into its raw rx and tx file handles (each
    /// duplicated via `try_clone`, so `self`'s own copies close normally when
    /// it drops at the end of this call).
    ///
    /// Only valid for a bare (`RxMode::Direct`) transport, i.e. `remote` from
    /// [`pair`](Self::pair) — panics for a relay-backed one, since its `rx`
    /// has already been moved into the background relay thread and handing
    /// out a raw handle to the same pipe end would race that thread's own
    /// reads.
    pub fn into_split(self) -> (File, File) {
        match &self.rx_mode {
            RxMode::Direct { rx, .. } => {
                let rx = rx.try_clone().expect("into_split: failed to duplicate rx fd");
                let tx = self.tx.try_clone().expect("into_split: failed to duplicate tx fd");
                (rx, tx)
            }
            RxMode::Relayed { .. } => panic!(
                "InternalPipeTransport::into_split is only valid for a bare transport \
                 (e.g. `remote` from pair()) — this one's rx is owned by a background relay thread"
            ),
        }
    }

    /// Creates a matched local/remote pair of transports connected by
    /// cross-wired OS pipes, using the crate's default relay ring capacity.
    /// Useful for in-process testing and for injecting a console transport
    /// ahead of `DeviceModule::instantiate` (see `TransportSlot`).
    pub fn pair(reporter: TransportReporter) -> io::Result<((Self, ChannelRelay<u8>), Self)> {
        Self::pair_with_capacity(reporter, CHANNEL_CAPACITY)
    }

    /// Creates a matched pair of *bare* transports (`RxMode::Direct` on both
    /// ends) connected by cross-wired OS pipes — no relay or background
    /// thread on either side.
    ///
    /// For synchronous in-process testing where a device under test drains
    /// simulated input through a separately hand-fed `ChannelRelay` rather
    /// than through this transport's own relay (see e.g. `Console`'s test
    /// module): the device's endpoint only needs a working `send()` for
    /// outbound verification, and since nothing ever drains a real relay for
    /// either end in that scenario, building one anyway would risk a
    /// deadlock at drop time (a discarded, undrained `ChannelRelay` needs its
    /// background thread signaled to stop, but that thread is owned by
    /// whichever endpoint the caller may have already moved into a device
    /// long before the relay is dropped — see `pair`'s `local` for the
    /// mechanism that makes that signaling possible, which a plain discarded
    /// local variable can't reliably arrange). `pub` (not `pub(crate)`) so
    /// `tests/*.rs`, a separate crate, can reach it too, for the same reason
    /// [`ChannelRelay::spawn`] needed to go `pub`.
    pub fn pair_direct() -> io::Result<(Self, Self)> {
        let (a_rx, a_tx) = os_pipe()?;
        let (b_rx, b_tx) = os_pipe()?;
        set_nonblocking(&a_rx)?;
        set_nonblocking(&a_tx)?;
        set_nonblocking(&b_rx)?;
        set_nonblocking(&b_tx)?;

        let local = Self {
            tx: b_tx,
            reporter: TransportReporter::pending(None),
            rx_mode: RxMode::Direct { rx: a_rx, connected: Arc::new(AtomicBool::new(true)) },
        };
        let remote = Self {
            tx: a_tx,
            reporter: TransportReporter::pending(None),
            rx_mode: RxMode::Direct { rx: b_rx, connected: Arc::new(AtomicBool::new(true)) },
        };
        Ok((local, remote))
    }

    /// Same as [`pair`](Self::pair), with `local`'s relay ring capacity
    /// parameterized. `pub(crate)` and used only by tests that need to force
    /// a deterministic ring overflow rather than relying on timing.
    pub(crate) fn pair_with_capacity(
        reporter: TransportReporter,
        capacity: usize,
    ) -> io::Result<((Self, ChannelRelay<u8>), Self)> {
        let (a_rx, a_tx) = os_pipe()?;
        let (b_rx, b_tx) = os_pipe()?;
        set_nonblocking(&a_tx)?;
        set_nonblocking(&b_rx)?;
        set_nonblocking(&b_tx)?;

        let (local, relay) = Self::with_relay(a_rx, b_tx, reporter, capacity)?;

        let remote = Self {
            tx: a_tx,
            reporter: TransportReporter::pending(None),
            rx_mode: RxMode::Direct { rx: b_rx, connected: Arc::new(AtomicBool::new(true)) },
        };

        Ok(((local, relay), remote))
    }

    /// Builds a relay-backed (`RxMode::Relayed`) transport: spawns the
    /// background thread that owns `rx` and drains it into a fresh ring,
    /// wrapping the ring's consumer side via `ChannelRelay::from_parts`.
    fn with_relay(
        rx: File,
        tx: File,
        reporter: TransportReporter,
        capacity: usize,
    ) -> io::Result<(Self, ChannelRelay<u8>)> {
        let connected = Arc::new(AtomicBool::new(true));
        let (interrupt_r, interrupt_w) = os_pipe()?;
        let (stop_tx, stop_rx) = bounded::<()>(1);
        let (producer, consumer) = RingBuffer::new(capacity);

        let thread_connected = Arc::clone(&connected);
        let thread_reporter = reporter.clone();
        let handle = thread::spawn(move || {
            run_relay_thread(rx, producer, stop_rx, interrupt_r, thread_connected, thread_reporter);
        });
        let relay = ChannelRelay::from_parts(consumer, handle);

        let transport = Self {
            tx,
            reporter,
            rx_mode: RxMode::Relayed {
                connected,
                stop_tx: Some(stop_tx),
                interrupt_w: Some(interrupt_w),
            },
        };
        transport.reporter.report_connected(None);
        Ok((transport, relay))
    }

    fn connected_flag(&self) -> &Arc<AtomicBool> {
        match &self.rx_mode {
            RxMode::Direct { connected, .. } => connected,
            RxMode::Relayed { connected, .. } => connected,
        }
    }

    /// Direct, synchronous, non-blocking read of `rx` — only meaningful for
    /// a bare (`RxMode::Direct`) instance (`remote`, or any `pair_direct()`
    /// end); always `None` for a relay-backed (`local`) instance, whose `rx`
    /// has been moved into the background relay thread. Deliberately an
    /// **inherent** method, not part of `Transport`: existing device tests
    /// call this on a concrete `InternalPipeTransport` (`remote`) purely to
    /// verify outbound sends, a use that has nothing to do with the inbound
    /// relay path every real transport now uses instead. Keeping it off the
    /// trait means it can never be reached through a `Box<dyn Transport>` —
    /// which is what let issue #233 slip past `LedMatrix`'s own tests: those
    /// tests built the device's transport via `pair_direct()`, whose
    /// `try_recv` still worked, while production `LedMatrix` used
    /// `PtyTransport`/`TcpSocketTransport`, where the identical call (back
    /// when it was still a trait method) was a hard no-op. See the module
    /// documentation for the `local`/`remote` asymmetry.
    pub fn try_recv(&mut self) -> Option<u8> {
        let mut newly_disconnected = false;
        let result = match &mut self.rx_mode {
            RxMode::Direct { rx, connected } => {
                let mut buf = [0u8; 1];
                match rx.read(&mut buf) {
                    Ok(1) => Some(buf[0]),
                    Ok(_) => {
                        newly_disconnected = connected.swap(false, Ordering::Release);
                        None
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => None,
                    Err(_) => {
                        newly_disconnected = connected.swap(false, Ordering::Release);
                        None
                    }
                }
            }
            RxMode::Relayed { .. } => None,
        };
        if newly_disconnected {
            self.reporter.report_disconnected(None, "connection closed".to_string());
        }
        result
    }
}

impl Transport for InternalPipeTransport {
    /// Writes directly and synchronously — this transport has no separate
    /// background task on the outbound side (unlike Pty/Pipe), so there's
    /// nothing to hand a byte off to. Never blocks: `WouldBlock` (pipe full)
    /// is counted via `TransportReporter::note_outbound_drop`, never
    /// surfaced as an error; a hard write failure marks the transport
    /// disconnected and reports via `TransportReporter::report_error`.
    /// `report_counts` fires inline after each drop, since (per the module
    /// documentation) there's no periodic task here to host a timer for it.
    fn send(&mut self, byte: u8) {
        if !self.connected_flag().load(Ordering::Acquire) {
            self.reporter.note_outbound_drop();
            self.reporter.report_counts();
            return;
        }
        match self.tx.write_all(&[byte]) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                self.reporter.note_outbound_drop();
                self.reporter.report_counts();
            }
            Err(e) => {
                let reason = format!("write error: {e}");
                if self.connected_flag().swap(false, Ordering::Release) {
                    self.reporter.report_disconnected(None, reason);
                }
                self.reporter.report_error(TransportError::Io(e));
            }
        }
    }

    fn is_connected(&self) -> bool {
        self.connected_flag().load(Ordering::Acquire)
    }

    fn shutdown(&mut self) {
        if self.connected_flag().swap(false, Ordering::Release) {
            self.reporter.report_disconnected(None, "shutdown".to_string());
        }
        if let RxMode::Relayed { stop_tx, interrupt_w, .. } = &mut self.rx_mode {
            if let Some(tx) = stop_tx.take() {
                let _ = tx.send(());
            }
            if let Some(mut w) = interrupt_w.take() {
                let _ = w.write_all(&[0u8]);
            }
        }
    }
}

impl Drop for InternalPipeTransport {
    /// Ensures a relay-backed transport's background thread is always
    /// signaled to stop, even if `shutdown` was never called explicitly —
    /// see the module documentation's shutdown-contract discussion. Relies
    /// on this running before the paired `ChannelRelay<u8>` is dropped,
    /// which holds throughout this crate since every device stores its
    /// `transport` field ahead of its `relay` field (fields drop in
    /// declaration order).
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Background thread body for a relay-backed transport. Blocks in
/// `libc::poll()` on both `rx` and the read end of a self-pipe
/// (`interrupt_r`) used solely to interrupt that wait — see the module
/// documentation for why a self-pipe is needed here, unlike every other
/// `ChannelRelay` producer thread. Once `rx` is confirmed readable, reads one
/// byte and pushes it into `producer` via the shared `push_and_park` retry
/// helper (itself interruptible via `stop_rx`, for the case where the ring is
/// full when the self-pipe signal arrives). Exits on the self-pipe signal, on
/// EOF/error reading `rx`, or once `push_and_park` reports the ring's
/// consumer is gone. Reports a disconnect edge via `reporter` on exit,
/// guarded by `connected` so a `shutdown()` that already reported it
/// synchronously isn't double-counted.
fn run_relay_thread(
    mut rx: File,
    mut producer: Producer<u8>,
    stop_rx: Receiver<()>,
    interrupt_r: File,
    connected: Arc<AtomicBool>,
    reporter: TransportReporter,
) {
    let rx_fd = rx.as_raw_fd();
    let interrupt_fd = interrupt_r.as_raw_fd();
    let mut buf = [0u8; 1];

    let reason = 'relay: loop {
        let mut fds = [
            libc::pollfd { fd: rx_fd, events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: interrupt_fd, events: libc::POLLIN, revents: 0 },
        ];
        let rc = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, -1) };
        if rc < 0 {
            if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            break 'relay "poll error".to_string();
        }
        if fds[1].revents & libc::POLLIN != 0 {
            break 'relay "shutdown".to_string(); // shutdown requested
        }
        if fds[0].revents & libc::POLLIN != 0 {
            match rx.read(&mut buf) {
                Ok(1) => {
                    if !push_and_park(&mut producer, buf[0], &stop_rx) {
                        break 'relay "shutdown".to_string();
                    }
                }
                Ok(_) => break 'relay "connection closed".to_string(),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => break 'relay "connection closed".to_string(),
            }
        } else if fds[0].revents & (libc::POLLHUP | libc::POLLERR) != 0 {
            break 'relay "connection closed".to_string();
        }
    };
    // Guarded (`swap`, not `store`): `shutdown()` may have already reported
    // this disconnect synchronously before signaling this thread to
    // exit, in which case this is a no-op.
    if connected.swap(false, Ordering::Release) {
        reporter.report_disconnected(None, reason);
    }
}

fn os_pipe() -> io::Result<(File, File)> {
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
    use crate::emulator::{DeviceEvent, DeviceId, device_event_channel};
    use std::time::Duration;

    fn pair() -> ((InternalPipeTransport, ChannelRelay<u8>), InternalPipeTransport) {
        InternalPipeTransport::pair(TransportReporter::pending(None)).unwrap()
    }

    /// Shuts `transport` down (stopping its background relay thread, not
    /// just dropping the ring) and drops it plus `relay`. See the module
    /// documentation and `InternalPipeTransport`'s `Drop` impl for why this
    /// ordering makes the subsequent `ChannelRelay::drop`'s join prompt.
    fn close(mut transport: InternalPipeTransport, relay: ChannelRelay<u8>) {
        transport.shutdown();
        drop((transport, relay));
    }

    #[test]
    fn send_recv_round_trip() {
        let ((mut local, relay), mut remote) = pair();
        local.send(0x42);
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(remote.try_recv(), Some(0x42));
        close(local, relay);
    }

    #[test]
    fn remote_try_recv_returns_none_when_empty() {
        let ((local, relay), mut remote) = pair();
        assert_eq!(remote.try_recv(), None);
        close(local, relay);
    }

    #[test]
    fn local_relay_receives_bytes_sent_by_remote() {
        let ((local, mut relay), mut remote) = pair();
        remote.send(0x7A);
        std::thread::sleep(Duration::from_millis(20));

        let mut got = Vec::new();
        relay.drain_into(|b| got.push(b));
        assert_eq!(got, vec![0x7A]);

        close(local, relay);
    }

    #[test]
    fn local_is_connected_initially_true() {
        let ((local, relay), _remote) = pair();
        assert!(local.is_connected());
        close(local, relay);
    }

    #[test]
    fn remote_is_connected_initially_true() {
        let ((local, relay), remote) = pair();
        assert!(remote.is_connected());
        close(local, relay);
    }

    #[test]
    fn shutdown_marks_local_disconnected() {
        let ((mut local, relay), _remote) = pair();
        local.shutdown();
        assert!(!local.is_connected());
        drop(relay);
    }

    #[test]
    fn shutdown_interrupts_relay_thread_blocked_on_no_data() {
        // The background thread is parked in libc::poll() with no timeout
        // and nothing has ever been sent to it. If shutdown()'s self-pipe
        // interrupt didn't work, `close` (which joins the thread via
        // ChannelRelay::drop) would hang forever.
        let ((local, relay), _remote) = pair();

        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            close(local, relay);
            let _ = done_tx.send(());
        });

        done_rx
            .recv_timeout(Duration::from_millis(500))
            .expect("shutdown did not interrupt the relay thread's blocking poll()");
    }

    #[test]
    fn into_split_returns_working_raw_handles() {
        let ((mut local, mut relay), remote) = pair();
        let (mut remote_rx, mut remote_tx) = remote.into_split();

        // local -> remote_rx
        local.send(0x99);
        std::thread::sleep(Duration::from_millis(20));
        let mut buf = [0u8; 1];
        remote_rx.read_exact(&mut buf).unwrap();
        assert_eq!(buf[0], 0x99);

        // remote_tx -> local's relay
        remote_tx.write_all(&[0x55]).unwrap();
        std::thread::sleep(Duration::from_millis(20));
        let mut got = Vec::new();
        relay.drain_into(|b| got.push(b));
        assert_eq!(got, vec![0x55]);

        close(local, relay);
    }

    #[test]
    #[should_panic(expected = "into_split is only valid for a bare transport")]
    fn into_split_panics_for_relay_backed_transport() {
        let ((local, _relay), _remote) = pair();
        let _ = local.into_split();
    }

    #[test]
    fn outbound_full_pipe_increments_drop_counter_and_is_reported() {
        let (sender, mut receiver) = device_event_channel();
        let reporter = TransportReporter::pending(Some(sender));
        reporter.bind(DeviceId(99));
        let ((mut local, relay), _remote) = InternalPipeTransport::pair(reporter.clone()).unwrap();
        assert!(matches!(receiver.try_recv(), Ok(DeviceEvent::TransportConnected { .. })));

        // Force the OS pipe buffer (default 64KiB on Linux) full without
        // ever draining it from the remote side, so send() observes
        // WouldBlock deterministically rather than relying on timing.
        for _ in 0..(128 * 1024) {
            local.send(0);
        }

        match receiver.try_recv() {
            Ok(DeviceEvent::OutboundBytesDropped { device, count }) => {
                assert_eq!(device, DeviceId(99));
                assert!(count >= 1);
            }
            other => panic!("unexpected event: {other:?}"),
        }

        close(local, relay);
    }
}
