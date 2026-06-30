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
//! - Bits 4–2: Shift register mode (000=off, 001=in/T2, 010=in/PHI2, 011=in/ext,
//!   100=out free/T2, 101=out/T2, 110=out/PHI2, 111=out/ext)
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
//!
//! # Virtual peripheral connections
//!
//! Virtual peripherals connect to the VIA over byte-stream transports using the
//! [`crate::emulator::device::via_protocol`] message protocol. Any number of transports may
//! be attached with [`Via6522::attach_transport`]; each undergoes an independent format
//! negotiation handshake.
//!
//! **Handshake.** The peripheral opens the connection by sending a single format-selector byte.
//! On the next [`IoDevice::tick`] call that receives it, the VIA completes the handshake and
//! immediately sends a six-message state dump (port A value, port B value, CA1, CA2, CB1,
//! CB2), giving the peripheral the current GPIO state before any further exchange.
//!
//! **VIA → peripheral (outgoing).** After the handshake the VIA sends a
//! [`ViaProtocolMessage::PortStateChange`] or [`ViaProtocolMessage::ControlSignalChange`]
//! message whenever the observable GPIO state changes:
//! - Writes to ORB/ORA or the DDR registers that alter output-pin state.
//! - Timer 1 PB7 toggles (when `ACR_T1_PB7_OUTPUT` is set and messages are not suppressed).
//!
//! Every attached transport that has completed its handshake receives the message.
//!
//! **Peripheral → VIA (incoming).** The peripheral drives the VIA's input pins by sending
//! `PortStateChange` and `ControlSignalChange` messages at any time after the handshake.
//! Incoming messages update the VIA's latched input state (`input_a`/`input_b`) and the
//! control-signal lines (CA1, CA2, CB1, CB2). If the resulting edge matches the PCR
//! configuration, the corresponding IFR bit is set and an IRQ may be asserted.

use crate::emulator::device::{DeviceId, ErrorSender, IoDevice};
use crate::emulator::device::via_protocol::{
    ViaProtocolDecoder, ViaProtocolEncoder, ViaProtocolFormat, ViaProtocolMessage,
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
const ACR_SR_MODE_MASK:     u8 = 0x1C;
const SR_MODE_DISABLED:     u8 = 0x00; // ACR bits 4–2 = 000
const SR_MODE_IN_T2:        u8 = 0x04; // 001: shift in under T2
const SR_MODE_IN_PHI2:      u8 = 0x08; // 010: shift in under PHI2
const SR_MODE_IN_EXT:       u8 = 0x0C; // 011: shift in under CB1 (external)
const SR_MODE_OUT_FREE_T2:  u8 = 0x10; // 100: shift out free-running at T2 rate
const SR_MODE_OUT_T2:       u8 = 0x14; // 101: shift out under T2
const SR_MODE_OUT_PHI2:     u8 = 0x18; // 110: shift out under PHI2
const SR_MODE_OUT_EXT:      u8 = 0x1C; // 111: shift out under CB1 (external)
const ACR_T1_PB7_OUTPUT:    u8 = 0x80; // Timer 1 square wave output mode
const ACR_T2_PB6_COUNT:     u8 = 0x20; // Timer 2 pulse count mode

// --- PCR masks ---
const PCR_CA1_EDGE:  u8 = 0x01;
const PCR_CA2_MASK:  u8 = 0x0E;
const PCR_CB1_EDGE:  u8 = 0x10;
const PCR_CB2_MASK:  u8 = 0xE0;

// --- Timer 1 modes ---
#[allow(dead_code)]
const T1_MODE_ONE_SHOT:  u8 = 0x00; // reserved for explicit mode comparison
const T1_MODE_FREE_RUN:  u8 = 0x40;

/// One active transport connection with its associated protocol state.
struct TransportSlot {
    /// The underlying byte-stream transport.
    transport: Box<dyn Transport>,
    /// Encoder for outgoing protocol messages (format selected after handshake).
    encoder: ViaProtocolEncoder,
    /// Decoder for incoming protocol messages.
    decoder: ViaProtocolDecoder,
    /// True once the format-negotiation handshake has completed.
    handshake_done: bool,
    /// Last `Transport::connection_id()` value seen; used to detect reconnection.
    last_connection_id: u64,
}

impl TransportSlot {
    fn new(transport: Box<dyn Transport>) -> Self {
        let last_connection_id = transport.connection_id();
        Self {
            transport,
            encoder: ViaProtocolEncoder::new(),
            decoder: ViaProtocolDecoder::new(),
            handshake_done: false,
            last_connection_id,
        }
    }

    /// Resets handshake and codec state for a new client session.
    fn reset(&mut self) {
        self.encoder = ViaProtocolEncoder::new();
        self.decoder = ViaProtocolDecoder::new();
        self.handshake_done = false;
    }
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
    /// Timer 2 latch high byte; needed to reload T2 for free-running SR mode.
    t2_latch_hi: u8,
    /// True when timer 2 is actively counting.
    t2_running: bool,

    // --- Shift register ---
    /// Shift register data byte.
    sr: u8,
    /// Number of bits shifted so far in the current operation (0–7; resets after 8th bit).
    sr_count: u8,
    /// True when an SR shift operation is in progress.
    sr_running: bool,
    /// True when shifting out (toward peripheral); false when shifting in.
    sr_shifting_out: bool,
    /// SR-driven CB1 clock-line state (internal; separate from the `cb1` input latch).
    sr_cb1: bool,

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

    // --- Transport connections ---
    /// All active transport connections; each peripheral sees all port and signal state changes.
    transports: Vec<TransportSlot>,

    // --- Async error reporting ---
    /// Destination for async transport error events.
    error_sender: Option<ErrorSender>,
    /// Identity used in error events.
    device_id: Option<DeviceId>,

    /// Suppress Timer 1 PB7 protocol messages even when ACR enables PB7 output.
    suppress_t1_pb7_messages: bool,
}

impl Via6522 {
    /// Creates a new `Via6522` in reset state.
    pub fn new() -> Self {
        Self {
            orb: 0, ora: 0, ddrb: 0, ddra: 0,
            input_b: 0, input_a: 0,
            t1_counter: 0, t1_latch: 0, t1_running: false, t1_pb7: false,
            t2_counter: 0, t2_latch_lo: 0, t2_latch_hi: 0, t2_running: false,
            sr: 0, sr_count: 0, sr_running: false, sr_shifting_out: false, sr_cb1: false,
            acr: 0, pcr: 0, ifr: 0, ier: 0,
            ca1: false, ca2: false, cb1: false, cb2: false,
            transports: Vec::new(),
            error_sender: None,
            device_id: None,
            suppress_t1_pb7_messages: false,
        }
    }

    /// Attaches a transport. All attached transports receive every port and control-signal
    /// state change; any number of peripherals may be connected simultaneously.
    pub fn attach_transport(&mut self, transport: Box<dyn Transport>) {
        self.transports.push(TransportSlot::new(transport));
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
        // Fix (issue #99): square wave output mode takes priority over input/output mode
        // selected by DDRB bit 7.
        let mut orb = (self.orb & self.ddrb) | (self.input_b & !self.ddrb);
        if self.acr & ACR_T1_PB7_OUTPUT != 0 {
            orb = (orb & 0x7f) | if self.t1_pb7 { 0x80 } else { 0 };
        }
        orb
    }

    /// Reads the effective state of port A: output pins from ORA, input pins from input_a.
    fn read_port_a(&self) -> u8 {
        (self.ora & self.ddra) | (self.input_a & !self.ddra)
    }

    // --- Protocol transmission helpers ---

    /// Encodes `message` and sends it to all transport connections that have completed
    /// the format-negotiation handshake.
    fn send_to_all(&mut self, message: ViaProtocolMessage) {
        for i in 0..self.transports.len() {
            if !self.transports[i].handshake_done { continue; }
            let mut bytes = Vec::new();
            self.transports[i].encoder.encode(message, &mut bytes);
            for b in bytes {
                if let Err(e) = self.transports[i].transport.send(b) {
                    self.report_error(e);
                    break;
                }
            }
        }
    }

    /// Sends the full port and control-signal state dump to transport `idx` after
    /// format negotiation, so the newly-connected peripheral can synchronise.
    fn send_state_dump(&mut self, idx: usize) {
        let port_a = self.read_port_a();
        let port_b = self.read_port_b();
        let (ca1, ca2, cb1, cb2) = (self.ca1, self.ca2, self.cb1, self.cb2);
        // Control signal bit layout: CA1=bit1, CA2=bit0, CB1=bit3, CB2=bit2.
        let msgs = [
            ViaProtocolMessage::PortStateChange { port: 'A', value: port_a },
            ViaProtocolMessage::PortStateChange { port: 'B', value: port_b },
            ViaProtocolMessage::ControlSignalChange { signals: 0x02, state: ca1 },
            ViaProtocolMessage::ControlSignalChange { signals: 0x01, state: ca2 },
            ViaProtocolMessage::ControlSignalChange { signals: 0x08, state: cb1 },
            ViaProtocolMessage::ControlSignalChange { signals: 0x04, state: cb2 },
        ];
        for msg in msgs {
            let mut bytes = Vec::new();
            self.transports[idx].encoder.encode(msg, &mut bytes);
            for b in bytes {
                if let Err(e) = self.transports[idx].transport.send(b) {
                    self.report_error(e);
                    return;
                }
            }
        }
    }

    // --- Handshake / incoming byte processing ---

    /// Polls all transport connections for incoming bytes and processes decoded messages.
    fn poll_transports(&mut self) {
        for i in 0..self.transports.len() {
            // Detect reconnection: a new client session began since the last poll.
            let current_id = self.transports[i].transport.connection_id();
            if current_id != self.transports[i].last_connection_id {
                self.transports[i].last_connection_id = current_id;
                self.transports[i].reset();
            }

            while let Some(byte) = self.transports[i].transport.try_recv() {
                let msg = self.transports[i].decoder.feed(byte);

                // First qualifying byte locks the format → complete the handshake.
                if !self.transports[i].handshake_done
                    && self.transports[i].decoder.format().is_some()
                {
                    self.transports[i].handshake_done = true;
                    if self.transports[i].decoder.format() == Some(ViaProtocolFormat::Binary) {
                        self.transports[i].encoder.select_binary();
                    }
                    self.send_state_dump(i);
                }

                if let Some(m) = msg {
                    self.apply_message(m);
                }
            }
        }
    }

    /// Applies an incoming protocol message, updating port inputs and triggering interrupts.
    fn apply_message(&mut self, msg: ViaProtocolMessage) {
        match msg {
            ViaProtocolMessage::PortStateChange { port: 'A', value } => {
                let old = self.input_a;
                self.input_a = value;
                if old != value {
                    // CA1 latches on configured edge.
                    let pos_edge = self.pcr & PCR_CA1_EDGE != 0;
                    let triggered = if pos_edge {
                        (old & !value) != 0 || (!old & value) != 0
                    } else {
                        true
                    };
                    if triggered { self.set_ifr(IRQ_CA1); }
                }
            }
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
                // T2 pulse-counting mode: count negative PB6 transitions.
                if self.acr & ACR_T2_PB6_COUNT != 0 && self.t2_running {
                    let old_pb6 = (old >> 6) & 1 != 0;
                    let new_pb6 = (value >> 6) & 1 != 0;
                    if old_pb6 && !new_pb6 {
                        let (new_counter, wrapped) = self.t2_counter.overflowing_sub(1);
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
            ViaProtocolMessage::ControlSignalChange { signals, state } => {
                if signals & 0x02 != 0 { // CA1
                    let pos_edge = self.pcr & PCR_CA1_EDGE != 0;
                    if self.ca1 != state && state == pos_edge { self.set_ifr(IRQ_CA1); }
                    self.ca1 = state;
                }
                if signals & 0x01 != 0 { // CA2 (when configured as input)
                    let ca2_mode = (self.pcr & PCR_CA2_MASK) >> 1;
                    if ca2_mode < 4 { // input modes
                        let pos_edge = ca2_mode & 0x02 != 0;
                        if self.ca2 != state && state == pos_edge { self.set_ifr(IRQ_CA2); }
                        self.ca2 = state;
                    }
                }
                if signals & 0x08 != 0 { // CB1
                    let pos_edge = self.pcr & PCR_CB1_EDGE != 0;
                    if self.cb1 != state {
                        if state == pos_edge { self.set_ifr(IRQ_CB1); }
                        self.cb1 = state;
                        if state && matches!(self.sr_mode(), SR_MODE_IN_EXT | SR_MODE_OUT_EXT) {
                            self.sr_clock();
                        }
                    }
                }
                if signals & 0x04 != 0 { // CB2 (when configured as input)
                    let cb2_mode = (self.pcr & PCR_CB2_MASK) >> 5;
                    if cb2_mode < 4 { // input modes
                        let pos_edge = cb2_mode & 0x02 != 0;
                        if self.cb2 != state && state == pos_edge { self.set_ifr(IRQ_CB2); }
                        self.cb2 = state;
                    }
                }
            }
            _ => {}
        }
    }

    fn sr_mode(&self) -> u8 {
        self.acr & ACR_SR_MODE_MASK
    }

    /// Clocks one bit through the shift register: pulses CB1 low then high, shifts
    /// a bit out on CB2 (or samples CB2 for shift-in), and sets `IRQ_SR` after 8 bits.
    fn sr_clock(&mut self) {
        if !self.sr_running { return; }

        let shifting_out = self.sr_shifting_out;
        let bit_index = 7 - self.sr_count; // MSB first

        // CB1 falling edge (clock low).
        self.sr_cb1 = false;
        self.send_to_all(ViaProtocolMessage::ControlSignalChange { signals: 0x08, state: false });

        if shifting_out {
            let bit = (self.sr >> bit_index) & 1 != 0;
            self.cb2 = bit;
            self.send_to_all(ViaProtocolMessage::ControlSignalChange { signals: 0x04, state: bit });
        }

        // CB1 rising edge (clock high; data sampled here for shift-in).
        self.sr_cb1 = true;
        self.send_to_all(ViaProtocolMessage::ControlSignalChange { signals: 0x08, state: true });

        if !shifting_out {
            let bit = self.cb2;
            let mask = 1u8 << bit_index;
            if bit { self.sr |= mask; } else { self.sr &= !mask; }
        }

        self.sr_count += 1;

        if self.sr_count >= 8 {
            self.sr_count = 0;
            self.set_ifr(IRQ_SR);
            let self_terminating = !matches!(self.sr_mode(), SR_MODE_OUT_FREE_T2);
            if self_terminating {
                self.sr_running = false;
            }
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
                        self.send_to_all(ViaProtocolMessage::PortStateChange { port: 'B', value: pb });
                    }
                }
                if self.acr & T1_MODE_FREE_RUN != 0 {
                    // Reload from latch; account for any cycles past zero.
                    self.t1_counter = self.t1_latch;
                } else {
                    self.t1_running = false;
                    self.t1_counter = 0xFFFF;
                }
            } else {
                self.t1_counter = new_counter;
            }
        }

        // PHI2 SR modes: clock the shift register once per tick.
        if self.sr_running && matches!(self.sr_mode(), SR_MODE_IN_PHI2 | SR_MODE_OUT_PHI2) {
            self.sr_clock();
        }

        // Timer 2 (timed mode only; PB6 pulse-counting handled in apply_message).
        if self.t2_running && self.acr & ACR_T2_PB6_COUNT == 0 {
            let (new_counter, wrapped) = self.t2_counter.overflowing_sub(cycles as u16);
            if wrapped || new_counter == 0 {
                if matches!(self.sr_mode(), SR_MODE_IN_T2 | SR_MODE_OUT_T2 | SR_MODE_OUT_FREE_T2) {
                    self.sr_clock();
                    // T2 reloads for the next SR clock unless the SR just finished.
                    // For OUT_FREE_T2 (free-running), sr_running stays true indefinitely.
                    if self.sr_running {
                        self.t2_counter = ((self.t2_latch_hi as u16) << 8) | self.t2_latch_lo as u16;
                        return; // keep T2 running; no IRQ_T2
                    }
                    // SR complete (IN_T2 / OUT_T2 self-terminating): fall through to stop T2.
                }
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
                    self.send_to_all(ViaProtocolMessage::PortStateChange { port: 'B', value: new_b });
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
                    self.send_to_all(ViaProtocolMessage::PortStateChange { port: 'A', value: new_a });
                }
            }
            0x2 => {
                let old_ddrb = self.ddrb;
                self.ddrb = value;
                // DDR change may change the effective output; send update.
                let old_b = (self.orb & old_ddrb) | (self.input_b & !old_ddrb);
                let new_b = self.read_port_b();
                if old_b != new_b {
                    self.send_to_all(ViaProtocolMessage::PortStateChange { port: 'B', value: new_b });
                }
            }
            0x3 => {
                let old_ddra = self.ddra;
                self.ddra = value;
                let old_a = (self.ora & old_ddra) | (self.input_a & !old_ddra);
                let new_a = self.read_port_a();
                if old_a != new_a {
                    self.send_to_all(ViaProtocolMessage::PortStateChange { port: 'A', value: new_a });
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
                // Drive PB7 low when timer starts (per datasheet).
                if self.acr & ACR_T1_PB7_OUTPUT != 0 {
                    // Get current PB7 state without considering DDRB
                    let prev_pb7 = self.orb & 0x80 != 0;
                    // Drive PB7 low
                    let prev_t1_pb7 = self.t1_pb7;
                    self.t1_pb7 = false;
                    let new_pb = self.read_port_b();
                    // Send a message only if PB7 was previously high or Timer 1 was holding PB7 high
                    if prev_pb7 || prev_t1_pb7  {
                        self.send_to_all(ViaProtocolMessage::PortStateChange { port: 'B', value: new_pb });
                    }
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
                // Write T2 counter high — loads counter and latch, starts timer.
                self.t2_latch_hi = value;
                self.t2_counter = ((value as u16) << 8) | self.t2_latch_lo as u16;
                self.t2_running = true;
                self.clear_ifr(IRQ_T2);
            }
            0xA => {
                self.sr = value;
                self.clear_ifr(IRQ_SR);
                let mode = self.sr_mode();
                if mode != SR_MODE_DISABLED {
                    self.sr_count = 0;
                    self.sr_running = true;
                    self.sr_shifting_out = mode >= SR_MODE_OUT_FREE_T2;
                }
            }
            0xB => {
                let prev_acr = self.acr;
                self.acr = value;
                // If PB7 output mode is disabled while Timer 1 is holding PB7 high, and if
                // PB7 is configured as an output and is being driven low, we must signal
                // PB7's transition to the low state.
                let pb7_output_disabled = (prev_acr & ACR_T1_PB7_OUTPUT) != 0 && (self.acr & ACR_T1_PB7_OUTPUT) == 0;
                let pb7_output_low = (self.ddrb & 0x80) != 0 && (self.orb & 0x80) == 0;
                if pb7_output_disabled && self.t1_pb7 && pb7_output_low {
                    self.send_to_all(ViaProtocolMessage::PortStateChange { port: 'B', value: self.orb });
                }
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
                    self.send_to_all(ViaProtocolMessage::PortStateChange { port: 'A', value: new_a });
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
        self.poll_transports();
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

    fn device_with_pipe() -> (Via6522, PipeTransport) {
        let (local, remote) = PipeTransport::pair().unwrap();
        let mut via = Via6522::new();
        via.attach_transport(Box::new(local));
        (via, remote)
    }

    fn collect_bytes(remote: &mut PipeTransport) -> Vec<u8> {
        let mut buf = Vec::new();
        loop {
            match remote.try_recv() {
                Some(b) => buf.push(b),
                None => break,
            }
        }
        buf
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

    #[test]
    fn t1_one_shot_with_pb7_output_sends_pb7_low_at_start_when_needed() {
        let (mut via, mut remote) = device_with_pipe();
        remote.send(0x20).unwrap();
        std::thread::sleep(Duration::from_millis(1));
        via.tick(1); // process handshake

        via.orb = 0x80;     // set PB7 high
        via.write(0xB, T1_MODE_ONE_SHOT | ACR_T1_PB7_OUTPUT);
        via.write(0x4, 10);
        via.write(0x5, 0);
        via.tick(5);

        let received = collect_bytes(&mut remote);
        assert!(received.windows(3).any(|w| w == b"B00"),
                "expected B00 in {:?}", String::from_utf8_lossy(&received));

        via.tick(5);

        let received = collect_bytes(&mut remote);
        assert!(received.windows(3).any(|w| w == b"B80"),
                "expected B80 in {:?}", String::from_utf8_lossy(&received));

        via.orb = 0x0;          // set PB7 low
        via.t1_pb7 = false;     // Timer 1 PB7 is driving PB7 low
        via.write(0xB, T1_MODE_ONE_SHOT | ACR_T1_PB7_OUTPUT);
        via.write(0x4, 10);
        via.write(0x5, 0);
        via.tick(10);

        let received = collect_bytes(&mut remote);
        assert!(!received.windows(3).any(|w| w == b"B00"),
                "didn't expect B00 in {:?}", String::from_utf8_lossy(&received));
        assert!(received.windows(3).any(|w| w == b"B80"),
                "expected B80 in {:?}", String::from_utf8_lossy(&received));
    }

    #[test]
    fn t1_pb7_output_mode_sends_pb7_low_if_needed_when_pb7_output_mode_disabled() {
        let (mut via, mut remote) = device_with_pipe();
        remote.send(0x20).unwrap();
        std::thread::sleep(Duration::from_millis(1));
        via.tick(1); // process handshake

        via.ddrb = 0x80;    // PB7 is an output
        via.orb = 0x00;     // set PB7 low
        via.write(0xB, T1_MODE_ONE_SHOT | ACR_T1_PB7_OUTPUT);
        via.write(0x4, 5);
        via.write(0x5, 0);
        via.tick(5);

        let received = collect_bytes(&mut remote);
        assert!(received.windows(3).any(|w| w == b"B80"),
                "expected B80 in {:?}", String::from_utf8_lossy(&received));

        via.write(0xB, 0);      // disable PB7 output mode

        let received = collect_bytes(&mut remote);
        assert!(received.windows(3).any(|w| w == b"B00"),
                "expected B00 in {:?}", String::from_utf8_lossy(&received));

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
        via.write(0x4, 5);
        via.write(0x5, 0);
        let before = via.read_port_b() & 0x80;
        via.tick(5);
        let after = via.read_port_b() & 0x80;
        assert_ne!(before, after);
    }

    #[test]
    fn t1_pb7_overrides_orb7() {
        let (mut via, mut remote) = device_with_pipe();
        remote.send(0x20).unwrap();
        std::thread::sleep(Duration::from_millis(1));
        via.tick(1); // process handshake

        let received = collect_bytes(&mut remote);
        // The state dump sends initial state; ORB write sends "B00".
        assert!(received.windows(3).any(|w| w == b"B00"),
                "expected B00 in {:?}", String::from_utf8_lossy(&received));

        via.write(0xB, ACR_T1_PB7_OUTPUT | T1_MODE_FREE_RUN);
        via.write(0x4, 5);
        via.write(0x5, 0);

        via.tick(5);
        let received = collect_bytes(&mut remote);
        // When the timer reaches zero, port B output should be emitted with PB7 = 1
        assert!(received.windows(3).any(|w| w == b"B80"),
                "expected B80 in {:?}", String::from_utf8_lossy(&received));

        via.tick(5);
        let received = collect_bytes(&mut remote);
        // When the timer reaches zero again, port B output should be emitted with PB7 = 0
        assert!(received.windows(3).any(|w| w == b"B00"),
                "expected B80 in {:?}", String::from_utf8_lossy(&received));

    }

    // --- Port output sends protocol message ---

    #[test]
    fn write_orb_sends_port_b_state_change() {
        let (mut via, mut remote) = device_with_pipe();
        remote.send(0x20).unwrap();
        std::thread::sleep(Duration::from_millis(1));
        via.tick(1); // process handshake

        // Configure PB0 as output.
        via.write(0x2, 0x01);
        via.write(0x0, 0x01); // drive PB0 high

        std::thread::sleep(Duration::from_millis(1));
        let received = collect_bytes(&mut remote);
        // The state dump sends initial state; ORB write sends "B01".
        assert!(received.windows(3).any(|w| w == b"B01"),
            "expected B01 in {:?}", String::from_utf8_lossy(&received));
    }

    // --- Format negotiation handshake ---

    #[test]
    fn binary_format_selector_triggers_handshake() {
        let (mut via, mut remote) = device_with_pipe();
        remote.send(0xFF).unwrap(); // binary selector
        std::thread::sleep(Duration::from_millis(1));
        via.tick(1);
        assert!(via.transports[0].handshake_done);
    }

    #[test]
    fn ascii_format_selector_triggers_handshake() {
        let (mut via, mut remote) = device_with_pipe();
        remote.send(0x20).unwrap();
        std::thread::sleep(Duration::from_millis(1));
        via.tick(1);
        assert!(via.transports[0].handshake_done);
    }

    // --- Incoming port message updates input pins ---

    #[test]
    fn incoming_port_b_message_updates_input_b() {
        let (mut via, mut remote) = device_with_pipe();
        remote.send(0x20).unwrap(); // ASCII
        std::thread::sleep(Duration::from_millis(1));
        via.tick(1); // handshake

        for b in b"BAB".iter() { remote.send(*b).unwrap(); }
        std::thread::sleep(Duration::from_millis(1));
        via.tick(1);

        // PB pins configured as inputs (DDRB=0), so read returns input_b.
        assert_eq!(via.read(0x0), 0xAB);
    }

    #[test]
    fn incoming_port_a_message_updates_input_a() {
        let (mut via, mut remote) = device_with_pipe();
        remote.send(0x20).unwrap(); // ASCII
        std::thread::sleep(Duration::from_millis(1));
        via.tick(1); // handshake

        for b in b"A55".iter() { remote.send(*b).unwrap(); }
        std::thread::sleep(Duration::from_millis(1));
        via.tick(1);

        // PA pins configured as inputs (DDRA=0), so read returns input_a.
        assert_eq!(via.read(0x1), 0x55);
    }

    // --- Control signal interrupts ---

    #[test]
    fn incoming_ca1_low_triggers_irq_when_neg_edge_configured() {
        let (mut via, mut remote) = device_with_pipe();
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
        let (mut via, mut remote) = device_with_pipe();
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
        let (mut via, mut remote) = device_with_pipe();
        via.write(0xE, 0x81); // enable CA2 IRQ
        via.write(0xC, 0x00); // PCR bits 3:1 = 000 → CA2 input, negative edge
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
        let (mut via, mut remote) = device_with_pipe();
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
        let (mut via, mut remote) = device_with_pipe();
        via.write(0xE, 0x88); // enable CB2 IRQ
        via.write(0xC, 0x00); // PCR bits 7:5 = 000 → CB2 input, negative edge
        via.cb2 = true;

        remote.send(0x20).unwrap();
        std::thread::sleep(Duration::from_millis(1));
        via.tick(1);

        for b in b"CB20".iter() { remote.send(*b).unwrap(); }
        std::thread::sleep(Duration::from_millis(1));
        via.tick(1);

        assert_ne!(via.peek(0xD) & IRQ_CB2, 0);
    }

    // --- peek does not affect timer counters or flags ---

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
    fn state_dump_sends_all_six_messages() {
        let (mut via, mut remote) = device_with_pipe();
        via.ca1 = true;
        via.cb2 = true;

        remote.send(0x20).unwrap(); // ASCII selector
        std::thread::sleep(Duration::from_millis(1));
        via.tick(1); // handshake → state dump sent

        std::thread::sleep(Duration::from_millis(1));
        let received = collect_bytes(&mut remote);
        let s = String::from_utf8_lossy(&received);

        assert!(s.contains("A00"), "expected A00 in state dump, got: {s}");
        assert!(s.contains("B00"), "expected B00 in state dump, got: {s}");
        assert!(s.contains("CA11"), "expected CA11 in state dump, got: {s}");
        assert!(s.contains("CA20"), "expected CA20 in state dump, got: {s}");
        assert!(s.contains("CB10"), "expected CB10 in state dump, got: {s}");
        assert!(s.contains("CB21"), "expected CB21 in state dump, got: {s}");
    }

    // --- Multiple transports ---

    #[test]
    fn multiple_transports_both_receive_state_dump() {
        let (local1, mut remote1) = PipeTransport::pair().unwrap();
        let (local2, mut remote2) = PipeTransport::pair().unwrap();
        let mut via = Via6522::new();
        via.attach_transport(Box::new(local1));
        via.attach_transport(Box::new(local2));

        remote1.send(0x20).unwrap();
        remote2.send(0x20).unwrap();
        std::thread::sleep(Duration::from_millis(1));
        via.tick(1);

        assert!(via.transports[0].handshake_done);
        assert!(via.transports[1].handshake_done);

        std::thread::sleep(Duration::from_millis(1));
        let r1 = collect_bytes(&mut remote1);
        let r2 = collect_bytes(&mut remote2);

        assert!(r1.windows(3).any(|w| w == b"A00"),
            "transport 1 missing state dump: {:?}", String::from_utf8_lossy(&r1));
        assert!(r2.windows(3).any(|w| w == b"A00"),
            "transport 2 missing state dump: {:?}", String::from_utf8_lossy(&r2));
    }

    // --- Interrupts for CA1, CA2, CB1, CB2  ---

    const PCR_CB1_INPUT_NEGATIVE_EDGE: u8 = 0;
    const PCR_CB1_INPUT_POSITIVE_EDGE: u8 = PCR_CB1_EDGE;
    const PCR_CA1_INPUT_NEGATIVE_EDGE: u8 = 0;
    const PCR_CA1_INPUT_POSITIVE_EDGE: u8 = PCR_CA1_EDGE;

    const PCR_CB2_INDEPENDENT_INTERRUPT_INPUT_NEGATIVE_EDGE: u8 = 0b00100000;
    const PCR_CB2_INDEPENDENT_INTERRUPT_INPUT_POSITIVE_EDGE: u8 = 0b01100000;
    const PCR_CA2_INDEPENDENT_INTERRUPT_INPUT_NEGATIVE_EDGE: u8 = 0b00000010;
    const PCR_CA2_INDEPENDENT_INTERRUPT_INPUT_POSITIVE_EDGE: u8 = 0b00000110;

    #[test]
    fn ca1_negative_edge_triggers_irq_when_level_high() {
        let (mut via, mut remote) = device_with_pipe();
        handshake(&mut via, &mut remote);
        via.ca1 = true;
        via.write(0xc, PCR_CA1_INPUT_NEGATIVE_EDGE);
        via.apply_message(ViaProtocolMessage::ControlSignalChange { signals: 0x02, state: false });
        via.tick(2);
        let int_flags = via.read(0xd);
        assert_ne!(int_flags & IRQ_CA1, 0);
    }

    #[test]
    fn ca1_negative_edge_does_not_trigger_irq_when_level_low() {
        let (mut via, mut remote) = device_with_pipe();
        handshake(&mut via, &mut remote);
        via.ca1 = false;
        via.write(0xc, PCR_CA1_INPUT_NEGATIVE_EDGE);
        via.apply_message(ViaProtocolMessage::ControlSignalChange { signals: 0x02, state: false });
        via.tick(2);
        let int_flags = via.read(0xd);
        assert_eq!(int_flags & IRQ_CA1, 0);
    }

    #[test]
    fn ca1_positive_edge_triggers_irq_when_level_low() {
        let (mut via, mut remote) = device_with_pipe();
        handshake(&mut via, &mut remote);
        via.ca1 = false;
        via.write(0xc, PCR_CA1_INPUT_POSITIVE_EDGE);
        via.apply_message(ViaProtocolMessage::ControlSignalChange { signals: 0x02, state: true });
        via.tick(2);
        let int_flags = via.read(0xd);
        assert_ne!(int_flags & IRQ_CA1, 0);
    }

    #[test]
    fn ca1_positive_edge_does_not_trigger_irq_when_level_high() {
        let (mut via, mut remote) = device_with_pipe();
        handshake(&mut via, &mut remote);
        via.ca1 = true;
        via.write(0xc, PCR_CA1_INPUT_POSITIVE_EDGE);
        via.apply_message(ViaProtocolMessage::ControlSignalChange { signals: 0x02, state: true });
        via.tick(2);
        let int_flags = via.read(0xd);
        assert_eq!(int_flags & IRQ_CA1, 0);
    }

    #[test]
    fn cb1_negative_edge_triggers_irq_when_level_high() {
        let (mut via, mut remote) = device_with_pipe();
        handshake(&mut via, &mut remote);
        via.cb1 = true;
        via.write(0xc, PCR_CB1_INPUT_NEGATIVE_EDGE);
        via.apply_message(ViaProtocolMessage::ControlSignalChange { signals: 0x08, state: false });
        via.tick(2);
        let int_flags = via.read(0xd);
        assert_ne!(int_flags & IRQ_CB1, 0);
    }

    #[test]
    fn cb1_negative_edge_does_not_trigger_irq_when_level_low() {
        let (mut via, mut remote) = device_with_pipe();
        handshake(&mut via, &mut remote);
        via.cb1 = false;
        via.write(0xc, PCR_CB1_INPUT_NEGATIVE_EDGE);
        via.apply_message(ViaProtocolMessage::ControlSignalChange { signals: 0x08, state: false });
        via.tick(2);
        let int_flags = via.read(0xd);
        assert_eq!(int_flags & IRQ_CB1, 0);
    }

    #[test]
    fn cb1_positive_edge_triggers_irq_when_level_low() {
        let (mut via, mut remote) = device_with_pipe();
        handshake(&mut via, &mut remote);
        via.cb1 = false;
        via.write(0xc, PCR_CB1_INPUT_POSITIVE_EDGE);
        via.apply_message(ViaProtocolMessage::ControlSignalChange { signals: 0x08, state: true });
        via.tick(2);
        let int_flags = via.read(0xd);
        assert_ne!(int_flags & IRQ_CB1, 0);
    }

    #[test]
    fn cb1_positive_edge_does_not_trigger_irq_when_level_high() {
        let (mut via, mut remote) = device_with_pipe();
        handshake(&mut via, &mut remote);
        via.cb1 = true;
        via.write(0xc, PCR_CB1_INPUT_POSITIVE_EDGE);
        via.apply_message(ViaProtocolMessage::ControlSignalChange { signals: 0x08, state: true });
        via.tick(2);
        let int_flags = via.read(0xd);
        assert_eq!(int_flags & IRQ_CB1, 0);
    }

    #[test]
    fn ca2_negative_edge_triggers_irq_when_level_high() {
        let (mut via, mut remote) = device_with_pipe();
        handshake(&mut via, &mut remote);
        via.ca2 = true;
        via.write(0xc, PCR_CA2_INDEPENDENT_INTERRUPT_INPUT_NEGATIVE_EDGE);
        via.apply_message(ViaProtocolMessage::ControlSignalChange { signals: 0x01, state: false });
        via.tick(2);
        let int_flags = via.read(0xd);
        assert_ne!(int_flags & IRQ_CA2, 0);
    }

    #[test]
    fn ca2_negative_edge_does_not_trigger_irq_when_level_low() {
        let (mut via, mut remote) = device_with_pipe();
        handshake(&mut via, &mut remote);
        via.ca2 = false;
        via.write(0xc, PCR_CA2_INDEPENDENT_INTERRUPT_INPUT_NEGATIVE_EDGE);
        via.apply_message(ViaProtocolMessage::ControlSignalChange { signals: 0x01, state: false });
        via.tick(2);
        let int_flags = via.read(0xd);
        assert_eq!(int_flags & IRQ_CA2, 0);
    }

    #[test]
    fn ca2_positive_edge_triggers_irq_when_level_low() {
        let (mut via, mut remote) = device_with_pipe();
        handshake(&mut via, &mut remote);
        via.ca2 = false;
        via.write(0xc, PCR_CA2_INDEPENDENT_INTERRUPT_INPUT_POSITIVE_EDGE);
        via.apply_message(ViaProtocolMessage::ControlSignalChange { signals: 0x01, state: true });
        via.tick(2);
        let int_flags = via.read(0xd);
        assert_ne!(int_flags & IRQ_CA2, 0);
    }

    #[test]
    fn ca2_positive_edge_does_not_trigger_irq_when_level_high() {
        let (mut via, mut remote) = device_with_pipe();
        handshake(&mut via, &mut remote);
        via.ca2 = true;
        via.write(0xc, PCR_CA2_INDEPENDENT_INTERRUPT_INPUT_POSITIVE_EDGE);
        via.apply_message(ViaProtocolMessage::ControlSignalChange { signals: 0x01, state: true });
        via.tick(2);
        let int_flags = via.read(0xd);
        assert_eq!(int_flags & IRQ_CA2, 0);
    }

    #[test]
    fn cb2_negative_edge_triggers_irq_when_level_high() {
        let (mut via, mut remote) = device_with_pipe();
        handshake(&mut via, &mut remote);
        via.cb2 = true;
        via.write(0xc, PCR_CB2_INDEPENDENT_INTERRUPT_INPUT_NEGATIVE_EDGE);
        via.apply_message(ViaProtocolMessage::ControlSignalChange { signals: 0x04, state: false });
        via.tick(2);
        let int_flags = via.read(0xd);
        assert_ne!(int_flags & IRQ_CB2, 0);
    }

    #[test]
    fn cb2_negative_edge_does_not_trigger_irq_when_level_low() {
        let (mut via, mut remote) = device_with_pipe();
        handshake(&mut via, &mut remote);
        via.cb2 = false;
        via.write(0xc, PCR_CB2_INDEPENDENT_INTERRUPT_INPUT_NEGATIVE_EDGE);
        via.apply_message(ViaProtocolMessage::ControlSignalChange { signals: 0x04, state: false });
        via.tick(2);
        let int_flags = via.read(0xd);
        assert_eq!(int_flags & IRQ_CB2, 0);
    }

    #[test]
    fn cb2_positive_edge_triggers_irq_when_level_low() {
        let (mut via, mut remote) = device_with_pipe();
        handshake(&mut via, &mut remote);
        via.cb2 = false;
        via.write(0xc, PCR_CB2_INDEPENDENT_INTERRUPT_INPUT_POSITIVE_EDGE);
        via.apply_message(ViaProtocolMessage::ControlSignalChange { signals: 0x04, state: true });
        via.tick(2);
        let int_flags = via.read(0xd);
        assert_ne!(int_flags & IRQ_CB2, 0);
    }

    #[test]
    fn cb2_positive_edge_does_not_trigger_irq_when_level_high() {
        let (mut via, mut remote) = device_with_pipe();
        handshake(&mut via, &mut remote);
        via.cb2 = true;
        via.write(0xc, PCR_CB2_INDEPENDENT_INTERRUPT_INPUT_POSITIVE_EDGE);
        via.apply_message(ViaProtocolMessage::ControlSignalChange { signals: 0x04, state: true });
        via.tick(2);
        let int_flags = via.read(0xd);
        assert_eq!(int_flags & IRQ_CB2, 0);
    }

    // --- Shift register ---

    fn sr_device_with_pipe_and_mode(acr: u8) -> (Via6522, PipeTransport) {
        let (mut via, remote) = device_with_pipe();
        via.write(0xB, acr);
        (via, remote)
    }

    fn handshake(via: &mut Via6522, remote: &mut PipeTransport) {
        remote.send(0x20).unwrap(); // ASCII
        std::thread::sleep(Duration::from_millis(1));
        via.tick(1);
        std::thread::sleep(Duration::from_millis(1));
        collect_bytes(remote); // drain state dump
    }

    #[test]
    fn sr_disabled_mode_is_noop() {
        let mut via = device();
        via.write(0xB, SR_MODE_DISABLED); // ACR SR mode = 000
        via.write(0xA, 0xAB);            // write SR data
        for _ in 0..20 { via.tick(1); }
        assert_eq!(via.peek(0xD) & IRQ_SR, 0, "IRQ_SR should not fire when disabled");
        assert!(!via.sr_running);
        assert_eq!(via.peek(0xA), 0xAB);
    }

    #[test]
    fn sr_shift_out_t2_sends_cb1_cb2_messages() {
        let (mut via, mut remote) = sr_device_with_pipe_and_mode(SR_MODE_OUT_T2);
        handshake(&mut via, &mut remote);

        // Load T2 = 2, write SR to start (ACR already set).
        via.write(0x8, 2u8);
        via.write(0x9, 0x00); // T2 starts
        via.write(0xA, 0b10110100); // MSB=1,1,0,1,1,0,1,0 → shifts out MSB first

        // 8 T2 underflows → 8 SR clocks. Each clock: CB1 low + optional CB2 + CB1 high.
        for _ in 0..8 {
            via.tick(2); // T2 underflow fires sr_clock
        }

        std::thread::sleep(Duration::from_millis(1));
        let received = collect_bytes(&mut remote);
        let s = String::from_utf8_lossy(&received);

        // Expect CB1 messages to appear (at least 8 low + 8 high).
        let cb1_low_count = s.match_indices("CB10").count();
        let cb1_high_count = s.match_indices("CB11").count();
        assert_eq!(cb1_low_count, 8, "expected 8 CB10 messages, got {cb1_low_count}; output: {s}");
        assert_eq!(cb1_high_count, 8, "expected 8 CB11 messages, got {cb1_high_count}; output: {s}");

        // Verify MSB-first bit order: 0b10110100 → bits 1,0,1,1,0,1,0,0
        let expected_bits = [1u8, 0, 1, 1, 0, 1, 0, 0];
        let mut cb2_bits: Vec<u8> = Vec::new();
        let mut search = s.as_ref();
        while let Some(pos) = search.find("CB2") {
            if pos + 4 <= search.len() {
                let state_char = &search[pos + 3..pos + 4];
                cb2_bits.push(if state_char == "1" { 1 } else { 0 });
            }
            search = &search[pos + 3..];
        }
        assert_eq!(cb2_bits.len(), 8, "expected 8 CB2 messages, got {}: {s}", cb2_bits.len());
        for (i, (&expected, &got)) in expected_bits.iter().zip(cb2_bits.iter()).enumerate() {
            assert_eq!(got, expected, "bit {i}: expected CB2={expected}, got CB2={got}");
        }
    }

    #[test]
    fn sr_shift_out_t2_sets_ifr_after_8_clocks() {
        let mut via = device();
        via.write(0xB, SR_MODE_OUT_T2);
        via.write(0x8, 2u8);
        via.write(0x9, 0x00);
        via.write(0xA, 0xAA);
        for _ in 0..8 { via.tick(2); }
        assert_ne!(via.peek(0xD) & IRQ_SR, 0, "IRQ_SR must be set after 8 T2 underflows");
    }

    #[test]
    fn sr_shift_out_t2_stops_after_8_bits() {
        let mut via = device();
        via.write(0xB, SR_MODE_OUT_T2);
        via.write(0x8, 2u8);
        via.write(0x9, 0x00);
        via.write(0xA, 0xFF);
        for _ in 0..8 { via.tick(2); }
        assert!(!via.sr_running, "sr_running must be false after self-terminating mode completes");
    }

    #[test]
    fn sr_shift_out_free_t2_continues_indefinitely() {
        let mut via = device();
        via.write(0xB, SR_MODE_OUT_FREE_T2);
        via.write(0x8, 2u8);
        via.write(0x9, 0x00);
        via.write(0xA, 0x55);
        // Tick 20 T2-length intervals (well past 8 bits).
        for _ in 0..20 { via.tick(2); }
        assert!(via.sr_running, "sr_running must remain true for free-running mode");
    }

    #[test]
    fn sr_shift_in_ext_clk_captures_cb2() {
        let (mut via, mut remote) = sr_device_with_pipe_and_mode(SR_MODE_IN_EXT);
        handshake(&mut via, &mut remote);

        via.write(0xA, 0x00); // start SR shift-in

        // Clock in byte 0b10110010 by toggling CB2 then sending CB1 rising edge.
        // Bit order: MSB first, so bit7=1, bit6=0, bit5=1, bit4=1, bit3=0, bit2=0, bit1=1, bit0=0
        let bits = [1u8, 0, 1, 1, 0, 0, 1, 0];
        for bit in bits {
            via.cb2 = bit != 0;
            // CB1 rising edge clocks the SR.
            via.apply_message(ViaProtocolMessage::ControlSignalChange { signals: 0x08, state: true });
            // CB1 back low (not a clock).
            via.apply_message(ViaProtocolMessage::ControlSignalChange { signals: 0x08, state: false });
        }

        assert_eq!(via.sr, 0b10110010, "shifted-in byte mismatch: got 0x{:02X}", via.sr);
        assert_ne!(via.peek(0xD) & IRQ_SR, 0, "IRQ_SR must be set after 8 external clocks");
    }

    #[test]
    fn sr_shift_in_phi2_sets_ifr_after_8_ticks() {
        let mut via = device();
        via.write(0xB, SR_MODE_IN_PHI2);
        via.write(0xA, 0x00); // start SR shift-in
        for _ in 0..8 { via.tick(1); }
        assert_ne!(via.peek(0xD) & IRQ_SR, 0, "IRQ_SR must be set after 8 PHI2 ticks");
    }

    #[test]
    fn sr_shift_out_phi2_sends_cb1_cb2_messages() {
        let (mut via, mut remote) = sr_device_with_pipe_and_mode(SR_MODE_OUT_PHI2);
        handshake(&mut via, &mut remote);

        via.write(0xA, 0b10110100); // write SR to start shifting out

        for _ in 0..8 { via.tick(1); }

        std::thread::sleep(Duration::from_millis(1));
        let received = collect_bytes(&mut remote);
        let s = String::from_utf8_lossy(&received);

        let cb1_low_count = s.match_indices("CB10").count();
        let cb1_high_count = s.match_indices("CB11").count();
        assert_eq!(cb1_low_count, 8, "expected 8 CB10 messages, got {cb1_low_count}; output: {s}");
        assert_eq!(cb1_high_count, 8, "expected 8 CB11 messages, got {cb1_high_count}; output: {s}");
    }

    #[test]
    fn sr_shift_out_phi2_sets_ifr_after_8_ticks() {
        let mut via = device();
        via.write(0xB, SR_MODE_OUT_PHI2);
        via.write(0xA, 0xAA);
        for _ in 0..8 { via.tick(1); }
        assert_ne!(via.peek(0xD) & IRQ_SR, 0, "IRQ_SR must be set after 8 PHI2 ticks");
    }

    #[test]
    fn sr_shift_out_phi2_stops_after_8_bits() {
        let mut via = device();
        via.write(0xB, SR_MODE_OUT_PHI2);
        via.write(0xA, 0xFF);
        for _ in 0..8 { via.tick(1); }
        assert!(!via.sr_running, "sr_running must be false after PHI2 shift-out completes");
    }

    // --- T2 pulse-counting ---

    #[test]
    fn t2_pulse_count_pb6_neg_transition_decrements_counter() {
        let (mut via, mut remote) = device_with_pipe();
        via.write(0xB, ACR_T2_PB6_COUNT);
        via.write(0x8, 5u8);
        via.write(0x9, 0x00); // T2 = 5, starts
        handshake(&mut via, &mut remote);

        // PB6 high→low: negative transition should decrement counter.
        via.apply_message(ViaProtocolMessage::PortStateChange { port: 'B', value: 0x40 }); // PB6=1
        via.apply_message(ViaProtocolMessage::PortStateChange { port: 'B', value: 0x00 }); // PB6=0

        assert_eq!(via.t2_counter, 4, "expected T2 counter = 4 after one PB6 neg transition");
    }

    #[test]
    fn t2_pulse_count_fires_irq_on_underflow() {
        let (mut via, mut remote) = device_with_pipe();
        via.write(0xE, 0xA0); // enable T2 IRQ
        via.write(0xB, ACR_T2_PB6_COUNT);
        via.write(0x8, 3u8);
        via.write(0x9, 0x00); // T2 = 3
        handshake(&mut via, &mut remote);

        for _ in 0..3 {
            via.apply_message(ViaProtocolMessage::PortStateChange { port: 'B', value: 0x40 });
            via.apply_message(ViaProtocolMessage::PortStateChange { port: 'B', value: 0x00 });
        }

        assert_ne!(via.peek(0xD) & IRQ_T2, 0, "IRQ_T2 must fire after 3 PB6 neg transitions");
        assert!(via.irq_active());
        assert!(!via.t2_running);
    }

    #[test]
    fn t2_pulse_count_ignores_positive_transitions() {
        let (mut via, mut remote) = device_with_pipe();
        via.write(0xB, ACR_T2_PB6_COUNT);
        via.write(0x8, 5u8);
        via.write(0x9, 0x00);
        handshake(&mut via, &mut remote);

        // PB6 low → high (positive transition) must not decrement.
        via.apply_message(ViaProtocolMessage::PortStateChange { port: 'B', value: 0x00 }); // PB6=0
        via.apply_message(ViaProtocolMessage::PortStateChange { port: 'B', value: 0x40 }); // PB6=1

        assert_eq!(via.t2_counter, 5, "positive PB6 transition must not decrement T2 counter");
    }

    #[test]
    fn t2_timed_mode_not_affected_by_pb6() {
        let (mut via, mut remote) = device_with_pipe();
        // ACR bit 5 clear → timed mode.
        via.write(0xB, 0x00);
        via.write(0x8, 100u8);
        via.write(0x9, 0x00);
        handshake(&mut via, &mut remote);

        via.apply_message(ViaProtocolMessage::PortStateChange { port: 'B', value: 0x40 });
        via.apply_message(ViaProtocolMessage::PortStateChange { port: 'B', value: 0x00 });

        // Counter should still be near 100 (only a tick or two from handshake).
        assert!(via.t2_counter >= 98, "timed T2 must not be decremented by PB6 transitions");
    }

    #[test]
    fn multiple_transports_both_receive_port_state_change() {
        let (local1, mut remote1) = PipeTransport::pair().unwrap();
        let (local2, mut remote2) = PipeTransport::pair().unwrap();
        let mut via = Via6522::new();
        via.attach_transport(Box::new(local1));
        via.attach_transport(Box::new(local2));

        remote1.send(0x20).unwrap();
        remote2.send(0x20).unwrap();
        std::thread::sleep(Duration::from_millis(1));
        via.tick(1);

        // Drain state dumps.
        std::thread::sleep(Duration::from_millis(1));
        collect_bytes(&mut remote1);
        collect_bytes(&mut remote2);

        // Drive PA0 high.
        via.write(0x3, 0x01); // DDRA: PA0 = output
        via.write(0x1, 0x01); // ORA: PA0 high

        std::thread::sleep(Duration::from_millis(1));
        let r1 = collect_bytes(&mut remote1);
        let r2 = collect_bytes(&mut remote2);

        assert!(r1.windows(3).any(|w| w == b"A01"),
            "transport 1 missing A01: {:?}", String::from_utf8_lossy(&r1));
        assert!(r2.windows(3).any(|w| w == b"A01"),
            "transport 2 missing A01: {:?}", String::from_utf8_lossy(&r2));
    }

    // --- Reconnection resets handshake ---

    /// A `PipeTransport` wrapper that exposes a `reconnect()` method for testing.
    ///
    /// Swaps in a fresh pipe pair and bumps the `connection_id` counter so that
    /// `poll_transports` detects a new client session and resets the protocol state.
    struct ReconnectablePipe {
        inner: PipeTransport,
        id: u64,
    }

    impl ReconnectablePipe {
        fn new(inner: PipeTransport) -> Self {
            Self { inner, id: 0 }
        }

        fn reconnect(&mut self, new_pipe: PipeTransport) {
            self.inner = new_pipe;
            self.id += 1;
        }
    }

    impl Transport for ReconnectablePipe {
        fn try_recv(&mut self) -> Option<u8> { self.inner.try_recv() }
        fn send(&mut self, byte: u8) -> Result<(), crate::emulator::transport::TransportError> { self.inner.send(byte) }
        fn is_connected(&self) -> bool { self.inner.is_connected() }
        fn connection_id(&self) -> u64 { self.id }
        fn shutdown(&mut self) { self.inner.shutdown() }
    }

    #[test]
    fn reconnection_resets_handshake_and_sends_state_dump() {
        // First connection: complete the handshake.
        let (local1, mut remote1) = PipeTransport::pair().unwrap();
        let pipe = ReconnectablePipe::new(local1);
        let mut via = Via6522::new();
        via.attach_transport(Box::new(pipe));

        remote1.send(0x20).unwrap(); // ASCII selector
        std::thread::sleep(Duration::from_millis(1));
        via.tick(1); // handshake completes
        assert!(via.transports[0].handshake_done, "first handshake must complete");
        std::thread::sleep(Duration::from_millis(1));
        collect_bytes(&mut remote1); // drain first state dump

        // Simulate disconnect + reconnect: swap in a fresh pipe and bump the connection ID.
        let (local2, mut remote2) = PipeTransport::pair().unwrap();
        // SAFETY: downcast — we know slot 0 holds a ReconnectablePipe.
        let slot_transport = via.transports[0].transport.as_mut();
        let reconnectable = unsafe {
            &mut *(slot_transport as *mut dyn Transport as *mut ReconnectablePipe)
        };
        reconnectable.reconnect(local2);

        // Tick once without sending a format selector — the reconnect detection should
        // fire but the handshake should not yet be marked done (awaiting format byte).
        via.tick(1);
        assert!(!via.transports[0].handshake_done,
            "handshake_done must be false after reconnect and before new format byte");

        // New client sends format selector; handshake should re-complete and new state dump sent.
        remote2.send(0xFF).unwrap(); // binary selector
        std::thread::sleep(Duration::from_millis(1));
        via.tick(1);
        assert!(via.transports[0].handshake_done,
            "handshake_done must be true after new client sends format byte");

        std::thread::sleep(Duration::from_millis(1));
        let received = collect_bytes(&mut remote2);
        assert!(!received.is_empty(),
            "new client must receive a state dump after reconnect handshake");
    }
}
