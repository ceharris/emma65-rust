# Wire Protocols

[I/O Devices](io-devices.md) describes each device's bus-facing register
behavior — what a 6502 program sees. This appendix documents the byte-level
protocols four of those devices speak over an attached
[`Transport`](io-devices.md#transport-options) to talk to something *outside*
the emulator process. It's reference material for writing a peripheral
implementation (real hardware, a script, another emulator) or a replacement
for one of the bundled SDL2 peripheral binaries — not needed for ordinary use
of the emulator or debugger.

Two families of protocol, serving different needs:

- **Peer-communication protocols** — [Via6522](appendix-via-protocol.md) and
  [Mc6840](appendix-ptm-protocol.md) exchange GPIO/timer signal state
  bidirectionally with one or more connected peripherals, mirroring how a
  real VIA or PTM talks to the hardware wired to its pins. A newly connected
  peripheral receives a full state dump; every peripheral thereafter sees
  every state change, whether it originated on the device (a program writing
  a register) or from another connected peripheral.
- **External rendering protocols** — [CharDisplay](appendix-display-protocol.md),
  [LedMatrix](appendix-led-matrix-protocol.md), and
  [LcdDisplay](appendix-lcd-display-protocol.md) stream composited frame
  data to the bundled `emma65-display`, `emma65-led-matrix`, and
  `emma65-lcd-display` SDL2 peripheral binaries (see
  [Running the Display Peripheral](running-the-display-peripheral.md),
  [Running the LED Matrix Peripheral](running-the-led-matrix-peripheral.md),
  and [Running the LCD Display Peripheral](running-the-lcd-display-peripheral.md)).
  These only matter when running the plain `emma65` CLI standalone — the
  debugger renders all three devices in-process and never speaks any of
  these protocols.

| Protocol | Device (config `type`) | Direction | Encoding | Transport requirement |
|----------|-------------------------|-----------|----------|------------------------|
| [VIA Peer Protocol](appendix-via-protocol.md) | `Via6522` (`via/6522`) | bidirectional | ASCII or Binary, selected by `protocol=` | multipoint (`tcp:`/`unix:`) |
| [PTM Peer Protocol](appendix-ptm-protocol.md) | `Mc6840` (`ptm/6840`) | bidirectional | ASCII or Binary, selected by `protocol=` | multipoint (`tcp:`/`unix:`) |
| [Character Display External Protocol](appendix-display-protocol.md) | `CharDisplay` (`display`) | outbound frames + inbound keystrokes | Binary only | atomic `send_bytes` (`pipe:` only) |
| [LED Matrix External Protocol](appendix-led-matrix-protocol.md) | `LedMatrix` (`display/matrix`) | outbound only | Binary only | atomic `send_bytes` (`pipe:` only) |
| [LCD Display External Protocol](appendix-lcd-display-protocol.md) | `LcdDisplay` (`display/lcd`) | outbound only | Binary only | atomic `send_bytes` (`pipe:` only) |

The three rendering protocols additionally require an atomic
[`Transport::send_bytes`](io-devices.md#transport-options) — either the
whole outbound message is delivered or none of it is — because all three are
framed with no length prefixes or delimiters; a partial write would desync
the stream with no way to resynchronize. In practice this rules out every
transport but `pipe:`, and the config module for each device rejects any
other transport spec at instantiation time. The peer-communication protocols
have no such requirement, since every message is short and self-delimiting.
