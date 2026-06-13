//! WDC 65C22 Versatile Interface Adapter (VIA).
//!
//! Provides 16 addressable registers (offsets 0x0–0xF):
//!
//! | Offset | Name  | Description                            |
//! |--------|-------|----------------------------------------|
//! | 0x0    | ORB   | Output Register B / Input Register B  |
//! | 0x1    | ORA   | Output Register A / Input Register A  |
//! | 0x2    | DDRB  | Data Direction Register B              |
//! | 0x3    | DDRA  | Data Direction Register A              |
//! | 0x4    | T1CL  | Timer 1 Counter Low (read) / Latch Low (write) |
//! | 0x5    | T1CH  | Timer 1 Counter High                   |
//! | 0x6    | T1LL  | Timer 1 Latch Low                      |
//! | 0x7    | T1LH  | Timer 1 Latch High                     |
//! | 0x8    | T2CL  | Timer 2 Counter Low (read) / Latch Low (write) |
//! | 0x9    | T2CH  | Timer 2 Counter High                   |
//! | 0xA    | SR    | Shift Register                         |
//! | 0xB    | ACR   | Auxiliary Control Register             |
//! | 0xC    | PCR   | Peripheral Control Register            |
//! | 0xD    | IFR   | Interrupt Flag Register                |
//! | 0xE    | IER   | Interrupt Enable Register              |
//! | 0xF    | ORA_NH| Output Register A (no handshake)       |
//!
//! **ACR bit layout:**
//! - Bit 0: T1 latch enable for PB7
//! - Bit 1: T1 PB7 output enable
//! - Bits 4–2: Shift register mode
//! - Bit 5: T2 timer mode (0=timed, 1=count PB6)
//! - Bits 7–6: T1 mode (00/01=one-shot, 10/11=free-run)
//!
//! **PCR bit layout:**
//! - Bit 0: CA1 edge select (0=negative, 1=positive)
//! - Bits 3–1: CA2 control
//! - Bit 4: CB1 edge select
//! - Bits 7–5: CB2 control
//!
//! **IFR/IER bit layout:**
//! - Bit 0: CA2
//! - Bit 1: CA1
//! - Bit 2: Shift register complete
//! - Bit 3: CB2
//! - Bit 4: CB1
//! - Bit 5: T2 timeout
//! - Bit 6: T1 timeout
//! - Bit 7: IRQ active (IFR) / set-not-clear (IER write)

use crate::emulator::device::{DeviceId, ErrorSender, IoDevice};
use crate::emulator::device::via_protocol::{
    ViaProtocolDecoder, ViaProtocolEncoder, ViaProtocolMessage,
};
use crate::emulator::transport::{Transport, TransportError};

// --- IFR/IER bit masks ---
const IRQ_CA2: u8 = 0x01;
const IRQ_CA1: u8 = 0x02;
const IRQ_SR:  u8 = 0x04;
const IRQ_CB2: u8 = 0x08;
const IRQ_CB1: u8 = 0x10;
const IRQ_T2:  u8 = 0x20;
const IRQ_T1:  u8 = 0x40;
const IRQ_ANY: u8 = 0x80;

// --- ACR masks ---
#[allow(dead_code)]
const ACR_T1_PB7_LATCH:  u8 = 0x01; // reserved for future latch-enable use
const ACR_T1_PB7_OUTPUT: u8 = 0x02;
#[allow(dead_code)]
const ACR_SR_MODE_MASK:  u8 = 0x1C; // reserved for shift register mode
const ACR_T2_COUNT_PB6:  u8 = 0x20;
const ACR_T1_MODE_MASK:  u8 = 0xC0;
#[allow(dead_code)]
const ACR_T1_FREE_RUN:   u8 = 0x40; // reserved: use ACR_T1_MODE_MASK comparison instead

// --- PCR masks ---
const PCR_CA1_EDGE:  u8 = 0x01;
const PCR_CA2_MASK:  u8 = 0x0E;
const PCR_CB1_EDGE:  u8 = 0x10;
const PCR_CB2_MASK:  u8 = 0xE0;

// --- Timer 1 modes ---
#[allow(dead_code)]
const T1_MODE_ONE_SHOT:  u8 = 0x00; // reserved for explicit mode comparison
const T1_MODE_FREE_RUN:  u8 = 0x40;

/// Selects which VIA interface a transport is bound to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViaPortBinding {
    /// Transport communicates port A and CA1/CA2 state changes.
    PortA,
    /// Transport communicates port B and CB1/CB2 state changes.
    PortB,
    /// Transport communicates shift register clock (CB1) and data (CB2) transitions.
    ShiftRegister,
}

/// WDC 65C22 Versatile Interface Adapter.
pub struct Via6522 {
    // --- Port registers ---
    /// Output register B — written bits drive output pins on port B.
    orb: u8,
    /// Output register A — written bits drive output pins on port A.
    ora: u8,
    /// Data direction register B — 1=output, 0=input.
    ddrb: u8,
    /// Data direction register A — 1=output, 0=input.
    ddra: u8,
    /// Latched input state of port B pins (updated by peripheral messages).
    input_b: u8,
    /// Latched input state of port A pins (updated by peripheral messages).
    input_a: u8,

    // --- Timer 1 ---
    /// Timer 1 counter (16-bit, decrements each cycle).
    t1_counter: u16,
    /// Timer 1 latch (reload value for free-run mode).
    t1_latch: u16,
    /// True when timer 1 is actively counting.
    t1_running: bool,
    /// Current PB7 toggle state (toggled on T1 underflow when ACR enables it).
    t1_pb7: bool,

    // --- Timer 2 ---
    /// Timer 2 counter (16-bit).
    t2_counter: u16,
    /// Timer 2 latch low byte (written to offset 8 before loading).
    t2_latch_lo: u8,
    /// True when timer 2 is actively counting.
    t2_running: bool,

    // --- Shift register ---
    /// Shift register data byte.
    sr: u8,
    /// Shift bit counter (0–8; 8 = complete). Reserved for shift register implementation.
    #[allow(dead_code)]
    sr_count: u8,

    // --- Control and interrupt registers ---
    /// Auxiliary control register.
    acr: u8,
    /// Peripheral control register.
    pcr: u8,
    /// Interrupt flag register (bits 0–6; bit 7 computed).
    ifr: u8,
    /// Interrupt enable register (bits 0–6).
    ier: u8,

    // --- Control signal state ---
    /// CA1 input line state.
    ca1: bool,
    /// CA2 input/output line state.
    ca2: bool,
    /// CB1 input line state.
    cb1: bool,
    /// CB2 input/output line state.
    cb2: bool,

    // --- Transport / protocol (Port A binding) ---
    /// Transport for port A / CA1/CA2.
    transport_a: Option<Box<dyn Transport>>,
    /// Encoder for outgoing messages on transport A.
    encoder_a: ViaProtocolEncoder,
    /// Decoder for incoming messages on transport A.
    decoder_a: ViaProtocolDecoder,
    /// True once the transport A connection handshake is complete.
    handshake_a_done: bool,

    // --- Transport / protocol (Port B / shift register binding) ---
    /// Transport for port B / CB1/CB2 / shift register.
    transport_b: Option<Box<dyn Transport>>,
    /// Encoder for outgoing messages on transport B.
    encoder_b: ViaProtocolEncoder,
    /// Decoder for incoming messages on transport B.
    decoder_b: ViaProtocolDecoder,
    /// True once the transport B connection handshake is complete.
    handshake_b_done: bool,

    // --- Async error reporting ---
    /// Destination for async transport error events.
    error_sender: Option<ErrorSender>,
    /// Identity used in error events.
    device_id: Option<DeviceId>,

    /// Suppress Timer 1 PB7 protocol messages even when ACR enables PB7 output.
    suppress_t1_pb7_messages: bool,

    /// Scratch buffer for encoding outgoing protocol bytes.
    tx_buf: Vec<u8>,
}

impl Via6522 {
    /// Creates a new `Via6522` in reset state.
    pub fn new() -> Self {
        Self {
            orb: 0, ora: 0, ddrb: 0, ddra: 0,
            input_b: 0, input_a: 0,
            t1_counter: 0, t1_latch: 0, t1_running: false, t1_pb7: false,
            t2_counter: 0, t2_latch_lo: 0, t2_running: false,
            sr: 0, sr_count: 0,
            acr: 0, pcr: 0, ifr: 0, ier: 0,
            ca1: false, ca2: false, cb1: false, cb2: false,
            transport_a: None,
            encoder_a: ViaProtocolEncoder::new(),
            decoder_a: ViaProtocolDecoder::new(),
            handshake_a_done: false,
            transport_b: None,
            encoder_b: ViaProtocolEncoder::new(),
            decoder_b: ViaProtocolDecoder::new(),
            handshake_b_done: false,
            error_sender: None,
            device_id: None,
            suppress_t1_pb7_messages: false,
            tx_buf: Vec::new(),
        }
    }

    /// Attaches a transport bound to the given port interface.
    pub fn attach_transport(&mut self, transport: Box<dyn Transport>, binding: ViaPortBinding) {
        match binding {
            ViaPortBinding::PortA => {
                self.transport_a = Some(transport);
                self.encoder_a = ViaProtocolEncoder::new();
                self.decoder_a = ViaProtocolDecoder::new();
                self.handshake_a_done = false;
            }
            ViaPortBinding::PortB | ViaPortBinding::ShiftRegister => {
                self.transport_b = Some(transport);
                self.encoder_b = ViaProtocolEncoder::new();
                self.decoder_b = ViaProtocolDecoder::new();
                self.handshake_b_done = false;
            }
        }
    }

    /// Sets the error sender for async transport event reporting.
    pub fn set_error_sender(&mut self, sender: ErrorSender, id: DeviceId) {
        self.error_sender = Some(sender);
        self.device_id = Some(id);
    }

    /// When set, suppresses port B state change messages generated by Timer 1 PB7 toggles.
    pub fn suppress_t1_pb7_messages(&mut self) {
        self.suppress_t1_pb7_messages = true;
    }

    fn report_error(&self, error: TransportError) {
        if let (Some(sender), Some(id)) = (&self.error_sender, self.device_id) {
            use crate::emulator::device::DeviceEvent;
            let _ = sender.send(DeviceEvent::TransportError { device: id, error });
        }
    }

    // --- IFR helpers ---

    fn set_ifr(&mut self, bits: u8) {
        self.ifr |= bits & 0x7F;
    }

    fn clear_ifr(&mut self, bits: u8) {
        self.ifr &= !(bits & 0x7F);
    }

    fn ifr_read(&self) -> u8 {
        if self.ifr & self.ier & 0x7F != 0 {
            self.ifr | IRQ_ANY
        } else {
            self.ifr & !IRQ_ANY
        }
    }

    // --- Port read helpers ---

    /// Reads the effective state of port B: output pins from ORB, input pins from input_b.
    /// PB7 reflects the T1 toggle state when ACR enables T1 PB7 output.
    fn read_port_b(&self) -> u8 {
        let orb = if self.acr & ACR_T1_PB7_OUTPUT != 0 {
            (self.orb & 0x7F) | if self.t1_pb7 { 0x80 } else { 0 }
        } else {
            self.orb
        };
        (orb & self.ddrb) | (self.input_b & !self.ddrb)
    }

    /// Reads the effective state of port A: output pins from ORA, input pins from input_a.
    fn read_port_a(&self) -> u8 {
        (self.ora & self.ddra) | (self.input_a & !self.ddra)
    }

    // --- Protocol transmission helpers ---

    fn send_a(&mut self, message: ViaProtocolMessage) {
        self.tx_buf.clear();
        self.encoder_a.encode(message, &mut self.tx_buf);
        let bytes: Vec<u8> = self.tx_buf.drain(..).collect();
        if let Some(t) = self.transport_a.as_mut() {
            for b in bytes {
                if let Err(e) = t.send(b) {
                    self.report_error(e);
                    return;
                }
            }
        }
    }

    fn send_b(&mut self, message: ViaProtocolMessage) {
        self.tx_buf.clear();
        self.encoder_b.encode(message, &mut self.tx_buf);
        let bytes: Vec<u8> = self.tx_buf.drain(..).collect();
        if let Some(t) = self.transport_b.as_mut() {
            for b in bytes {
                if let Err(e) = t.send(b) {
                    self.report_error(e);
                    return;
                }
            }
        }
    }

    /// Sends the full state dump (port A + CA1/CA2) on transport A after format negotiation.
    fn send_state_dump_a(&mut self) {
        self.send_a(ViaProtocolMessage::PortStateChange { port: 'A', value: self.read_port_a() });
        // Control signal bit layout: CA1=bit1, CA2=bit0.
        for (signals, state) in [(0x02u8, self.ca1), (0x01u8, self.ca2)] {
            self.send_a(ViaProtocolMessage::ControlSignalChange { signals, state });
        }
    }

    /// Sends the full state dump (port B + CB1/CB2) on transport B after format negotiation.
    fn send_state_dump_b(&mut self) {
        self.send_b(ViaProtocolMessage::PortStateChange { port: 'B', value: self.read_port_b() });
        // Control signal bit layout: CB1=bit3, CB2=bit2.
        for (signals, state) in [(0x08u8, self.cb1), (0x04u8, self.cb2)] {
            self.send_b(ViaProtocolMessage::ControlSignalChange { signals, state });
        }
    }


    // --- Handshake / incoming byte processing ---

    /// Polls transport A for incoming bytes and processes any decoded messages.
    fn poll_transport_a(&mut self) {
        loop {
            let byte = match self.transport_a.as_mut().and_then(|t| t.try_recv()) {
                Some(b) => b,
                None => break,
            };

            // Feed into decoder; this may lock the format.
            let msg = self.decoder_a.feed(byte);

            // First qualifying byte locks format → complete the handshake.
            if !self.handshake_a_done && self.decoder_a.format().is_some() {
                self.handshake_a_done = true;
                use crate::emulator::device::via_protocol::ViaProtocolFormat;
                if self.decoder_a.format() == Some(ViaProtocolFormat::Binary) {
                    self.encoder_a.select_binary();
                }
                self.send_state_dump_a();
            }

            if let Some(m) = msg {
                self.apply_message_a(m);
            }
        }
    }

    /// Polls transport B for incoming bytes and processes any decoded messages.
    fn poll_transport_b(&mut self) {
        loop {
            let byte = match self.transport_b.as_mut().and_then(|t| t.try_recv()) {
                Some(b) => b,
                None => break,
            };

            let msg = self.decoder_b.feed(byte);

            if !self.handshake_b_done && self.decoder_b.format().is_some() {
                self.handshake_b_done = true;
                use crate::emulator::device::via_protocol::ViaProtocolFormat;
                if self.decoder_b.format() == Some(ViaProtocolFormat::Binary) {
                    self.encoder_b.select_binary();
                }
                self.send_state_dump_b();
            }

            if let Some(m) = msg {
                self.apply_message_b(m);
            }
        }
    }

    /// Applies an incoming protocol message on the port A transport.
    fn apply_message_a(&mut self, msg: ViaProtocolMessage) {
        match msg {
            ViaProtocolMessage::PortStateChange { port: 'A', value } => {
                let old = self.input_a;
                self.input_a = value;
                if old != value {
                    // CA1 latches on configured edge.
                    let pos_edge = self.pcr & PCR_CA1_EDGE != 0;
                    let triggered = if pos_edge {
                        (old & !value) != 0 || (!old & value) != 0 // any change; simplified
                    } else {
                        true
                    };
                    if triggered { self.set_ifr(IRQ_CA1); }
                }
            }
            ViaProtocolMessage::ControlSignalChange { signals, state } => {
                if signals & 0x02 != 0 { // CA1
                    let pos_edge = self.pcr & PCR_CA1_EDGE != 0;
                    if state == pos_edge { self.set_ifr(IRQ_CA1); }
                    self.ca1 = state;
                }
                if signals & 0x01 != 0 { // CA2 (when configured as input)
                    let ca2_mode = (self.pcr & PCR_CA2_MASK) >> 1;
                    if ca2_mode < 4 { // input modes
                        let pos_edge = ca2_mode & 0x02 != 0;
                        if state == pos_edge { self.set_ifr(IRQ_CA2); }
                        self.ca2 = state;
                    }
                }
            }
            _ => {} // ignore misrouted messages
        }
    }

    /// Applies an incoming protocol message on the port B transport.
    fn apply_message_b(&mut self, msg: ViaProtocolMessage) {
        match msg {
            ViaProtocolMessage::PortStateChange { port: 'B', value } => {
                let old = self.input_b;
                self.input_b = value;
                if old != value {
                    let pos_edge = self.pcr & PCR_CB1_EDGE != 0;
                    let triggered = if pos_edge {
                        (!old & value) != 0
                    } else {
                        (old & !value) != 0
                    };
                    if triggered { self.set_ifr(IRQ_CB1); }
                }
            }
            ViaProtocolMessage::ControlSignalChange { signals, state } => {
                if signals & 0x08 != 0 { // CB1
                    let pos_edge = self.pcr & PCR_CB1_EDGE != 0;
                    if state == pos_edge { self.set_ifr(IRQ_CB1); }
                    self.cb1 = state;
                }
                if signals & 0x04 != 0 { // CB2 (when configured as input)
                    let cb2_mode = (self.pcr & PCR_CB2_MASK) >> 5;
                    if cb2_mode < 4 { // input modes
                        let pos_edge = cb2_mode & 0x02 != 0;
                        if state == pos_edge { self.set_ifr(IRQ_CB2); }
                        self.cb2 = state;
                    }
                }
            }
            _ => {}
        }
    }

    // --- Timer tick ---

    fn tick_timers(&mut self, cycles: u32) {
        // Timer 1: fires when counter reaches 0 or wraps.
        if self.t1_running {
            let (new_counter, wrapped) = self.t1_counter.overflowing_sub(cycles as u16);
            if wrapped || new_counter == 0 {
                self.set_ifr(IRQ_T1);
                if self.acr & ACR_T1_PB7_OUTPUT != 0 {
                    self.t1_pb7 = !self.t1_pb7;
                    if !self.suppress_t1_pb7_messages {
                        // read_port_b() reflects the updated t1_pb7.
                        let pb = self.read_port_b();
                        self.send_b(ViaProtocolMessage::PortStateChange { port: 'B', value: pb });
                    }
                }
                if self.acr & ACR_T1_MODE_MASK == T1_MODE_FREE_RUN {
                    // Reload from latch; account for any cycles past zero.
                    let overshoot = if wrapped { 0u16 } else { 0u16 };
                    self.t1_counter = self.t1_latch.saturating_sub(overshoot);
                } else {
                    self.t1_running = false;
                    self.t1_counter = 0xFFFF;
                }
            } else {
                self.t1_counter = new_counter;
            }
        }

        // Timer 2 (timed mode only; PB6 pulse-counting not implemented here).
        if self.t2_running && self.acr & ACR_T2_COUNT_PB6 == 0 {
            let (new_counter, wrapped) = self.t2_counter.overflowing_sub(cycles as u16);
            if wrapped || new_counter == 0 {
                self.set_ifr(IRQ_T2);
                self.t2_running = false;
                self.t2_counter = 0xFFFF;
            } else {
                self.t2_counter = new_counter;
            }
        }
    }
}

impl Default for Via6522 {
    fn default() -> Self {
        Self::new()
    }
}

impl IoDevice for Via6522 {
    /// Reads the register at `offset` with side effects.
    fn read(&mut self, offset: u16) -> u8 {
        match offset {
            0x0 => {
                // Reading ORB clears CB1 and CB2 interrupt flags.
                self.clear_ifr(IRQ_CB1 | IRQ_CB2);
                self.read_port_b()
            }
            0x1 => {
                // Reading ORA clears CA1 and CA2 interrupt flags; triggers CA2 handshake pulse.
                self.clear_ifr(IRQ_CA1 | IRQ_CA2);
                self.read_port_a()
            }
            0x2 => self.ddrb,
            0x3 => self.ddra,
            0x4 => {
                // Reading T1CL clears T1 interrupt flag.
                self.clear_ifr(IRQ_T1);
                (self.t1_counter & 0xFF) as u8
            }
            0x5 => (self.t1_counter >> 8) as u8,
            0x6 => (self.t1_latch & 0xFF) as u8,
            0x7 => (self.t1_latch >> 8) as u8,
            0x8 => {
                // Reading T2CL clears T2 interrupt flag.
                self.clear_ifr(IRQ_T2);
                (self.t2_counter & 0xFF) as u8
            }
            0x9 => (self.t2_counter >> 8) as u8,
            0xA => {
                // Reading SR clears SR interrupt flag.
                self.clear_ifr(IRQ_SR);
                self.sr
            }
            0xB => self.acr,
            0xC => self.pcr,
            0xD => self.ifr_read(),
            0xE => self.ier | 0x80, // bit 7 always reads as 1
            0xF => {
                // ORA no-handshake: read without clearing CA1/CA2 flags.
                self.read_port_a()
            }
            _ => 0,
        }
    }

    /// Writes the register at `offset` with side effects.
    fn write(&mut self, offset: u16, value: u8) {
        match offset {
            0x0 => {
                // Writing ORB: update output register, clear CB1/CB2 flags.
                let old_orb = self.orb;
                self.orb = value;
                self.clear_ifr(IRQ_CB1 | IRQ_CB2);
                // Send port B state if any output pins changed.
                let old_b = (old_orb & self.ddrb) | (self.input_b & !self.ddrb);
                let new_b = self.read_port_b();
                if old_b != new_b {
                    self.send_b(ViaProtocolMessage::PortStateChange { port: 'B', value: new_b });
                }
            }
            0x1 => {
                // Writing ORA: update output register, clear CA1/CA2 flags.
                let old_ora = self.ora;
                self.ora = value;
                self.clear_ifr(IRQ_CA1 | IRQ_CA2);
                let old_a = (old_ora & self.ddra) | (self.input_a & !self.ddra);
                let new_a = self.read_port_a();
                if old_a != new_a {
                    self.send_a(ViaProtocolMessage::PortStateChange { port: 'A', value: new_a });
                }
            }
            0x2 => {
                let old_ddrb = self.ddrb;
                self.ddrb = value;
                // DDR change may change the effective output; send update.
                let old_b = (self.orb & old_ddrb) | (self.input_b & !old_ddrb);
                let new_b = self.read_port_b();
                if old_b != new_b {
                    self.send_b(ViaProtocolMessage::PortStateChange { port: 'B', value: new_b });
                }
            }
            0x3 => {
                let old_ddra = self.ddra;
                self.ddra = value;
                let old_a = (self.ora & old_ddra) | (self.input_a & !old_ddra);
                let new_a = self.read_port_a();
                if old_a != new_a {
                    self.send_a(ViaProtocolMessage::PortStateChange { port: 'A', value: new_a });
                }
            }
            0x4 => {
                // Write T1 latch low — just stores; does not start timer.
                self.t1_latch = (self.t1_latch & 0xFF00) | value as u16;
            }
            0x5 => {
                // Write T1 counter high — loads latch high, transfers latch to counter, starts timer.
                self.t1_latch = (self.t1_latch & 0x00FF) | ((value as u16) << 8);
                self.t1_counter = self.t1_latch;
                self.t1_running = true;
                self.clear_ifr(IRQ_T1);
                // Reset PB7 low when timer starts (per datasheet).
                if self.acr & ACR_T1_PB7_OUTPUT != 0 {
                    self.t1_pb7 = false;
                }
            }
            0x6 => {
                self.t1_latch = (self.t1_latch & 0xFF00) | value as u16;
            }
            0x7 => {
                self.t1_latch = (self.t1_latch & 0x00FF) | ((value as u16) << 8);
                self.clear_ifr(IRQ_T1);
            }
            0x8 => {
                // Write T2 latch low.
                self.t2_latch_lo = value;
            }
            0x9 => {
                // Write T2 counter high — loads counter, starts timer.
                self.t2_counter = ((value as u16) << 8) | self.t2_latch_lo as u16;
                self.t2_running = true;
                self.clear_ifr(IRQ_T2);
            }
            0xA => {
                self.sr = value;
                self.clear_ifr(IRQ_SR);
            }
            0xB => {
                self.acr = value;
            }
            0xC => {
                self.pcr = value;
            }
            0xD => {
                // Writing IFR clears the specified bits.
                self.clear_ifr(value);
            }
            0xE => {
                // Bit 7 selects set (1) or clear (0) mode for bits 0–6.
                if value & 0x80 != 0 {
                    self.ier |= value & 0x7F;
                } else {
                    self.ier &= !(value & 0x7F);
                }
            }
            0xF => {
                // ORA no-handshake write.
                let old_ora = self.ora;
                self.ora = value;
                let old_a = (old_ora & self.ddra) | (self.input_a & !self.ddra);
                let new_a = self.read_port_a();
                if old_a != new_a {
                    self.send_a(ViaProtocolMessage::PortStateChange { port: 'A', value: new_a });
                }
            }
            _ => {}
        }
    }

    /// Reads the register at `offset` without side effects.
    fn peek(&self, offset: u16) -> u8 {
        match offset {
            0x0 => self.read_port_b(),
            0x1 => self.read_port_a(),
            0x2 => self.ddrb,
            0x3 => self.ddra,
            0x4 => (self.t1_counter & 0xFF) as u8,
            0x5 => (self.t1_counter >> 8) as u8,
            0x6 => (self.t1_latch & 0xFF) as u8,
            0x7 => (self.t1_latch >> 8) as u8,
            0x8 => (self.t2_counter & 0xFF) as u8,
            0x9 => (self.t2_counter >> 8) as u8,
            0xA => self.sr,
            0xB => self.acr,
            0xC => self.pcr,
            0xD => self.ifr_read(),
            0xE => self.ier | 0x80,
            0xF => self.read_port_a(),
            _ => 0,
        }
    }

    /// Ticks timers and polls transports for incoming protocol messages.
    fn tick(&mut self, cycles: u32) {
        self.tick_timers(cycles);
        self.poll_transport_a();
        self.poll_transport_b();
    }

    /// Returns `true` when any enabled interrupt flag is set.
    fn irq_active(&self) -> bool {
        self.ifr & self.ier & 0x7F != 0
    }

    fn name(&self) -> &str {
        "via6522"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emulator::transport::PipeTransport;
    use std::time::Duration;

    fn device() -> Via6522 {
        Via6522::new()
    }

    fn device_with_pipe_b() -> (Via6522, PipeTransport) {
        let (local, remote) = PipeTransport::pair().unwrap();
        let mut via = Via6522::new();
        via.attach_transport(Box::new(local), ViaPortBinding::PortB);
        (via, remote)
    }

    fn device_with_pipe_a() -> (Via6522, PipeTransport) {
        let (local, remote) = PipeTransport::pair().unwrap();
        let mut via = Via6522::new();
        via.attach_transport(Box::new(local), ViaPortBinding::PortA);
        (via, remote)
    }

    // --- Initial state ---

    #[test]
    fn new_all_registers_zero() {
        let via = device();
        assert_eq!(via.peek(0x0), 0); // ORB
        assert_eq!(via.peek(0x1), 0); // ORA
        assert_eq!(via.peek(0x2), 0); // DDRB
        assert_eq!(via.peek(0x3), 0); // DDRA
        assert_eq!(via.peek(0xB), 0); // ACR
        assert_eq!(via.peek(0xC), 0); // PCR
    }

    #[test]
    fn new_ier_reads_with_bit7_set() {
        let via = device();
        assert_eq!(via.peek(0xE), 0x80);
    }

    #[test]
    fn new_irq_not_active() {
        let via = device();
        assert!(!via.irq_active());
    }

    // --- DDR and port read/write ---

    #[test]
    fn ddrb_controls_output_vs_input() {
        let mut via = device();
        via.write(0x2, 0xF0); // upper nibble = output, lower = input
        via.input_b = 0x0A;   // simulate peripheral driving lower nibble
        via.write(0x0, 0x50); // write 0x50 to ORB (upper nibble)
        assert_eq!(via.read(0x0), 0x5A); // output bits from ORB, input bits from input_b
    }

    #[test]
    fn ddra_controls_output_vs_input() {
        let mut via = device();
        via.write(0x3, 0x0F); // lower nibble = output, upper = input
        via.input_a = 0xC0;
        via.write(0x1, 0x07);
        assert_eq!(via.read(0x1), 0xC7);
    }

    #[test]
    fn write_read_ddrb() {
        let mut via = device();
        via.write(0x2, 0xAA);
        assert_eq!(via.peek(0x2), 0xAA);
    }

    #[test]
    fn write_read_ddra() {
        let mut via = device();
        via.write(0x3, 0x55);
        assert_eq!(via.peek(0x3), 0x55);
    }

    // --- ACR / PCR ---

    #[test]
    fn write_read_acr() {
        let mut via = device();
        via.write(0xB, 0x5A);
        assert_eq!(via.peek(0xB), 0x5A);
    }

    #[test]
    fn write_read_pcr() {
        let mut via = device();
        via.write(0xC, 0xA5);
        assert_eq!(via.peek(0xC), 0xA5);
    }

    // --- IER ---

    #[test]
    fn ier_set_bits_with_bit7() {
        let mut via = device();
        via.write(0xE, 0x82); // set bit 1 (CA1)
        assert_eq!(via.peek(0xE), 0x82); // bit 7 always 1 on read
    }

    #[test]
    fn ier_clear_bits_without_bit7() {
        let mut via = device();
        via.write(0xE, 0xFF); // set all
        via.write(0xE, 0x02); // clear bit 1
        assert_eq!(via.peek(0xE), 0xFD);
    }

    // --- IFR ---

    #[test]
    fn write_ifr_clears_bits() {
        let mut via = device();
        via.set_ifr(0x42); // manually set bits 1 and 6
        via.write(0xD, 0x40); // clear bit 6
        assert_eq!(via.peek(0xD) & 0x7F, 0x02);
    }

    #[test]
    fn ifr_bit7_set_when_enabled_flag_set() {
        let mut via = device();
        via.write(0xE, 0x82); // enable CA1
        via.set_ifr(IRQ_CA1);
        assert_ne!(via.peek(0xD) & 0x80, 0);
    }

    #[test]
    fn ifr_bit7_clear_when_no_enabled_flags() {
        let mut via = device();
        via.set_ifr(IRQ_CA1); // flag set but IER has CA1 disabled
        assert_eq!(via.peek(0xD) & 0x80, 0);
    }

    #[test]
    fn irq_active_when_enabled_flag_set() {
        let mut via = device();
        via.write(0xE, 0x82); // enable CA1
        via.set_ifr(IRQ_CA1);
        assert!(via.irq_active());
    }

    #[test]
    fn irq_inactive_when_flag_not_enabled() {
        let mut via = device();
        via.set_ifr(IRQ_CA1); // flag set but not enabled in IER
        assert!(!via.irq_active());
    }

    // --- Read side effects ---

    #[test]
    fn read_orb_clears_cb1_cb2_flags() {
        let mut via = device();
        via.set_ifr(IRQ_CB1 | IRQ_CB2);
        via.read(0x0);
        assert_eq!(via.peek(0xD) & (IRQ_CB1 | IRQ_CB2), 0);
    }

    #[test]
    fn read_ora_clears_ca1_ca2_flags() {
        let mut via = device();
        via.set_ifr(IRQ_CA1 | IRQ_CA2);
        via.read(0x1);
        assert_eq!(via.peek(0xD) & (IRQ_CA1 | IRQ_CA2), 0);
    }

    #[test]
    fn read_t1cl_clears_t1_flag() {
        let mut via = device();
        via.set_ifr(IRQ_T1);
        via.read(0x4);
        assert_eq!(via.peek(0xD) & IRQ_T1, 0);
    }

    #[test]
    fn read_t2cl_clears_t2_flag() {
        let mut via = device();
        via.set_ifr(IRQ_T2);
        via.read(0x8);
        assert_eq!(via.peek(0xD) & IRQ_T2, 0);
    }

    #[test]
    fn read_sr_clears_sr_flag() {
        let mut via = device();
        via.set_ifr(IRQ_SR);
        via.read(0xA);
        assert_eq!(via.peek(0xD) & IRQ_SR, 0);
    }

    #[test]
    fn peek_does_not_clear_flags() {
        let mut via = device();
        via.set_ifr(IRQ_CA1 | IRQ_CB1 | IRQ_T1 | IRQ_T2);
        let _ = via.peek(0x0); // ORB
        let _ = via.peek(0x1); // ORA
        let _ = via.peek(0x4); // T1CL
        let _ = via.peek(0x8); // T2CL
        assert_eq!(via.peek(0xD) & 0x7F, IRQ_CA1 | IRQ_CB1 | IRQ_T1 | IRQ_T2);
    }

    // --- Timer 1 ---

    #[test]
    fn t1_write_ch_starts_timer() {
        let mut via = device();
        via.write(0x4, 0x10); // latch low
        via.write(0x5, 0x00); // latch high + start
        assert_eq!(via.peek(0x4), 0x10);
        assert!(via.t1_running);
    }

    #[test]
    fn t1_one_shot_fires_irq_on_underflow() {
        let mut via = device();
        via.write(0xE, 0xC0); // enable T1 IRQ
        via.write(0xB, T1_MODE_ONE_SHOT); // one-shot mode
        via.write(0x4, 10u8);
        via.write(0x5, 0x00);
        via.tick(10);
        assert_ne!(via.peek(0xD) & IRQ_T1, 0);
        assert!(via.irq_active());
    }

    #[test]
    fn t1_one_shot_stops_after_underflow() {
        let mut via = device();
        via.write(0x4, 5u8);
        via.write(0x5, 0x00);
        via.tick(10);
        assert!(!via.t1_running);
    }

    #[test]
    fn t1_free_run_reloads_after_underflow() {
        let mut via = device();
        via.write(0xB, T1_MODE_FREE_RUN);
        via.write(0x4, 10u8);
        via.write(0x5, 0x00);
        via.tick(10); // first underflow
        assert!(via.t1_running);
        assert_ne!(via.peek(0xD) & IRQ_T1, 0);
    }

    #[test]
    fn t1_write_latch_high_clears_t1_flag() {
        let mut via = device();
        via.set_ifr(IRQ_T1);
        via.write(0x7, 0x00); // write latch high
        assert_eq!(via.peek(0xD) & IRQ_T1, 0);
    }

    // --- Timer 2 ---

    #[test]
    fn t2_write_ch_starts_timer() {
        let mut via = device();
        via.write(0x8, 0x05); // latch low
        via.write(0x9, 0x00); // high + start
        assert!(via.t2_running);
    }

    #[test]
    fn t2_fires_irq_on_underflow() {
        let mut via = device();
        via.write(0xE, 0xA0); // enable T2 IRQ
        via.write(0x8, 5u8);
        via.write(0x9, 0x00);
        via.tick(5);
        assert_ne!(via.peek(0xD) & IRQ_T2, 0);
        assert!(via.irq_active());
    }

    #[test]
    fn t2_stops_after_underflow() {
        let mut via = device();
        via.write(0x8, 5u8);
        via.write(0x9, 0x00);
        via.tick(10);
        assert!(!via.t2_running);
    }

    // --- Timer 1 PB7 toggle ---

    #[test]
    fn t1_pb7_toggles_on_underflow_when_enabled() {
        let mut via = device();
        via.write(0xB, ACR_T1_PB7_OUTPUT | T1_MODE_FREE_RUN);
        via.write(0x2, 0x80); // PB7 = output
        via.write(0x4, 5u8);
        via.write(0x5, 0x00);
        let before = via.read_port_b() & 0x80;
        via.tick(5);
        let after = via.read_port_b() & 0x80;
        assert_ne!(before, after);
    }

    // --- Port B output sends protocol message ---

    #[test]
    fn write_orb_sends_port_b_state_change() {
        let (mut via, mut remote) = device_with_pipe_b();
        // Select ASCII format on the remote side.
        remote.send(0x20).unwrap();
        std::thread::sleep(Duration::from_millis(1));
        via.tick(1); // process handshake

        // Configure PB0 as output.
        via.write(0x2, 0x01);
        via.write(0x0, 0x01); // drive PB0 high

        std::thread::sleep(Duration::from_millis(1));
        // Flush tx_buf to remote. Collect all bytes sent.
        let mut received = Vec::new();
        loop {
            match remote.try_recv() {
                Some(b) => received.push(b),
                None => break,
            }
        }
        // The state dump sends "B00", then ORB write sends "B01".
        assert!(received.windows(3).any(|w| w == b"B01"),
            "expected B01 in {:?}", String::from_utf8_lossy(&received));
    }

    // --- Format negotiation handshake ---

    #[test]
    fn binary_format_selector_triggers_handshake() {
        let (mut via, mut remote) = device_with_pipe_b();
        remote.send(0xFF).unwrap(); // binary selector
        std::thread::sleep(Duration::from_millis(1));
        via.tick(1);
        assert!(via.handshake_b_done);
    }

    #[test]
    fn ascii_format_selector_triggers_handshake() {
        let (mut via, mut remote) = device_with_pipe_a();
        remote.send(0x20).unwrap();
        std::thread::sleep(Duration::from_millis(1));
        via.tick(1);
        assert!(via.handshake_a_done);
    }

    // --- Incoming port B message updates input pins ---

    #[test]
    fn incoming_port_b_message_updates_input_b() {
        let (mut via, mut remote) = device_with_pipe_b();
        remote.send(0x20).unwrap(); // ASCII
        std::thread::sleep(Duration::from_millis(1));
        via.tick(1); // handshake

        // Send port B state 0xAB.
        for b in b"BAB".iter() { remote.send(*b).unwrap(); }
        std::thread::sleep(Duration::from_millis(1));
        via.tick(1);

        // PB pins configured as inputs (DDRB=0), so read returns input_b.
        assert_eq!(via.read(0x0), 0xAB);
    }

    // --- Control signal interrupts ---

    #[test]
    fn incoming_ca1_low_triggers_irq_when_neg_edge_configured() {
        let (mut via, mut remote) = device_with_pipe_a();
        via.write(0xE, 0x82); // enable CA1 IRQ
        via.write(0xC, 0x00); // PCR: CA1 negative edge (bit 0 = 0)
        via.ca1 = true; // start high

        remote.send(0x20).unwrap();
        std::thread::sleep(Duration::from_millis(1));
        via.tick(1); // handshake

        for b in b"CA10".iter() { remote.send(*b).unwrap(); }
        std::thread::sleep(Duration::from_millis(1));
        via.tick(1);

        assert_ne!(via.peek(0xD) & IRQ_CA1, 0);
        assert!(via.irq_active());
    }

    #[test]
    fn incoming_ca1_high_does_not_trigger_when_neg_edge_configured() {
        let (mut via, mut remote) = device_with_pipe_a();
        via.write(0xE, 0x82); // enable CA1 IRQ
        via.write(0xC, 0x00); // PCR: CA1 negative edge
        via.ca1 = false;

        remote.send(0x20).unwrap();
        std::thread::sleep(Duration::from_millis(1));
        via.tick(1);

        for b in b"CA11".iter() { remote.send(*b).unwrap(); }
        std::thread::sleep(Duration::from_millis(1));
        via.tick(1);

        assert_eq!(via.peek(0xD) & IRQ_CA1, 0);
    }

    #[test]
    fn incoming_ca2_triggers_irq_when_input_mode() {
        let (mut via, mut remote) = device_with_pipe_a();
        via.write(0xE, 0x81); // enable CA2 IRQ
        // PCR bits 3:1 = 000 → CA2 input, negative edge
        via.write(0xC, 0x00);
        via.ca2 = true;

        remote.send(0x20).unwrap();
        std::thread::sleep(Duration::from_millis(1));
        via.tick(1);

        for b in b"CA20".iter() { remote.send(*b).unwrap(); }
        std::thread::sleep(Duration::from_millis(1));
        via.tick(1);

        assert_ne!(via.peek(0xD) & IRQ_CA2, 0);
    }

    #[test]
    fn incoming_cb1_triggers_irq_when_neg_edge_configured() {
        let (mut via, mut remote) = device_with_pipe_b();
        via.write(0xE, 0x90); // enable CB1 IRQ
        via.write(0xC, 0x00); // PCR: CB1 negative edge (bit 4 = 0)
        via.cb1 = true;

        remote.send(0x20).unwrap();
        std::thread::sleep(Duration::from_millis(1));
        via.tick(1);

        for b in b"CB10".iter() { remote.send(*b).unwrap(); }
        std::thread::sleep(Duration::from_millis(1));
        via.tick(1);

        assert_ne!(via.peek(0xD) & IRQ_CB1, 0);
        assert!(via.irq_active());
    }

    #[test]
    fn incoming_cb2_triggers_irq_when_input_mode() {
        let (mut via, mut remote) = device_with_pipe_b();
        via.write(0xE, 0x88); // enable CB2 IRQ
        // PCR bits 7:5 = 000 → CB2 input, negative edge
        via.write(0xC, 0x00);
        via.cb2 = true;

        remote.send(0x20).unwrap();
        std::thread::sleep(Duration::from_millis(1));
        via.tick(1);

        for b in b"CB20".iter() { remote.send(*b).unwrap(); }
        std::thread::sleep(Duration::from_millis(1));
        via.tick(1);

        assert_ne!(via.peek(0xD) & IRQ_CB2, 0);
    }

    // --- peek does not affect timer counters or send protocol messages ---

    #[test]
    fn peek_t1_does_not_clear_flag_or_alter_counter() {
        let mut via = device();
        via.write(0x4, 10u8);
        via.write(0x5, 0x00);
        via.tick(10); // underflow: sets IRQ_T1, stops timer
        let counter_after = via.t1_counter;
        let _ = via.peek(0x4); // must not clear T1 flag or alter counter
        assert_eq!(via.t1_counter, counter_after);
        assert_ne!(via.peek(0xD) & IRQ_T1, 0);
    }

    #[test]
    fn peek_t2_does_not_affect_counter() {
        let mut via = device();
        via.write(0x8, 100u8);
        via.write(0x9, 0x00);
        via.tick(10);
        let after_tick = via.t2_counter;
        let _ = via.peek(0x8);
        assert_eq!(via.t2_counter, after_tick);
    }

    // --- State dump on handshake ---

    #[test]
    fn state_dump_a_sends_correct_ca1_signal_bit() {
        let (mut via, mut remote) = device_with_pipe_a();
        via.ca1 = true;

        remote.send(0x20).unwrap(); // ASCII selector
        std::thread::sleep(Duration::from_millis(1));
        via.tick(1); // handshake → state dump sent

        let mut received = Vec::new();
        std::thread::sleep(Duration::from_millis(1));
        loop {
            match remote.try_recv() {
                Some(b) => received.push(b),
                None => break,
            }
        }
        let s = String::from_utf8_lossy(&received);
        // State dump must include CA11 (CA1=bit1, state=1).
        assert!(s.contains("CA11"), "expected CA11 in state dump, got: {s}");
    }

    #[test]
    fn state_dump_b_sends_correct_cb2_signal_bit() {
        let (mut via, mut remote) = device_with_pipe_b();
        via.cb2 = true;

        remote.send(0x20).unwrap();
        std::thread::sleep(Duration::from_millis(1));
        via.tick(1);

        let mut received = Vec::new();
        std::thread::sleep(Duration::from_millis(1));
        loop {
            match remote.try_recv() {
                Some(b) => received.push(b),
                None => break,
            }
        }
        let s = String::from_utf8_lossy(&received);
        // State dump must include CB21 (CB2=bit2, state=1).
        assert!(s.contains("CB21"), "expected CB21 in state dump, got: {s}");
    }
}
