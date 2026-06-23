Debugger Terminal Architecture
==============================

This document describes how the Xterm.js terminal UI component interfaces
with the emulator's PipeTransport to provide the debugger's virtual
terminal window.

Design Goals
------------

- Low-latency display output — characters produced by the emulated CPU
  appear in the terminal with minimal delay.
- Responsive keyboard input — keystrokes in the terminal window reach
  the emulated console device promptly.
- No blocking — neither the emulated CPU nor the UI thread stalls waiting
  on the other side.
- Clean integration with Tauri 2's multi-window and async command model.

Component Overview
------------------

```
┌────────────────────────────────┐       ┌───────────────────────────────────────┐
│     Terminal Window (WebView)   │       │           Tauri Backend (Rust)         │
│                                │       │                                       │
│  ┌──────────────────────────┐  │       │  ┌─────────────────────────────────┐  │
│  │       Xterm.js            │  │       │  │        Terminal Bridge           │  │
│  │                          │  │       │  │                                 │  │
│  │  onData ─────────────────────────────────▶ write_terminal(bytes)         │  │
│  │                          │  │  IPC  │  │         │                       │  │
│  │  term.write(bytes) ◀─────────────────────── event: "terminal-output"     │  │
│  │                          │  │       │  │         │                       │  │
│  └──────────────────────────┘  │       │  │         ▼                       │  │
│                                │       │  │   PipeTransport (remote end)    │  │
└────────────────────────────────┘       │  │     tx: write ──┐              │  │
                                         │  │     rx: read  ◀─┼──────┐       │  │
                                         │  └─────────────────┼──────┼───────┘  │
                                         │                    │      │          │
                                         │                    ▼      │          │
                                         │  ┌─────────────────────────────────┐  │
                                         │  │   OS Pipe (kernel buffer)       │  │
                                         │  └─────────────────────────────────┘  │
                                         │                    │      ▲          │
                                         │                    ▼      │          │
                                         │  ┌─────────────────────────────────┐  │
                                         │  │  Console Device (on Bus)        │  │
                                         │  │    PipeTransport (local end)    │  │
                                         │  │      rx: read ◀─┘              │  │
                                         │  │      tx: write ─┘              │  │
                                         │  │                                 │  │
                                         │  │  (called by CPU via IoDevice    │  │
                                         │  │   read/write during execution)  │  │
                                         │  └─────────────────────────────────┘  │
                                         └───────────────────────────────────────┘
```

Data Flow
---------

### Display Output (Console → Terminal)

1. The emulated CPU executes a store instruction to the console device's
   data register (offset 0).
2. `Console::write()` calls `transport.send(byte)` on the local end of
   the PipeTransport, which performs a non-blocking write into the OS
   pipe's kernel buffer.
3. The **Terminal Bridge** (a Tokio task in the Tauri backend) holds the
   remote end of the PipeTransport. It runs an async poll loop that
   reads available bytes from the remote end's `rx` file descriptor
   using `tokio::io::AsyncReadExt` (after wrapping the non-blocking fd
   in an `AsyncFd`).
4. When bytes are available, the bridge emits a Tauri event
   (`"terminal-output"`) carrying the byte payload to the terminal
   window.
5. The frontend event listener receives the payload and calls
   `term.write(bytes)` on the Xterm.js instance.

### Keyboard Input (Terminal → Console)

1. The user types in the terminal window. Xterm.js fires `onData` with
   the input bytes (already encoded per terminal mode — raw characters
   or escape sequences).
2. The frontend calls a Tauri command (`write_terminal`) passing the
   input bytes.
3. The Tauri command handler writes the bytes into the remote end's `tx`
   pipe via a non-blocking write.
4. On the next poll cycle, the emulated CPU reads the console device's
   latch register (offset 1), which triggers `transport.try_recv()` on
   the local end, pulling a byte from the OS pipe kernel buffer.

Terminal Bridge
---------------

The Terminal Bridge is a long-lived Tokio task spawned when the debugger
session starts. It owns the remote end of the PipeTransport pair created
during console device instantiation.

### Structure

```rust
struct TerminalBridge {
    remote_rx: AsyncFd<File>,   // remote PipeTransport rx, wrapped for async
    remote_tx: File,            // remote PipeTransport tx (non-blocking)
    app_handle: AppHandle,      // Tauri app handle for emitting events
    window_label: String,       // target window for events
}
```

### Output Polling Task

```rust
async fn poll_output(bridge: &TerminalBridge) {
    let mut buf = [0u8; 256];
    loop {
        // Wait for readability on the pipe fd
        let mut guard = bridge.remote_rx.readable().await?;
        match guard.try_io(|fd| fd.get_ref().read(&mut buf)) {
            Ok(Ok(0)) => break,              // EOF — pipe closed
            Ok(Ok(n)) => {
                bridge.app_handle.emit_to(
                    &bridge.window_label,
                    "terminal-output",
                    &buf[..n],
                );
            }
            Ok(Err(e)) => break,             // fatal read error
            Err(_would_block) => continue,   // spurious wake
        }
    }
}
```

Key characteristics:
- Reads in chunks (up to 256 bytes) for efficiency — batching multiple
  characters produced in rapid succession into a single event.
- Uses `AsyncFd::readable()` to sleep without busy-polling, waking only
  when the kernel pipe buffer has data.
- The 256-byte read buffer is a reasonable batch size: large enough to
  capture a burst of output from a tight loop, small enough to keep
  latency perceptible as "instant" (sub-millisecond event emission
  once data is available).

### Input Command Handler

```rust
#[tauri::command]
async fn write_terminal(bytes: Vec<u8>, state: State<TerminalBridge>) {
    for &b in &bytes {
        // Non-blocking write to the remote tx pipe
        loop {
            match state.remote_tx.write(&[b]) {
                Ok(_) => break,
                Err(e) if e.kind() == WouldBlock => {
                    // Pipe buffer full — yield briefly and retry
                    tokio::task::yield_now().await;
                }
                Err(_) => return,  // pipe broken
            }
        }
    }
}
```

The write side is simpler because keyboard input arrives at human typing
speed — the kernel pipe buffer (typically 64 KB on Linux) will never
fill under normal use. The WouldBlock retry is a safety measure, not an
expected path.

Session Lifecycle
-----------------

### Startup

1. `PipeTransport::pair()` creates the local/remote pipe pair.
2. The local end is attached to the Console device via
   `console.attach_transport(Box::new(local))`.
3. The remote end is handed to a `TerminalBridge` which is spawned as a
   Tokio task and stored in Tauri's managed state.
4. The terminal window is opened (separate OS window). Its frontend
   initializes Xterm.js and registers the `"terminal-output"` event
   listener plus the `write_terminal` command binding via `onData`.

### Shutdown

1. When the debugger session ends, the Console device is dropped, which
   closes the local pipe file descriptors.
2. The bridge's output poll loop sees EOF (read returns 0) and exits.
3. The bridge task completes; the remote pipe descriptors are dropped.
4. The terminal window is closed.

### Debugger Pause (Step/Halt)

No special handling is needed. When the CPU is halted:
- The output poll loop simply has no data to read and sleeps on
  `readable()` — no CPU cost.
- Keyboard input accumulates in the kernel pipe buffer and is consumed
  on the next CPU poll cycle after execution resumes.

Performance Characteristics
---------------------------

### Latency

- **Output:** One async wakeup + one Tauri event emission per batch.
  With Tokio's epoll-based reactor, wakeup latency is typically
  sub-millisecond. Xterm.js rendering adds one animation frame
  (~16ms at 60fps). Total: under 20ms from CPU write to screen pixel.
- **Input:** One IPC command round-trip + OS pipe write. Negligible
  compared to human typing intervals.

### Throughput

- Batched reads (256 bytes) amortize per-event IPC overhead during
  high-speed output (e.g. screen clears, scrolling text).
- The OS pipe kernel buffer (64 KB on Linux) absorbs bursts from the
  CPU without backpressure affecting emulated execution speed. At
  1.8 MHz with a typical ACIA-speed output routine (~1000 cycles per
  character), peak sustained output is roughly 1800 characters/second —
  well within what the pipe buffer and event system can handle without
  dropping frames.

### Backpressure

- If the frontend cannot keep up with event processing (unlikely at
  expected character rates), the OS pipe buffer acts as the natural
  backpressure point. The kernel buffer is large enough that this
  scenario is not expected under normal use.
- If it did fill, `Console::write()` → `transport.send()` would return
  `TransportError::Full`, which the console device reports via the error
  sender. The CPU program's polling behavior naturally pauses output
  until the device accepts another byte.

### Free-Running Mode

During free-run at full clock speed, the output polling task and event
emission operate independently of the CPU execution thread. The pipe's
kernel buffer decouples the two — the CPU writes at its pace, and the
bridge drains and forwards at its pace. No synchronization primitives
are needed beyond the pipe itself.

Alternatives Considered
-----------------------

### WebSocket Between Backend and Frontend

Rejected: Tauri's built-in event system is more natural for same-process
communication, avoids the overhead of a TCP/WebSocket stack within a
single application, and provides the target-window routing needed for
the multi-window architecture.

### Shared Memory / Ring Buffer

Rejected: Adds complexity for marginal gain. The expected throughput
(~2000 chars/sec maximum) is orders of magnitude below where shared
memory outperforms pipe I/O. OS pipes provide built-in flow control
and are well-optimized in the kernel.

### Direct Polling from Frontend (Timer-Based)

Rejected: A frontend setInterval polling approach would add unnecessary
latency (at least one poll interval) and waste CPU when idle. The
event-driven approach achieves lower latency with zero idle cost.

Integration with PipeTransport
------------------------------

Currently, `PipeTransport` is not instantiable from device configuration
(it is used directly in tests via `PipeTransport::pair()`). The
`ConsoleModule` only constructs transports from a `TransportSpec` (Tcp,
Unix, Pty). The debugger needs to inject a pre-created `PipeTransport`
whose remote end it retains for the Terminal Bridge.

The `ConsoleModule` (in `src/emulator/config/console.rs`) will be
extended to support an injected transport. Specifically, when a
`PipeTransport` (or any pre-created `Box<dyn Transport>`) is provided
via the `InstantiationContext`, the console module uses it instead of
constructing one from a `TransportSpec`. This keeps the debugger's bus
construction flowing through the same config module path used by the
CLI emulator, with the transport being the only difference.

### Startup Sequence

1. Call `PipeTransport::pair()` to create the local/remote pair.
2. Place the local end (as `Box<dyn Transport>`) into the
   `InstantiationContext` (via a new optional field).
3. The `ConsoleModule::instantiate()` method checks for an injected
   transport before falling back to `TransportSpec`-based construction.
4. The remote end is handed to the `TerminalBridge` Tokio task.
5. Bus construction proceeds normally through `Config::build()`.