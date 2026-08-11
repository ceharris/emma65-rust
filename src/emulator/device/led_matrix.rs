//! An RGB LED matrix display adapter.
//!
//! This device emulates the parallel bus interface of an RGB LED matrix display unit
//! that is managed by a microcontroller (MCU). The display unit is fully responsible
//! for buffering the display image and refreshing the display itself. The 6502 bus
//! interface provides registers used to convey parameters and commands that write
//! single pixels, fill rectangles, blit images, etc. The display is double-buffered — all
//! updates are written to an off-display buffer, and a buffer swap command is executed
//! to exchange the off-screen buffer with the on-screen buffer.
//!
//! ## Color Palette
//! To reduce blit bandwidth requirements on the 8-bit parallel bus interface, the
//! display uses a 256-color palette. Each palette entry stores a 16-bit RGB565 color.
//! All operations that modify the display content de-reference an 8-bit color palette
//! index to obtain the desired 16-bit RGB565 color. The default color palette has an
//! organization that is very similar to the 256-color palette in Xterm:
//!
//! - `0..7` - black, red, green, blue, yellow, cyan, magenta, and white
//! - `8..15` - bright variants of colors 0..7
//! - `16..231` - 6x6x6 color RGB color cube
//! - `232..255` - 24-level grayscale ramp
//!
//! ## Registers
//! The registers provided by this module are as follows:
//!
//! | Offset | Mnemonic  | Description                | Note |
//! |--------|-----------|----------------------------|------|
//! |   0x0  | X         | X coordinate               | 1    |
//! |   0x1  | Y         | Y coordinate               | 1    |
//! |   0x2  | WIDTH     | Width                      | 1    |
//! |   0x3  | HEIGHT    | Height                     | 1    |
//! |   0x4  | COLOR     | Color palette index        | 2    |
//! |   0x5  | IFR       | Interrupt Flag Register    | 3    |
//! |   0x6  | IER       | Interrupt Enable Register  | 4    |
//! |   0x7  | CMD/DATA  | Command/Data Register      | 5    |
//!
//! ### Notes
//! 1. Registers used for pixel location and dimensions are strictly clamped at one less than
//!    the corresponding horizontal or vertical dimension of the display matrix. These registers
//!    may be written at any time; values are latched internally at the start of any command.
//!    Display matrix dimensions are always a small power of two (e.g. 32, 64, 128, 256). Unused bit
//!    positions are ignored on write. Reading any of these registers returns the last value written
//!    except that unused bit positions read as zero.
//! 2. The COLOR register stores an index into the 256-color RGB565 color palette in the display.
//!    This register may be written at any time; the value is latched internally at the start
//!    of any command. A read of this register returns the last value written.
//! 3. The IFR register contains a composite IRQ bit that indicates whether any of the other bits
//!    in the register are set. The other bit positions correspond to status outputs from the
//!    display. See **Interrupt Flag Register** below.
//! 4. The IER register is used to set/clear bits used as a mask to determine which status
//!    conditions in the IFR will assert the 6502 IRQ signal. See **Interrupt Enable Register**
//!    below.
//! 5. The CMD/DATA register is used to execute commands on the display. This register may be
//!    written only when the READY bit in the IFR is set. See **Commands** below for a description
//!    of the available commands.
//!
//! ### Interrupt Flags Register (IFR)
//! This register provides the status of the display device and can be used in either direct
//! polling or interrupt-driven I/O with the device. The register contains four relevant status
//! bits:
//!
//! |   7  |   6   |   5  |   4  |   3  |   2  |   1  |   0  |
//! |------|-------|------|------|------|------|------|------|
//! | IRQ  | READY | BLIT | FILL |  0   |   0  |   0  |   0  |
//!
//! - IRQ - logical OR of READY, BLIT, and FILL
//! - READY - indicates that the CMD/DATA register is ready for a write operation
//! - BLIT - indicates that a BLIT operation has been completed
//! - FILL - indicates that a FILL operation has been completed
//!
//! Any bit other than IRQ may be cleared by writing a byte to the IFR with a one in the
//! corresponding bit position.
//!
//! The unused bit positions are ignored on write and always read as zero.
//!
//! ### Interrupt Enable Register (IER)
//! This register determines which status bits in the IFR, when set, will assert the CPU's IRQ
//! signal. The register contains three relevant enable bits that correspond to bits in the IFR:
//!
//! |   7       |   6   |   5  |   4  |   3  |   2  |   1  |   0  |
//! |-----------|-------|------|------|------|------|------|------|
//! | Set/Clear | READY | BLIT | FILL |  0   |   0  |   0  |   0  |
//!
//! The IRQ signal is asserted whenever the following expression is true:
//!
//! ```text
//! (IFR.READY & IER.READY) | (IFR.BLIT & IER.BLIT) | (IFR.FILL & IER.FILL)
//! ```
//!
//! To set any bit in the IER, write a byte in which bit 7 is 1 and the corresponding IFR bit is
//! also 1. To clear any bit in the IER, write a byte in which bit 7 is 0 and the corresponding
//! IFR bit is 1.
//!
//! The unused bit positions are ignored on write and always read as zero. Bit 7 always reads as 1.
//!
//! ## Commands
//! The CMD/DATA register may be written only when the READY bit in the IFR is set. When issuing a
//! command, first write the relevant parameter registers (X, Y, WIDTH, HEIGHT, COLOR) and then
//! write the command to the CMD/DATA register.
//!
//! Upon writing the register, the READY bit in the IFR will be cleared and will remain low
//! until the display has finished executing the command. The READY bit is cleared by writing to
//! the CMD/DATA register. The READY bit can also be cleared by writing a byte to the IFR in which
//! the READY bit is set.
//!
//! | Opcode | Mnemonic   | Parameters                 | Description                       |
//! |--------|------------|----------------------------|-----------------------------------|
//! |   0x0  | NOP        | (none)                     | No operation                      |
//! |   0x1  | BRIGHTNESS | (none)                     | Set brightness level to COLOR     |
//! |   0x2  | SET_COLOR  | COLOR, X, Y                | Set palette at COLOR              |
//! |   0x3  | PIXEL      | X, Y, COLOR                | Write a pixel at X,Y              |
//! |   0x4  | BLIT       | X, Y, WIDTH, HEIGHT        | Blit of size WxH at X,Y           |
//! |   0x5  | BLIT_ALL   | (none)                     | Blit the entire framebuffer       |
//! |   0x6  | FILL       | X, Y, WIDTH, HEIGHT, COLOR | Fill a WxH rectangle at X,Y       |
//! |   0x7  | FILL_ALL   | COLOR                      | Fill the entire framebuffer       |
//! |  0xFF  | SWAP       | (none)                     | Swap framebuffers                 |
//!
//! ### NOP: No Operation
//! This command has no effect on the display itself. It can be used to clear the READY bit in the
//! IFR upon handling an interrupt if no subsequent command is needed.
//!
//! ### BRIGHTNESS: Set Display Brightness Level
//! This command immediately changes the display driver's overall brightness (intensity)
//! to the level specified by COLOR (interpreted as a level between 0 and 100%).
//!
//! ### SET_COLOR: Set a Palette Color
//! The color palette consists of a 256-slot table of 16-bit RGB565 colors. RGB565 color uses
//! 5 bits for red, 6 bits for green, and 5 bits for blue. The 16-bit representation is
//! `[ R4 R3 R2 R1 R0 G5 G4 G3 | G2 G1 G0 B4 B3 B2 B1 B0 ]`; the most significant byte holds the
//! red component and half of the green component, while the least significant byte holds the
//! other half of the green component and the blue component.
//!
//! The SET_COLOR command stores the RGB565 color represented by `Y` (color MSB) and `X` (color LSB)
//! into the color palette at the index given by `COLOR`. Note that color palette indices are
//! de-referenced during PIXEL, BLIT, CLEAR, and FILL operations. This implies that a change to
//! a color palette slot has no effect on colors stored in the current on-screen buffer that is
//! used to refresh the display. Moreover, it is possible to change a given palette slot between
//! commands that reference that entry in the palette.
//!
//! ### PIXEL: Writes a Pixel
//! The PIXEL command is used to set the pixel at `X,Y` in the off-screen buffer to the palette
//! color specified by `COLOR`.
//!
//! ### BLIT: Modifies a Region of Pixels in the Off-Screen Framebuffer
//! The BLIT command accepts a sequence of color palette indices to modify every pixel in the
//! off-screen framebuffer within the bounds of the rectangle of size `WIDTH x HEIGHT` whose
//! upper-left corner is at `X,Y`. Note that clamping of values written to the WIDTH and
//! HEIGHT registers precludes using BLIT to modify every pixel in the framebuffer; use
//! BLIT_ALL instead. See **Blit Operation** below for details on performing a blit.
//!
//! ### BLIT_ALL: Modifies all Pixels in the Off-Screen Framebuffer
//! The BLIT_ALL command accepts a sequence of color palette indices to modify every pixel
//! the off-screen framebuffer. See **Blit Operation** below for details on performing a blit.
//!
//! ### FILL: Fill a Rectangular Area of the Off-Screen Framebuffer
//! Sets every pixel in the off-screen framebuffer that lies within the rectangle of size
//! `WIDTH x HEIGHT` whose upper left corner is at pixel `X,Y` to the palette color given by
//! `COLOR`. Note that clamping of values written to the WIDTH and HEIGHT registers precludes
//! using FILL to fill the entire framebuffer; use FILL_ALL instead. See **Fill Operation** below
//! for details on performing a fill.
//!
//! ### FILL_ALL: Fill the Full Dimensions of the Off-Screen Framebuffer
//! Sets every pixel in the off-screen framebuffer to the palette color given by `COLOR`.
//! See **Fill Operation** below for details on performing a fill.
//!
//! ### SWAP: Swap the On-Screen and Off-Screen Buffers
//! The display is double-buffered. All commands that modify the display content operate on an
//! off-screen buffer, while the display continues to refresh from the on-screen buffer. After
//! a sequence of updates to the display is completed, the SWAP operation exchanges the buffers,
//! using the previously off-screen buffer to refresh the display, and making the previously
//! on-screen buffer available for subsequent updates. The swap operation is practically
//! instantaneous.
//!
//! ## Blit Operation
//! After issuing the BLIT or BLIT_ALL command the next _N_ bytes written to the CMD/DATA register
//! are interpreted as color palette indices. For BLIT, _N_ is the product of the dimensions given
//! by the WIDTH and HEIGHT registers. For BLIT_ALL, _N_ is the product of the dimensions of the
//! display matrix; i.e. the entire framebuffer is modified.
//!
//! A blit writes the color of every pixel in the specified region, using the RGB565 color indexed
//! by each color index received from the CMD/DATA port, from left to right and top to bottom.
//! After writing a BLIT or BLIT_ALL command, and after writing any byte in the sequence of color
//! indices, the program must wait until the READY bit in the IFR is set before writing the next
//! byte.
//!
//! Upon issuing a BLIT or BLIT_ALL command, the `BLIT` bit in the IFR is cleared. When the last
//! expected data byte has been accepted by the CMD/DATA register, the `BLIT` bit in the IFR is set.
//! When this occurs, if `BLIT` is enabled in the IER, IRQ is asserted. The `BLIT` bit is cleared
//! by writing a byte to the IFR in which the `BLIT` bit is set.
//!
//! ## Fill Operation
//! A fill operation is used to efficiently set all pixels in a region of the off-screen
//! framebuffer to the RGB565 color in the color palette indexed by COLOR. FILL is used to fill a
//! rectangular region smaller than display dimension. FILL_ALL is used to fill the entire
//! framebuffer.
//!
//! Upon issuing the FILL or FILL_ALL command the `FILL` bit in the IFR is cleared. When completed,
//! the `FILL` bit in the IFR is set. If `FILL` is enabled in the IER, IRQ is asserted. The `FILL`
//! bit is cleared by writing a byte to the IFR in which the `FILL` bit is set.
//!
//! # Virtual Display Communication Protocol
//! This display adapter is intended to be used with a virtual display that implements the
//! functionality of the RGB LED Display Matrix. The virtual display connects to this adapter over
//! a supported transport connection (typically a UNIX-domain socket for minimal latency).
//!
//! A byte-oriented binary communication protocol is used on a transport connection between this
//! adapter and a virtual display. As device registers are written by the 6502 program, the
//! values written are immediately conveyed to the virtual display via the SEND_REGISTER message.
//! On state changes within the virtual display that effect the IFR bits (`READY`, `BLIT`, or
//! `FILL`), the virtual display immediately sends the IFR state to the adapter via the SEND_IFR
//! message. Pending messages from the virtual display are received and applied to the adapter's
//! internal state at each bus tick.
//!
//! | Message Type  | Direction                     | Format        | Description                                       |
//! |---------------|-------------------------------|---------------|---------------------------------------------------|
//! | SEND_REGISTER | Bus Device -> Virtual Display | `<u8>` `<u8>` | Sends the value of a register as `offset` `value` |
//! | SEND_IFR      | Virtual Display -> Bus Device | `<u8>`        | Sends the state of the IFR register as `value`    |
//!
//! When a virtual display first connects to the bus device over the configured transport, the
//! adapter immediately sends a dump of all registers via a sequence of SEND_REGISTER messages.
//! The virtual display should respond by sending the appropriate IFR value.
//!
use crate::emulator::{AddressRange, ChannelRelay, IoDevice, LogCategory, LogLevel, LogSender, Transport, TransportEvent, log_msg};

const IFR_IRQ:   u8 = 0b1000_0000;
const IFR_READY: u8 = 0b0100_0000;
const IFR_BLIT:  u8 = 0b0010_0000;
const IFR_FILL:  u8 = 0b0001_0000;
const IFR_MASK:  u8 = IFR_READY | IFR_BLIT | IFR_FILL;

const WIDTH_IN_PIXELS: u32 = 64;
const HEIGHT_IN_PIXELS: u32 = 64;

const COMMAND_BLIT: u8 = 4;
const COMMAND_BLIT_ALL: u8 = 5;
const COMMAND_FILL: u8 = 6;
const COMMAND_FILL_ALL: u8 = 7;

/// An RGB LED matrix display adapter.
pub struct LedMatrix {
    name: &'static str,
    address_range: AddressRange,
    transport: Option<Box<dyn Transport>>,
    /// Paired with `transport`; drained into connection/IFR state each `tick()`.
    relay: Option<ChannelRelay<TransportEvent>>,

    x: u8,
    y: u8,
    width: u8,
    height: u8,
    color: u8,
    ifr: u8,
    ier: u8,
    cmd_data: u8,
    connection_tag: Option<u8>,
    blit_bytes_remaining: u32,
    /// Sender for structured diagnostic messages (e.g. `reset()`).
    log_sender: LogSender,
}

impl LedMatrix {

    pub fn new(name: &'static str, address_range: AddressRange) -> Self {
        Self {
            name,
            address_range,
            transport: None,
            relay: None,
            x: 0,
            y: 0,
            width: 0,
            height: 0,
            color: 0,
            ifr: 0,
            ier: 0,
            cmd_data: 0,
            connection_tag: None,
            blit_bytes_remaining: 0,
            log_sender: LogSender::default(),
        }
    }

    /// Attaches a transport and its paired tagged relay for byte-stream IO.
    pub fn attach_transport(&mut self, transport: Box<dyn Transport>, relay: ChannelRelay<TransportEvent>) {
        self.transport = Some(transport);
        self.relay = Some(relay);
    }

    /// Installs a log sender for diagnostic messages (e.g. `reset()`).
    pub fn set_log_sender(&mut self, sender: LogSender) {
        self.log_sender = sender;
    }

    fn write_ifr(&mut self, value: u8) {
        self.ifr &= !(value & IFR_MASK) & IFR_MASK;
        if self.ifr != 0 {
            self.ifr |= IFR_IRQ
        }
    }

    fn write_ier(&mut self, value: u8) {
        let is_set = value & IFR_IRQ != 0;
        let mask = value & (IFR_READY | IFR_BLIT | IFR_FILL);
        if is_set {
            self.ier |= mask;
        } else {
            self.ier &= !mask;
        }
    }

    fn poll_transport(&mut self) {
        let mut relay = self.relay.take();
        let mut transport = self.transport.take();
        if let (Some(relay), Some(transport)) = (relay.as_mut(), transport.as_mut()) {
            let mut events = Vec::new();
            relay.drain_into(|event| events.push(event));
            for event in events {
                match event {
                    TransportEvent::Connected(tag) => {
                        self.send_registers(transport);
                        self.connection_tag = Some(tag);
                    },
                    TransportEvent::Disconnected(tag)
                        if let Some(connection_tag) = self.connection_tag
                            && tag == connection_tag => {
                        self.connection_tag = None;
                    },
                    TransportEvent::Data(tag, value)
                        if let Some(connection_tag) = self.connection_tag
                            && tag == connection_tag => {
                        self.ifr |= value & IFR_MASK;
                        if self.ifr != 0 {
                            self.ifr |= IFR_IRQ
                        }
                    }
                    _ => ()
                }
            }
        }
        self.relay = relay;
        self.transport = transport;
    }

    fn send_registers(&mut self, transport: &mut Box<dyn Transport>) {
        self.send_register_via_transport(transport, 0, self.x);
        self.send_register_via_transport(transport, 1, self.y);
        self.send_register_via_transport(transport, 2, self.width);
        self.send_register_via_transport(transport, 3, self.height);
        self.send_register_via_transport(transport, 4, self.color);
        self.send_register_via_transport(transport, 5, self.ifr);
        self.send_register_via_transport(transport, 6, self.ier);
        self.send_register_via_transport(transport, 7, self.cmd_data);
    }

    fn send_register(&mut self, offset: u8, value: u8) {
        let mut transport = self.transport.take();
        if let Some(transport) = transport.as_mut() {
            self.send_register_via_transport(transport, offset, value);
        }
        self.transport = transport;
    }

    fn send_register_via_transport(&mut self, transport: &mut Box<dyn Transport>, offset: u8, value: u8) {
        transport.send(offset);
        transport.send(value);
    }


}

impl IoDevice for LedMatrix {

    fn read(&mut self, address: u16) -> u8 {
        self.peek(address)
    }

    fn write(&mut self, address: u16, value: u8) {
        let offset = (address - self.address_range.start) as u8;
        match offset {
            0 => {
                self.x = value & (WIDTH_IN_PIXELS - 1) as u8;
                self.send_register(offset, self.x);
            }
            1 => {
                self.y = value & (HEIGHT_IN_PIXELS - 1) as u8;
                self.send_register(offset, self.y);
            }
            2 => {
                self.width = value & (WIDTH_IN_PIXELS - 1) as u8;
                self.send_register(offset, self.width);
            }
            3 => {
                self.height = value & (HEIGHT_IN_PIXELS - 1) as u8;
                self.send_register(offset, self.height);
            },
            4 => {
                self.color = value;
                self.send_register(offset, self.color);
            }
            5 => {
                self.write_ifr(value);
                self.send_register(offset, self.ifr);
            },
            6 => {
                self.write_ier(value);
                self.send_register(offset, self.ier);
            },
            7 => {
                self.cmd_data = value;
                let clear_mask = if self.blit_bytes_remaining > 0 {
                    self.blit_bytes_remaining -= 1;
                    0
                } else {
                    match value {
                        COMMAND_BLIT => {
                            self.blit_bytes_remaining = (self.width as u32) * (self.height as u32);
                            IFR_BLIT
                        }
                        COMMAND_BLIT_ALL => {
                            self.blit_bytes_remaining = WIDTH_IN_PIXELS * HEIGHT_IN_PIXELS;
                            IFR_BLIT
                        }
                        COMMAND_FILL | COMMAND_FILL_ALL => IFR_FILL,
                        _ => 0,
                    }
                };
                self.write_ifr(IFR_READY | clear_mask);
                self.send_register(offset, self.cmd_data);
            },
            _ => (),
        }
    }

    fn peek(&self, address: u16) -> u8 {
        match address - self.address_range.start {
            0 => self.x,
            1 => self.y,
            2 => self.width,
            3 => self.height,
            4 => self.color,
            5 => self.ifr,
            6 => self.ier | IFR_IRQ,
            7 => self.cmd_data,
            _ => 0xFF,
        }
    }

    fn claims(&self, address: u16) -> bool {
        self.address_range.contains(address)
    }

    fn tick(&mut self, _cycles: u32) {
        self.poll_transport();
    }

    fn reset(&mut self) {
        self.blit_bytes_remaining = 0;
        log_msg!(self.log_sender, LogLevel::Info, LogCategory::Device, "{} @0x{:04x} reset", self.name(), self.address_range.start);
    }

    fn irq_active(&self) -> bool {
        self.ifr & self.ier != 0
    }

    fn name(&self) -> &str {
        self.name
    }

    fn shutdown(&mut self) {
        if let Some(transport) = self.transport.as_mut() {
            transport.shutdown();
        }
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emulator::transport::InternalPipeTransport;
    use crossbeam_channel::{Sender, unbounded};
    use std::time::Duration;

    const DEVICE_NAME: &str = "LedMatrixDisplay";
    const DEVICE_ADDRESS: u16 = 0xD000;
    /// Client tag used for every simulated peripheral connection in these tests.
    const TAG: u8 = 1;

    fn device() -> LedMatrix {
        LedMatrix::new(DEVICE_NAME, AddressRange::new(DEVICE_ADDRESS, DEVICE_ADDRESS + 7))
    }

    /// `remote` is the device's outbound-write sink (verified via
    /// `collect_bytes`); `tx` feeds the device's inbound tagged relay
    /// directly, simulating a virtual display connecting/disconnecting or
    /// sending IFR updates. `pair_direct()` gives both ends of the test pipe
    /// no relay of their own, so this hand-fed tagged relay stands in for it.
    fn device_with_pipe() -> (LedMatrix, InternalPipeTransport, Sender<TransportEvent>) {
        let (local, remote) = InternalPipeTransport::pair_direct().unwrap();
        let (tx, rx) = unbounded();
        let relay = ChannelRelay::spawn(rx, 256);
        let mut device = LedMatrix::new(DEVICE_NAME,
                                        AddressRange::new(DEVICE_ADDRESS, DEVICE_ADDRESS + 7));
        device.attach_transport(Box::new(local), relay);
        (device, remote, tx)
    }

    /// Sends `event` into the device's inbound relay and waits for the relay
    /// thread to have made it visible to the next `drain_into` call.
    fn send_event(tx: &Sender<TransportEvent>, event: TransportEvent) {
        tx.send(event).unwrap();
        std::thread::sleep(Duration::from_millis(5));
    }

    fn collect_bytes(remote: &mut InternalPipeTransport) -> Vec<u8> {
        let mut buf = Vec::new();
        while let Some(b) = remote.try_recv() {
            buf.push(b);
        }
        buf
    }

    #[test]
    fn write_ifr_to_clear_bits() {
        let mut device = device();
        device.ifr = IFR_IRQ | IFR_READY | IFR_BLIT | IFR_FILL;
        device.write_ifr(IFR_READY);
        assert_eq!(device.ifr, IFR_IRQ | IFR_BLIT | IFR_FILL);
        device.write_ifr(IFR_BLIT);
        assert_eq!(device.ifr, IFR_IRQ | IFR_FILL);
        device.write_ifr(IFR_FILL);
        assert_eq!(device.ifr, 0);
    }

    #[test]
    fn write_ier_to_set_bits() {
        let mut device = device();
        device.write_ier(IFR_IRQ | IFR_READY);
        assert_eq!(device.ier, IFR_READY);
        device.write_ier(IFR_IRQ | IFR_BLIT);
        assert_eq!(device.ier, IFR_READY | IFR_BLIT);
        device.write_ier(IFR_IRQ | IFR_FILL);
        assert_eq!(device.ier, IFR_READY | IFR_BLIT | IFR_FILL);
        // no other bits can be set
        device.write_ifr(!IFR_MASK);
        assert_eq!(device.ier, IFR_READY | IFR_BLIT | IFR_FILL);
    }

    #[test]
    fn write_ier_to_clear_bits() {
        let mut device = device();
        device.ier = IFR_READY | IFR_BLIT | IFR_FILL;
        device.write_ier(IFR_READY);
        assert_eq!(device.ier, IFR_BLIT | IFR_FILL);
        device.write_ier(IFR_BLIT);
        assert_eq!(device.ier, IFR_FILL);
        device.write_ier(IFR_FILL);
        assert_eq!(device.ier, 0);
    }

    #[test]
    fn poll_transport_on_connected() {
        let (mut device, mut remote, tx) = device_with_pipe();
        device.x = 1;
        device.y = 2;
        device.width = 3;
        device.height = 4;
        device.color = 5;
        device.ifr = 6;
        device.ier = 7;
        device.cmd_data = 8;
        send_event(&tx, TransportEvent::Connected(TAG));
        device.poll_transport();
        assert_eq!(device.connection_tag, Some(TAG));
        assert_eq!(collect_bytes(&mut remote), vec![
            0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8,
        ]);
    }

    #[test]
    fn poll_transport_after_connected_observes_expected_tag() {
        let (mut device, _remote, tx) = device_with_pipe();
        device.connection_tag = Some(TAG);
        tx.send(TransportEvent::Data(TAG, IFR_READY)).unwrap();
        tx.send(TransportEvent::Data(TAG, IFR_BLIT)).unwrap();
        tx.send(TransportEvent::Data(TAG, 0)).unwrap();
        tx.send(TransportEvent::Data(TAG, !IFR_MASK)).unwrap();
        std::thread::sleep(Duration::from_millis(5));
        device.poll_transport();
        assert_eq!(device.ifr, IFR_IRQ | IFR_READY | IFR_BLIT);
        device.poll_transport();
        assert_eq!(device.ifr, IFR_IRQ | IFR_READY | IFR_BLIT);
    }

    #[test]
    fn poll_transport_after_connected_ignores_other_tag() {
        let (mut device, _remote, tx) = device_with_pipe();
        device.ifr = 0;
        device.connection_tag = Some(2);
        send_event(&tx, TransportEvent::Data(TAG, IFR_READY));
        device.poll_transport();
        assert_eq!(device.ifr, 0);
    }

    #[test]
    fn poll_transport_clears_connection_tag_on_disconnect_for_same_tag() {
        let (mut device, _remote, tx) = device_with_pipe();
        device.connection_tag = Some(TAG);
        send_event(&tx, TransportEvent::Disconnected(TAG));
        device.poll_transport();
        assert_eq!(device.connection_tag, None);
    }

    #[test]
    fn poll_transport_ignores_disconnect_for_other_tag() {
        let (mut device, _remote, tx) = device_with_pipe();
        device.connection_tag = Some(2);
        send_event(&tx, TransportEvent::Disconnected(TAG));
        device.poll_transport();
        assert_eq!(device.connection_tag, Some(2));
    }

    #[test]
    fn send_register() {
        let (mut device, mut remote, _tx) = device_with_pipe();
        device.send_register(0x55, 0xAA);
        assert_eq!(collect_bytes(&mut remote), vec![0x55, 0xAA]);
    }

    #[test]
    fn read_registers() {
        let mut device = device();
        device.x = 1;
        device.y = 2;
        device.width = 3;
        device.height = 4;
        device.color = 5;
        device.ifr = 6;
        device.ier = 7;
        device.cmd_data = 8;
        assert_eq!(device.read(DEVICE_ADDRESS), 1);
        assert_eq!(device.read(DEVICE_ADDRESS + 1), 2);
        assert_eq!(device.read(DEVICE_ADDRESS + 2), 3);
        assert_eq!(device.read(DEVICE_ADDRESS + 3), 4);
        assert_eq!(device.read(DEVICE_ADDRESS + 4), 5);
        assert_eq!(device.read(DEVICE_ADDRESS + 5), 6);
        // IER bit 7 should always read as 1
        assert_eq!(device.read(DEVICE_ADDRESS + 6), IFR_IRQ | 7);
        assert_eq!(device.read(DEVICE_ADDRESS + 7), 8);
    }

    #[test]
    fn write_registers_parameters() {
        let (mut device, mut remote, _tx) = device_with_pipe();
        // writing X should clamp to display width in pixels less one
        device.write(DEVICE_ADDRESS, 0xFF);
        assert_eq!(device.x, (WIDTH_IN_PIXELS - 1) as u8);
        assert_eq!(collect_bytes(&mut remote), vec![0, device.x]);
        // writing Y should clamp to display height in pixels less one
        device.write(DEVICE_ADDRESS + 1, 0xFF);
        assert_eq!(device.y, (HEIGHT_IN_PIXELS - 1) as u8);
        assert_eq!(collect_bytes(&mut remote), vec![1, device.y]);
        // writing WIDTH should clamp to display width in pixels less one
        device.write(DEVICE_ADDRESS + 2, 0xFF);
        assert_eq!(device.width, (WIDTH_IN_PIXELS - 1) as u8);
        assert_eq!(collect_bytes(&mut remote), vec![2, device.width]);
        // writing HEIGHT should clamp to display height in pixels
        device.write(DEVICE_ADDRESS + 3, 0xFF);
        assert_eq!(device.height, (HEIGHT_IN_PIXELS - 1) as u8);
        assert_eq!(collect_bytes(&mut remote), vec![3, device.height]);
        // writing COLOR should transfer value written
        device.write(DEVICE_ADDRESS + 4, 0xFF);
        assert_eq!(collect_bytes(&mut remote), vec![4, device.color]);
    }

    #[test]
    fn write_ifr_register() {
        let (mut device, mut remote, _tx) = device_with_pipe();
        // Here we're just confirming that the address targets the IFR.
        // See `write_ifr_to_clear_bits` for the more comprehensive test.
        device.ifr = IFR_IRQ | IFR_READY;
        device.write(DEVICE_ADDRESS + 5, IFR_READY);
        assert_eq!(device.ifr, 0);
        assert_eq!(collect_bytes(&mut remote), vec![5, device.ifr]);
    }

    #[test]
    fn write_ier_register() {
        let (mut device, mut remote, _tx) = device_with_pipe();
        // Here we're just confirming that the address targets the IFR.
        // See `write_ier_to_set_bits` and `write_ier_to_clear_bits` as the more comprehensive tests.
        device.ier = IFR_READY;
        device.write(DEVICE_ADDRESS + 6, IFR_READY);
        assert_eq!(device.ier, 0);
        assert_eq!(collect_bytes(&mut remote), vec![6, device.ier]);
    }

    #[test]
    fn write_cmd_data_register() {
        let (mut device, mut remote, _tx) = device_with_pipe();
        // Here we're just confirming that the address targets the IFR.
        // See `write_ier_to_set_bits` and `write_ier_to_clear_bits` as the more comprehensive tests.
        device.ifr = IFR_IRQ | IFR_READY;
        device.write(DEVICE_ADDRESS + 7, 0x55);
        assert_eq!(device.cmd_data, 0x55);
        assert_eq!(device.ifr, 0);
        assert_eq!(collect_bytes(&mut remote), vec![7, device.cmd_data]);
    }

    #[test]
    fn write_command_blit_clears_ifr_blit_and_sets_expected_bytes() {
        let mut device = device();
        device.width = (WIDTH_IN_PIXELS - 1) as u8;
        device.height = (HEIGHT_IN_PIXELS - 1) as u8;
        device.ifr = IFR_BLIT;
        device.write(DEVICE_ADDRESS + 7, COMMAND_BLIT);
        assert_eq!(device.ifr, 0);
        assert_eq!(device.blit_bytes_remaining, (WIDTH_IN_PIXELS - 1) * (HEIGHT_IN_PIXELS - 1))
    }

    #[test]
    fn write_command_blit_all_clears_ifr_blit_and_sets_expected_bytes() {
        let mut device = device();
        device.ifr = IFR_BLIT;
        device.write(DEVICE_ADDRESS + 7, COMMAND_BLIT_ALL);
        assert_eq!(device.ifr, 0);
        assert_eq!(device.blit_bytes_remaining, WIDTH_IN_PIXELS * HEIGHT_IN_PIXELS);
    }

    #[test]
    fn write_command_fill_clears_ifr_fill() {
        let mut device = device();
        device.ifr = IFR_FILL;
        device.write(DEVICE_ADDRESS + 7, COMMAND_FILL);
        assert_eq!(device.ifr, 0);
    }

    #[test]
    fn write_command_fill_all_clears_ifr_fill() {
        let mut device = device();
        device.ifr = IFR_FILL;
        device.write(DEVICE_ADDRESS + 7, COMMAND_FILL_ALL);
        assert_eq!(device.ifr, 0);
    }

    #[test]
    fn peek_registers() {
        let mut device = device();
        device.x = 1;
        device.y = 2;
        device.width = 3;
        device.height = 4;
        device.color = 5;
        device.ifr = 6;
        device.ier = 7;
        device.cmd_data = 8;
        assert_eq!(device.peek(DEVICE_ADDRESS), 1);
        assert_eq!(device.peek(DEVICE_ADDRESS + 1), 2);
        assert_eq!(device.peek(DEVICE_ADDRESS + 2), 3);
        assert_eq!(device.peek(DEVICE_ADDRESS + 3), 4);
        assert_eq!(device.peek(DEVICE_ADDRESS + 4), 5);
        assert_eq!(device.peek(DEVICE_ADDRESS + 5), 6);
        // IER bit 7 should always read as 1
        assert_eq!(device.peek(DEVICE_ADDRESS + 6), IFR_IRQ | 7);
        assert_eq!(device.peek(DEVICE_ADDRESS + 7), 8);
    }

    #[test]
    fn irq_active() {
        let mut device = device();
        device.ier = IFR_MASK;
        device.ifr = 0;
        assert!(!device.irq_active());
        device.ifr = IFR_READY;
        assert!(device.irq_active());
        device.ifr = IFR_BLIT;
        assert!(device.irq_active());
        device.ifr = IFR_FILL;
        assert!(device.irq_active());
    }

    #[test]
    fn reset_logs_device_message() {
        let (sender, rx) = crate::emulator::logging::test_channel_sender(4);
        let mut device = device();
        device.set_log_sender(sender);
        device.reset();
        let received = rx.recv().unwrap();
        assert_eq!(received.category, LogCategory::Device);
        assert_eq!(received.message, format!("{DEVICE_NAME} @0x{DEVICE_ADDRESS:04x} reset"));
    }

}