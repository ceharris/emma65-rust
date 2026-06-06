# Emulator Product Context and Implementation Plan Discussion

## Context Setting

**User:** In `emulator-plan.md` is a plan for a Rust-based emulator for the 65C02 processor. I'd like to collaborate with you to develop a project plan for implementation. But first, I'd like to share a bit more context about how the output of this project fits into the larger product.

**Assistant:** [Read emulator-plan.md — a thorough architecture document covering the WDC65C02 emulator as a Rust library crate with CPU, bus, device, transport, interrupt, watch, execution, and disassembler modules.]

This is a thorough architecture document. I'd love to hear the broader context — how does this emulator library fit into the larger product? Understanding the consumer(s) of this API will help shape the implementation plan.

---

## Product Context

**User:** The emulator isn't a standalone Rust crate. It will be a module within `emma65` which is a Rust crate that produces the binary that will provide the user interface for running and debugging programs for an emulated 6502 microcomputer. The user interface will be based on the Tauri framework. The `watch` module within `emma65` provides the scanner, parser, compiler, and evaluator for watch expressions. The emulator described in the plan will comprise the `emulator` module for Emma65.

**Assistant:** So the plan's `emu65c02` crate structure maps to an `emulator` module inside the `emma65` Tauri app, and the watch expression system (`watch/`) lives as a sibling module in `emma65` rather than inside the emulator. That means the emulator consumes the watch evaluator through internal module boundaries rather than a crate-level API.

What else would you like to share about Emma65 before we dig into the implementation plan? I'm curious about things like:

- **Current state** — does the `emma65` crate already exist with some structure (Tauri scaffolding, the watch module, etc.), or are we starting from scratch?
- **The Tauri UI's relationship to the emulator** — what does the debugging/running experience look like from the user's perspective?
- **Any other modules** in `emma65` that the emulator will interact with beyond `watch`.

---

## Watch Module Placement

**User:** The emma65 crate already exists and the watch module has already been developed and tested. The watch module could become a submodule for the emulator if that makes for a cleaner design.

**Assistant:** On the watch module placement — keeping it as a sibling is likely cleaner. The emulator's `WatchEvaluator` consumes compiled watch expressions, but the compilation itself (scanning, parsing) doesn't need any emulator types. A sibling relationship keeps the watch language independently testable and avoids pulling parser machinery into the emulator module.

Still curious about the remaining questions — particularly which IO devices the target machine actually uses, and whether this is emulating a specific board design or a general-purpose configurable system.

---

## Tauri UI Vision

**User:** The Tauri UI side has not been developed. The vision is that it will provide views such as code disassembly, memory dumps, register display, a watch expressions view with the ability to create new watches, plus UI controls to single step the processor, or to step the processor repeatedly using a loop under UI control (for slow execution), and to start and run the processor asynchronously. The VT220 terminal window for the system console will be implemented using `xterm.js`. In addition the basic debugging features described above, the UI will also provide the ability to create machine configurations with ROM, RAM, and emulated IO devices.

**Assistant:** That's a clear picture — a full-featured debugger IDE with a configurable machine. The UI being undeveloped is useful to know; it means the emulator's API is the foundation everything else builds on.

---

## Machine Model

**User:** This will be a general-purpose configurable system, using any combination of the built-in IO device emulations and memory components, organized according to the user's specification. Additionally the device SPI described in the plan can be used by a sophisticated user to create emulated (or real) devices that interface to the system.

**Assistant:** A general-purpose configurable system means all four built-in devices are equally important — no shortcuts by deferring any. And the SPI being a real extensibility point means the `IoDevice` trait boundary needs to be clean and well-documented from the start.

---

## Constraints and Preferences

**User:** We've selected Tokio for the device transport layer. Is there a reason to introduce a different async framework for Tauri? Should require Rust 2024. Make practical use of existing crates, rather than trying to minimize dependencies.

**Assistant:** Tauri 2.x uses Tokio under the hood, so no reason to introduce anything else — it's Tokio all the way down. Rust 2024 and pragmatic dependency use noted. Crates like `bitflags`, `crossbeam`, `thiserror`, `tracing`, etc. are all fair game where they fit.

---

## Development Process

**User:** I'm looking for a plan that builds incrementally, in units of work sized appropriately to allow human review via merge requests. I'd prefer work to be relatively tightly scoped to one subsystem at a time to the extent practical. I'd prefer to see unit tests developed with each unit of work, with subsequent units of work used to introduce more complex integration tests and functional tests.

**Assistant:** That's a clear development philosophy — bottom-up, test-as-you-go, with PR-sized units that a reviewer can hold in their head. Integration tests emerge naturally as subsystems compose.

---

## VIA Peripheral Protocol Discussion

**User:** Before we continue on the implementation plan I'd like to discuss how a peripheral device might communicate with the VIA 6522, and further refine that aspect of the architecture. [Provided `emulator-via-protocol.md` — a protocol specification for VIA peripheral communication over sockets, with binary and ASCII message formats for GPIO port state changes and control signal state changes.]

**Assistant:** Raised five questions about the protocol:
1. Shift register operation — per-transition control signal messages vs. a bulk message type
2. Timer 1 PB7 toggling — whether toggles should generate port state messages
3. Initial state on connection — should the VIA send current state when a peripheral connects
4. Transport trait fit — VIA layers protocol framing on byte-oriented Transport
5. SPI consumers — should the protocol codec be a public reusable type

### Shift Register

**User:** The per-transition approach is adequate. If we discover a use case that requires more efficient use of the shift register, the protocol seems to leave plenty of room for extension.

**Assistant:** Agreed — per-transition it is. The protocol's byte space has room for future message types.

### Timer 1 PB7 Toggle

**User:** Let's make that a configurable feature. By default, PB7 toggling should be enabled. So the configuration option would suppress that behavior.

**Assistant:** Clear — PB7 toggle messages enabled by default, with an option to suppress.

### Initial State on Connection

**User:** The peripheral decides whether the protocol will be ASCII or binary on the current transport connection. As soon as the protocol format is known, the VIA should send the current state. The protocol document suggests that the peripheral should send 0xFF or 0x20 upon connection to select the protocol. The current state of the IO pins could then be transmitted.

**Assistant:** Connection handshake: peripheral connects, sends format selector, VIA recognizes the format, then sends full state dump (port A, port B, and all four control signals — 6 messages).

### Transport Trait Fit

**User:** The VIA should be responsible for implementing its protocol internally. The protocol module should be designed so that message encoding and decoding can be thoroughly unit-tested independent of the VIA emulation logic and transport layer.

**Assistant:** Separating the protocol codec from the VIA emulation logic gives three independently testable layers: protocol encoding/decoding, VIA register/timer/interrupt behavior, and transport IO.

### SPI Consumers

**User:** Exposing the protocol codec is a nice touch.

**Assistant:** The protocol codec will be part of the public SPI surface alongside `IoDevice`, `Transport`, etc.

### Summary of Decisions

1. **Shift register**: Per-transition control signal messages (CB1/CB2). Extensible later if needed.
2. **Timer 1 PB7**: Port B state change messages sent on toggle, enabled by default, configurable to suppress.
3. **Connection handshake**: Peripheral sends format selector, VIA responds with full state dump.
4. **Transport trait**: Unchanged. VIA handles protocol framing internally.
5. **Protocol codec**: Separate module, independently unit-testable, exposed as public SPI type.
