# VIA Peer Protocol

Wire protocol for a peripheral to exchange GPIO port and control-signal
state with a [`Via6522`](io-devices.md#6522-versatile-interface-adapter-via6522)
device (config type `via/6522`) over an attached
[`Transport`](io-devices.md#transport-options). Implemented in
`src/emulator/device/protocol/via.rs`.

## Connection semantics

The VIA supports multiple concurrent peripheral connections over a
multipoint transport (`tcp:` or `unix:`) — `pty:`/`pipe:` transports are
point-to-point and can't tag messages per connected client, so the config
module rejects them for `via/6522` at instantiation time. Just as on real
hardware, it's up to the peripherals themselves not to interfere with each
other (e.g. two peripherals both driving the same output pin).

When a peripheral connects, the VIA immediately sends it a full state dump
covering both ports and their control signals, so it starts with an accurate
picture without having to wait for the next change. After that, whenever any
connected peripheral changes VIA state — or a 6502 program does, by writing
a port configured for output — every connected peripheral is informed of the
change. Unrecognized peripheral input is silently ignored; there is no error
signaling in either direction.

Message encoding — ASCII or Binary — is chosen per device at configuration
time via the `protocol` attribute (`protocol = "ascii"` or `protocol =
"binary"`; default `ascii`), not negotiated per connection. Every peripheral
connected to a given VIA instance uses the same encoding.

```toml
[[devices]]
type = "via/6522"
address = 0x9000
protocol = "binary"
transport = "unix:/path/to/via.sock"
```

The ASCII encoding is useful for interactive sessions from a terminal or
socket utility, for education or debugging, and for simple scripting. The
binary encoding is compact and efficient, and is the better choice for a
real peripheral implementation.

## Message types

Six message types are defined:

1. **Port State** — sent by the VIA to convey the current state of Port A or
   B; may also be sent by a peripheral to configure the state of all pins of
   the subject port.
2. **Ctrl State** — sent by the VIA to convey the current state of the
   control pins for Port A or B; may also be sent by a peripheral to
   configure the state of both control pins for the subject port.
3. **Reset Port** — sent by a peripheral to reset any combination of bits in
   the specified port. The VIA sends this to convey bit-level state changes
   as needed. The message includes a mask byte identifying, with ones in the
   corresponding bit positions, which bits to reset.
4. **Set Port** — sent by a peripheral to set any combination of bits in the
   specified port. The VIA sends this to convey bit-level state changes as
   needed. The message includes a mask byte identifying, with ones in the
   corresponding bit positions, which bits to set.
5. **Reset Ctrl** — sent by a peripheral to reset either control pin (`Cx1`
   or `Cx2`) for the specified port. The VIA sends this to convey changes in
   individual control signals. The ASCII message specifies the pin to reset;
   the binary message specifies a mask identifying which control bits to
   reset.
6. **Set Ctrl** — sent by a peripheral to set either control pin (`Cx1` or
   `Cx2`) for the specified port. The VIA sends this to convey changes in
   individual control signals. The ASCII message specifies the pin to set;
   the binary message specifies a mask identifying which control bits to
   set.

## ASCII protocol

Short strings of printable ASCII characters. A receiver must discard
non-printable ASCII control characters (`0x00`–`0x1F`, `0x7F`), spaces
(`0x20`), and any byte with the high-order bit set, and must not distinguish
upper case from lower case letters.

As an aid to human readability, the VIA separates distinct messages with a
single space, and emits a canonical CR (`0x0D`) LF (`0x0A`) after every 72
characters of messages and spaces.

| Message Type | Format | Example | Description                       |
|--------------|--------|---------|-----------------------------------|
| Port State   | `pxx`  | `A55`   | Port A state is `0x55`            |
| Ctrl State   | `Cpuv` | `CB10`  | Port B CB1 is high and CB2 is low |
| Reset Port   | `Rpxx` | `RBF0`  | Reset port B bits 4 through 7     |
| Set Port     | `Spxx` | `SA03`  | Set port A bits 0 and 1           |
| Reset Ctrl   | `RCpu` | `RCA2`  | Reset ctrl CA2 (low)              |
| Set Ctrl     | `SCpu` | `SCB1`  | Set ctrl CB1 (high)               |

- _p_ is a port identifier `A` or `B`
- _u_ is a control pin identifier `1` or `2`
- _v_ is a bit state `0` or `1`
- _x_ is an ASCII hexadecimal digit `0`..`F`

## Binary protocol

Each message starts with a byte whose high-order bit is set. The upper four
bits distinguish the message type; the lower four carry parameters. **Port
State**, **Reset Port**, and **Set Port** messages are followed by one
additional data byte.

| Message Type | Format                 | Example                | Description                        |
|--------------|-------------------------|-------------------------|-------------------------------------|
| Port State   | `10000p00` `bbbbbbbb`   | `10000110` `01010101`   | Port B state: PB=0x55               |
| Ctrl State   | `10010puv`              | `10010001`              | Port A control state: CA1=0 CA2=1   |
| Reset Port   | `10100pyz` `bbbbbbbb`   | `10100000` `00000011`   | Reset PA bits 0 and 1               |
| Set Port     | `10110pyz` `bbbbbbbb`   | `10110110` `11110000`   | Set CB1 and PB bits 4 through 7     |
| Reset Ctrl   | `11000pyz`              | `11000101`              | Reset CB2                           |
| Set Ctrl     | `11010pyz`              | `11010011`              | Set CA1 and CA2                     |

- _b_ is an arbitrary bit, `0` or `1`
- _p_ identifies the port; `0` for port A, `1` for port B
- _u_ and _v_ are the states of control pins `Cp1` and `Cp2` respectively;
  `0` or `1`
- _y_ and _z_ are mask bits identifying whether control pins `Cp1` and `Cp2`
  respectively are affected by a reset or set operation; `0` or `1`
