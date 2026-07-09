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
//! - Bit 0: PA latch enable (latch IRA on CA1 active edge)
//! - Bit 1: PB latch enable (latch IRB on CB1 active edge)
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

use std::time::Duration;
use log::debug;
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
const ACR_PA_LATCH_ENABLE:  u8 = 0x01; // bit 0: latch IRA on CA1 active edge
const ACR_PB_LATCH_ENABLE:  u8 = 0x02; // bit 1: latch IRB on CB1 active edge

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
    /// Address at which this device is registered on the bus; see `IoDevice::base_address`.
    address: u16,

    // --- Port registers ---
    /// Output register B — written bits drive output pins on port B.
    orb: u8,
    /// Output register A — written bits drive output pins on port A.
    ora: u8,
    /// Data direction register B — 1=output, 0=input.
    ddrb: u8,
    /// Data direction register A — 1=output, 0=input.
    ddra: u8,
    /// Live input state of port B pins (updated by peripheral messages).
    input_b: u8,
    /// Live input state of port A pins (updated by peripheral messages).
    input_a: u8,
    /// IRA latch: port A value captured at the last CA1 active edge (used when ACR bit 0 set).
    ira_latch: u8,
    /// IRB latch: port B value captured at the last CB1 active edge (used when ACR bit 1 set).
    irb_latch: u8,

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
    /// True when T2 will fire IRQ on its next underflow; cleared on underflow, re-armed on T2CH write.
    t2_irq_armed: bool,

    // --- Shift register ---
    /// Shift register data byte.
    sr: u8,
    /// Counts down a shift operation; starts at 8, stops at 0
    sr_count: u8,
    /// True when shifting out (toward peripheral); false when shifting in.
    sr_shifting_out: bool,
    /// True when free-running mode under T2 control; T2 should be restarted at next tick
    sr_t2_restart: bool,
    /// True if the clock source is external
    sr_external: bool,

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

}

impl Via6522 {
    /// Creates a new `Via6522` in reset state.
    pub fn new() -> Self {
        Self {
            address: 0,
            orb: 0, ora: 0, ddrb: 0, ddra: 0,
            input_b: 0, input_a: 0, ira_latch: 0, irb_latch: 0,
            t1_counter: 0, t1_latch: 0, t1_running: false, t1_pb7: false,
            t2_counter: 0, t2_latch_lo: 0, t2_latch_hi: 0, t2_running: false, t2_irq_armed: false,
            sr: 0, sr_count: 0, sr_shifting_out: false, sr_t2_restart: false, sr_external: false,
            acr: 0, pcr: 0, ifr: 0, ier: 0,
            ca1: false, ca2: false, cb1: false, cb2: false,
            transports: Vec::new(),
            error_sender: None,
            device_id: None,
        }
    }

    /// Sets the address at which this device is registered on the bus.
    pub fn with_address(mut self, address: u16) -> Self {
        self.address = address;
        self
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

    /// Reads the effective state of port B: output pins from ORB, input pins from input_b or
    /// irb_latch when PB latch enable (ACR bit 1) is set. PB7 reflects the T1 toggle state
    /// when ACR enables T1 PB7 output.
    fn read_port_b(&self) -> u8 {
        // Fix (issue #99): square wave output mode takes priority over input/output mode
        // selected by DDRB bit 7.
        let input = if self.acr & ACR_PB_LATCH_ENABLE != 0 { self.irb_latch } else { self.input_b };
        let mut orb = (self.orb & self.ddrb) | (input & !self.ddrb);
        if self.acr & ACR_T1_PB7_OUTPUT != 0 {
            orb = (orb & 0x7f) | if self.t1_pb7 { 0x80 } else { 0 };
        }
        orb
    }

    /// Reads the effective state of port A: output pins from ORA, input pins from input_a or
    /// ira_latch when PA latch enable (ACR bit 0) is set.
    fn read_port_a(&self) -> u8 {
        let input = if self.acr & ACR_PA_LATCH_ENABLE != 0 { self.ira_latch } else { self.input_a };
        (self.ora & self.ddra) | (input & !self.ddra)
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

    /// Sends the full port and control-signal state dump to all connected
    /// peripherals that have completed the handshake.
    fn send_state_to_all(&mut self) {
        for i in 0..self.transports.len() {
            if self.transports[i].handshake_done {
                self.send_state_dump(i);
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
                            if self.t2_irq_armed {
                                self.set_ifr(IRQ_T2);
                                self.t2_irq_armed = false;
                            }
                            self.t2_counter = new_counter; // wraps naturally to 0xFFFF; counter keeps running
                        } else {
                            self.t2_counter = new_counter;
                        }
                    }
                }
            }
            ViaProtocolMessage::ControlSignalChange { signals, state } => {
                if signals & 0x02 != 0 { // CA1
                    let pos_edge = self.pcr & PCR_CA1_EDGE != 0;
                    if self.ca1 != state && state == pos_edge {
                        self.set_ifr(IRQ_CA1);
                        // Capture IRA latch on CA1 active edge when PA latch enable is set.
                        if self.acr & ACR_PA_LATCH_ENABLE != 0 {
                            self.ira_latch = self.input_a;
                        }
                        // CA1 active edge releases CA2 handshake output.
                        let ca2_mode = (self.pcr & PCR_CA2_MASK) >> 1;
                        if ca2_mode == 4 && !self.ca2 {
                            self.ca2 = true;
                            self.send_to_all(ViaProtocolMessage::ControlSignalChange { signals: 0x01, state: true });
                        }
                    }
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
                    if matches!(self.sr_mode(), SR_MODE_IN_EXT | SR_MODE_OUT_EXT)  {
                        self.sr_update(state);
                    } else if self.cb1 != state {
                        self.cb1 = state;
                        let pos_edge = self.pcr & PCR_CB1_EDGE != 0;
                        if state == pos_edge {
                            self.set_ifr(IRQ_CB1);
                            // Capture IRB latch on CB1 active edge when PB latch enable is set.
                            if self.acr & ACR_PB_LATCH_ENABLE != 0 {
                                self.irb_latch = self.input_b;
                            }
                            // CB1 active edge releases CB2 handshake output.
                            let cb2_mode = (self.pcr & PCR_CB2_MASK) >> 5;
                            if cb2_mode == 4 && !self.cb2 {
                                self.cb2 = true;
                                self.send_to_all(ViaProtocolMessage::ControlSignalChange { signals: 0x04, state: true });
                            }
                        }
                    }
                }
                if signals & 0x04 != 0 { // CB2 (when configured as input)
                    let mode = self.sr_mode();
                    if !matches!(mode, SR_MODE_DISABLED) {
                        if matches!(mode, SR_MODE_IN_T2 | SR_MODE_IN_PHI2 | SR_MODE_IN_EXT) {
                            self.cb2 = state;
                        }
                    } else {
                        let cb2_mode = (self.pcr & PCR_CB2_MASK) >> 5;
                        if cb2_mode < 4 && self.cb2 != state { // input modes only
                            self.cb2 = state;
                            let pos_edge = cb2_mode & 0x02 != 0;
                            if state == pos_edge {
                                self.set_ifr(IRQ_CB2);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn sr_mode(&self) -> u8 {
        self.acr & ACR_SR_MODE_MASK
    }

    /// Asserts CA2 low (handshake) or pulses it low then high (pulse) after an ORA access.
    /// Does nothing if CA2 is not in an output mode that responds to ORA access (modes 100/101).
    fn assert_ca2_handshake_or_pulse(&mut self) {
        let ca2_mode = (self.pcr & PCR_CA2_MASK) >> 1;
        match ca2_mode {
            4 if self.ca2 => { // handshake: assert low; released by CA1 active edge
                self.ca2 = false;
                self.send_to_all(ViaProtocolMessage::ControlSignalChange { signals: 0x01, state: false });
            }
            5 => { // pulse: low then immediately high
                self.send_to_all(ViaProtocolMessage::ControlSignalChange { signals: 0x01, state: false });
                self.send_to_all(ViaProtocolMessage::ControlSignalChange { signals: 0x01, state: true });
            }
            _ => {}
        }
    }

    /// Asserts CB2 low (handshake) or pulses it low then high (pulse) after an ORB write.
    /// Does nothing if CB2 is not in an output mode that responds to ORB write (modes 100/101).
    fn assert_cb2_handshake_or_pulse(&mut self) {
        let cb2_mode = (self.pcr & PCR_CB2_MASK) >> 5;
        match cb2_mode {
            4 if self.cb2 => { // handshake: assert low; released by CB1 active edge
                self.cb2 = false;
                self.send_to_all(ViaProtocolMessage::ControlSignalChange { signals: 0x04, state: false });
            }
            5 => { // pulse: low then immediately high
                self.send_to_all(ViaProtocolMessage::ControlSignalChange { signals: 0x04, state: false });
                self.send_to_all(ViaProtocolMessage::ControlSignalChange { signals: 0x04, state: true });
            }
            _ => {}
        }
    }

    fn sr_update(&mut self, rising: bool) {
        if self.sr_count == 0 { return };
        if !rising {
            if !self.sr_external {
                self.send_to_all(ViaProtocolMessage::ControlSignalChange { signals: 0x08, state: false });
            }
            if self.sr_shifting_out {
                let bit = (self.sr >> (self.sr_count - 1)) & 1 != 0;
                self.cb2 = bit;
                self.send_to_all(ViaProtocolMessage::ControlSignalChange { signals: 0x04, state: bit });
            }
        }
        if rising {
            if !self.sr_external {
                self.send_to_all(ViaProtocolMessage::ControlSignalChange { signals: 0x08, state: true });
            }
            if !self.sr_shifting_out {
                let bit = self.cb2;
                let mask = 1u8 << (self.sr_count - 1);
                if bit { self.sr |= mask; } else { self.sr &= !mask; }
            }
            self.sr_count -= 1;
            let t2_ctrl = matches!(self.sr_mode(), SR_MODE_IN_T2 | SR_MODE_OUT_T2 | SR_MODE_OUT_FREE_T2);
            if self.sr_count == 0 {
                // Free-running mode never asserts IRQ (per datasheet).
                if !matches!(self.sr_mode(), SR_MODE_OUT_FREE_T2) {
                    self.set_ifr(IRQ_SR);
                }
                if t2_ctrl {
                    self.t2_running = false;
                    self.t2_counter = 0xffff;
                    if matches!(self.sr_mode(), SR_MODE_OUT_FREE_T2) {
                        self.sr_t2_restart = true;
                    }
                }
            }
            else if t2_ctrl {
                self.t2_counter = ((self.t2_latch_hi as u16) << 8) | self.t2_latch_lo as u16;
                self.sr_update(false);
            }
        }
    }

    fn sr_start(&mut self) {
        self.clear_ifr(IRQ_SR);
        let mode = self.sr_mode();
        if mode != SR_MODE_DISABLED {
            self.sr_count = 8;
            self.cb1 = true;
            self.sr_shifting_out = mode >= SR_MODE_OUT_FREE_T2;
            self.sr_external = matches!(mode, SR_MODE_IN_EXT | SR_MODE_OUT_EXT);
            if matches!(mode, SR_MODE_IN_T2 | SR_MODE_OUT_T2 | SR_MODE_OUT_FREE_T2) {
                self.t2_counter = ((self.t2_latch_hi as u16) << 8) | self.t2_latch_lo as u16;
                self.t2_running = true;
                self.sr_update(false);
            }
        }
    }

    // NOTE: Correct underflow behavior requires cycles == 1. When cycles > 1,
    // overflowing_sub may skip past zero without triggering the underflow condition
    // if the decrement exceeds the remaining count. IoDevice::tick() always passes
    // cycles = 1 in practice, but callers must not pass larger values.
    fn tick_timer_1(&mut self, cycles: u32) {
        if self.t1_running {
            let (new_counter, wrapped) = self.t1_counter.overflowing_sub(cycles as u16);
            if wrapped || new_counter == 0 {
                self.set_ifr(IRQ_T1);
                if self.acr & ACR_T1_PB7_OUTPUT != 0 {
                    self.t1_pb7 = !self.t1_pb7;
                    // read_port_b() reflects the updated t1_pb7.
                    let pb = self.read_port_b();
                    self.send_to_all(ViaProtocolMessage::PortStateChange { port: 'B', value: pb });
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
    }

    // NOTE: Same cycles == 1 constraint as tick_timer_1; see comment there.
    fn tick_timer_2(&mut self, cycles: u32) {
        if self.t2_running && self.acr & ACR_T2_PB6_COUNT == 0 {
            let (new_counter, wrapped) = self.t2_counter.overflowing_sub(cycles as u16);
            if wrapped || new_counter == 0 {
                if matches!(self.sr_mode(), SR_MODE_IN_T2 | SR_MODE_OUT_T2 | SR_MODE_OUT_FREE_T2) {
                    self.sr_update(true);
                } else {
                    if self.t2_irq_armed {
                        self.set_ifr(IRQ_T2);
                        self.t2_irq_armed = false;
                    }
                    self.t2_counter = new_counter; // wraps naturally to 0xFFFF; counter keeps running
                }
            } else {
                self.t2_counter = new_counter;
            }
        } else if self.sr_t2_restart {
            self.sr_t2_restart = false;
            self.sr_start();
        }
    }

    fn tick_timers(&mut self, cycles: u32) {
        self.tick_timer_1(cycles);
        self.tick_timer_2(cycles);
    }

}

impl Default for Via6522 {
    fn default() -> Self {
        Self::new()
    }
}

impl IoDevice for Via6522 {
    fn base_address(&self) -> u16 {
        self.address
    }

    /// Reads the register at `offset` with side effects.
    fn read(&mut self, offset: u16) -> u8 {
        match offset {
            0x0 => {
                // Reading ORB clears CB1 flag and CB2 flag (only in non-independent input modes).
                self.clear_ifr(IRQ_CB1);
                let cb2_mode = (self.pcr & PCR_CB2_MASK) >> 5;
                if cb2_mode != 1 && cb2_mode != 3 { self.clear_ifr(IRQ_CB2); }
                self.read_port_b()
            }
            0x1 => {
                // Reading ORA clears CA1 flag and CA2 flag (only in non-independent input modes).
                self.clear_ifr(IRQ_CA1);
                let ca2_mode = (self.pcr & PCR_CA2_MASK) >> 1;
                if ca2_mode != 1 && ca2_mode != 3 { self.clear_ifr(IRQ_CA2); }
                self.assert_ca2_handshake_or_pulse();
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
                self.sr_start();
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
                // Writing ORB: update output register, clear CB1/CB2 flags (CB2 only in non-independent modes).
                let old_orb = self.orb;
                self.orb = value;
                self.clear_ifr(IRQ_CB1);
                let cb2_mode = (self.pcr & PCR_CB2_MASK) >> 5;
                if cb2_mode != 1 && cb2_mode != 3 { self.clear_ifr(IRQ_CB2); }
                // Send port B state if any output pins changed.
                let old_b = (old_orb & self.ddrb) | (self.input_b & !self.ddrb);
                let new_b = self.read_port_b();
                if old_b != new_b {
                    self.send_to_all(ViaProtocolMessage::PortStateChange { port: 'B', value: new_b });
                }
                self.assert_cb2_handshake_or_pulse();
            }
            0x1 => {
                // Writing ORA: update output register, clear CA1/CA2 flags (CA2 only in non-independent modes).
                let old_ora = self.ora;
                self.ora = value;
                self.clear_ifr(IRQ_CA1);
                let ca2_mode = (self.pcr & PCR_CA2_MASK) >> 1;
                if ca2_mode != 1 && ca2_mode != 3 { self.clear_ifr(IRQ_CA2); }
                let old_a = (old_ora & self.ddra) | (self.input_a & !self.ddra);
                let new_a = self.read_port_a();
                if old_a != new_a {
                    self.send_to_all(ViaProtocolMessage::PortStateChange { port: 'A', value: new_a });
                }
                self.assert_ca2_handshake_or_pulse();
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
                    // Send a message only if PB7 was previously high or Timer 1 was holding PB7 high
                    if prev_pb7 || prev_t1_pb7  {
                        let pb = self.read_port_b();
                        self.send_to_all(ViaProtocolMessage::PortStateChange { port: 'B', value: pb });
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
                self.t2_irq_armed = true;
                self.clear_ifr(IRQ_T2);
            }
            0xA => {
                self.sr = value;
                self.sr_start();
            }
            0xB => {
                // NOTE: Changing SR mode bits or timer-control bits while a shift register
                // operation or timer is actively running produces undefined behavior. The
                // datasheet does not specify mid-operation ACR writes and this implementation
                // makes no attempt to handle them (a running SR continues with its existing
                // sr_count/sr_shifting_out/sr_external state). Real software should stop any
                // active operation before reconfiguring the ACR.
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
                // Manual output modes take effect immediately on PCR write.
                let ca2_mode = (self.pcr & PCR_CA2_MASK) >> 1;
                match ca2_mode {
                    6 if self.ca2 => { // manual low
                        self.ca2 = false;
                        self.send_to_all(ViaProtocolMessage::ControlSignalChange { signals: 0x01, state: false });
                    }
                    7 if !self.ca2 => { // manual high
                        self.ca2 = true;
                        self.send_to_all(ViaProtocolMessage::ControlSignalChange { signals: 0x01, state: true });
                    }
                    _ => {}
                }
                let cb2_mode = (self.pcr & PCR_CB2_MASK) >> 5;
                match cb2_mode {
                    6 if self.cb2 => { // manual low
                        self.cb2 = false;
                        self.send_to_all(ViaProtocolMessage::ControlSignalChange { signals: 0x04, state: false });
                    }
                    7 if !self.cb2 => { // manual high
                        self.cb2 = true;
                        self.send_to_all(ViaProtocolMessage::ControlSignalChange { signals: 0x04, state: true });
                    }
                    _ => {}
                }
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
        self.poll_transports();
        for i in 0..2*cycles {
            if self.sr_count > 0 && matches!(self.sr_mode(), SR_MODE_IN_PHI2 | SR_MODE_OUT_PHI2) {
                self.sr_update(i & 1 != 0);
                std::thread::sleep(Duration::from_micros(500));
            }
            self.poll_transports();
            if i & 1 == 0 {
                self.tick_timers(1);
            }
        }
    }

    /// Resets the device without disturbing state that is under peripheral control
    fn reset(&mut self) {
        let address = self.address;
        let transports = std::mem::take(&mut self.transports);
        let error_sender = self.error_sender.take();
        let device_id = self.device_id;
        // state that must be preserved because it is under peripheral control
        let input_b = self.input_b;
        let input_a = self.input_a;
        let ca1 = self.ca1;
        let ca2 = self.ca2;
        let cb1 = self.cb1;
        let cb2 = self.cb2;
        // state that should be preserved for consistency with real hardware
        let t1_latch = self.t1_latch;
        let t1_counter = self.t1_counter;
        let t2_latch_lo = self.t2_latch_lo;
        let t2_latch_hi = self.t2_latch_hi;
        let t2_counter = self.t2_counter;
        let sr = self.sr;
        *self = Self::new();
        self.address = address;
        self.transports = transports;
        self.error_sender = error_sender;
        self.device_id = device_id;
        // restore state under peripheral control
        self.input_b = input_b;
        self.input_a = input_a;
        self.ca1 = ca1;
        self.ca2 = ca2;
        self.cb1 = cb1;
        self.cb2 = cb2;
        // restore state that should be unaffected by reset
        self.t1_latch = t1_latch;
        self.t1_counter = t1_counter;
        self.t2_latch_lo = t2_latch_lo;
        self.t2_latch_hi = t2_latch_hi;
        self.t2_counter = t2_counter;
        self.sr = sr;
        debug!("{} {} reset", self.name(), device_id.unwrap());
        self.send_state_to_all();
    }

    /// Returns `true` when any enabled interrupt flag is set.
    fn irq_active(&self) -> bool {
        self.ifr & self.ier & 0x7F != 0
    }

    fn name(&self) -> &str {
        "via/6522"
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

    fn send_bytes(remote: &mut PipeTransport, s: &str) {
        for c in s.as_bytes() {
            let b = *c;
            remote.send(b).unwrap();
        }
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
    fn t2_disarms_irq_after_underflow() {
        let mut via = device();
        via.write(0x8, 5u8);
        via.write(0x9, 0x00);
        via.tick(10);
        assert!(!via.t2_irq_armed, "IRQ arm must be cleared after underflow");
        assert!(via.t2_running, "counter must keep running after underflow");
    }

    #[test]
    fn t2_counter_continues_after_underflow() {
        let mut via = device();
        via.write(0x8, 5u8);
        via.write(0x9, 0x00);
        via.tick(5); // 5th tick_timers call: counter reaches 0 → underflow, counter = 0
        // Next tick immediately underflows again (0 - 1 wraps to 0xFFFF), then keeps decrementing.
        via.tick(3); // 3 more tick_timers calls: 0→0xFFFF, 0xFFFF→0xFFFE, 0xFFFE→0xFFFD
        assert!(via.t2_counter < 0xFFFF, "counter must keep decrementing after underflow, got 0x{:04X}", via.t2_counter);
    }

    #[test]
    fn t2_does_not_fire_irq_again_after_underflow_without_reload() {
        let mut via = device();
        via.write(0x8, 3u8);
        via.write(0x9, 0x00);
        via.tick(3); // underflow → IRQ fires
        via.write(0xD, IRQ_T2); // clear IRQ_T2
        // Tick enough for counter to wrap from 0xFFFF back to near 0 and underflow again.
        via.tick(0xFFFF);
        assert_eq!(via.peek(0xD) & IRQ_T2, 0, "IRQ_T2 must not fire again without reloading T2CH");
    }

    // --- Timer 1 additional coverage ---

    #[test]
    fn t1_latch_low_read_returns_latch_not_counter() {
        let mut via = device();
        via.write(0x4, 0x10u8); // latch low = 0x10
        via.write(0x5, 0x00);   // start timer, period = 0x0010
        via.tick(8);             // counter now 0x0008
        assert_eq!(via.read(0x6), 0x10, "T1L-L read must return latch, not counter");
    }

    #[test]
    fn t1_latch_low_read_does_not_clear_irq() {
        let mut via = device();
        via.set_ifr(IRQ_T1);
        via.read(0x6); // T1L-L read must not clear IRQ_T1
        assert_ne!(via.peek(0xD) & IRQ_T1, 0, "IRQ_T1 must not be cleared by T1L-L read");
    }

    #[test]
    fn t1_latch_high_write_does_not_reload_counter() {
        let mut via = device();
        via.write(0x4, 20u8);
        via.write(0x5, 0x00); // start, period = 20
        via.tick(8);           // counter ≈ 12
        let counter_before = via.t1_counter;
        via.write(0x7, 0x00); // write T1L-H — clears IRQ but must not reload counter
        assert_eq!(via.t1_counter, counter_before, "T1L-H write must not reload the running counter");
    }

    #[test]
    fn t1_retrigger_restarts_countdown_from_new_latch() {
        let mut via = device();
        via.write(0x4, 20u8);
        via.write(0x5, 0x00); // start, period = 20
        via.tick(5);           // mid-countdown
        via.write(0x4, 0x0Au8); // new latch low = 10
        via.write(0x5, 0x00);   // write T1CH — re-triggers with new period 10
        assert_eq!(via.t1_counter, 10, "re-trigger must load new latch value into counter");
        assert!(via.t1_running);
        assert_eq!(via.peek(0xD) & IRQ_T1, 0, "IRQ_T1 must be cleared on re-trigger");
    }

    #[test]
    fn t1_free_run_fires_irq_on_consecutive_underflows() {
        let mut via = device();
        via.write(0xB, T1_MODE_FREE_RUN);
        via.write(0x4, 5u8);
        via.write(0x5, 0x00); // period = 5
        via.tick(5);           // first underflow
        assert_ne!(via.peek(0xD) & IRQ_T1, 0, "IRQ_T1 must be set after first underflow");
        via.read(0x4);         // clear IRQ_T1 by reading T1CL
        via.tick(5);           // second underflow
        assert_ne!(via.peek(0xD) & IRQ_T1, 0, "IRQ_T1 must fire again after second underflow");
    }

    #[test]
    fn t1_latch_update_while_running_takes_effect_on_next_reload() {
        let mut via = device();
        via.write(0xB, T1_MODE_FREE_RUN);
        via.write(0x4, 20u8);
        via.write(0x5, 0x00); // period = 20, timer running
        // Update latch only while running (offsets 0x4 and 0x7 — no counter reload).
        via.write(0x4, 0x0Au8); // new latch low = 10
        via.write(0x7, 0x00);   // new latch high = 0; IRQ cleared, counter not reloaded
        via.tick(20);            // first underflow: reloads from new latch (10)
        via.read(0x4);           // clear IRQ_T1
        via.tick(10);            // second period of 10
        assert_ne!(via.peek(0xD) & IRQ_T1, 0, "IRQ_T1 must fire with the new latch period after reload");
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

    // CA2 bits 3:1 / CB2 bits 7:5: non-independent input modes and output mode
    const PCR_CA2_INPUT_NEGATIVE_EDGE: u8 = 0b00000000; // bits 3:1 = 000
    const PCR_CA2_INPUT_POSITIVE_EDGE: u8 = 0b00000100; // bits 3:1 = 010
    const PCR_CA2_OUTPUT_LOW:          u8 = 0b00001100; // bits 3:1 = 110
    const PCR_CB2_INPUT_NEGATIVE_EDGE: u8 = 0b00000000; // bits 7:5 = 000
    const PCR_CB2_INPUT_POSITIVE_EDGE: u8 = 0b01000000; // bits 7:5 = 010
    const PCR_CB2_OUTPUT_LOW:          u8 = 0b11000000; // bits 7:5 = 110

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

    #[test]
    fn ca2_non_independent_negative_edge_triggers_irq_when_level_high() {
        let (mut via, mut remote) = device_with_pipe();
        handshake(&mut via, &mut remote);
        via.ca2 = true;
        via.write(0xC, PCR_CA2_INPUT_NEGATIVE_EDGE);
        send_bytes(&mut remote, "CA20");
        via.tick(1);
        assert_ne!(via.peek(0xD) & IRQ_CA2, 0);
    }

    #[test]
    fn ca2_non_independent_positive_edge_triggers_irq_when_level_low() {
        let (mut via, mut remote) = device_with_pipe();
        handshake(&mut via, &mut remote);
        via.ca2 = false;
        via.write(0xC, PCR_CA2_INPUT_POSITIVE_EDGE);
        send_bytes(&mut remote, "CA21");
        via.tick(1);
        assert_ne!(via.peek(0xD) & IRQ_CA2, 0);
    }

    #[test]
    fn ca2_output_mode_does_not_trigger_irq() {
        let (mut via, mut remote) = device_with_pipe();
        handshake(&mut via, &mut remote);
        via.ca2 = true;
        via.write(0xC, PCR_CA2_OUTPUT_LOW);
        send_bytes(&mut remote, "CA20");
        via.tick(1);
        assert_eq!(via.peek(0xD) & IRQ_CA2, 0);
    }

    #[test]
    fn ca2_output_mode_does_not_update_ca2_state() {
        let (mut via, mut remote) = device_with_pipe();
        handshake(&mut via, &mut remote);
        // Write PCR first (ca2 is false by default so no immediate transition).
        via.write(0xC, PCR_CA2_OUTPUT_LOW);
        // A peripheral message asserting CA2 high must not overwrite the driven-low state.
        send_bytes(&mut remote, "CA21");
        via.tick(1);
        assert!(!via.ca2, "ca2 must not be overwritten by peripheral message in output mode");
    }

    #[test]
    fn cb2_non_independent_negative_edge_triggers_irq_when_level_high() {
        let (mut via, mut remote) = device_with_pipe();
        handshake(&mut via, &mut remote);
        via.cb2 = true;
        via.write(0xC, PCR_CB2_INPUT_NEGATIVE_EDGE);
        send_bytes(&mut remote, "CB20");
        via.tick(1);
        assert_ne!(via.peek(0xD) & IRQ_CB2, 0);
    }

    #[test]
    fn cb2_non_independent_positive_edge_triggers_irq_when_level_low() {
        let (mut via, mut remote) = device_with_pipe();
        handshake(&mut via, &mut remote);
        via.cb2 = false;
        via.write(0xC, PCR_CB2_INPUT_POSITIVE_EDGE);
        send_bytes(&mut remote, "CB21");
        via.tick(1);
        assert_ne!(via.peek(0xD) & IRQ_CB2, 0);
    }

    #[test]
    fn cb2_output_mode_does_not_trigger_irq() {
        let (mut via, mut remote) = device_with_pipe();
        handshake(&mut via, &mut remote);
        via.cb2 = true;
        via.write(0xC, PCR_CB2_OUTPUT_LOW);
        send_bytes(&mut remote, "CB20");
        via.tick(1);
        assert_eq!(via.peek(0xD) & IRQ_CB2, 0);
    }

    #[test]
    fn cb2_output_mode_does_not_update_cb2_state() {
        let (mut via, mut remote) = device_with_pipe();
        handshake(&mut via, &mut remote);
        // Write PCR first (cb2 is false by default so no immediate transition).
        via.write(0xC, PCR_CB2_OUTPUT_LOW);
        // A peripheral message asserting CB2 high must not overwrite the driven-low state.
        send_bytes(&mut remote, "CB21");
        via.tick(1);
        assert!(!via.cb2, "cb2 must not be overwritten by peripheral message in output mode");
    }

    // --- Handshake/pulse output modes ---

    const PCR_CA2_HANDSHAKE_OUTPUT:   u8 = 0b00001000; // bits 3:1 = 100
    const PCR_CA2_PULSE_OUTPUT:       u8 = 0b00001010; // bits 3:1 = 101
    const PCR_CA2_MANUAL_HIGH_OUTPUT: u8 = 0b00001110; // bits 3:1 = 111
    const PCR_CB2_HANDSHAKE_OUTPUT:   u8 = 0b10000000; // bits 7:5 = 100
    const PCR_CB2_PULSE_OUTPUT:       u8 = 0b10100000; // bits 7:5 = 101
    const PCR_CB2_MANUAL_HIGH_OUTPUT: u8 = 0b11100000; // bits 7:5 = 111

    // --- IFR clearing with independent input modes ---

    #[test]
    fn ca2_independent_input_ifr_not_cleared_by_ora_read() {
        let mut via = device();
        via.set_ifr(IRQ_CA2);
        via.write(0xC, PCR_CA2_INDEPENDENT_INTERRUPT_INPUT_NEGATIVE_EDGE);
        via.read(0x1);
        assert_ne!(via.peek(0xD) & IRQ_CA2, 0, "IRQ_CA2 must not be cleared by ORA read in independent mode");
    }

    #[test]
    fn ca2_independent_input_ifr_not_cleared_by_ora_write() {
        let mut via = device();
        via.set_ifr(IRQ_CA2);
        via.write(0xC, PCR_CA2_INDEPENDENT_INTERRUPT_INPUT_NEGATIVE_EDGE);
        via.write(0x1, 0x00);
        assert_ne!(via.peek(0xD) & IRQ_CA2, 0, "IRQ_CA2 must not be cleared by ORA write in independent mode");
    }

    #[test]
    fn ca2_non_independent_input_ifr_cleared_by_ora_read() {
        let mut via = device();
        via.set_ifr(IRQ_CA2);
        via.write(0xC, PCR_CA2_INPUT_NEGATIVE_EDGE);
        via.read(0x1);
        assert_eq!(via.peek(0xD) & IRQ_CA2, 0, "IRQ_CA2 must be cleared by ORA read in non-independent mode");
    }

    #[test]
    fn ca2_non_independent_input_ifr_cleared_by_ora_write() {
        let mut via = device();
        via.set_ifr(IRQ_CA2);
        via.write(0xC, PCR_CA2_INPUT_NEGATIVE_EDGE);
        via.write(0x1, 0x00);
        assert_eq!(via.peek(0xD) & IRQ_CA2, 0, "IRQ_CA2 must be cleared by ORA write in non-independent mode");
    }

    #[test]
    fn cb2_independent_input_ifr_not_cleared_by_orb_read() {
        let mut via = device();
        via.set_ifr(IRQ_CB2);
        via.write(0xC, PCR_CB2_INDEPENDENT_INTERRUPT_INPUT_NEGATIVE_EDGE);
        via.read(0x0);
        assert_ne!(via.peek(0xD) & IRQ_CB2, 0, "IRQ_CB2 must not be cleared by ORB read in independent mode");
    }

    #[test]
    fn cb2_independent_input_ifr_not_cleared_by_orb_write() {
        let mut via = device();
        via.set_ifr(IRQ_CB2);
        via.write(0xC, PCR_CB2_INDEPENDENT_INTERRUPT_INPUT_NEGATIVE_EDGE);
        via.write(0x0, 0x00);
        assert_ne!(via.peek(0xD) & IRQ_CB2, 0, "IRQ_CB2 must not be cleared by ORB write in independent mode");
    }

    #[test]
    fn cb2_non_independent_input_ifr_cleared_by_orb_read() {
        let mut via = device();
        via.set_ifr(IRQ_CB2);
        via.write(0xC, PCR_CB2_INPUT_NEGATIVE_EDGE);
        via.read(0x0);
        assert_eq!(via.peek(0xD) & IRQ_CB2, 0, "IRQ_CB2 must be cleared by ORB read in non-independent mode");
    }

    #[test]
    fn cb2_non_independent_input_ifr_cleared_by_orb_write() {
        let mut via = device();
        via.set_ifr(IRQ_CB2);
        via.write(0xC, PCR_CB2_INPUT_NEGATIVE_EDGE);
        via.write(0x0, 0x00);
        assert_eq!(via.peek(0xD) & IRQ_CB2, 0, "IRQ_CB2 must be cleared by ORB write in non-independent mode");
    }

    // --- Manual output modes ---

    #[test]
    fn ca2_manual_low_sends_ca2_low_message_on_pcr_write() {
        let (mut via, mut remote) = device_with_pipe();
        handshake(&mut via, &mut remote);
        via.ca2 = true;
        via.write(0xC, PCR_CA2_OUTPUT_LOW);
        let received = collect_bytes(&mut remote);
        let s = String::from_utf8_lossy(&received);
        assert!(s.contains("CA20"), "expected CA20 after PCR manual-low write, got: {s}");
        assert!(!via.ca2);
    }

    #[test]
    fn ca2_manual_high_sends_ca2_high_message_on_pcr_write() {
        let (mut via, mut remote) = device_with_pipe();
        handshake(&mut via, &mut remote);
        // ca2 starts false by default
        via.write(0xC, PCR_CA2_MANUAL_HIGH_OUTPUT);
        let received = collect_bytes(&mut remote);
        let s = String::from_utf8_lossy(&received);
        assert!(s.contains("CA21"), "expected CA21 after PCR manual-high write, got: {s}");
        assert!(via.ca2);
    }

    #[test]
    fn cb2_manual_low_sends_cb2_low_message_on_pcr_write() {
        let (mut via, mut remote) = device_with_pipe();
        handshake(&mut via, &mut remote);
        via.cb2 = true;
        via.write(0xC, PCR_CB2_OUTPUT_LOW);
        let received = collect_bytes(&mut remote);
        let s = String::from_utf8_lossy(&received);
        assert!(s.contains("CB20"), "expected CB20 after PCR manual-low write, got: {s}");
        assert!(!via.cb2);
    }

    #[test]
    fn cb2_manual_high_sends_cb2_high_message_on_pcr_write() {
        let (mut via, mut remote) = device_with_pipe();
        handshake(&mut via, &mut remote);
        // cb2 starts false by default
        via.write(0xC, PCR_CB2_MANUAL_HIGH_OUTPUT);
        let received = collect_bytes(&mut remote);
        let s = String::from_utf8_lossy(&received);
        assert!(s.contains("CB21"), "expected CB21 after PCR manual-high write, got: {s}");
        assert!(via.cb2);
    }

    // --- CA2 read handshake ---

    #[test]
    fn ca2_handshake_output_asserts_on_ora_read() {
        let (mut via, mut remote) = device_with_pipe();
        handshake(&mut via, &mut remote);
        via.ca2 = true;
        via.write(0xC, PCR_CA2_HANDSHAKE_OUTPUT);
        collect_bytes(&mut remote); // drain any PCR-triggered messages
        via.read(0x1);
        let received = collect_bytes(&mut remote);
        let s = String::from_utf8_lossy(&received);
        assert!(s.contains("CA20"), "expected CA20 after ORA read in handshake mode, got: {s}");
        assert!(!via.ca2);
    }

    #[test]
    fn ca2_handshake_output_releases_on_ca1_active_edge() {
        let (mut via, mut remote) = device_with_pipe();
        handshake(&mut via, &mut remote);
        via.ca2 = true;
        via.ca1 = true; // start high so falling edge triggers
        via.write(0xC, PCR_CA2_HANDSHAKE_OUTPUT | PCR_CA1_INPUT_NEGATIVE_EDGE);
        collect_bytes(&mut remote);
        via.read(0x1); // assert CA2 low
        collect_bytes(&mut remote); // drain CA20
        send_bytes(&mut remote, "CA10"); // CA1 falling edge — active edge in neg-edge mode
        via.tick(1);
        let received = collect_bytes(&mut remote);
        let s = String::from_utf8_lossy(&received);
        assert!(s.contains("CA21"), "expected CA21 after CA1 active edge releases handshake, got: {s}");
        assert!(via.ca2);
    }

    #[test]
    fn ca2_handshake_not_triggered_by_ora_nh_read() {
        let (mut via, mut remote) = device_with_pipe();
        handshake(&mut via, &mut remote);
        via.ca2 = true;
        via.write(0xC, PCR_CA2_HANDSHAKE_OUTPUT);
        collect_bytes(&mut remote);
        via.read(0xF); // ORA no-handshake
        let received = collect_bytes(&mut remote);
        let s = String::from_utf8_lossy(&received);
        assert!(!s.contains("CA20"), "CA20 must not be sent on ORA_NH read, got: {s}");
        assert!(via.ca2, "ca2 must remain high after ORA_NH read");
    }

    #[test]
    fn ca2_handshake_output_not_asserted_when_already_low() {
        let (mut via, mut remote) = device_with_pipe();
        handshake(&mut via, &mut remote);
        via.ca2 = false; // already asserted
        via.write(0xC, PCR_CA2_HANDSHAKE_OUTPUT);
        collect_bytes(&mut remote);
        via.read(0x1);
        let received = collect_bytes(&mut remote);
        let s = String::from_utf8_lossy(&received);
        assert!(!s.contains("CA20"), "CA20 must not be sent redundantly when already low, got: {s}");
    }

    // --- CA2 write handshake ---

    #[test]
    fn ca2_handshake_output_asserts_on_ora_write() {
        let (mut via, mut remote) = device_with_pipe();
        handshake(&mut via, &mut remote);
        via.ca2 = true;
        via.write(0xC, PCR_CA2_HANDSHAKE_OUTPUT);
        collect_bytes(&mut remote);
        via.write(0x1, 0x00);
        let received = collect_bytes(&mut remote);
        let s = String::from_utf8_lossy(&received);
        assert!(s.contains("CA20"), "expected CA20 after ORA write in handshake mode, got: {s}");
        assert!(!via.ca2);
    }

    #[test]
    fn ca2_write_handshake_releases_on_ca1_active_edge() {
        let (mut via, mut remote) = device_with_pipe();
        handshake(&mut via, &mut remote);
        via.ca2 = true;
        via.ca1 = true;
        via.write(0xC, PCR_CA2_HANDSHAKE_OUTPUT | PCR_CA1_INPUT_NEGATIVE_EDGE);
        collect_bytes(&mut remote);
        via.write(0x1, 0x00); // assert CA2 low
        collect_bytes(&mut remote);
        send_bytes(&mut remote, "CA10");
        via.tick(1);
        let received = collect_bytes(&mut remote);
        let s = String::from_utf8_lossy(&received);
        assert!(s.contains("CA21"), "expected CA21 after CA1 active edge, got: {s}");
        assert!(via.ca2);
    }

    // --- CA2 pulse mode ---

    #[test]
    fn ca2_pulse_output_on_ora_read_sends_low_then_high() {
        let (mut via, mut remote) = device_with_pipe();
        handshake(&mut via, &mut remote);
        via.write(0xC, PCR_CA2_PULSE_OUTPUT);
        collect_bytes(&mut remote);
        via.read(0x1);
        let received = collect_bytes(&mut remote);
        let s = String::from_utf8_lossy(&received);
        let low_pos  = s.find("CA20").expect("expected CA20 in pulse output");
        let high_pos = s.find("CA21").expect("expected CA21 in pulse output");
        assert!(low_pos < high_pos, "CA20 must precede CA21 in pulse sequence");
    }

    #[test]
    fn ca2_pulse_output_on_ora_write_sends_low_then_high() {
        let (mut via, mut remote) = device_with_pipe();
        handshake(&mut via, &mut remote);
        via.write(0xC, PCR_CA2_PULSE_OUTPUT);
        collect_bytes(&mut remote);
        via.write(0x1, 0x00);
        let received = collect_bytes(&mut remote);
        let s = String::from_utf8_lossy(&received);
        let low_pos  = s.find("CA20").expect("expected CA20 in pulse output");
        let high_pos = s.find("CA21").expect("expected CA21 in pulse output");
        assert!(low_pos < high_pos, "CA20 must precede CA21 in pulse sequence");
    }

    #[test]
    fn ca2_pulse_output_not_triggered_by_ora_nh() {
        let (mut via, mut remote) = device_with_pipe();
        handshake(&mut via, &mut remote);
        via.write(0xC, PCR_CA2_PULSE_OUTPUT);
        collect_bytes(&mut remote);
        via.read(0xF); // ORA no-handshake
        let received = collect_bytes(&mut remote);
        let s = String::from_utf8_lossy(&received);
        assert!(!s.contains("CA20"), "CA20 must not be sent on ORA_NH read, got: {s}");
        assert!(!s.contains("CA21"), "CA21 must not be sent on ORA_NH read, got: {s}");
    }

    // --- CB2 write handshake ---

    #[test]
    fn cb2_handshake_output_asserts_on_orb_write() {
        let (mut via, mut remote) = device_with_pipe();
        handshake(&mut via, &mut remote);
        via.cb2 = true;
        via.write(0xC, PCR_CB2_HANDSHAKE_OUTPUT);
        collect_bytes(&mut remote);
        via.write(0x0, 0x00);
        let received = collect_bytes(&mut remote);
        let s = String::from_utf8_lossy(&received);
        assert!(s.contains("CB20"), "expected CB20 after ORB write in handshake mode, got: {s}");
        assert!(!via.cb2);
    }

    #[test]
    fn cb2_handshake_output_releases_on_cb1_active_edge() {
        let (mut via, mut remote) = device_with_pipe();
        handshake(&mut via, &mut remote);
        via.cb2 = true;
        via.cb1 = true;
        via.write(0xC, PCR_CB2_HANDSHAKE_OUTPUT | PCR_CB1_INPUT_NEGATIVE_EDGE);
        collect_bytes(&mut remote);
        via.write(0x0, 0x00); // assert CB2 low
        collect_bytes(&mut remote);
        send_bytes(&mut remote, "CB10"); // CB1 falling edge — active in neg-edge mode
        via.tick(1);
        let received = collect_bytes(&mut remote);
        let s = String::from_utf8_lossy(&received);
        assert!(s.contains("CB21"), "expected CB21 after CB1 active edge releases handshake, got: {s}");
        assert!(via.cb2);
    }

    #[test]
    fn cb2_handshake_output_not_triggered_by_orb_read() {
        let (mut via, mut remote) = device_with_pipe();
        handshake(&mut via, &mut remote);
        via.cb2 = true;
        via.write(0xC, PCR_CB2_HANDSHAKE_OUTPUT);
        collect_bytes(&mut remote);
        via.read(0x0); // ORB read — must NOT trigger CB2
        let received = collect_bytes(&mut remote);
        let s = String::from_utf8_lossy(&received);
        assert!(!s.contains("CB20"), "CB20 must not be sent on ORB read, got: {s}");
        assert!(via.cb2, "cb2 must remain high after ORB read");
    }

    // --- CB2 pulse mode ---

    #[test]
    fn cb2_pulse_output_on_orb_write_sends_low_then_high() {
        let (mut via, mut remote) = device_with_pipe();
        handshake(&mut via, &mut remote);
        via.write(0xC, PCR_CB2_PULSE_OUTPUT);
        collect_bytes(&mut remote);
        via.write(0x0, 0x00);
        let received = collect_bytes(&mut remote);
        let s = String::from_utf8_lossy(&received);
        let low_pos  = s.find("CB20").expect("expected CB20 in pulse output");
        let high_pos = s.find("CB21").expect("expected CB21 in pulse output");
        assert!(low_pos < high_pos, "CB20 must precede CB21 in pulse sequence");
    }

    // --- PA/PB input latching ---

    #[test]
    fn pa_latch_disabled_reads_live_input() {
        let mut via = device();
        // ACR bit 0 clear — live input_a is returned.
        via.input_a = 0xAB;
        assert_eq!(via.read(0x1), 0xAB);
    }

    #[test]
    fn pa_latch_enabled_reads_captured_value_not_live_input() {
        let mut via = device();
        via.write(0xB, ACR_PA_LATCH_ENABLE);
        via.input_a = 0xAB;
        via.ira_latch = 0x55;
        assert_eq!(via.read(0x1), 0x55, "latch value must be returned, not live input");
    }

    #[test]
    fn pa_latch_captures_on_ca1_active_edge() {
        let mut via = device();
        via.write(0xB, ACR_PA_LATCH_ENABLE);
        via.write(0xC, PCR_CA1_INPUT_NEGATIVE_EDGE);
        via.ca1 = true;
        via.input_a = 0xCD;
        via.apply_message(ViaProtocolMessage::ControlSignalChange { signals: 0x02, state: false });
        assert_eq!(via.ira_latch, 0xCD, "ira_latch must capture input_a on CA1 active edge");
    }

    #[test]
    fn pa_latch_does_not_capture_on_inactive_ca1_edge() {
        let mut via = device();
        via.write(0xB, ACR_PA_LATCH_ENABLE);
        via.write(0xC, PCR_CA1_INPUT_NEGATIVE_EDGE);
        via.ca1 = false; // already low — no edge when we send low again
        via.input_a = 0xCD;
        via.ira_latch = 0x11; // sentinel
        via.apply_message(ViaProtocolMessage::ControlSignalChange { signals: 0x02, state: false });
        assert_eq!(via.ira_latch, 0x11, "ira_latch must not change when CA1 level does not change");
    }

    #[test]
    fn pa_latch_enabled_ora_nh_also_reads_latch() {
        let mut via = device();
        via.write(0xB, ACR_PA_LATCH_ENABLE);
        via.input_a = 0xAB;
        via.ira_latch = 0x55;
        assert_eq!(via.read(0xF), 0x55, "ORA_NH must also return latch value when latch is enabled");
    }

    #[test]
    fn pb_latch_disabled_reads_live_input() {
        let mut via = device();
        via.input_b = 0xAB;
        assert_eq!(via.read(0x0), 0xAB);
    }

    #[test]
    fn pb_latch_enabled_reads_captured_value_not_live_input() {
        let mut via = device();
        via.write(0xB, ACR_PB_LATCH_ENABLE);
        via.input_b = 0xAB;
        via.irb_latch = 0x55;
        assert_eq!(via.read(0x0), 0x55, "latch value must be returned, not live input");
    }

    #[test]
    fn pb_latch_captures_on_cb1_active_edge() {
        let mut via = device();
        via.write(0xB, ACR_PB_LATCH_ENABLE);
        via.write(0xC, PCR_CB1_INPUT_NEGATIVE_EDGE);
        via.cb1 = true;
        via.input_b = 0xCD;
        via.apply_message(ViaProtocolMessage::ControlSignalChange { signals: 0x08, state: false });
        assert_eq!(via.irb_latch, 0xCD, "irb_latch must capture input_b on CB1 active edge");
    }

    #[test]
    fn pa_latch_captures_via_transport_in_same_tick() {
        // Validates the full protocol path: port state and CA1 edge arriving via transport
        // in the same tick — port update must precede the capture.
        let (mut via, mut remote) = device_with_pipe();
        handshake(&mut via, &mut remote);
        via.write(0xB, ACR_PA_LATCH_ENABLE);
        via.write(0xC, PCR_CA1_INPUT_POSITIVE_EDGE); // positive edge
        // Send port A update then CA1 rising edge in a single burst.
        send_bytes(&mut remote, "A3F CA11");
        via.tick(1); // poll_transports processes both messages in order
        assert_eq!(via.ira_latch, 0x3F, "ira_latch must capture the value from the same-tick port update");
        assert_eq!(via.read(0x1), 0x3F, "ORA read must return latched value");
    }

    #[test]
    fn pa_latch_holds_value_after_subsequent_port_update() {
        // Validates that the latch retains its captured value when input_a changes afterwards.
        let (mut via, mut remote) = device_with_pipe();
        handshake(&mut via, &mut remote);
        via.write(0xB, ACR_PA_LATCH_ENABLE);
        via.write(0xC, PCR_CA1_INPUT_POSITIVE_EDGE);
        // Capture 0x3F via CA1 rising edge.
        send_bytes(&mut remote, "A3F CA11");
        via.tick(1);
        // Port A changes after the latch was captured.
        send_bytes(&mut remote, "AFF");
        via.tick(1);
        assert_eq!(via.read(0x1), 0x3F, "latched value must be held despite subsequent port update");
    }

    #[test]
    fn pb_latch_captures_via_transport_in_same_tick() {
        let (mut via, mut remote) = device_with_pipe();
        handshake(&mut via, &mut remote);
        via.write(0xB, ACR_PB_LATCH_ENABLE);
        via.write(0xC, PCR_CB1_INPUT_POSITIVE_EDGE);
        send_bytes(&mut remote, "B5A CB11");
        via.tick(1);
        assert_eq!(via.irb_latch, 0x5A, "irb_latch must capture the value from the same-tick port update");
        assert_eq!(via.read(0x0), 0x5A, "ORB read must return latched value");
    }

    #[test]
    fn pb_latch_holds_value_after_subsequent_port_update() {
        let (mut via, mut remote) = device_with_pipe();
        handshake(&mut via, &mut remote);
        via.write(0xB, ACR_PB_LATCH_ENABLE);
        via.write(0xC, PCR_CB1_INPUT_POSITIVE_EDGE);
        send_bytes(&mut remote, "B5A CB11");
        via.tick(1);
        send_bytes(&mut remote, "BFF");
        via.tick(1);
        assert_eq!(via.read(0x0), 0x5A, "latched value must be held despite subsequent port update");
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
        assert_eq!(via.sr_count, 0);
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
        assert_eq!(s.trim(), "CB10 CB21 CB11 CB10 CB20 CB11 CB10 CB21 CB11 CB10 CB21 CB11 CB10 CB20 CB11 CB10 CB21 CB11 CB10 CB20 CB11 CB10 CB20 CB11");
    }

    #[test]
    fn sr_shift_out_ext_sends_cb2_messages() {
        let (mut via, mut remote) = sr_device_with_pipe_and_mode(SR_MODE_OUT_EXT);
        handshake(&mut via, &mut remote);
        via.write(0xA, 0b10110100); // MSB=1,1,0,1,1,0,1,0 → shifts out MSB first
        for _ in 0..8 {
            send_bytes(&mut remote, " CB10");
            via.tick(5);
            send_bytes(&mut remote, " CB11");
        }
        let received = collect_bytes(&mut remote);
        let s = String::from_utf8_lossy(&received);
        assert_eq!(s.trim(), "CB21 CB20 CB21 CB21 CB20 CB21 CB20 CB20");
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
        assert_eq!(via.sr_count, 0, "sr_count must be zero after self-terminating mode completes");
    }

    #[test]
    fn sr_shift_out_free_t2_continues_indefinitely() {
        let mut via = device();
        via.write(0xB, SR_MODE_OUT_FREE_T2);
        via.write(0x8, 2u8);
        via.write(0x9, 0x00);
        via.write(0xA, 0x55);
        for _ in 0..16 { via.tick(1); }
        assert!(via.sr_t2_restart, "expect T2 restart flag for free-running mode");
    }

    #[test]
    fn sr_shift_out_free_t2_never_sets_ifr() {
        let mut via = device();
        via.write(0xB, SR_MODE_OUT_FREE_T2);
        via.write(0x8, 2u8);
        via.write(0x9, 0x00);
        via.write(0xA, 0xAA);
        // Run for two full 8-bit cycles.
        for _ in 0..8 { via.tick(2); }
        via.tick(1); // trigger restart
        for _ in 0..8 { via.tick(2); }
        assert_eq!(via.peek(0xD) & IRQ_SR, 0, "IRQ_SR must never be set in free-running OUT_FREE_T2 mode");
    }

    #[test]
    fn sr_shift_out_free_t2_sends_cb1_cb2_messages() {
        let (mut via, mut remote) = sr_device_with_pipe_and_mode(SR_MODE_OUT_FREE_T2);
        handshake(&mut via, &mut remote);
        via.write(0x8, 5u8);
        via.write(0x9, 0);
        via.write(0xA, 0b10110100); // MSB=1,1,0,1,1,0,1,0 → shifts out MSB first
        for _ in 0..8 {
            via.tick(5);
        }
        via.tick(1);
        for _ in 0..8 {
            via.tick(5);
        }
        let received = collect_bytes(&mut remote);
        let s = String::from_utf8_lossy(&received);
        assert_eq!(s.trim(), "\
        CB10 CB21 CB11 CB10 CB20 CB11 CB10 CB21 CB11 CB10 CB21 CB11 \
        CB10 CB20 CB11 CB10 CB21 CB11 CB10 CB20 CB11 CB10 CB20 CB11 \
        CB10 CB21 CB11 CB10 CB20 CB11 CB10 CB21 CB11 CB10 CB21 CB11 \
        CB10 CB20 CB11 CB10 CB21 CB11 CB10 CB20 CB11 CB10 CB20 CB11");
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
            // CB1 back low (not a clock).
            via.apply_message(ViaProtocolMessage::ControlSignalChange { signals: 0x08, state: false });
            via.cb2 = bit != 0;
            // CB1 rising edge clocks the SR.
            via.apply_message(ViaProtocolMessage::ControlSignalChange { signals: 0x08, state: true });
        }

        assert_eq!(via.sr, 0b10110010, "shifted-in byte mismatch: got 0x{:02X}", via.sr);
        assert_ne!(via.peek(0xD) & IRQ_SR, 0, "IRQ_SR must be set after 8 external clocks");
    }

    #[test]
    fn sr_shift_in_phi2() {
        let mut via = device();
        via.write(0xB, SR_MODE_IN_PHI2);
        via.read(0xa);
        let b = 0b11010010;
        for i in 0..8 {
            let mask = 1u8 << (7 - i);
            via.cb2 = b & mask != 0;
            via.tick(1);
        }
        assert_eq!(via.peek(0xa), b);
        assert_ne!(via.peek(0xD) & IRQ_SR, 0, "IRQ_SR must be set after 8 PHI2 ticks");
    }

    #[test]
    fn sr_shift_in_t2() {
        let mut via = device();
        via.write(0xb, SR_MODE_IN_T2);
        via.write(0x8, 2u8);
        via.write(0x9, 0);
        via.read(0xa);
        let b = 0b11010010;
        for i in 0..8 {
            let mask = 1u8 << (7 - i);
            via.cb2 = b & mask != 0;
            via.tick(2);
        }
        assert_eq!(via.peek(0xa), b, "expected {b:02x} got {:02x}", via.peek(0xa));
        assert_ne!(via.peek(0xd) & IRQ_SR, 0, "IRQ_SR must be set after 8 PHI2 ticks");
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
        assert_eq!(s.trim(), "CB10 CB21 CB11 CB10 CB20 CB11 CB10 CB21 CB11 CB10 CB21 CB11 CB10 CB20 CB11 CB10 CB21 CB11 CB10 CB20 CB11 CB10 CB20 CB11");
    }

    #[test]
    fn sr_shift_out_phi2_sets_ifr_after_8_ticks() {
        let mut via = device();
        via.write(0xB, SR_MODE_OUT_PHI2);
        via.write(0xA, 0xAA);
        for _ in 0..9 { via.tick(1); }
        assert_ne!(via.peek(0xD) & IRQ_SR, 0, "IRQ_SR must be set after 8 PHI2 ticks");
    }

    #[test]
    fn sr_shift_out_phi2_stops_after_8_bits() {
        let mut via = device();
        via.write(0xB, SR_MODE_OUT_PHI2);
        via.write(0xA, 0xFF);
        for _ in 0..9 { via.tick(1); }
        assert_eq!(via.sr_count, 0, "sr_count must be zero after PHI2 shift-out completes");
    }

    #[test]
    fn sr_in_ext_data_captured_via_transport() {
        let (mut via, mut remote) = sr_device_with_pipe_and_mode(SR_MODE_IN_EXT);
        handshake(&mut via, &mut remote);

        via.write(0xA, 0x00); // start shift-in
        collect_bytes(&mut remote);

        let data = 0b10110100u8;
        for i in 0..8 {
            let bit = (data >> (7 - i)) & 1;
            send_bytes(&mut remote, " CB10");
            send_bytes(&mut remote, if bit != 0 { " CB21" } else { " CB20" });
            send_bytes(&mut remote, " CB11");
            via.tick(1); // poll processes CB10, CB2x, CB11 in order
        }

        assert_eq!(via.peek(0xA), data, "shifted-in byte mismatch: expected 0x{data:02X} got 0x{:02X}", via.peek(0xA));
        assert_ne!(via.peek(0xD) & IRQ_SR, 0, "IRQ_SR must be set after 8 external clocks");
    }

    #[test]
    fn sr_in_t2_data_captured_via_transport() {
        let (mut via, mut remote) = sr_device_with_pipe_and_mode(SR_MODE_IN_T2);
        handshake(&mut via, &mut remote);

        via.write(0x8, 2u8); // T2 period = 2 cycles
        via.read(0xA);        // sr_start → CB10 sent, T2 begins counting
        collect_bytes(&mut remote); // drain CB10

        let data = 0b10110100u8;
        for i in 0..8 {
            let bit = (data >> (7 - i)) & 1;
            // Pre-send CB2x so poll_transports at the top of tick captures it
            // before T2 underflows and calls sr_update(true).
            send_bytes(&mut remote, if bit != 0 { " CB21" } else { " CB20" });
            via.tick(2); // T2 counts 2→0 → captures cb2, sends CB11 + CB10 (if not last)
            collect_bytes(&mut remote);
        }

        assert_eq!(via.peek(0xA), data, "shifted-in byte mismatch: expected 0x{data:02X} got 0x{:02X}", via.peek(0xA));
        assert_ne!(via.peek(0xD) & IRQ_SR, 0, "IRQ_SR must be set after 8 T2 clocks");
    }

    #[test]
    fn sr_in_t2_sends_cb1_clock_to_peripheral() {
        let (mut via, mut remote) = sr_device_with_pipe_and_mode(SR_MODE_IN_T2);
        handshake(&mut via, &mut remote);

        via.write(0x8, 2u8);
        via.read(0xA); // sr_start → sr_update(false) → CB10 sent
        let received = collect_bytes(&mut remote);
        let s = String::from_utf8_lossy(&received);
        assert!(s.contains("CB10"), "expected CB10 after SR start, got: {s}");

        for _ in 0..7 {
            via.tick(2);
            let received = collect_bytes(&mut remote);
            let s = String::from_utf8_lossy(&received);
            assert!(s.contains("CB11"), "expected CB11 (rising) in: {s}");
            assert!(s.contains("CB10"), "expected CB10 (falling for next bit) in: {s}");
        }

        via.tick(2); // 8th bit: rising edge only, no next falling
        let received = collect_bytes(&mut remote);
        let s = String::from_utf8_lossy(&received);
        assert!(s.contains("CB11"), "expected CB11 for last bit in: {s}");
        assert!(!s.contains("CB10"), "unexpected CB10 after last bit in: {s}");
    }

    #[test]
    fn sr_in_phi2_data_captured_via_transport() {
        let (mut via, mut remote) = sr_device_with_pipe_and_mode(SR_MODE_IN_PHI2);
        handshake(&mut via, &mut remote);

        via.read(0xA); // start shift-in
        collect_bytes(&mut remote);

        let data = 0b10110100u8;
        for i in 0..8 {
            let bit = (data >> (7 - i)) & 1;
            // Pre-send CB2x so poll_transports at the top of tick captures it
            // before the PHI2 rising edge calls sr_update(true).
            send_bytes(&mut remote, if bit != 0 { " CB21" } else { " CB20" });
            via.tick(1); // falling edge (i=0) → CB10; rising edge (i=1) → captures cb2, CB11
            collect_bytes(&mut remote);
        }

        assert_eq!(via.peek(0xA), data, "shifted-in byte mismatch: expected 0x{data:02X} got 0x{:02X}", via.peek(0xA));
        assert_ne!(via.peek(0xD) & IRQ_SR, 0, "IRQ_SR must be set after 8 PHI2 ticks");
    }

    #[test]
    fn sr_in_phi2_sends_cb1_clock_to_peripheral() {
        let (mut via, mut remote) = sr_device_with_pipe_and_mode(SR_MODE_IN_PHI2);
        handshake(&mut via, &mut remote);

        via.read(0xA); // start shift-in
        collect_bytes(&mut remote);

        for _ in 0..8 {
            via.tick(1); // falling edge (i=0) → CB10; rising edge (i=1) → CB11
            let received = collect_bytes(&mut remote);
            let s = String::from_utf8_lossy(&received);
            assert!(s.contains("CB10"), "expected CB10 in: {s}");
            assert!(s.contains("CB11"), "expected CB11 in: {s}");
        }
    }

    #[test]
    fn sr_out_ext_sets_ifr_after_8_clocks() {
        let (mut via, mut remote) = sr_device_with_pipe_and_mode(SR_MODE_OUT_EXT);
        handshake(&mut via, &mut remote);

        via.write(0xA, 0xAA); // start shift-out
        collect_bytes(&mut remote);

        for _ in 0..8 {
            send_bytes(&mut remote, " CB10 CB11");
            via.tick(1); // poll processes falling then rising edge; sr_count decrements
        }

        assert_ne!(via.peek(0xD) & IRQ_SR, 0, "IRQ_SR must be set after 8 external clocks");
    }

    // --- SR does not set T2/CB1/CB2 IFR bits ---

    #[test]
    fn sr_in_t2_does_not_set_t2_cb1_cb2_ifr_bits() {
        let mut via = device();
        via.write(0xB, SR_MODE_IN_T2);
        via.write(0x8, 2u8);
        via.write(0x9, 0x00);
        via.read(0xA);
        for _ in 0..8 { via.tick(2); }
        let ifr = via.peek(0xD);
        assert_eq!(ifr & IRQ_T2,  0, "IRQ_T2  must not be set during IN_T2 shift");
        assert_eq!(ifr & IRQ_CB1, 0, "IRQ_CB1 must not be set during IN_T2 shift");
        assert_eq!(ifr & IRQ_CB2, 0, "IRQ_CB2 must not be set during IN_T2 shift");
    }

    #[test]
    fn sr_out_t2_does_not_set_t2_cb1_cb2_ifr_bits() {
        let mut via = device();
        via.write(0xB, SR_MODE_OUT_T2);
        via.write(0x8, 2u8);
        via.write(0x9, 0x00);
        via.write(0xA, 0xAA);
        for _ in 0..8 { via.tick(2); }
        let ifr = via.peek(0xD);
        assert_eq!(ifr & IRQ_T2,  0, "IRQ_T2  must not be set during OUT_T2 shift");
        assert_eq!(ifr & IRQ_CB1, 0, "IRQ_CB1 must not be set during OUT_T2 shift");
        assert_eq!(ifr & IRQ_CB2, 0, "IRQ_CB2 must not be set during OUT_T2 shift");
    }

    #[test]
    fn sr_out_free_t2_does_not_set_t2_cb1_cb2_ifr_bits() {
        let mut via = device();
        via.write(0xB, SR_MODE_OUT_FREE_T2);
        via.write(0x8, 2u8);
        via.write(0x9, 0x00);
        via.write(0xA, 0xAA);
        for _ in 0..8 { via.tick(2); }
        via.tick(1); // trigger restart
        for _ in 0..8 { via.tick(2); }
        let ifr = via.peek(0xD);
        assert_eq!(ifr & IRQ_T2,  0, "IRQ_T2  must not be set during OUT_FREE_T2 shift");
        assert_eq!(ifr & IRQ_CB1, 0, "IRQ_CB1 must not be set during OUT_FREE_T2 shift");
        assert_eq!(ifr & IRQ_CB2, 0, "IRQ_CB2 must not be set during OUT_FREE_T2 shift");
    }

    #[test]
    fn sr_in_phi2_does_not_set_cb1_cb2_ifr_bits() {
        let mut via = device();
        via.write(0xB, SR_MODE_IN_PHI2);
        via.read(0xA);
        via.cb2 = true;
        for _ in 0..8 { via.tick(1); }
        let ifr = via.peek(0xD);
        assert_eq!(ifr & IRQ_CB1, 0, "IRQ_CB1 must not be set during IN_PHI2 shift");
        assert_eq!(ifr & IRQ_CB2, 0, "IRQ_CB2 must not be set during IN_PHI2 shift");
    }

    #[test]
    fn sr_out_phi2_does_not_set_cb1_cb2_ifr_bits() {
        let mut via = device();
        via.write(0xB, SR_MODE_OUT_PHI2);
        via.write(0xA, 0xAA);
        for _ in 0..8 { via.tick(1); }
        let ifr = via.peek(0xD);
        assert_eq!(ifr & IRQ_CB1, 0, "IRQ_CB1 must not be set during OUT_PHI2 shift");
        assert_eq!(ifr & IRQ_CB2, 0, "IRQ_CB2 must not be set during OUT_PHI2 shift");
    }

    #[test]
    fn sr_in_ext_does_not_set_cb1_cb2_ifr_bits() {
        let (mut via, mut remote) = sr_device_with_pipe_and_mode(SR_MODE_IN_EXT);
        handshake(&mut via, &mut remote);
        via.write(0xA, 0x00);
        collect_bytes(&mut remote);
        for _ in 0..8 {
            send_bytes(&mut remote, " CB10 CB21 CB11");
            via.tick(1);
        }
        let ifr = via.peek(0xD);
        assert_eq!(ifr & IRQ_CB1, 0, "IRQ_CB1 must not be set during IN_EXT shift");
        assert_eq!(ifr & IRQ_CB2, 0, "IRQ_CB2 must not be set during IN_EXT shift");
    }

    #[test]
    fn sr_out_ext_does_not_set_cb1_cb2_ifr_bits() {
        let (mut via, mut remote) = sr_device_with_pipe_and_mode(SR_MODE_OUT_EXT);
        handshake(&mut via, &mut remote);
        via.write(0xA, 0xAA);
        collect_bytes(&mut remote);
        for _ in 0..8 {
            send_bytes(&mut remote, " CB10 CB11");
            via.tick(1);
        }
        let ifr = via.peek(0xD);
        assert_eq!(ifr & IRQ_CB1, 0, "IRQ_CB1 must not be set during OUT_EXT shift");
        assert_eq!(ifr & IRQ_CB2, 0, "IRQ_CB2 must not be set during OUT_EXT shift");
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
        assert!(!via.t2_irq_armed, "IRQ arm must be cleared after pulse-count underflow");
        assert!(via.t2_running, "counter must keep running after pulse-count underflow");
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

    #[test]
    fn reset_preserves_bus_config() {
        let (mut device, _) = device_with_pipe();
        device.device_id = Some(DeviceId(0));
        device.reset();
        assert!(!device.transports.is_empty(), "expected transport to be preserved");
        assert!(device.device_id.is_some(), "expected device ID to be preserved");
    }

    #[test]
    fn reset_clears_irq() {
        let mut device = device();
        device.ier = IRQ_CB1;
        device.ifr = IRQ_CB1;
        assert!(device.irq_active(), "expected IRQ active");
        device.reset();
        assert!(!device.irq_active(), "expected IRQ inactive after reset");
        assert_eq!(device.ifr_read(), 0);
        assert_eq!(device.ier, 0);
    }

    #[test]
    fn reset_preserves_peripheral_state() {
        let mut device = device();
        device.ca1 = true;
        device.ca2 = true;
        device.cb1 = true;
        device.cb2 = true;
        device.input_a = 0x55;
        device.input_b = 0xaa;
        device.reset();
        assert!(device.ca1, "expected CA1 set");
        assert!(device.ca2, "expected CA2 set");
        assert!(device.cb1, "expected CB1 set");
        assert!(device.cb2, "expected CB2 set");
        assert_eq!(device.input_a, 0x55);
        assert_eq!(device.input_b, 0xaa);
    }

    #[test]
    fn reset_preserves_unaffected_registers() {
        let mut device = device();
        device.t1_counter = 0x55aa;
        device.t1_latch = 0x55aa;
        device.t2_counter = 0xaa55;
        device.t2_latch_lo = 0x55;
        device.t2_latch_hi = 0xaa;
        device.sr = 0xd2;
        device.reset();
        assert_eq!(device.t1_counter, 0x55aa);
        assert_eq!(device.t1_latch, 0x55aa);
        assert_eq!(device.t2_counter, 0xaa55);
        assert_eq!(device.t2_latch_lo, 0x55);
        assert_eq!(device.t2_latch_hi, 0xaa);
        assert_eq!(device.sr, 0xd2);
    }

}
