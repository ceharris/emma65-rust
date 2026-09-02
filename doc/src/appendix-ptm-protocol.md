# PTM Peer Protocol

Wire protocol for a peripheral to exchange clock, gate, and timer-output
state with an [`Mc6840`](io-devices.md#mc6840-programmable-timer-module-ptm6840)
device (config type `ptm/6840`) over an attached
[`Transport`](io-devices.md#transport-options). Implemented in
`src/emulator/device/protocol/ptm.rs`.

## Connection semantics

Like the [VIA Peer Protocol](appendix-via-protocol.md), the PTM supports
multiple concurrent peripheral connections over a multipoint transport
(`tcp:` or `unix:`); `pty:`/`pipe:` transports are rejected for `ptm/6840`
at instantiation time because they can't tag messages per connected client.

When a peripheral connects, the PTM immediately sends it a full state dump
covering all three timers' clock, gate, and output state. After that,
whenever any connected peripheral changes PTM state — or a 6502 program does
— every connected peripheral is informed of the change.

Unlike the VIA, the PTM's message set is asymmetric: the PTM only *accepts*
messages designated as sent by the peripheral (clock and gate edge
transitions), and a peripheral only *receives* messages designated as sent
by the PTM (clock, gate, and output state updates). Any other received
message is silently ignored; there is no error signaling in either
direction.

Message encoding — ASCII or Binary — is chosen per device at configuration
time via the `protocol` attribute (`protocol = "ascii"` or `protocol =
"binary"`; default `ascii`), not negotiated per connection, exactly as for
the VIA.

```toml
[[devices]]
type = "ptm/6840"
address = 0xA000
protocol = "binary"
transport = "unix:/path/to/ptm.sock"
```

The ASCII encoding is useful for interactive sessions from a terminal or
socket utility, for education or debugging. The binary encoding is compact
and efficient.

## ASCII protocol

Short strings of printable ASCII characters. A receiver must ignore
non-printable ASCII control characters (`0x00`–`0x1F`, `0x7F`), spaces
(`0x20`), and any byte with the high-order bit set, and must not distinguish
upper case from lower case letters.

As an aid to human readability, the PTM separates distinct messages with a
single space, and emits a canonical CR (`0x0D`) LF (`0x0A`) after every 72
characters of messages and spaces.

| Message Type | Sent By    | Format | Example | Description                                                                                                              |
|--------------|------------|--------|---------|----------------------------------------------------------------------------------------------------------------------------|
| Clock Edge   | Peripheral | `Cnp`  | `C21`   | Change the state of an input clock signal; _n_ is the subject timer (1..3); _p_ is the polarity (0=negative, 1=positive) |
| Gate Edge    | Peripheral | `Gnp`  | `G30`   | Change the state of an input gate signal; _n_ is the subject timer (1..3); _p_ is the polarity (0=negative, 1=positive)  |
| Clock State  | MC6840     | `Txyz` | `T010`  | Clock input state; _x_, _y_, _z_ are the state (0 or 1) of timers 1, 2, 3 respectively                                   |
| Gate State   | MC6840     | `Uxyz` | `U101`  | Gate input state; _x_, _y_, _z_ are the state (0 or 1) of timers 1, 2, 3 respectively                                    |
| Output State | MC6840     | `Vxyz` | `V001`  | Timer output state; _x_, _y_, _z_ are the state (0 or 1) of timers 1, 2, 3 respectively                                  |

## Binary protocol

Each message is a single bit-mapped byte with the high-order bit set;
subsequent bits determine the message type and parameters. A receiver
(peripheral or MC6840) must ignore any received byte whose upper nibble
(bits 4–7) doesn't match one of the recognized patterns below.

| Message Type | Sent By    | b7 | b6 | b5 | b4 | b3 | b2 | b1 | b0 | Description                                                                                              |
|--------------|------------|----|----|----|----|----|----|----|----|------------------------------------------------------------------------------------------------------------|
| Clock Edge   | Peripheral | 1  | 0  | 0  | 0  | P  | C3 | C2 | C1 | _P_ is the polarity (0=negative, 1=positive); each `Cx` set to 1 signals a transition of clock input _Cx_ |
| Gate Edge    | Peripheral | 1  | 0  | 0  | 1  | P  | G3 | G2 | G1 | _P_ is the polarity (0=negative, 1=positive); each `Gx` set to 1 signals a transition of gate input _Gx_  |
| Clock State  | MC6840     | 1  | 0  | 1  | 0  | 0  | C3 | C2 | C1 | Each `Cx` is the current state of clock input _Cx_                                                        |
| Gate State   | MC6840     | 1  | 0  | 1  | 1  | 0  | G3 | G2 | G1 | Each `Gx` is the current state of gate input _Gx_                                                         |
| Output State | MC6840     | 1  | 1  | 0  | 0  | 0  | O3 | O2 | O1 | Each `Ox` is the current state of timer output _Ox_                                                       |

_x_ ranges over `1..3`, identifying one of the PTM's three timers, in all
rows above.
