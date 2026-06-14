Virtual 6522 VIA
================

This document describes a protocol for peripheral communication with
for a virtual 6522 Versatile Interface Adapter (VIA) over a transport such
as a socket or PTY.

The VIA is indeed a very versatile system component,
providing 16 independent GPIO pins with additional control signals, two 
16-bit timers, and a shift register for parallel-to-serial or 
serial-to-parallel conversion.

The VIA provides two 8-bit GPIO ports (PA and PB) and corresponding control
signals (CA1, CA2, CB1, CB2). The pins of the GPIO ports can be individually
configured for as inputs or outputs. The control signals play a role
in handshaking, in use of the VIA's shift register, and can be used for
simple single-bit output or interrupt-driven input.

This virtual implementation supports all the VIA's capabilities and
can communicate asynchronously with an external virtual peripheral over
a configurable transport. Program control of the virtual VIA is the same as when
using a real 6522 VIA. To the fullest extent possible, the virtual
VIA behaves and responds in the same manner as the real hardware.

External Peripheral Protocol
----------------------------

The virtual VIA communicates with an external peripheral using a network
socket. It supports both IP sockets and UNIX-domain sockets, allowing it
to be used in a wide variety of integration scenarios. It listens passively
for an incoming connection from a peripheral. Once connected, the VIA
transmits and receives data with the peripheral asynchronously, conveying
and updating the GPIO and control pin states in registers accessible under
program control by the virtual MPU. Incoming GPIO pin state changes from
the peripheral can trigger interrupt conditions on the VIA, just as they
would on real hardware.

The protocol defines two message types:
  1. GPIO port state change.
  2. Control signal state change.

The same message types and formats are used in both directions on the socket;
from the VIA to the peripheral and from the peripheral to the VIA. For example,
when a GPIO port whose pins are configured for output is written by a program
running on the 6502, a port state change message is sent to the connected
peripheral. When the connected peripheral changes state in a manner that needs
to be signaled to GPIO pins configured as inputs, the peripheral sends a port
state change message to the VIA. Similarly, messages for control signal state
changes are communicated according to changes to these controls on the VIA or
by the external peripheral.

The protocol supports two representation formats; a compact binary format, and 
an ASCII format. Either format can be selected by an external peripheral after
connecting to the VIA. The ASCII format is especially useful for allowing a
person (connected via a simple terminal) to play the role of the external 
peripheral, and can be very helpful in learning how to use the VIA in programs
for the 6502.

An implementation of the protocol MUST silently ignore received data that 
does not correspond to legitimate protocol messages. No provision exists in 
this protocol for communicating syntactic or semantic errors.

An implementation of the protocol SHOULD disconnect the socket on any 
indication of an (operating system) error in communicating on the socket.

### Compact Binary Protocol

In the compact binary protocol all messages consist of just one or two bytes
of data.

The binary protocol is selected by the first message received on the socket
whose high-order bit is set. Subsequently, the recipient MUST use only the 
binary protocol for communicating on the connected socket; it must send 
messages in the binary format and must accept only binary format messages.

**Port State Change**. This message consists of a one-byte port identifier, 
followed by a second byte describing the state of the port's pins. Port A is 
identified using 0x80 as the leading byte, while port B is identified using 
0x90 as the leading byte. The second byte communicates the state of the port's 
pins.

**Control State Change**. A control state change message consists of a 
single byte. The upper four bits of the message indicate whether the 
signal is to be set (to logic 1) or cleared (to logic 0). The lower four 
bits indicate which control signals are subject to the change. Negative 
control signal transitions are signified by 0xC in the upper four bits of the 
message, while positive transitions are signified by 0xD.

| Message Type         | Representation  | Example   | Comment       |
|----------------------|-----------------|-----------|---------------|
| Port A State Change  | `0x80 <b>`      | `80` `55` | PA state 0x55 |
| Port B State Change  | `0x90 <b>`      | `90` `AA` | PB state 0xAA |
| Clear Control Signal | `0xC<n>`        | `C8`      | CB1 state 0   |      
| Set Control Signal   | `0xD<n>`        | `D1`      | CA2 state 1   |      

In Port State Change messages, `<b>` is the byte communicating the state of 
the port's pins. Bits that are inputs from the perspective of the sender 
SHOULD be communicated as zeroes, and the receiver SHOULD ignore the state of
these bits on receipt.

In Control Signal Change messages, `<n>` is four bits that represent the 
signals that are to be changed. The order of these bits is consistent with 
conventions used in the VIA's Interrupt Flag and Interrupt Enable registers.

| bit 3 | bit 2 | bit 1 | bit 0 |
|-------|-------|-------|-------|
| CB1   | CB2   | CA1   | CA2   |

The binary protocol is selected by the first message received on the socket
whose high-order bit is set. Subsequently, the VIA will use only the binary
protocol for communicating with the external peripheral on the connected 
socket; it will send messages in the binary format and will accept only
binary format messages from the peripheral. 

**Until the binary mode has been selected, the VIA will communicate using the 
ASCII protocol**. To ensure that the VIA uses only binary format messages,
the peripheral should send a single byte `0xFF` upon connection. 
The high-order bit of this byte will select the binary protocol, but it will 
be otherwise ignored. It is possible that the peripheral could receive ASCII
format messages before the mode is selected. The peripheral should ignore 
data received until the binary mode is acknowledged by reciept of a byte 
with the high-order bit set.

### ASCII Protocol

The ASCII protocol makes use of printing characters and can be easily 
typed at a keyboard by a human, as well as being useful in peripherals 
that make use of scripting languages for which binary communication may be 
more difficult.

The ASCII protocol is selected by the first character received on the socket
whose high-order bit is not set. Subsequently, the recipient MUST use 
only the ASCII protocol for messages communicated on the socket; it must send 
messages in the ASCII format and must accept only ASCII format messages.

An implementation of the ASCII protocol MUST ignore all control character 
codes (0x00..0x1F), space (0x20), delete (0x7f), and any character whose
high-order bit is set (0x80..0xFF), up to the start of a valid message. 
These characters are not allowed within the sequence of characters that 
compromise a message.

An implementation MUST NOT distinguish ASCII upper case letters (0x40..0x5A) 
from ASCII lower case letters (0x60..0x7A). For brevity, the message 
descriptions that follow use upper case letters, but messages may consist 
of uppercase and/or lowercase letters with no distinction between the two.

**Port Change Message**. Port change messages consist of three consecutive
ASCII characters. The first character is either 'A' or 'B', signifying the 
subject port. This must be followed by two ASCII hexadecimal digits 
('0'..'9', 'A'..'F'), which represent an 8-bit value describing the 
state of the port's pins.

**Control Signal Message**. Control change messages consist of four 
consecutive ASCII characters. The first character is 'C', signifying a 
control signal change message. This is followed by either 'A' or 'B', 
designating the associated port, then '1' or '2' to designate the 
control signal within the port. The final character is either '0' or '1'
to indicate either a logic 0 or logic 1 state for the signal, respectively.

| Message Type         | Representation | Example  | Comment       |
|----------------------|----------------|----------|---------------|
| Port A State Change  | `A<h><h>`      | `A5C`    | PA state 0x5C |
| Port B State Change  | `B<h><h>`      | `BD3`    | PB state 0xD3 | 
| Clear Control Signal | `C<p><n>0`     | `CA10`   | CA1 state 0   |
| Set Control Signal   | `C<p><n>1`     | `CB21`   | CB2 state 1   | 

In Port State Change messages, `<h>` is an ASCII hexdecimal digit 
'0'..'9' or 'A'..'F'. Bits that are inputs from the perspective of
the sender SHOULD be communicated as zeroes, and the receiver SHOULD ignore
the state of these bits on receipt.

In Control Signal Change messages, `<p>` is an ASCII letter 'A' or 'B', `<n>` 
is an ASCII digit '1' or '2'.

Because spaces and control characters are ignored, you can type a series 
of messages with intervening spaces or newlines for easy reading; the VIA
includes a space between each message in ASCII mode as an aid to readability.

On receipt of a valid start-of-message character ('A', 'B', 'C'), an 
implementation MUST consume characters from the stream for the full length 
of the given message type, even if an invalid character is detected within 
the message. Until a valid start-of-message character is received, an 
implementation MUST ignore all other received characters.

**Unless the binary mode is selected, the VIA will communicate using the
ASCII protocol**. To ensure that the VIA uses only ASCII format messages,
the peripheral should send a space (0x20) or any ASCII control character
(0x00..0x1f, 0x7f) upon connection to the VIA. This character will select 
the ASCII mode, but will be otherwise ignored.