# Transport Relay Redesign — Implementation Plan

## Purpose

Replace polling-based `Transport::try_recv()` with an `rtrb`-backed ring
buffer relay so bus device `tick()` implementations drain all pending
inbound data/events without polling a transport directly. This plan also
folds in a cleanup of connection-lifecycle event handling and error
reporting, prompted by the observation that point-to-point transports
(pipe, PTY) and multipoint transports (TCP, Unix socket) have genuinely
different needs around connection lifecycle, even though they share one
`Transport` trait (§2) and one error-reporting mechanism (§1.8). The change
is not confined to `src/emulator/transport/` — every `IoDevice` that owns a
transport and every `DeviceModule` that constructs one is affected; see §9.

This document is the accumulated result of a design conversation and is
meant to be used as a steering document for implementation — not a spec to
transcribe literally without judgment. Where an open question remains
unresolved, it is called out explicitly rather than silently decided.

---

## 1. Core Design Decisions (settled)

### 1.1 Two transport categories, and why they differ

- **Point-to-point (P2P)**: `InternalPipeTransport`, `PipeTransport`,
  `PtyTransport`. Single peer. Connection-lifecycle events
  (`Connected`/`Disconnected`) are **not needed** — nothing downstream
  consumes them for these transports today. They exist in the current code
  only because `ChannelBridge<TransportEvent>` was the shared plumbing
  available, not because of a real requirement. **Remove them for P2P.**
  Connection *state* (is a peer currently attached) remains real and is
  tracked separately via `Transport::is_connected()`, independent of
  the data relay.

- **Multipoint**: `TcpSocketTransport`, `UnixSocketTransport`. Multiple
  concurrent client connections fan into one logical stream. Connection
  lifecycle **is** load-bearing here — `ProtocolManager` (or equivalent)
  needs `Connected`/`Disconnected` events to know when a given `u8` tag is
  safe to reuse, since tags are recycled. **Keep `TransportEvent` for
  multipoint.**

The dividing line is *"does anything consume connection-lifecycle
events,"* not *"is this the same code shape as another transport."* Keep
these two facts (lifecycle-event necessity vs. multi-producer fan-in
necessity) conceptually distinct even though they happen to align on the
same P2P/multipoint split today. A future P2P transport with real
intermittent-connection semantics could need `TransportEvent` without
needing a fan-in relay thread.

### 1.2 Why a dedicated relay *thread*, not just an async task

`rtrb::Producer::push` never blocks — on a full ring it returns
`Err(PushError::Full(item))` immediately. To get "block until `tick()` has
drained space" behavior (rather than dropping or busy-spinning), the
pushing side must be **parkable** via `std::thread::park` /
`Thread::unpark`. A tokio task cannot be parked without stalling every
other task scheduled on that worker thread — including, critically, the
reactor polling for the socket/PTY reads that would eventually unblock it.
So:

- The **relay thread is a plain OS thread**, blocking on
  `crossbeam_channel::Receiver::recv()`, pushing into `rtrb::Producer`, and
  calling `thread::park()` on `PushError::Full`.
- `tick()` drains the `rtrb::Consumer` side and calls `unpark()` on the
  relay thread's `JoinHandle` after freeing space. `Thread::unpark`'s
  documented "remembered token" semantics make this race-free without
  extra synchronization — an `unpark()` that arrives before the
  corresponding `park()` is not lost.
- This is the **shared mechanism (`ChannelRelay<T>`)** behind both P2P and
  multipoint inbound relays. It is agnostic to producer count; it just
  needs a `Receiver<T>`.

### 1.3 Why outbound does *not* need a relay thread

- `tick()` is the CPU thread and must never block. Outbound `push` into
  `rtrb::Producer<u8>` must be non-blocking on `Full`, with drops counted
  rather than surfaced as an error (matches the *existing* behavior of
  `Transport::send`, which today never surfaces send failures to callers
  either).
- The consuming side is unchanged in shape from today: an existing tokio
  task (`drain_outbound` for P2P, `pump_outbound` for multipoint) pops
  non-blockingly on a `Notify` wakeup and performs the actual IO write.
  Only the underlying data structure changes, from `crossbeam_channel` to
  `rtrb`.
- **Do not add a service thread for outbound in either category.**

### 1.4 Why outbound matters for latency, not just symmetry

For MC6840 fanout scenarios, inbound data received from one peripheral on
a multipoint connection must be re-broadcast as outbound to every other
peripheral on that same connection, often within the same tick. Outbound
traffic there is coupled to (and can be amplified by) inbound traffic — it
is not low-frequency, decoupled, best-effort traffic the way a single
interactive byte stream might be. This has two consequences:

- Outbound ring buffer capacity/overflow behavior for multipoint
  transports should be sized and reasoned about deliberately, not treated
  as an afterthought.
- If a bus device needs to guarantee "an inbound tagged event is
  re-published as outbound within the same tick it was observed," the
  device's `tick()` should drain inbound and push outbound in one tight
  loop (per event), not as two separate passes (drain-all-inbound, then a
  second pass emitting all outbound) — the latter does not change worst
  case correctness but is worth being intentional about.

### 1.5 Diagnostic-only drop counting (both directions, where applicable)

All drop counts introduced by this redesign are **purely diagnostic**. The
expected user response to a nonzero count is: stop the emulator, resize
the relevant ring buffer, restart. They are **not** correctness signals
and must never gate or change device/transport behavior at runtime.

- **Outbound drops** (both P2P and multipoint): counted via
  `Arc<AtomicU64>`, incremented with `Ordering::Relaxed` (CPU thread is the
  sole writer — `tick()` is the only caller of the outbound push). Reported
  via `swap(0, Ordering::Relaxed)` on a `tokio::time::interval` (~1s
  default; exact cadence is UX-only and not critical) from the existing
  outbound-pump tokio task. Surfaced as a new event —
  `DeviceEvent::OutboundBytesDropped { device: DeviceId, count: u64 }` —
  mirroring the existing `DeviceEvent::TransportError { device, error }`
  shape (keyed by `DeviceId` alone; no direction/channel field needed
  unless a device has more than one transport, which is out of scope
  here).

- **Inbound ingress drops — multipoint only**: the existing
  `crossbeam_channel` MPSC inside `ChannelBridge`/per-client fan-in (today
  a *blocking* `in_tx.send()` inside `run_client_task`) must become
  `try_send()` with its own `Arc<AtomicU64>` counter and periodic report —
  `DeviceEvent::InboundEventsDropped { device: DeviceId, count: u64 }`.
  This is necessary because a blocking `send()` there, combined with the
  relay thread parking on a full downstream `rtrb` ring, would stall a
  tokio worker thread (see §1.6). The existing channel capacity
  (`CHANNEL_CAPACITY = 256`) is assumed adequate as-is: for the expected
  real-world case (Unix domain socket, and TCP similarly), OS-level socket
  buffers provide backpressure to the peer once local buffers fill, so
  this channel only needs to absorb the gap between "kernel has bytes
  ready" and "tokio task gets scheduled," not sustained backlog. Revisit
  sizing only if evidence suggests otherwise.

- **Inbound — P2P**: **no drop counter needed.** Backpressure here is
  handled entirely by park/unpark against the single-producer channel; the
  backlog accumulates upstream in OS I/O buffers rather than being
  dropped. There is no ingress fan-in stage analogous to multipoint's MPSC.

### 1.6 Why multipoint inbound ingress can't rely purely on park/unpark

Multipoint has two relay stages, not one:

```
N client tasks --(crossbeam MPSC, in_tx.send)--> [stage 1: fan-in]
                                                   --(ChannelRelay: recv + park/push)--> [stage 2] --> rtrb --> tick()
```

Park/unpark at stage 2 only protects stage 2's *own* producer (the relay
thread). If the relay thread parks, it stops draining the MPSC at stage 1.
If stage 1's `send()` is blocking, every `run_client_task` — which runs
inside `tokio::spawn`, sharing worker threads with other clients' socket
reads/writes — can then block on that send, stalling unrelated tasks
sharing that worker. This is the same class of problem being avoided by
using a plain thread for stage 2, reintroduced one hop upstream if stage 1
is left blocking. Hence: stage 1 must be `try_send()` + counter, not a
blocking `send()`.

### 1.7 Shutdown contract

**Revised during implementation of checklist item 3** (`PtyTransport`
rewrite): the original "no internal stop flag" design below caused real,
repeated friction — `ChannelRelay`'s own unit tests needed careful `Sender`
drop-ordering to avoid hanging, and `PtyTransport`'s async tests deadlocked
outright, since dropping a `ChannelRelay` inside a `#[tokio::test]` async fn
blocks the very Tokio worker needed to drive the producer task to drop its
sender in the first place. `ChannelRelay` now carries an internal stop
signal instead (a small `crossbeam_channel`, used only by `Drop`, selected
against alongside the data channel) — see the shutdown-contract section of
`src/emulator/transport/mod.rs`'s module doc for the final design. For a
`spawn`-constructed relay, `Drop` is now prompt unconditionally: it no
longer depends on any `Sender<T>` being dropped, from anywhere, including
from within an async task on a Tokio worker. `from_parts`-constructed relays
(§3; first real consumer is `InternalPipeTransport`, §4.4, not yet
implemented) don't have this signal wired to their caller-owned thread yet
— that thread reads off a raw fd rather than a `crossbeam_channel`, so an
interruption mechanism for it needs its own design when item 7 lands, and
the "drop the sender/close the fd before joining" caveat below still applies
there until it does.

The original design is preserved below for traceability, but its "senders
must be dropped first" premise no longer applies to `spawn`-constructed
relays as described above.

- ~~`ChannelRelay<T>` carries **no internal stop flag**. Given the fixed,
  small, enumerable set of transport implementations that construct one,
  the simpler contract is acceptable: **the owner must ensure every
  corresponding `Sender<T>` is dropped before dropping the
  `ChannelRelay`.** `Drop for ChannelRelay` unparks (in case the thread is
  parked on a full ring) and then `join()`s. If the relay thread is
  instead blocked in `rx.recv()` with a live sender still outstanding
  anywhere, that join blocks indefinitely — this is a known, accepted
  footgun scoped to the small set of call sites this plan enumerates, not
  a general-purpose public API.~~

- ~~Every tokio task holding an `in_tx: Sender<_>` (or a clone of one) must
  **drop it as the first action on its shutdown branch**, before any other
  cleanup (child process reaping, final event sends, `on_exit` callbacks).
  This decouples "the relay thread can stop blocking" from "the rest of
  this task's teardown work," which may take arbitrarily long (e.g.
  waiting on a child process).~~
    - ~~Example: in `run_pipe_task`'s `select!`, the shutdown branch should
      `drop(in_tx)` before or independent of calling `on_exit(...)`.~~
    - ~~Example: in `run_client_task`, the shutdown branch should
      `drop(in_tx)` and skip the final `Disconnected` send entirely, since
      nothing reads events past a whole-bus shutdown — confirmed against
      `ProtocolManager`'s actual usage (§4.2, §7 "Resolved during design
      review").~~ (This specific sub-point — skipping the final
      `Disconnected` send on whole-bus shutdown — is independently still
      correct and unaffected by the stop-signal change; it's about event
      semantics, not relay shutdown.)

- For multipoint, the listener-accept loop (`run_tcp_task`/`run_unix_task`)
  already has a `_ = shutdown_rx.changed() => break` branch preventing new
  client tasks from being spawned after shutdown begins. Analysis during
  design concluded there is no correctness gap here: a client task that
  wins a last-instant race against the shutdown branch and gets spawned
  anyway will still observe shutdown via its own `select!` and drop its
  sender promptly. This is a bounded, harmless delay, not a stall risk. No
  fix needed. (Unaffected by the stop-signal change — this is about client
  task spawning, not relay shutdown.)

- ~~**Precondition supplied by the project**: `Bus::drop` runs on a plain
  owning thread, never inside a tokio worker. This is what makes it safe
  for `ChannelRelay::drop`'s `join()` to block synchronously — the block
  is bounded by ordinary tokio scheduling latency (sub-millisecond in
  practice) once senders are dropped first as required above, not by IO or
  process-exit latency.~~ No longer a precondition for `spawn`-constructed
  relays — `Drop` is now safe and prompt from any thread, including a Tokio
  worker. Still worth keeping true regardless (§6's `Bus::drop` design is
  unaffected either way).

- **Today, `Transport::shutdown()` is not called in production** — buses
  and CPUs live for the process lifetime. This redesign is also the
  vehicle for wiring `Transport`/relay shutdown into `Bus::drop`, so that a
  bus can be torn down and reconstructed without stranding OS resources
  (threads, fds, sockets) or hanging the process. This wiring is in scope
  for this work, not a followup.

### 1.8 Error reporting moves from `send`'s return value into the transport

Today, `Transport::send(&mut self, byte: u8) -> Result<(), TransportError>`
is the sole error channel: each device calls `send`, and on `Err` invokes a
`report_error: Box<dyn Fn(TransportError) + Send>` closure it was handed
post-construction via `set_error_sender(sender, DeviceId)`. That closure
(built once by `transport::reporter`) wraps the error into
`DeviceEvent::TransportError { device, error }` and pushes it onto the
device's `ErrorSender`.

This redesign moves that responsibility into the transport itself, so
`send` can drop the `Result` entirely and satisfy a uniform "never blocks,
never errors" contract across every transport, including
`InternalPipeTransport` (this resolves what was Open Question 3 in an
earlier draft of this plan — no per-transport exception is needed once
error reporting no longer rides on `send`'s return value):

- Each transport is constructed with a `TransportReporter` (see §2),
  supplied by the caller (ultimately a `DeviceModule::instantiate`
  implementation) rather than attached afterward via a setter. This is not
  new plumbing — every builtin module already allocates its `DeviceId`
  *before* constructing its transport, specifically so it can build a
  reporting closure ahead of time (see `R6551Module::instantiate`,
  `src/emulator/config/r6551.rs:50-61`, which threads `device_id` into
  `context.pipe_exit_reporter(device_id)` before calling
  `to_transport_with_reporter`). `TransportReporter` generalizes and
  replaces that one-off, `FnOnce`-based, `Io`-only helper with something
  transports hold for their full lifetime and can invoke repeatedly.
- **`TransportError`'s reportable surface shrinks to `Io` only.** `Full`
  is not an error under this design — it *is* the outbound diagnostic drop
  counter from §1.5, so it's counted, never reported through
  `TransportReporter`. `Disconnected` is **not** part of `TransportError`
  reporting at all — getting its reporting semantics right needs
  transport-specific edge-triggering (fire once per connected→disconnected
  transition, not once per `send` attempted while already down) and, for
  multipoint, per-client rather than per-device attribution. Both concerns
  are resolved by wiring disconnection into the existing (currently unused)
  `DeviceEvent::TransportConnected`/`TransportDisconnected` events instead,
  with a `peer: Option<String>` field disambiguating multipoint's N clients
  — see §5.1 for the full design. **This is unrelated to
  `TransportEvent::Disconnected`** (§1.1's per-client tagged event,
  consumed by `ProtocolManager` to free reused tags) — that stays exactly
  as it is today; nothing here removes or changes it.
- Connection *state* (`is_connected()`) is unaffected by any of this — it
  continues to be tracked and polled exactly as before, independent of
  error reporting.

---

## 2. Trait Redesign

Replace the single `Transport` trait's `send` signature, but keep it as
one trait — **do not** split it into P2P/multipoint variants. An earlier
draft of this plan introduced `TransportControl` (renamed from
`Transport`) plus `PointToPointTransport`/`MultiClientTransport` subtraits,
on the theory that P2P and multipoint transports have different needs
(§1.1). On review, that's true of `TransportEvent` and the relay-stage
count, but **not** of the trait surface: both subtraits declared the exact
same `send(&mut self, byte: u8)` signature, so the split added a type-level
distinction with no corresponding behavioral difference — a caller holding
`&mut dyn PointToPointTransport` vs `&mut dyn MultiClientTransport` could
do nothing different with either. The P2P/multipoint distinction remains
real; it's just expressed at the concrete-struct and relay-instantiation
level (`ChannelRelay<u8>` vs `ChannelRelay<TransportEvent>`, §3), not
reified as a trait hierarchy. The `TransportControl` rename is dropped
along with the split — no justification for it was ever established, so
the trait stays `Transport`.

```rust
pub trait Transport: Send {
    fn is_connected(&self) -> bool;
    fn shutdown(&mut self);
    /// Never blocks and never returns an error. Outbound ring overflow is
    /// diagnostic-only (§1.5, counted not reported); hard I/O errors are
    /// reported asynchronously via the `TransportReporter` supplied at
    /// construction (§1.8), not through this call's return value.
    fn send(&mut self, byte: u8);
}
```

Construction returns `(Transport, Relay)` as a pair — **not** a
`self`-consuming `into_relay()`. The transport (for `send`/`is_connected`/
`shutdown`) and the relay (for the bus device's `tick()`-side drain) are
both needed concurrently by different owners; a consuming `into_relay()`
doesn't work once the transport retains state used after relay handoff.
The relay half is just `ChannelRelay<u8>` for P2P transports and
`ChannelRelay<TransportEvent>` for multipoint ones — see §3, which folds
in what earlier drafts called `ByteRelay`/`TaggedRelay` (dropped once it
became clear they'd be two byte-for-byte-identical wrapper structs around
the one generic type, differing only in `T` — the same problem as the
trait split above, minus any justification for keeping them separate).

`TransportEvent` (`Connected(u8)` / `Data(u8, u8)` / `Disconnected(u8)`)
and `TagAllocator` are retained as-is for multipoint use only.

### 2.1 `TransportReporter`

Bundles the callbacks a transport needs at construction time — replacing
both the old `report_dropped: F` closure parameter and the old
post-construction `set_error_sender`/`report_error` pattern (§1.8) with one
object built from an `InstantiationContext` and the `DeviceId` already
allocated for the device (see §1.8; same allocation point as today's
`pipe_exit_reporter`).

**`TransportReporter` is `Clone`** (cheaply — internally `Arc`-wrapped
counters plus a cloned `ErrorSender`, which is already a `Clone`able
`mpsc::UnboundedSender` today). This is load-bearing, not incidental: a
single `TransportReporter` is constructed once per transport but needs to
be used concurrently from multiple independent owners over the
transport's lifetime —

- the `Transport` struct itself, for `send()`'s `note_outbound_drop`/
  `report_error` (called from the CPU thread via `tick()`);
- the outbound-pump tokio task, for `note_outbound_drop`/`report_error`/
  `report_counts` (§4.1, §4.3);
- for multipoint (§4.2), *each* of the N independently-spawned
  `run_client_task`s, for `note_inbound_drop`.

Every owner above holds its own `.clone()` of the one `TransportReporter`
built at construction time; all clones share the same underlying counters
and `ErrorSender`, so counts/errors converge correctly regardless of which
clone incremented/reported them.

```rust
#[derive(Clone)]
pub struct TransportReporter { /* device_id: Arc<OnceLock<DeviceId>>, error_sender, Arc-wrapped drop counters */ }

impl TransportReporter {
    pub(crate) fn new(device_id: DeviceId, error_sender: Option<ErrorSender>) -> Self;

    /// Constructs a reporter whose `DeviceId` isn't known yet (§2.2) — for
    /// the `TransportSlot` injection path (§9.4), where the transport must
    /// be built before the device (and its `DeviceId`) exists. Every
    /// reporting call before `bind` is a silent no-op.
    pub(crate) fn pending(error_sender: Option<ErrorSender>) -> Self;

    /// Binds the `DeviceId` once the caller determines it (§2.2). Every
    /// existing `Clone` of this reporter — including ones already handed to
    /// background tasks that started before the ID was known — observes
    /// the bound ID from that point on, since it lives behind the same
    /// `Arc` as the counters/`ErrorSender`. Exactly-once, enforced by the
    /// underlying `OnceLock`.
    pub(crate) fn bind(&self, device_id: DeviceId);

    /// Reports a hard transport error. Currently only ever called with
    /// `TransportError::Io` (§1.8).
    pub fn report_error(&self, error: TransportError);

    /// Increments the outbound drop counter (§1.5). Every transport has one.
    pub fn note_outbound_drop(&self);

    /// Increments the inbound ingress-drop counter (§1.5). Multipoint only —
    /// P2P transports never call this.
    pub fn note_inbound_drop(&self);

    /// Swaps both counters to 0 and emits
    /// `DeviceEvent::OutboundBytesDropped`/`InboundEventsDropped` for any
    /// nonzero count. Called from the existing outbound-pump/ingress tokio
    /// tasks on a `tokio::time::interval` (§1.5) — except
    /// `InternalPipeTransport`, which has no tokio task and instead calls
    /// this synchronously from `send()` on some other trigger (§4.4).
    pub fn report_counts(&self);

    /// Reports a connect/disconnect edge (§5.1).
    pub fn report_connected(&self, peer: Option<String>);
    pub fn report_disconnected(&self, peer: Option<String>, reason: String);
}
```

Exact field/method shape (e.g. whether inbound counting should be a
separate type so P2P transports can't accidentally call
`note_inbound_drop`) is left to implementation judgment — the concrete
requirement this must satisfy is: one `TransportReporter` per transport,
built before the transport, carrying whatever `DeviceId`/`ErrorSender`/
counter state its constructor and background tasks need for the lifetime
of that transport.

### 2.2 `TransportReporter::pending`/`bind` — the `DeviceId`-comes-later case

**Found during design review**: the "allocate `device_id`, then build
`TransportReporter`, then construct the transport" ordering that every
`DeviceModule::instantiate` follows (§9.2) is inverted by `TransportSlot`
(§9.4) — `main.rs:26` calls `InternalPipeTransport::stdio()` *before*
`config.emulator.build_with_context(...)` is even called at `main.rs:36`,
and the console's `DeviceId` isn't allocated until `ConsoleModule::instantiate`
runs deep inside that call. At the point `stdio()`/`pair()` are called,
**no `DeviceIdAllocator` exists yet at all** — it's constructed internally
inside `build_devices` (`emulator.rs:141`), unreachable from `main.rs`.
`debugger/src-tauri/src/lib.rs`'s `load_session` (§9.4) has the identical
problem for `pair()`.

Resolution: `TransportReporter` supports being constructed without a
`DeviceId` and bound to one later —

- `TransportReporter::pending(error_sender)` constructs a reporter with no
  `DeviceId` yet (internally, `device_id: Arc<OnceLock<DeviceId>>`, unset).
  Every reporting method is a silent no-op until bound — acceptable because
  everything `TransportReporter` reports is diagnostic/best-effort (§1.5,
  §5.1), not a correctness signal, and the unbound window is short in
  practice (`main.rs`/`load_session` call `build_with_context` immediately
  after constructing the transport).
- `main.rs`/`load_session` call `InternalPipeTransport::stdio()`/`pair()`
  with a `TransportReporter::pending(...)`, exactly as they would with a
  normal one — the transport and its background machinery don't need to
  know whether their reporter is bound yet.
- `ConsoleModule::instantiate` — once it allocates `device_id` as it
  already does today (`console.rs:49`) — calls `reporter.bind(device_id)`
  on the `TransportReporter` obtained from the injected `TransportSlot`
  entry (threaded through alongside the transport and relay, §9.4). Every
  existing `Clone` of that reporter, including ones already handed to
  background tasks that started before the ID was known, observes the
  bound ID from that point on, since it lives behind the same `Arc` as the
  counters/`ErrorSender` (§2.1's `Clone` design already relies on this
  sharing).
- This is narrower in scope than reintroducing `set_error_sender`
  (§1.8) — only the `DeviceId` half is bound after construction, and only
  for the two call sites that structurally require it; every other
  transport still gets a fully-formed `TransportReporter` up front via
  `TransportReporter::new`.

---

## 3. `ChannelRelay<T>` (shared machinery — implement first)

This is the one new component every transport depends on. Implement and
unit-test it before touching any individual transport file.

`ChannelRelay<T>` is `pub`, not `pub(crate)` — unlike earlier drafts, there
is no separate `ByteRelay`/`TaggedRelay` wrapper (§2), so this is the type
that crosses crate boundaries directly (e.g. into `src/bin/emulator/`
and `debugger/src-tauri/`, §9.4). `T` is `u8` for P2P transports,
`TransportEvent` for multipoint.

**Updated during implementation of checklist item 3** — see §1.7's "Revised
during implementation" note. `ChannelRelay` gained an internal stop signal,
changing the shape below from what was originally sketched here:

```rust
pub struct ChannelRelay<T> {
    consumer: Consumer<T>,          // rtrb
    handle: Option<JoinHandle<()>>,
    stop: Option<Sender<()>>,       // None for from_parts; Some for spawn
}

impl<T: Send + 'static> ChannelRelay<T> {
    /// Spawns a thread that selects between `rx` and its own internal stop
    /// channel, pushing each item received from `rx` into a new `rtrb` ring
    /// of the given `capacity` via `push_and_park`. Exits when `rx`
    /// disconnects *or* when `Drop` signals it to stop. The shape needed by
    /// every transport except `InternalPipeTransport` (§4.4), which has no
    /// `crossbeam_channel` hop to receive from.
    pub(crate) fn spawn(rx: Receiver<T>, capacity: usize) -> Self;

    /// Wraps an already-running relay thread's `Consumer<T>`/`JoinHandle`
    /// directly, without spawning anything or requiring a `Receiver<T>`.
    /// For `InternalPipeTransport` (§4.4): its thread reads directly off a
    /// raw fd (no crossbeam channel in between) and pushes into its own
    /// `rtrb::Producer<T>` using [`push_and_park`], the same park/push
    /// retry loop `spawn`'s thread body uses internally — factored out so
    /// both call sites share it rather than duplicating the retry logic.
    /// The caller owns constructing the thread and the ring; this just
    /// takes ownership of the resulting handle and consumer so the
    /// caller's return type matches every other transport's
    /// `(Self, ChannelRelay<T>)` shape. Has no stop signal wired up yet —
    /// item 7 will need to design an equivalent interruption mechanism for
    /// a thread that blocks on a raw fd read rather than a channel recv.
    pub(crate) fn from_parts(consumer: Consumer<T>, handle: JoinHandle<()>) -> Self;

    fn pop(&mut self) -> Option<T>;
    fn unpark(&self);

    /// Drains all currently available items, then unparks the relay
    /// thread. Call once per `tick()`.
    pub fn drain_into(&mut self, f: impl FnMut(T));
}

/// Push `item` into `producer`, parking the current thread on
/// `PushError::Full` and retrying once unparked, until either the push
/// succeeds or `stop` signals shutdown while parked (in which case `item`
/// is dropped). Shared by `spawn`'s internal thread body and
/// `InternalPipeTransport`'s custom thread (§4.4), so the retry logic
/// exists in exactly one place.
pub(crate) fn push_and_park<T>(producer: &mut Producer<T>, item: T, stop: &Receiver<()>) -> bool;

// Drop: send the stop signal (if any), unpark (harmless if not parked),
// then join(). For a `spawn`-constructed relay this is always prompt,
// regardless of whether any Sender<T> feeding it is still alive — see the
// shutdown contract in §1.7 and in transport/mod.rs's module doc.
```

Only `spawn`/`from_parts` (construction) and `drain_into` (the
device-facing drain) are part of the public surface; `pop`/`unpark` are
private implementation details of `drain_into` now that there's no wrapper
type needing them.

Relay thread body (used by `spawn`): a `crossbeam_channel::Select` over `rx`
and the internal stop channel; on data, `push_and_park`s it (aborting the
loop if `push_and_park` reports a stop request); on the stop branch or `rx`
disconnecting, exits.

**Unit tests to include:**
- Round-trip: send N items via `Sender`, confirm `drain_into` yields them
  in order.
- Backpressure: fill the ring to capacity without draining, confirm the
  relay thread is parked (e.g. indirectly — sends beyond capacity don't
  panic or get lost; after a `drain_into` call frees space and unparks,
  subsequent items do arrive on the next `drain_into`).
- Shutdown: drop all `Sender`s, confirm `ChannelRelay::drop` returns
  promptly (bounded test timeout) rather than hanging.
- Shutdown with a live `Sender`: confirm `ChannelRelay::drop` is *still*
  prompt even when a `Sender<T>` is deliberately kept alive past the
  relay's drop — the scenario the internal stop signal exists for.

---

## 4. Per-Transport Rewrite Plan

Recommended implementation order: `ChannelRelay` → `PtyTransport` (P2P,
already prototyped in this conversation) → `UnixSocketTransport`
(multipoint, representative pair) → `TcpSocketTransport` (near-identical
to Unix socket — factor shared code once both exist) → `PipeTransport` →
`InternalPipeTransport`.

### 4.1 `PtyTransport` (P2P — prototyped; use as reference shape)

- `open()` signature becomes:
  ```rust
  pub fn open(symlink_path: Option<&Path>, reporter: TransportReporter)
      -> std::io::Result<(Self, ChannelRelay<u8>)>
  ```
- Spawns: (a) `ChannelRelay::spawn` over a plain `crossbeam_channel` fed by
  the existing `AsyncFd`-based tokio task; (b) that same tokio task also
  owns the outbound `rtrb::Consumer<u8>`, pops on `Notify`, writes to the
  master fd, calls `reporter.note_outbound_drop()` on overflow and
  `reporter.report_error(TransportError::Io(e))` on a hard write error, and
  calls `reporter.report_counts()` on a `tokio::time::interval`.
- `PtyTransport` struct retains: `outbound: Producer<u8>`,
  `outbound_notify`, `shutdown_tx`, `slave_path`, `_slave`,
  `client_connected: Arc<AtomicBool>`, `symlink_path`, plus a
  `reporter: TransportReporter` clone (§2.1) for `send()`'s own
  `note_outbound_drop`/`report_error` calls. The old standalone
  `outbound_dropped: Arc<AtomicU64>` field is **dropped** — that counter now
  lives inside `TransportReporter`, shared via `Clone` with the
  outbound-pump tokio task rather than duplicated as a separate field.
- **Remove entirely**: `TransportEvent::Connected/Disconnected` emission,
  the old `connection_counter`/unique-connection-ID tracking (`pty.rs:168`,
  incremented at `pty.rs:184` — confirmed dead: it's written but never
  read anywhere, not even internally, let alone externally), all
  `try_recv_tagged`-based polling.
- `is_connected()` stays driven by real read/EIO activity on
  `client_connected`, independent of any relay traffic (this was already
  true in the pre-redesign code and doesn't change).
- `send()` does not gate on `is_connected()` — PTY masters buffer writes
  in the kernel regardless of whether the slave is open; this existing
  behavior/rationale is preserved.
- Shutdown branch in the tokio task: `drop(in_tx)` first, then break.
- Full draft of this file was produced during design; use it as a
  starting point but re-verify against the final `ChannelRelay<u8>` API
  (§3) once it's actually implemented — some plumbing was referenced but
  not yet added to `mod.rs` at time of writing this plan.
- Test suite: full rewrite required. Old tests asserting
  `try_recv_tagged() == Some(TransportEvent::Connected(0))` etc. no longer
  apply. New tests should assert against `ChannelRelay<u8>::drain_into`. See
  drafted test module in design conversation for a starting point,
  **except** the `dropped_outbound_bytes_are_reported` test, which was
  identified as weak/racy — resolved in §7's "Resolved during design
  review" log: split into a plain `TransportReporter` unit test (no ring,
  fully deterministic) plus a `pub(crate) open_with_capacity(...)`
  constructor used only by this one test to force overflow with a
  capacity of 1–2 rather than relying on timing.

### 4.2 `UnixSocketTransport` / `TcpSocketTransport` (multipoint)

Not yet drafted in the design conversation — implement using the same
principles:

- `listen()` returns `(Self, ChannelRelay<TransportEvent>)` and takes a `reporter:
  TransportReporter` (§2.1), same as P2P. The struct itself retains a
  clone of `reporter` (same as `PtyTransport`, §4.1) for `send()`'s own
  `note_outbound_drop` call.
- Keep the existing `broadcast::channel` fan-out (`pump_outbound`) for
  outbound — but source it from `rtrb::Consumer<u8>` instead of
  `crossbeam_channel::Receiver<u8>`, with the same non-blocking-push +
  `reporter.note_outbound_drop()` + `reporter.report_counts()` pattern as
  P2P outbound. **Resolved**: `Transport::send` keeps checking `client_count`
  first — `if self.client_count.load(...) == 0 { return; }`, matching the
  existing early-out logic (`unix_socket.rs:64-66`) exactly, just without
  the `Result` — and this path must **not** call `reporter.note_outbound_drop()`.
  Unlike P2P's disconnected-send (§4.3, §4.4 — a transient edge away from a
  previously-active connection, correctly counted as a drop), multipoint's
  `client_count == 0` is often ordinary steady state (e.g. a debug console
  nobody has opened a terminal to yet, for the whole session). Counting
  every send during that idle period would leave the outbound-drop counter
  perpetually nonzero for a completely healthy, simply-unwatched device,
  defeating the diagnostic's purpose (§1.5: a nonzero count is only a
  useful signal if it's *not* the permanent baseline). Only pushes that
  overflow the ring *while at least one client is connected* count as
  drops.
- Keep `run_client_task`'s per-connection structure (`ClientSession`), but:
  - Change `in_tx.send(...)` to `in_tx.try_send(...)`, calling
    `reporter.note_inbound_drop()` on `Full` instead of a locally-declared
    counter.
  - On the shutdown branch specifically: `drop(in_tx)` first; skip the
    final `Disconnected` send on that branch — **confirmed safe**:
    `ProtocolManager::poll_transport` only does
    `self.slots.retain(|s| s.client_tag != tag)` on `Disconnected`
    (`src/emulator/device/protocol/manager.rs:105-107`), and neither
    `slots` nor the owning `TagAllocator` outlives the bus, so a skipped
    terminal event on whole-bus shutdown has no observable effect. Still
    decrement `client_count` and release the tag as before. (Individual
    client disconnects during normal operation are unaffected and still
    emit `Disconnected` as today.)
- `TagAllocator`, `ClientSession`, `pump_outbound`, `run_client_task`
  remain shared in `mod.rs` as today; only their internals change to match
  the new relay types and drop-counting.
- `reporter.report_counts()` covers both the outbound and inbound-ingress
  counters (§2.1) — no separate reporter task needed beyond what §2.1
  already centralizes.
- Once both Tcp and Unix variants are done, look for opportunities to
  further deduplicate — they were already near-identical before this
  redesign and will remain so after.
- Test suite: rewrite `concurrent_clients_are_tagged_and_counted` and
  friends against `ChannelRelay<TransportEvent>::drain_into` instead of
  `try_recv_tagged()`-in-a-loop. Add a test that forces `try_send` to fail
  (e.g. by not draining the relay/ring for a burst of sends) and asserts
  the ingress-drop counter increments and is reported.

### 4.3 `PipeTransport` (P2P)

Same shape as `PtyTransport`, adapted for child-process stdin/stdout
instead of a PTY master fd:

- `spawn()` returns `(Self, ChannelRelay<u8>)` and takes a `reporter:
  TransportReporter` (§2.1), same as `PtyTransport::open`. The struct
  itself retains a clone of `reporter` (same as `PtyTransport`, §4.1) for
  `send()`'s own `note_outbound_drop` call.
- `run_pipe_task`: inbound bytes from `stdout.read()` go to a
  `crossbeam_channel::Sender<u8>` feeding `ChannelRelay::spawn`; outbound
  bytes come from an `rtrb::Consumer<u8>` written to `stdin`, mirroring
  `drain_outbound`'s existing structure but against `rtrb` instead of
  `crossbeam_channel`, reporting drops/errors via `reporter` as in §4.1.
- **Remove** `TransportEvent::Connected/Disconnected` emission — same
  rationale as PTY, nothing downstream consumes it.
- **`is_connected()`'s current mechanism breaks under this redesign and
  must be replaced, not just left alone.** Today (`pipe.rs:28-103`),
  `connected: bool` only ever flips to `false` inside `try_recv` (line
  71-74) or `try_recv_tagged` (line 91-97), when one of those methods
  happens to observe a `TransportEvent::Disconnected` pulled off the
  bridge — the doc comment says as much outright: *"`is_connected` tracks
  the latest state seen via `try_recv_tagged` or `try_recv`"* (line 26-27).
  Once §2's `Transport` trait drops both methods entirely (replaced by
  draining a separate `ChannelRelay<u8>`), there is no remaining call site
  that ever observes the child exiting and updates `connected` — 
  `is_connected()` would report stale `true` forever after the child dies.
  This is unlike `InternalPipeTransport`, which already tracks connection
  state independently of its event stream (`mark_disconnected()` is called
  directly from `try_recv`/`send`'s own I/O logic, not from event-polling —
  `internal_pipe.rs:74-118` — so it's unaffected by this redesign) and
  unlike `PtyTransport`, which already uses the fix described below.
  **Fix**: switch to a background-task-owned `Arc<AtomicBool>` that
  `run_pipe_task` sets directly when it observes the child exiting —
  exactly the same information that already triggers `on_exit` and the
  (now-removed) `Disconnected` event send, just also stored independently
  of anything downstream polling for it. Apply the same
  `swap`-not-`store`/edge-triggered guard from §5.1's `PtyTransport` fix
  here too, since this same `Arc<AtomicBool>` is also `report_disconnected`'s
  wiring point (§5.1).
- **Keep** the `on_exit: F` callback and child-process exit-status
  handling as-is; this is orthogonal to the relay redesign.
- Shutdown branch: `drop(in_tx)` before calling `on_exit(...)` — this
  matters more here than for PTY, since `on_exit` may involve `child.wait()`
  which is not instant; the relay thread must not wait on that.
- **`send()` keeps its existing `if !self.connected { ... }` gate**, unlike
  PTY (§4.1, which never gates — kernel PTY masters buffer writes
  regardless of whether the slave is open, a real architectural reason that
  doesn't apply here). Once disconnected, the child's stdin pipe genuinely
  has no reader left, so skipping the write avoids a doomed syscall on
  every subsequent `send()`. The gate becomes: skip the write and call
  `reporter.note_outbound_drop()` (counted the same as a ring-overflow
  drop, per §1.5's diagnostic-only framing) instead of returning
  `Err(TransportError::Disconnected)`.
- Test suite: rewrite similarly to PtyTransport's.

### 4.4 `InternalPipeTransport` (P2P — the outlier)

This transport is the one case that can **collapse the read and the push
into a single blocking OS thread**, with no intermediate
`crossbeam_channel` at all, because its reads are already blocking OS
calls on a plain thread (not tokio-mediated):

- Drop the `O_NONBLOCK` fcntl dance and the `WouldBlock`-based polling
  entirely.
- Drop `connect_event_pending`/`disconnect_event_pending` bookkeeping
  entirely (no more `TransportEvent` participation).
- The struct retains the `reporter: TransportReporter` passed into its
  constructor directly — there's no separate background task here at all
  (that's the point of this section), so the struct itself is the only
  holder, used inline by `send()`'s own `note_outbound_drop`/`report_error`
  calls (§2.1).
- New shape: a plain thread does a blocking `rx.read()` loop directly
  against an `rtrb::Producer<u8>`, calling the shared `push_and_park` free
  function (§3) after each read instead of duplicating the park/push retry
  logic. The resulting `Consumer<u8>`/`JoinHandle` are wrapped via
  `ChannelRelay::from_parts` (§3) — **not** `ChannelRelay::spawn`, which
  requires a `crossbeam_channel::Receiver<T>` this transport never has,
  since it skips that hop entirely.
- Outbound: same `rtrb::Producer` + `TransportReporter` pattern as every
  other transport — **resolved**, no exception needed here (this was Open
  Question 3 in an earlier draft). Since error reporting no longer rides
  on `send`'s return value (§1.8), there's nothing left that's
  transport-specific about how errors get surfaced; `send()` can write
  directly and synchronously (no separate thread needed, since this
  transport's reads are already a blocking OS thread not mediated by
  tokio), calling `reporter.note_outbound_drop()` on a `WouldBlock`-style
  full condition and `reporter.report_error(TransportError::Io(e))` on a
  hard write failure, instead of returning `Result` as
  `tx.write_all()`-handling does today.
- **`send()` keeps its existing `if !self.connected { ... }` gate**, same
  decision and same rationale as `PipeTransport` (§4.3): once disconnected,
  the peer end genuinely has no reader, so skip the doomed write and call
  `reporter.note_outbound_drop()` instead of returning
  `Err(TransportError::Disconnected)`.
- **`reporter.report_counts()` needs a different trigger here than
  everywhere else.** §2.1 documents `report_counts()` as called "from the
  existing outbound-pump/ingress tokio tasks on a `tokio::time::interval`"
  — every other transport has such a task (PTY/`PipeTransport`'s outbound
  pump, multipoint's `pump_outbound`), but this transport deliberately has
  **no tokio task at all** (that's the point of this section). Spinning one
  up just to host a timer would reintroduce the async-task machinery this
  transport exists to avoid. Instead, call `reporter.report_counts()`
  synchronously from within `send()` itself — e.g. every N calls, or on
  transition into a nonzero drop count — trading wall-clock cadence for
  call-volume cadence for this one transport. Acceptable because the
  counter is diagnostic-only (§1.5) either way, and this transport (a
  process-internal pipe, no OS socket/PTY buffering or network peer in the
  way) is the least likely of any transport to actually see outbound
  drops.
- `stdio()`/`from_raw_fds()` construct a single instance each, so both
  return `(Self, ChannelRelay<u8>)`.
- `pair()` is asymmetric, not `((Self, ChannelRelay<u8>), (Self, ChannelRelay<u8>))`:
  today it returns two *symmetric* instances (`pair() -> io::Result<(Self,
  Self)>`), but only one end (`local`, attached to a device as an actual
  `Transport`) is ever used through the `Transport` trait in current call
  sites — the other end (`remote`) is immediately consumed via
  `into_split(self) -> (File, File)` (`internal_pipe.rs:53`), which
  bypasses `Transport`/relay machinery entirely to hand back raw `File`
  handles for direct fd-level I/O (see §9.4 — the debugger's terminal
  bridge does exactly this). Giving `remote` a `ChannelRelay<u8>` it never drains
  would spin up a `ChannelRelay` thread that's pure waste at best and races
  `into_split()`'s own raw reads at worst. So: `pub fn pair() ->
  io::Result<((Self, ChannelRelay<u8>), Self)>` — only `local` gets a relay;
  `remote` stays a bare `Self`, unchanged, still supporting `into_split()`.
- Test suite: rewrite against `ChannelRelay<u8>::drain_into`.

---

## 5. `DeviceEvent` Additions and Changes

Add two new variants alongside the existing `DeviceEvent::TransportError`:

```rust
DeviceEvent::OutboundBytesDropped { device: DeviceId, count: u64 }
DeviceEvent::InboundEventsDropped { device: DeviceId, count: u64 }
```

Both are diagnostic-only (§1.5) — do not let any device logic branch on
these events; they are for surfacing to whatever UI/log the user monitors,
with the expected remediation being "stop, resize the ring buffer, restart."

### 5.1 Wiring up `TransportConnected`/`TransportDisconnected` (existing,
currently unused)

**Discovered during design review**: `DeviceEvent::TransportConnected {
device: DeviceId }` and `DeviceEvent::TransportDisconnected { device:
DeviceId, reason: String }` (`device/mod.rs:44-53`) already exist and are
already handled distinctly from `TransportError` by `main.rs:69-79`
("device {} connected" / "device {} disconnected: {}"). But **nothing in
production ever constructs either variant** — `TransportConnected` is only
ever sent from a unit test (`device/mod.rs:147`, pure channel-plumbing),
and `TransportDisconnected` is never sent at all. This is a fully-designed,
UI-wired, never-connected-to-a-producer feature.

This changes the resolution for what was an Open Question about
`TransportError::Disconnected` (§1.8) — rather than inventing a new
error-reporting path for disconnects, wire transport connect/disconnect
transitions into *this* existing pair instead, edge-triggered (fire once
per transition, not once per `send()` attempted while down, per the
original motivation in §1.8):

- Add `peer: Option<String>` to both variants:
  ```rust
  TransportConnected { device: DeviceId, peer: Option<String> }
  TransportDisconnected { device: DeviceId, peer: Option<String>, reason: String }
  ```
  `peer` disambiguates multipoint's N-client case — each client's
  connect/disconnect gets its own event, individually attributable, rather
  than one ambiguous device-level signal that can't distinguish "one of
  three clients left" from "the whole transport went down." No
  `client_count`-edge bookkeeping needed; every per-client transition just
  reports.
- **P2P**: `peer: None` — single peer, nothing to disambiguate. Reported at
  the existing connected→disconnected edge (`client_connected`/`connected`
  state each P2P transport already tracks for `is_connected()`).
  **`PtyTransport` needs a specific guard, not a naive wire-up**:
  `run_pty_task` (`pty.rs:161-206`) already edge-triggers the *connect*
  side correctly — `if !client_connected.load(...) { ...;
  client_connected.store(true, ...) }` (`pty.rs:183-186`) only fires on the
  false→true transition. But `client_connected` gets set back to `false`
  in **two** places: the EIO branch (`pty.rs:194`, a genuine disconnect)
  and the task-exit path (`pty.rs:204`, which runs unconditionally on every
  shutdown, including when no client was ever attached at all). Calling
  `reporter.report_disconnected(None, reason)` naively at both `store(false,
  ...)` sites would report a spurious disconnect at `pty.rs:204` even when
  nothing was ever connected. Fix: use
  `client_connected.swap(false, Ordering::Release)` at both sites instead
  of `store`, and only call `report_disconnected` when the swap returns
  `true` — the same "was it previously true" guard the connect side already
  has, applied symmetrically to the disconnect side. Every other P2P
  transport's connect/disconnect tracking should be audited for the same
  pattern during implementation.
- **Multipoint**: `peer: Some(name)`, reported per-client at accept and at
  that client's disconnect — naturally aligned with the existing per-client
  `TransportEvent::Connected(tag)`/`Disconnected(tag)` lifecycle
  `run_client_task` already observes (§1.1), just also surfaced via
  `DeviceEvent` now. **The peer-naming mechanism differs by socket type**
  (resolved during design review, §7):
  - TCP: `TcpStream::peer_addr()` gives a real, meaningful `SocketAddr`
    (`IP:port`) — straightforward.
  - Unix domain sockets: `UnixStream::peer_addr()` is typically *unnamed*
    for client-side sockets (most UDS clients connect from an unbound
    socket) — it won't usually produce anything useful. Resolved: use
    `peer_cred()` (giving PID/UID/GID, stable on `tokio::net::UnixStream`)
    as the meaningful per-client identifier, formatted as e.g.
    `"pid=1234 uid=1000"`; fall back to the connection's `conn_tag`
    (already unique per session, just not very human-readable, e.g.
    `"conn#7"`) if `peer_cred()` errors.
  - Since `run_client_task` (`transport/mod.rs:225-238`) is generic over
    already-split `R`/`W` streams and has no access to the original
    `TcpStream`/`UnixStream` (needed for `peer_addr()`/`peer_cred()`), the
    peer name must be captured at the accept site in `tcp_socket.rs`/
    `unix_socket.rs` *before* splitting, and threaded into `ClientSession`
    (`transport/mod.rs:211-218`) as a new field alongside `conn_tag`.
- `TransportReporter` (§2.1) gains `report_connected`/`report_disconnected`
  — called from wherever each transport already tracks its
  connect/disconnect edge (P2P) or from `run_client_task`/accept
  (multipoint) — same `TransportReporter` clone (§2.1) every other
  reporting call already uses, no new plumbing beyond these two methods.
- **`main.rs:72,74` needs updating, not just the enum definition.** Its
  existing match arms destructure `DeviceEvent::TransportDisconnected {
  device, reason }` and `DeviceEvent::TransportConnected { device }`
  without `..` — adding `peer: Option<String>` to both variants breaks this
  compilation until these two arms are updated to account for the new
  field (e.g. include it in the printed message when `Some`).

---

## 6. `Bus::drop` Wiring

**There is currently no attachment point for this at all** — confirmed
during design review, not assumed: `Bus` has no `Drop` impl today
(`src/emulator/bus/mod.rs`), and `IoDevice` (`device/mod.rs:91-134`)
exposes no method that reaches a device's privately-held `Transport`/
`ProtocolManager`. This section is therefore not just "wire shutdown into
an existing path" — it requires adding the path itself:

- **`IoDevice` gets a new method**: `fn shutdown(&mut self) {}` (default
  no-op, so devices without a transport need no changes). Every device that
  owns a `Transport` directly (`Console`, `R6551`, `Mc6850`) overrides it to
  call `self.transport.shutdown()`.
- **`ProtocolManager<T>` gets its own `shutdown(&mut self)`**, forwarding to
  its owned `Transport::shutdown()`. Devices that own a `ProtocolManager`
  (`Via6522`, `Mc6840`) override `IoDevice::shutdown` to call
  `self.protocol_manager.shutdown()` — `ProtocolManager`'s internals stay
  private either way; the device never reaches through it to a raw
  `Transport`.
- **`impl Drop for Bus`** iterates `self.devices`, calling
  `device.shutdown()` on each, *before* Rust's normal field-drop order runs
  the devices' own `Drop` impls. This ordering is the whole point: a
  device's `shutdown()` call signals the owning tokio task (oneshot/watch,
  as today) to start its shutdown branch and drop `in_tx` first (§1.7), and
  that needs a chance to happen *before* the same device's `Drop` reaches
  its `ChannelRelay` field and blocks on `join()`. Concretely, in the
  correct order:
  1. `Bus::drop` calls `shutdown()` on every device (signals all transports
     at once, doesn't block on any of them individually).
  2. Each transport's owning tokio task drops its `in_tx`(s) as the first
     action on its shutdown branch (§1.7) — a code-level requirement on
     every `run_*_task`, not something `Bus::drop` itself can enforce;
     audit each rewritten transport against this requirement explicitly
     during review.
  3. Normal field-drop then runs each device's `Drop`, which drops its
     `ChannelRelay`, blocking (briefly, per §1.7's bound) on the relay
     thread's `join()`.
  4. Confirm no other resource (fds, sockets, symlinks) is stranded — e.g.
     `PtyTransport`'s symlink cleanup, `UnixSocketTransport`'s socket file
     cleanup.

Add an integration-level test (constructing and dropping a `Bus` with at
least one of each transport category attached) to confirm teardown
actually completes and doesn't hang, since this is precisely the failure
mode this whole shutdown-ordering exercise exists to prevent.

---

## 7. Open Questions (resolve before or during implementation, not after)

None remaining — all resolved during design review (see log below).

### Resolved during design review (kept here for traceability)

- ~~Unix domain socket peer naming~~ (§5.1) — resolved: use
  `tokio::net::UnixStream::peer_cred()` (stable, Linux-only target),
  called at accept time in `unix_socket.rs` before the stream is split,
  formatted as e.g. `"pid=1234 uid=1000"` for the `peer` field. Falls back
  to `conn_tag` (e.g. `"conn#7"`) if `peer_cred()` errors, so a naming
  failure never blocks the connection. TCP keeps `peer_addr()`'s
  `SocketAddr` as before, unaffected by this decision.

- ~~Report interval (1s default)~~ — confirmed purely diagnostic (§1.5)
  with no correctness implication either way; keep the 1s default,
  hardcoded, no per-transport configurability. Revisit only if real usage
  shows a need, at which point it's a small follow-up, not a redesign.

- ~~Outbound ring capacity for multipoint under MC6840-style fanout
  amplification~~ (§1.4) — the premise didn't hold up: `pump_outbound`
  sources from **one** shared `rtrb::Consumer<u8>` per transport; the
  N-way client fanout happens downstream in the existing
  `broadcast::channel` (`BROADCAST_CAPACITY`, `unix_socket.rs`/
  `tcp_socket.rs`), which this redesign doesn't touch. A tick never pushes
  more than once per byte into the new ring regardless of client count —
  confirmed against `Mc6840::send_state_to_all`/`ProtocolManager::send_to_all`
  (`mc6840.rs:569-572`, `manager.rs:67-74`), which encode at most 3 small
  state-change messages once per tick and push each byte exactly once, not
  once per connected peripheral. No per-transport configurable capacity or
  size increase needed; keep the existing default.
- ~~Deterministic testing of outbound drop-counting~~ — splits into two
  independent pieces once `TransportReporter`'s actual shape (§2.1) is
  accounted for. (a) Counter-increment (`note_outbound_drop`) plus
  `report_counts()`'s `DeviceEvent::OutboundBytesDropped` emission is pure
  logic with no ring or background thread involved at all — unit-test it
  directly against a bare `TransportReporter` (construct, call
  `note_outbound_drop()` N times, call `report_counts()`, assert the
  event and its count on the `ErrorSender`'s receiver), fully
  deterministic with no new machinery needed. (b) Only the per-transport
  "does a real ring overflow actually reach `note_outbound_drop()`" wiring
  test is racy, since public constructors (`PtyTransport::open()` etc., §4.1)
  don't expose ring capacity. Resolved: add a `pub(crate)`-only
  capacity-override constructor per transport (e.g.
  `PtyTransport::open_with_capacity(symlink_path, reporter, capacity)`,
  with `open()` calling it with the real default), used only by that one
  wiring test per transport to force overflow with a capacity of 1 or 2
  instead of relying on timing against the concurrently running outbound
  pump task.

- ~~Trait split (`TransportControl`/`PointToPointTransport`/
  `MultiClientTransport`) and the `Transport`→`TransportControl` rename~~ —
  reverted; no behavioral justification found for either (§2).
- ~~Skipping the final `Disconnected` send on the shutdown path for
  multipoint `run_client_task`~~ — confirmed safe; `ProtocolManager` only
  retains/filters its `slots` on that event and nothing outlives whole-bus
  teardown (§4.2).
- ~~`InternalPipeTransport` outbound writes: rtrb+drop-counter vs.
  synchronous `Result`~~ — resolved to the same rtrb+`TransportReporter`
  pattern as every other transport, once error reporting stopped riding on
  `send`'s return value (§1.8, §4.4).
- ~~`connection_counter` in `PtyTransport`~~ — confirmed dead: written
  (`pty.rs:184`) but never read anywhere, internally or externally. Safe
  to remove outright (§4.1).
- ~~`ByteRelay`/`TaggedRelay` wrapper structs~~ — dropped; they'd have been
  two byte-for-byte-identical wrappers around `ChannelRelay<T>` differing
  only in `T`, the same problem as the reverted trait split. Everything
  now uses `ChannelRelay<u8>`/`ChannelRelay<TransportEvent>` directly (§2,
  §3).
- ~~`ChannelRelay::spawn`'s `Receiver<T>`-only constructor didn't fit
  `InternalPipeTransport`'s collapsed single-thread design~~ — added
  `ChannelRelay::from_parts` plus a shared `push_and_park` free function
  (§3), so `InternalPipeTransport` can build its own thread/ring without a
  crossbeam-channel hop and still return a `ChannelRelay<u8>` like every
  other transport (§4.4).
- ~~`reporter.report_counts()` has nothing to hang a `tokio::time::interval`
  off of for `InternalPipeTransport`~~ (it has no tokio task at all) —
  resolved: call it synchronously from `send()` on some other trigger
  (every N calls, or on transition into nonzero), trading wall-clock
  cadence for call-volume cadence for this one transport only (§2.1, §4.4).
- ~~`TransportReporter` was passed by value into each transport's
  constructor with no way for the multiple concurrent owners that need
  it (the `Transport` struct itself, the outbound-pump task, and — for
  multipoint — each of N `run_client_task`s) to all get access~~ — resolved:
  `TransportReporter` is `Clone` (internally `Arc`-wrapped counters +
  cloned `ErrorSender`); each owner holds its own clone (§2.1). This also
  surfaced a stale leftover in §4.1's `PtyTransport` struct-retains list —
  a standalone `outbound_dropped: Arc<AtomicU64>` field that duplicated
  what `TransportReporter` now owns centrally; dropped in favor of the
  struct holding a `TransportReporter` clone directly (§4.1).
- ~~`TransportError::Disconnected` reporting~~ (previously deferred, §1.8)
  — resolved by discovering `DeviceEvent::TransportConnected`/
  `TransportDisconnected` already exist, fully wired into `main.rs`'s event
  loop, but are never constructed by any production code. Wire transport
  connect/disconnect edges into these instead of inventing new
  `TransportError` reporting, with a new `peer: Option<String>` field on
  both variants to disambiguate multipoint's N clients (§5.1).
- ~~`PtyTransport`'s exit path would report a spurious disconnect on every
  shutdown, even when no client was ever attached~~ — `run_pty_task`'s
  task-exit path unconditionally sets `client_connected` false; fixed by
  `swap`-not-`store` plus a "was it previously true" guard, mirroring the
  connect side's existing edge-trigger (§5.1).
- ~~`PipeTransport::is_connected()`'s entire mechanism depends on
  `try_recv`/`try_recv_tagged`, both removed by this redesign~~ — found
  during review to be a real break, not just a design nicety:
  `is_connected()` would report stale `true` forever after the child exits,
  since nothing else in `PipeTransport` ever updates `connected`. Fixed by
  switching to a background-task-owned `Arc<AtomicBool>`, the same pattern
  `PtyTransport`/`InternalPipeTransport` already use (§4.3).
- ~~Neither `PipeTransport` nor `InternalPipeTransport`'s existing
  `if !self.connected { return Err(...) }` `send()` gate had a stated
  disposition once `send()` stops returning `Result`~~ — resolved: both
  keep the gate (unlike PTY, which never gates, for a real architectural
  reason that doesn't apply to a pipe with a genuinely gone peer), skipping
  the doomed write and counting it as an outbound drop via
  `reporter.note_outbound_drop()` instead (§4.3, §4.4).
- ~~`TransportReporter` needs a `DeviceId` at construction, but `main.rs`/
  `load_session` construct the console's transport before any `DeviceId`
  — or the `DeviceIdAllocator` that produces one — exists~~ — resolved:
  `TransportReporter::pending`/`bind` (§2.2), letting these two call sites
  build a reporter with no `DeviceId` yet and bind it once
  `ConsoleModule::instantiate` allocates one, via the same `Arc`-shared
  state the `Clone` design already relies on. `TransportSlot` widens to a
  three-tuple (transport, relay, reporter) to carry it through (§9.4).
- ~~Whether multipoint's `client_count == 0` send should count as an
  outbound drop, mirroring P2P's decision to count sends-while-disconnected~~
  — resolved to the opposite answer: stays a silent no-op (§4.2). P2P's
  disconnect is a transient edge away from an active connection (correctly
  counted); multipoint's zero-clients state is often ordinary steady state
  (e.g. nobody's connected a terminal yet), so counting it would leave the
  drop counter perpetually nonzero for a healthy, simply-unwatched device.

---

## 9. Ripple Effects: `IoDevice`, `DeviceModule`, and `ProtocolManager`

Earlier drafts of this plan were written with only partial visibility into
the device layer and treated this as a `src/emulator/transport/`-internal
change. It is not: every device that owns a transport, and every
`DeviceModule` that constructs one, needs updating to match the new
`Transport`/relay/`TransportReporter` shapes. Call this out explicitly as
in-scope work, not a followup discovered mid-implementation.

### 9.1 `IoDevice` implementations (`Console`, `R6551`, `Mc6850`, and any other
device holding a transport directly)

- Each currently holds `transport: Option<Box<dyn Transport>>` and polls it
  in `tick()` via `try_recv()`/`try_recv_tagged()`. Since construction now
  returns `(Transport, Relay)` (§2) and transports drop
  `try_recv()`/`try_recv_tagged()` entirely, the device needs to hold both
  the `Box<dyn Transport>` (for `send`/`is_connected`/`shutdown`) and its
  paired `ChannelRelay<u8>`/`ChannelRelay<TransportEvent>` as separate fields, and `tick()` must
  drain the relay via `drain_into(...)` instead of polling.
- `send()` call sites collapse from
  ```rust
  if let Some(transport) = self.transport.as_mut()
      && let Err(e) = transport.send(value) {
      (self.report_error)(e);
  }
  ```
  to
  ```rust
  if let Some(transport) = self.transport.as_mut() {
      transport.send(value);
  }
  ```
- The `report_error`/`error_reporter: Box<dyn Fn(TransportError) + Send>`
  field and the `set_error_sender(&mut self, sender: ErrorSender, id:
  DeviceId)` method become dead weight and should be **removed** from each
  device — error reporting is now entirely the transport's responsibility,
  wired in at construction (§1.8), not attached to the device afterward.
- Each such device overrides the new `IoDevice::shutdown(&mut self)` (§6)
  to forward to `self.transport.shutdown()`.

### 9.2 `DeviceModule::instantiate` implementations (`R6551Module`,
`Mc6850Module`, `ConsoleModule`, etc.)

- Today's sequence — allocate `device_id`, build the transport via
  `spec.to_transport_with_reporter(context.pipe_exit_reporter(device_id))`,
  construct the device, call `dev.set_error_sender(sender.clone(),
  device_id)`, register on the bus — becomes: allocate `device_id`, build a
  `TransportReporter` from `context` + `device_id` (generalizing
  `pipe_exit_reporter` into something reusable for the transport's whole
  lifetime, not just process-exit — §1.8, §2.1), construct the transport
  (now returning `(Transport, Relay)`), pass **both** into the device's
  constructor, register on the bus. `set_error_sender` disappears from
  every module's instantiate path.
- `InstantiationContext::pipe_exit_reporter` (`registry.rs:32-42`) either
  gets replaced outright by the general-purpose `TransportReporter`
  constructor, or narrows to just the child-process-exit case if that
  remains a distinct need for `PipeTransport`/`InternalPipeTransport`
  beyond what `TransportReporter` covers — decide during implementation
  rather than carrying both mechanisms side by side.

### 9.3 `ProtocolManager` (used by `Via6522` and other multi-tag devices,
e.g. MC6840)

- `ProtocolManager` currently owns a `transport: Box<dyn Transport>`
  directly and polls it itself (`poll_transport`, calling
  `try_recv_tagged()` internally). Since transports drop
  `try_recv_tagged()`/`try_recv()` entirely (§2), `ProtocolManager` owns
  the `ChannelRelay<TransportEvent>` directly too, exactly as it owns `Box<dyn Transport>`
  today — the owning device (`Via6522`, `Mc6840`) has no reason to hold the
  relay itself, and both halves of `listen()`'s `(Transport, ChannelRelay<TransportEvent>)`
  pair go straight into `ProtocolManager`'s constructor. `poll_transport`
  restructures from "poll the transport" to "drain the relay."
- `send_to_all`/`send_all_to_all`/`poll_transport` currently return
  `Result<_, TransportError>`, matching today's `Transport::send`. These
  should drop `Result` to match the new infallible `send()` — any error
  reporting they did via the caller's `report_error` closure moves to
  whatever `TransportReporter` the underlying transport was constructed
  with (§1.8), same as every other device.
- `ProtocolManager<T>` gets its own `shutdown(&mut self)`, forwarding to
  its owned `Transport::shutdown()` (§6). `Via6522`/`Mc6840` override
  `IoDevice::shutdown` to call `self.protocol_manager.shutdown()` rather
  than reaching through it to a raw transport.
- **`poll_transport`'s "one message per call" contract doesn't survive the
  move to `drain_into`.** Today (`manager.rs:85-111`), `poll_transport`
  loops `while let Some(event) = self.transport.try_recv_tagged()` and
  `return`s as soon as *one* message decodes, deliberately leaving any
  further already-available events unconsumed until the next call (see
  `data_is_demultiplexed_per_tag`, `manager.rs:236-251` — two fully-queued
  messages take two calls to retrieve). `ChannelRelay<TransportEvent>::drain_into` has no
  partial-drain mode — it always drains everything currently buffered in
  one pass. That "let the caller partially drain" motivation no longer
  applies (nothing in `drain_into`'s design supports it), and no current
  device implementation actually depends on the one-at-a-time behavior. So
  `poll_transport` changes shape: one `drain_into` call feeds every
  available event through decode/dispatch, and the method returns **all**
  newly-decoded messages from that call (e.g. `Vec<T>` or similar) instead
  of `Option<T>` for just the first. Callers (`Via6522`, `Mc6840`) take
  whatever they get in one pass rather than being throttled to one message
  per `tick()`; the existing manager tests that assert one-message-per-call
  need rewriting to assert against the full batch instead.

### 9.4 `Console`'s `TransportSlot`/`console_transport` injection path

`TransportSlot` (`registry.rs:12`, `Arc<Mutex<Option<Box<dyn Transport>>>>`)
is a second, independent way a transport reaches a device — used only by
`Console`, and the mechanism behind every production call site that builds
a transport entirely outside `DeviceModule::instantiate`/`TransportReporter`.
There are two such call sites, both following the same shape:

- **`src/bin/emulator/main.rs:26-34`** calls `InternalPipeTransport::stdio()`
  directly, boxes the result, and stores it in a `TransportSlot`. `main.rs:52`
  also inspects whether the slot was consumed (`stdio_in_use`) to decide
  whether to put the terminal into raw mode.
- **`debugger/src-tauri/src/lib.rs:182-205`** (`load_session`) calls
  `InternalPipeTransport::pair()`, stores the `local` end in a
  `TransportSlot` the same way, and returns the `remote` end directly to
  its caller. At the call site (`lib.rs:1258-1260`), `remote.into_split()`
  hands back raw `(File, File)` read/write handles, which
  `run_terminal_bridge` (`lib.rs:208-231`, via `AsyncFd`) and
  `write_terminal` (`lib.rs:243-246`, via `TerminalTx`) drive directly —
  `remote` never goes through `Transport::send`/`try_recv` at all. Per
  §4.4, this is exactly why `pair()`'s asymmetric signature matters here:
  `remote` must stay a bare `InternalPipeTransport` with no `ChannelRelay<u8>` of
  its own, since nothing would ever drain one.

Both call sites need the same update: once `InternalPipeTransport::stdio()`
returns `(Self, ChannelRelay<u8>)` and `pair()` returns `((Self, ChannelRelay<u8>), Self)`
(§4.4), the `TransportSlot` path needs to carry the `local`/`stdio` relay
**and its `TransportReporter`** through alongside the transport — the
reporter can't be built with a real `DeviceId` here (§2.2: neither call
site has one yet), so both use `TransportReporter::pending(error_sender)`:

- `TransportSlot` becomes
  `Arc<Mutex<Option<(Box<dyn Transport>, ChannelRelay<u8>, TransportReporter)>>>`
  (or an equivalent small struct) — three pieces now, not two.
- `Console::attach_transport` takes all three, matching every other
  device's new shape (§9.1).
- `main.rs:26-34` constructs `TransportReporter::pending(None)` (no
  `error_sender` is threaded into `main.rs`'s `InstantiationContext` today
  either — unchanged), passes it into `stdio()`, and stores
  `(transport, relay, reporter)` in the slot. `load_session`
  (`lib.rs:191-194`) does the same with `pair()`, storing
  `(local, relay, reporter)` and returning `remote` unchanged.
- `ConsoleModule::instantiate` (`console.rs:58-61`), once it has allocated
  `device_id` as it already does today, calls `reporter.bind(device_id)`
  (§2.2) on the reporter it pulls out of the `TransportSlot`, alongside
  attaching the transport and relay.
- The three `console.rs` unit tests that build a slot directly
  (`instantiate_with_injected_transport`, `injected_transport_is_consumed`,
  `injected_transport_ignored_when_transport_spec_is_set`, all via
  `InternalPipeTransport::pair()`) need the same widening.

---

## 10. Suggested Implementation Order (checklist)

- [x] `ChannelRelay<T>` in `mod.rs` (`pub`, with `drain_into`,
      `from_parts`, and the shared `push_and_park` free function), with
      unit tests (§3). Revised while implementing item 3: gained an
      internal stop signal, replacing the original "senders must be
      dropped first" contract — see §1.7 and §3's "Updated during
      implementation" notes.
- [x] Single `Transport` trait + `TransportReporter` (§2, §2.1)
- [x] `PtyTransport` rewrite + tests (§4.1) — reference implementation.
      Deviates from the literal spec in one deliberate way: `try_recv()` is
      kept on the trait as a documented no-op stub rather than migrating any
      device to drain the returned `ChannelRelay<u8>` directly — R6551 and
      Mc6850 (not Console) turn out to be the ones that default to a PTY
      transport (`src/bin/emulator/config.rs`), so that migration is
      deliberately deferred to item 12 (IoDevice migration) rather than
      expanding this unit's scope. `TransportSpec::to_transport`'s Pty branch
      leaks the relay it gets back (`std::mem::forget`) rather than
      threading it anywhere, since nothing consumes it yet and dropping it
      would deadlock (join waits on a sender the generic call site has no
      way to close). Accepted per-user direction: prioritize compileable,
      narrowly-scoped, reviewable units over preserving PTY input at every
      commit on this branch.
- [ ] `UnixSocketTransport` rewrite + tests (§4.2) — reference multipoint
      implementation, including ingress `try_send` + drop counter
- [ ] `TcpSocketTransport` rewrite + tests (§4.2) — should mostly mirror
      Unix socket; look for shared-code opportunities
- [ ] `PipeTransport` rewrite + tests (§4.3)
- [ ] `InternalPipeTransport` rewrite + tests (§4.4)
- [x] `DeviceEvent::OutboundBytesDropped` / `InboundEventsDropped` (§5)
- [ ] `DeviceEvent::TransportConnected`/`TransportDisconnected` peer-field
      wiring (§5.1) — `peer: Option<String>` field and `main.rs` match-arm
      updates landed early (alongside `TransportReporter`, §2.1, since its
      `report_connected`/`report_disconnected` signatures needed the field to
      compile); still remaining: capture peer name at accept time in
      `tcp_socket.rs`/`unix_socket.rs` and thread through `ClientSession`,
      and actually call `report_connected`/`report_disconnected` from real
      transports
- [ ] `IoDevice::shutdown()` (default no-op) + `ProtocolManager::shutdown()`
      + `impl Drop for Bus` calling `shutdown()` on every device before
      normal field-drop runs + integration test (§6)
- [ ] `ProtocolManager`: drain a `ChannelRelay<TransportEvent>` instead of polling
      `try_recv_tagged()`; drop `Result` from `send_to_all`/
      `send_all_to_all`/`poll_transport` (§9.3)
- [ ] `IoDevice` implementations (`Console`, `R6551`, `Mc6850`, `Via6522`,
      and any other transport-owning device): hold transport + relay
      separately, drain the relay in `tick()`, drop the now-dead
      `report_error`/`set_error_sender` plumbing (§9.1)
- [ ] `DeviceModule::instantiate` implementations: build `TransportReporter`
      before constructing the transport, thread the returned relay into the
      device constructor, drop the `set_error_sender` call (§9.2)
- [ ] `TransportReporter::pending`/`bind` (§2.2), for the two call sites
      that must construct a transport before any `DeviceId` exists
- [ ] `TransportSlot`/`console_transport` injection path (`registry.rs`,
      `console.rs`, `main.rs`, `debugger/src-tauri/src/lib.rs`): widen to
      carry `(Box<dyn Transport>, ChannelRelay<u8>, TransportReporter)`;
      `pair()`'s asymmetric return needs `debugger/src-tauri`'s
      `load_session`/`into_split()` usage re-verified against the final
      signature (§9.4)
- [x] All Open Questions (§7) resolved during design review; no
      remaining decisions deferred to implementation time
- [ ] Documentation cleanup pass, once everything above is implemented: the
      source code will almost certainly outlive this plan document, so
      remove every reference to this plan's section numbers (e.g. "§1.7",
      "§4.1") from doc comments added during this work, replacing each with
      either a self-contained explanation or a proper intra-doc
      cross-reference link to the relevant item/module in the source itself
      (PR #227 review)
