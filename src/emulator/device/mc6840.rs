//! Motorola MC6840 Programmable Timer Module (PTM)
//!
//! A reasonably faithful emulation of the MC6840.
//!
//! The MC6840 has three 16-bit counters, three corresponding control registers (CR1, CR2, and CR3)
//! and a status register. Under software control the counters can be used to generate interrupts
//! and/or generate output signals for peripherals connected via a supported [Transport](crate::emulator::Transport).
//! Supported wave generation modes via virtual outputs include square-wave and pulse-width
//! modulation, in both continuous and single-shot modes. Supported measurement operations include
//! event counting, frequency measurement, and interval (pulse width measurement).
//!
//! The MC6840 occupies eight locations in the assigned address space. Offset 0 in the assigned
//! region supports writing to either CR1 or CR3 according to the configuration of CR3. All other
//! offsets address a single register on write or read.
//!
//! | Offset |  Write Operation                      |  Read Operation          |
//! |--------|---------------------------------------|--------------------------|
//! |   0    | If CR2.0 = 0 Write CR3 else Write CR1 | (no operation)           |
//! |   1    | Write CR2                             | Read Status Register     |
//! |   2    | Write MSB Buffer Register             | Read Timer 1 Counter     |
//! |   3    | Write Timer 1 Latch                   | Read LSB Buffer Register |
//! |   4    | Write MSB Buffer Register             | Read Timer 2 Counter     |
//! |   5    | Write Timer 2 Latch                   | Read LSB Buffer Register |
//! |   6    | Write MSB Buffer Register             | Read Timer 3 Counter     |
//! |   7    | Write Timer 3 Latch                   | Read LSB Buffer Register |
//!
//! To facilitate atomic writes of the 16-bit latches and atomic reads of the 16-bit counter for
//! each timer, the MC6840 uses two additional buffer registers. To write the latch for a timer,
//! the program first writes the most significant byte of the count to the MSB Buffer Register,
//! it then writes the least significant byte to the latch register offset for the target timer.
//! Upon writing the LSB, the full 16-bit value is transferred into the timer's latches. To read the
//! counter for the timer, the program first reads the offset for the subject timer's counter. This
//! read returns the most significant byte of the counter and simultaneously loads the least
//! significant byte into the LSB Buffer Register, which the program can subsequently read.
//! To simplify programming using index registers, the MSB Buffer Register and LSB Buffer Register
//! both respond to I/O requests at multiple offsets.
//!
//! Note that both 16-bit counters are organized in the address space in big-endian (MSB first)
//! order, rather than the little-endian order used by the 6502 microprocessor.
//!
//! # Virtual peripheral connections
//!
//! Virtual peripherals connect to the MC6840 PTM over byte-stream transports using the
//! [`crate::emulator::device::ptm_protocol`] message protocol. Any number of transports may
//! be attached using [`Mc6840::attach_transport`]; each undergoes an independent format
//! negotiation handshake.
//!
//! **Handshake.** The peripheral opens the connection by sending a single format-selector byte.
//! On the next [`IoDevice::tick`] call that receives it, the PTM completes the handshake and
//! immediately sends a state dump giving the peripheral the current state of the clock, gate,
//! and timer output signals before any further exchange.
//!
//! **PTM → peripheral (outgoing).** After the handshake, the PTM will transmit updates to timer
//! output signals, as well as updates to clock and gate signals (only if multiple peripherals are
//! connected) as changes occur in the state of the PTM's timers. Every attached transport that
//! has completed its handshake receives each update message from the PTM.
//!
//! **Peripheral → PTM (incoming).** The peripheral sends messages to signal negative- or
//! positive-edge transitions for any of the three clock input and three gate input signals.
//!

use super::ptm_protocol;
use super::ptm_protocol::PtmProtocolMessage;
use crate::emulator::{DeviceId, ErrorSender, IoDevice, ProtocolManager, ProtocolMessageEncoding, Transport, TransportError, transport};
use log::debug;

const T1: usize = 0;
const T2: usize = 1;
const T3: usize = 2;

const CTRL_CR1_ENABLE: u8          = 0b00000001;
const CTRL_T3_PRESCALE: u8         = 0b00000001;
const CTRL_INTERNAL_RESET: u8      = 0b00000001;

const CTRL_USE_EXTERNAL_CLOCK: u8  = 0b00000010;
const CTRL_COUNTER_DUAL_8BIT: u8   = 0b00000100;
const CTRL_IRQ_ENABLE: u8          = 0b01000000;
const CTRL_OUTPUT_ENABLE: u8       = 0b10000000;

const CTRL_MODE_MASK: u8           = 0b00111000;
const CTRL_MODE_COMPARE: u8        = 0b00001000;
const CTRL_MODE_DEFERRED_INIT: u8  = 0b00010000;
const CTRL_MODE_SINGLE_SHOT: u8    = 0b00100000;
const CTRL_MODE_PULSE_WIDTH: u8    = 0b00010000;
const CTRL_MODE_GREATER: u8        = 0b00100000;

const RESET_COUNT: u16 = 0xFFFF;

const IRQ_COMPOSITE: u8 = 0b10000000;

/// A prescaler (divider) used by Timer 3.
struct Prescaler {
    divisor: u8,
    count: u8,
}

impl Prescaler {

    fn new(divisor: u8) -> Self {
        Self {
            divisor,
            count: divisor,
        }
    }

    fn update_has_carry_out(&mut self) -> bool {
        self.count -= 1;
        let carry_out = self.count == 0;
        if carry_out {
            self.count = self.divisor;
        }
        carry_out
    }

}

/// Which transition an edge-triggered `Synchronizer` recognizes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Edge {
    Rising,
    Falling,
}

// Status of a comparison mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CompareStatus {
    Idle,
    Started,
    Stopped,
}

/// Synchronizes an asynchronous input to the E clock through a pipeline,
/// delaying either a detected falling edge (Clock/Gate) or the raw level
/// itself (Reset) by `depth` E cycles.
struct Synchronizer {
    pipeline: [bool; 4],
    // The pipeline's output as of the previous call, used to detect a
    // transition in the already-synchronized signal.
    prev_recognized: bool,
    depth: usize,
    edge_triggered: bool,
}

impl Synchronizer {

    /// Creates a new instance.
    /// ## Arguments
    /// - `depth` - 4 for Clock/Gate, 3 for Reset.
    /// - `edge_triggered` - true for Clock/Gate, false for Reset.
    ///
    fn new(depth: usize, prev_level: bool, edge_triggered: bool) -> Self {
        assert!((1..=4).contains(&depth));
        Self {
            pipeline: [prev_level; 4],
            prev_recognized: prev_level,
            depth,
            edge_triggered,
        }
    }

    /// Call once per full system clock cycle (E) with the current raw pin level.
    /// recognizing the given `edge`. For level-triggered instances (Reset),
    /// `edge` is ignored and the raw level is piped through.
    ///
    /// Returns whether the input is *recognized* on this tick, delayed by
    /// `depth` E cycles from the underlying transition/level:
    /// - edge_triggered: a one-cycle pulse, true only on the tick that the selected
    ///   edge finishes propagating through the pipeline.
    /// - level-triggered: tracks the raw level itself, delayed by
    ///   `depth` cycles — true for as long as the (delayed) level is
    ///   asserted, not just a single pulse.
    ///
    fn sample_for(&mut self, level: bool, edge: Edge) -> bool {
        // Read the pipeline's current output before shifting
        let recognized_level = self.pipeline[self.depth - 1];
        // Push in the new level
        for i in (1..self.depth).rev() {
            self.pipeline[i] = self.pipeline[i - 1];
        }
        self.pipeline[0] = level;

        // determine whether we need to detect an edge or simply report a level
        let result = if self.edge_triggered {
            match edge {
                Edge::Falling => self.prev_recognized && !recognized_level,
                Edge::Rising => !self.prev_recognized && recognized_level,
            }
        } else {
            recognized_level
        };

        self.prev_recognized = recognized_level;
        result
    }

    /// Call once per full system clock cycle (E) with the current raw pin level.
    /// Equivalent to `sample_for(level, Edge::Falling)`.
    fn sample(&mut self, level: bool) -> bool {
        self.sample_for(level, Edge::Falling)
    }

}

/// A timer unit of the MC6840 PTM.
/// All three timers in the PTM are modeled using this struct. The optional prescaler
/// of Timer 3 is handled at the device level, separate from the timer itself, similar
/// to the design of the real hardware.
///
struct Timer {
    // count to set at each load
    latch: u16,
    // true if the latch was zero at last load; used only for 16-bit continuous countdown mode
    carry_in: bool,
    // optional prescaler (used only by T3 when prescaling mode is enabled)
    prescaler: Option<Prescaler>,
    // count remaining after last load
    counter: u16,
    // true if the gate input has been triggered (on falling edge)
    triggered: bool,
    // mode selected in the control register
    mode: u8,
    // true if this timer is clocked by the clock pin (Cx)
    external_clock: bool,
    // true if the dual 8-bit mode has been selected
    dual8bit_mode: bool,
    // true if timeouts result in an interrupt
    irq_enabled: bool,
    // true if changes to output pin (Ox) are transmitted to transport
    output_enabled: bool,
    // true if this timer wants to assert CPU's IRQ
    irq_active: bool,
    compare_status: CompareStatus,
    awaiting_edge: Edge,
    // state of the output pin (Ox)
    output_state: bool,
    // raw level of the clock pin (Cx) last received from transport
    clock_level: bool,
    // synchronizer used to model the pipeline delay of the clock input (Cx)
    clock_sync: Synchronizer,
    // raw level of the gate pin (Gx) last received from transport
    gate_level: bool,
    // synchronizer used to model the pipeline delay of the gate input (Gx)
    gate_sync: Synchronizer,
}

impl Timer {

    fn new() -> Self {
        Timer {
            latch: RESET_COUNT,
            carry_in: false,
            prescaler: None,
            counter: 0,
            triggered: false,
            mode: 0,
            external_clock: false,
            dual8bit_mode: false,
            irq_enabled: false,
            irq_active: false,
            compare_status: CompareStatus::Idle,
            awaiting_edge: Edge::Falling,
            output_enabled: false,
            output_state: false,
            clock_level: true,
            clock_sync: Synchronizer::new(4, true, true),
            gate_level: false,
            gate_sync: Synchronizer::new(4, false, true),
        }
    }

    fn is_generating(&self) -> bool {
        self.mode & CTRL_MODE_COMPARE == 0
    }

    fn is_continuous(&self) -> bool {
        self.is_generating() && self.mode & CTRL_MODE_SINGLE_SHOT == 0
    }

    fn is_single_shot(&self) -> bool {
        self.is_generating() && self.mode & CTRL_MODE_SINGLE_SHOT != 0
    }

    fn is_deferred_init(&self) -> bool {
        self.is_generating() && self.mode & CTRL_MODE_DEFERRED_INIT != 0
    }

    fn is_comparing(&self) -> bool {
        self.mode & CTRL_MODE_COMPARE != 0
    }

    fn is_frequency(&self) -> bool {
        self.is_comparing() && self.mode & CTRL_MODE_PULSE_WIDTH == 0
    }

    fn is_compare_greater(&self) -> bool {
        self.is_comparing() && self.mode & CTRL_MODE_GREATER != 0
    }

    fn set_control_register(&mut self, value: u8) {
        self.compare_status = CompareStatus::Idle;
        self.mode = value & CTRL_MODE_MASK;
        self.dual8bit_mode = value & CTRL_COUNTER_DUAL_8BIT != 0;
        self.external_clock = value & CTRL_USE_EXTERNAL_CLOCK != 0;
        self.irq_enabled = value & CTRL_IRQ_ENABLE != 0;
        self.output_enabled = value & CTRL_OUTPUT_ENABLE != 0;
        if self.dual8bit_mode {
            self.output_state = false;
        }
    }

    fn clear_irq(&mut self) {
        self.irq_active = false;
    }

    fn irq_active(&self) -> bool {
        self.irq_active
    }

    fn is_zero(&self) -> bool {
        self.counter == 0 && (self.dual8bit_mode || !self.carry_in)
    }

    fn load(&mut self) {
        self.counter = self.latch;
        self.carry_in = self.latch == 0 && self.is_continuous();
        if self.dual8bit_mode && self.latch != 0 {
            self.output_state = false;
        }
    }

    fn init(&mut self) {
        self.load();
        self.irq_active = false;
        self.output_state = false;
        self.triggered = true;
        self.compare_status = if self.is_comparing() {
            CompareStatus::Started
        } else {
            CompareStatus::Idle
        };
        self.awaiting_edge = if self.is_comparing() && !self.is_frequency() {
            Edge::Rising
        } else {
            Edge::Falling
        }
    }

    fn decrement(&mut self) {
        if self.dual8bit_mode {
            let mut lsb = (self.counter & 0xFF) as u8;
            let mut msb = (self.counter >> 8) as u8;
            if lsb == 0 {
                lsb = (self.latch & 0xFF) as u8;
                if msb > 0 {
                    msb -= 1;
                    if msb == 0 {
                        self.output_state = true;
                    }
                }
            } else {
                lsb -= 1;
            }
            self.counter = u16::from_le_bytes([lsb, msb])
        } else {
            assert!(self.is_comparing() || self.carry_in || self.counter > 0, "expected non-zero counter");
            self.counter = self.counter.wrapping_sub(1);
        }
    }

    fn clock_counter(&mut self) {
        if !self.triggered {
            return
        }
        if self.is_continuous() && self.gate_level {
            return;
        }
        if self.is_zero() && self.is_generating() {
            if self.is_continuous() {
                self.load()
            } else {
                self.triggered = false;
            }
            self.irq_active = self.irq_enabled;
            self.output_state = if self.dual8bit_mode && self.latch != 0 || self.is_single_shot() {
                false
            } else {
                !self.output_state
            }
        } else {
            if self.is_zero() && self.is_comparing() {
                self.compare_status = CompareStatus::Idle;
                self.awaiting_edge = Edge::Falling;
            }
            self.decrement();
            self.carry_in = false;
            if self.is_single_shot() {
                self.output_state = true;
            }
        }
    }

    fn clock(&mut self) {
        if self.prescaler.is_none() {
            self.clock_counter();
        } else {
            let prescaler = self.prescaler.as_mut().unwrap();
            if prescaler.update_has_carry_out() {
                self.clock_counter();
            }
        }
    }

    fn check_comparison(&mut self, edge_detected: bool) {
        let stop = if self.is_compare_greater() {
            self.counter == 0
        } else {
            edge_detected
        };
        if stop {
            self.compare_status = CompareStatus::Stopped;
            self.irq_active = self.irq_enabled;
            self.awaiting_edge = Edge::Falling;
        } else if self.is_compare_greater() && edge_detected {
            self.compare_status = CompareStatus::Idle;
        }
    }

    fn tick(&mut self) {
        let edge_detected = self.gate_sync.sample_for(self.gate_level, self.awaiting_edge);
        let gate_triggered = edge_detected && self.awaiting_edge == Edge::Falling;
        let init_compare = !self.irq_active && (
            self.compare_status != CompareStatus::Started || self.is_compare_greater());
        let init_counter = self.is_generating() || init_compare;
        if gate_triggered && init_counter {
            self.init();
        } else {
            if self.is_comparing() && self.compare_status == CompareStatus::Started && !self.irq_active() {
                self.check_comparison(edge_detected);
            }
            let clock_triggered = self.clock_sync.sample(self.clock_level);
            if !self.external_clock || clock_triggered {
                self.clock();
            }
        }
    }

}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AsyncIoState {
    clocks: [bool; 3],
    gates: [bool; 3],
    outputs: [bool; 3],
}

impl AsyncIoState {

    fn new(timers: &[Timer; 3]) -> Self {
        let mut state = AsyncIoState {
            clocks: [false; 3],
            gates: [false; 3],
            outputs: [false; 3],
        };
        for (i, timer) in timers.iter().enumerate() {
            state.clocks[i] = timer.clock_level;
            state.gates[i] = timer.gate_level;
            state.outputs[i] = timer.output_state & timer.output_enabled;
        }
        state
    }

}

pub struct Mc6840 {
    name: &'static str,
    address: u16,
    protocol: ProtocolMessageEncoding,
    protocol_manager: Option<ProtocolManager<PtmProtocolMessage>>,
    report_error: Box<dyn Fn(TransportError) + Send>,

    latched_status: u8,
    lsb_buffer: u8,
    msb_buffer: u8,
    timers: [Timer; 3],
    cr1_enabled: bool,
    reset_active: bool,
}

impl Mc6840 {

    pub fn new(name: &'static str) -> Self {
        Mc6840 {
            name,
            address: 0,
            protocol: ProtocolMessageEncoding::Ascii,
            protocol_manager: None,
            report_error: Box::new(transport::no_op_reporter()),
            latched_status: 0,
            lsb_buffer: 0,
            msb_buffer: 0,
            timers: [
                Timer::new(),   // T1
                Timer::new(),   // T2
                Timer::new(),   // T3
            ],
            cr1_enabled: false,
            reset_active: false,
        }
    }

    /// Sets the address at which this device is registered on the bus.
    pub fn with_address(mut self, address: u16) -> Self {
        self.address = address;
        self
    }

    /// Sets the protocol format to use in communication with peripherals
    pub fn with_protocol(mut self, protocol: ProtocolMessageEncoding) -> Self {
        self.protocol = protocol;
        self
    }

    /// Attaches a transport. All attached transports receive every port and control-signal
    /// state change; any number of peripherals may be connected simultaneously.
    pub fn attach_transport(&mut self, transport: Box<dyn Transport>) {
        self.protocol_manager = Some(ProtocolManager::new(self.protocol, transport,
            ptm_protocol::new_encoder, ptm_protocol::new_decoder));
    }

    /// Sets the error sender for async transport event reporting.
    pub fn set_error_sender(&mut self, sender: ErrorSender, id: DeviceId) {
        self.report_error = transport::reporter(sender, id);
    }

    fn current_state(&self) -> Vec<PtmProtocolMessage> {
        let state = AsyncIoState::new(&self.timers);
        let messages = vec![
            PtmProtocolMessage::ClockState { clocks: state.clocks },
            PtmProtocolMessage::GateState { gates: state.gates },
            PtmProtocolMessage::OutputState { outputs: state.outputs },
        ];
        messages
    }

    fn updated_state(&self, before: &AsyncIoState, after: &AsyncIoState) -> Vec<PtmProtocolMessage> {
        let mut messages = Vec::new();
        if before.clocks != after.clocks {
            messages.push(PtmProtocolMessage::ClockState { clocks: after.clocks });
        }
        if before.gates != after.gates {
            messages.push(PtmProtocolMessage::GateState { gates: after.gates });
        }
        if before.outputs != after.outputs {
            messages.push(PtmProtocolMessage::OutputState { outputs: after.outputs });
        }
        messages
    }

    fn poll_transports(&mut self) {
        if self.protocol_manager.is_some() {
            let state = self.current_state();
            loop {
                match self.protocol_manager.as_mut().unwrap().poll_transport(&state) {
                    Ok(Some(m)) => self.apply_message(m),
                    Ok(None) => break,
                    Err(e) => {
                        (self.report_error)(e);
                        break;
                    }
                }
            }
        }
    }

    fn send_state_to_all(&mut self, messages: Vec<PtmProtocolMessage>) {
        if self.protocol_manager.is_some()
                && let Err(e) = self.protocol_manager.as_mut().unwrap().send_all_to_all(&messages) {
            (self.report_error)(e);
        }
    }

    fn tick_timers(&mut self) {
        for timer_id in 0..3 {
            self.timers[timer_id].tick();
        }
    }

    fn internal_reset(&mut self) {
        for i in 0..3 {
            self.timers[i].init();
        }
    }

    fn status(&self) -> u8 {
        let mut status = 0;
        for timer_id in 0..3 {
            if self.timers[timer_id].irq_active() {
                status |= 1 << timer_id as u8
            }
        }
        if status != 0 {
            status |= IRQ_COMPOSITE
        }
        status
    }

    fn apply_message(&mut self, message: PtmProtocolMessage) {
        match message {
            PtmProtocolMessage::ClockEdge { clocks, positive} => {
                for (i, clock) in clocks.iter().enumerate() {
                    if *clock {
                        self.timers[i].clock_level = positive;
                    }
                }
            }
            PtmProtocolMessage::GateEdge { gates, positive} => {
                for (i, gate) in gates.iter().enumerate() {
                    if *gate {
                        self.timers[i].gate_level = positive;
                    }
                }
            }
            _ => ()
        }
    }

}

impl IoDevice for Mc6840 {

    fn read(&mut self, address: u16) -> u8 {
        let offset = address - self.address;
        match offset {
            1 => {
                self.latched_status = self.status();
                self.latched_status
            },
            2 | 4 | 6 => {
                let timer_id = ((offset - 2) / 2) as usize;
                let irq_mask = 1 << timer_id as u8;
                if self.latched_status & irq_mask != 0 {
                    self.timers[timer_id].clear_irq()
                }
                let counter = self.timers[timer_id].counter;
                self.lsb_buffer = (counter & 0xff) as u8;
                (counter >> 8) as u8
            }
            3 | 5 | 7 => self.lsb_buffer,
            _ => 0,
        }
    }

    fn write(&mut self, address: u16, value: u8) {
        let offset = address - self.address;
        match offset {
            0 => {
                if self.cr1_enabled {
                    self.timers[T1].set_control_register(value);
                    self.reset_active = value & CTRL_INTERNAL_RESET != 0;
                    if self.reset_active {
                        self.internal_reset();
                    }
                } else {
                    self.timers[T3].set_control_register(value);
                    self.timers[T3].prescaler = if value & CTRL_T3_PRESCALE != 0 {
                        Some(Prescaler::new(8))
                    } else {
                        None
                    };
                }
            }
            1 => {
                self.timers[T2].set_control_register(value);
                self.cr1_enabled = value & CTRL_CR1_ENABLE != 0;
            },
            2 | 4 | 6 => self.msb_buffer = value,
            3 | 5 | 7 => {
                let timer_id = ((offset - 3) / 2) as usize;
                let latched_value = u16::from_le_bytes([value, self.msb_buffer]);
                self.timers[timer_id].latch = latched_value;
                if !self.timers[timer_id].is_deferred_init() {
                    self.timers[timer_id].init();
                }
            }
            _ => (),
        }
    }

    fn peek(&self, address: u16) -> u8 {
        let offset = address - self.address;
        match offset {
            1 => self.status(),
            2 | 4 | 6 => {
                let timer_id = ((offset - 2) / 2) as usize;
                let counter = self.timers[timer_id].counter;
                (counter >> 8) as u8
            }
            3 | 5 | 7 => {
                let timer_id = ((offset - 3) / 2) as usize;
                let counter = self.timers[timer_id].counter;
                (counter & 0xFF) as u8
            }
            _ => 0,
        }
    }

    fn tick(&mut self, cycles: u32) {
        let before = AsyncIoState::new(&self.timers);
        self.poll_transports();
        if !self.reset_active {
            for _ in 0..cycles {
                self.tick_timers();
            }
        }
        let after = AsyncIoState::new(&self.timers);
        self.send_state_to_all(self.updated_state(&before, &after));
    }

    fn reset(&mut self) {
        let address = self.address;
        let protocol_manager = std::mem::take(&mut self.protocol_manager);
        let report_error = std::mem::replace(&mut self.report_error, transport::no_op_reporter());
        *self = Self::new(self.name);
        self.address = address;
        self.protocol_manager = protocol_manager;
        self.report_error = report_error;
        debug!("{} @0x{:04x} reset", self.name(), self.address);
        self.internal_reset();
        self.send_state_to_all(self.current_state());
    }

    fn irq_active(&self) -> bool {
        self.status() != 0
    }

    fn name(&self) -> &str { self.name }

}

#[cfg(test)]
mod tests {
    use super::*;

    const IRQ_TIMER_1: u8   = 0b00000001;
    const IRQ_TIMER_2: u8   = 0b00000010;
    const IRQ_TIMER_3: u8   = 0b00000100;
    const CTRL_MODE_GENERATE: u8 = 0;
    const CTRL_MODE_CONTINUOUS: u8 = 0;
    const CTRL_MODE_FREQUENCY: u8 = 0;
    const CTRL_MODE_IMMEDIATE_INIT: u8 = 0;
    const CTRL_MODE_LESS: u8 = 0;

    fn device() -> Mc6840 {
        Mc6840::new("mc6840")
    }

    #[test]
    fn read_offset_zero_is_zero() {
        let mut device = device();
        assert_eq!(device.read(0), 0);
    }

    #[test]
    fn read_status_register() {
        let mut device = device();
        device.timers[T1].irq_active = true;
        device.timers[T2].irq_active = true;
        device.timers[T3].irq_active = true;
        assert_eq!(device.read(1),
                   IRQ_COMPOSITE | IRQ_TIMER_1 | IRQ_TIMER_2 | IRQ_TIMER_3);
    }

    #[test]
    fn read_lsb_registers() {
        let mut device = device();
        for i in 0..3 {
            device.lsb_buffer = i as u8;
            assert_eq!(device.read(3 + 2 * i), i as u8);
        }
    }

    #[test]
    fn read_counter_registers() {
        let mut device = device();
        for i in 0..3 {
            device.timers[i].counter = 0x55AA;
            device.lsb_buffer = 0;
            assert_eq!(device.read((2 + 2 * i) as u16), 0x55);
            assert_eq!(device.read((3 + 2 * i) as u16), 0xAA);
        }
    }

    #[test]
    fn read_counter_resets_interrupt_when_latched() {
        let mut device = device();
        device.timers[T2].irq_active = true;
        assert_eq!(device.read(1), IRQ_COMPOSITE | IRQ_TIMER_2);
        device.read(2 + 2 * (T2 as u16));
        assert!(!device.timers[T2].irq_active, "expected IRQ not active");
    }

    #[test]
    fn read_counter_retains_interrupt_when_not_latched() {
        let mut device = device();
        device.timers[T2].irq_active = true;
        assert!(device.irq_active(), "expected IRQ active");
        // no read of status register -> no latched status
        device.read(2 + 2 * (T2 as u16));
        assert!(device.timers[T2].irq_active, "expected IRQ active");
    }

    #[test]
    fn write_t1_control_register() {
        let mut device = device();
        device.write(1, 0x01);
        assert!(device.cr1_enabled, "expected CR1 enabled");
        device.write(0, 0xff & !CTRL_INTERNAL_RESET);
        assert!(!device.reset_active, "expected reset not active");
        assert!(device.timers[T1].external_clock, "expected external clock");
        assert_eq!(device.timers[T1].mode, 0xff & CTRL_MODE_MASK);
        assert!(device.timers[T1].irq_enabled, "expected IRQ enabled");
        assert!(device.timers[T1].output_enabled, "expected output enabled");
    }

    #[test]
    fn write_t1_control_register_internal_reset() {
        let mut device = device();
        device.timers[T2].irq_active = true;
        device.timers[T2].output_state = true;
        assert_eq!(device.timers[T2].latch, 0xFFFF);
        assert_eq!(device.timers[T2].counter, 0);
        device.write(1, CTRL_CR1_ENABLE);
        device.write(0, CTRL_INTERNAL_RESET);
        assert!(device.reset_active, "expected reset active");
        assert_eq!(device.timers[T2].counter, 0xFFFF);
        assert!(!device.timers[T2].irq_active, "expected IRQ not active");
        assert!(!device.timers[T2].output_state, "expected output state low");
        device.write(0, 0);
        assert!(!device.reset_active, "expected reset not active");
    }

    #[test]
    fn write_t2_control_register() {
        let mut device = device();
        device.write(1, 0xff);
        assert!(device.cr1_enabled, "expected CR1 enabled");
        assert!(device.timers[T2].external_clock, "expected external clock");
        assert_eq!(device.timers[T2].mode, 0xff & CTRL_MODE_MASK);
        assert!(device.timers[T2].irq_enabled, "expected IRQ enabled");
        assert!(device.timers[T2].output_enabled, "expected output enabled");
    }

    #[test]
    fn write_t3_control_register() {
        let mut device = device();
        assert!(!device.cr1_enabled, "expected CR3 enabled");
        device.write(0, 0xff & !CTRL_T3_PRESCALE);
        assert!(matches!(device.timers[T3].prescaler, None), "expected no prescaler");
        assert!(device.timers[T3].external_clock, "expected external clock");
        assert_eq!(device.timers[T3].mode, 0xff & CTRL_MODE_MASK);
        assert!(device.timers[T3].irq_enabled, "expected IRQ enabled");
        assert!(device.timers[T3].output_enabled, "expected output enabled");
    }

    #[test]
    fn write_t3_control_register_with_prescaler() {
        let mut device = device();
        device.write(0, CTRL_T3_PRESCALE);
        assert!(matches!(device.timers[T3].prescaler, Some(_)), "expected prescaler");
    }

    #[test]
    fn write_msb_registers() {
        let mut device = device();
        for i in 0..3 {
            device.write(2 * i + 2, i as u8);
            assert_eq!(device.msb_buffer, i as u8);
        }
    }

    #[test]
    fn write_latch_registers() {
        let mut device = device();
        for i in 0..3 {
            device.write(2 * i + 2, 0x55);
            device.write(2 * i + 3, 0xAA);
            assert_eq!(device.timers[i as usize].latch, u16::from_le_bytes([0xAA, 0x55]));
        }
    }

    #[test]
    fn write_latch_immediate_load() {
        let mut device = device();
        device.write(1, CTRL_MODE_GENERATE | CTRL_MODE_CONTINUOUS | CTRL_MODE_IMMEDIATE_INIT);
        device.write(2 + 2 * (T2 as u16), 0x55);
        device.write(3 + 2 * (T2 as u16), 0xAA);
        assert_eq!(device.timers[T2].latch, 0x55AA);
        assert_eq!(device.timers[T2].counter, 0x55AA);
    }

    #[test]
    fn write_latch_deferred_init() {
        let mut device = device();
        device.timers[T2].counter = 0;
        device.write(1, CTRL_MODE_GENERATE | CTRL_MODE_CONTINUOUS | CTRL_MODE_DEFERRED_INIT);
        device.write(2 + 2 * (T2 as u16), 0x55);
        device.write(3 + 2 * (T2 as u16), 0xAA);
        assert_eq!(device.timers[T2].latch, 0x55AA);
        assert_eq!(device.timers[T2].counter, 0);
    }

    #[test]
    fn write_mode_generate_continuous_immediate_init() {
        let mut device = device();
        device.write(1, CTRL_MODE_GENERATE | CTRL_MODE_CONTINUOUS | CTRL_MODE_IMMEDIATE_INIT);
        assert!(device.timers[T2].is_continuous());
        assert!(!device.timers[T2].is_deferred_init());
    }

    #[test]
    fn write_mode_compare_frequency_less() {
        let mut device = device();
        device.write(1, CTRL_MODE_COMPARE | CTRL_MODE_FREQUENCY | CTRL_MODE_LESS);
        assert!(device.timers[T2].is_frequency());
        assert!(!device.timers[T2].is_compare_greater());
    }

    #[test]
    fn write_mode_generate_continuous_deferred_init() {
        let mut device = device();
        device.write(1, CTRL_MODE_GENERATE | CTRL_MODE_CONTINUOUS | CTRL_MODE_DEFERRED_INIT);
        assert!(device.timers[T2].is_continuous());
        assert!(device.timers[T2].is_deferred_init());
    }

    #[test]
    fn write_mode_compare_pulse_width_less() {
        let mut device = device();
        device.write(1, CTRL_MODE_COMPARE | CTRL_MODE_PULSE_WIDTH | CTRL_MODE_LESS);
        assert!(device.timers[T2].is_comparing());
        assert!(!device.timers[T2].is_frequency());
        assert!(!device.timers[T2].is_compare_greater());
    }

    #[test]
    fn write_mode_generate_single_shot_immediate_init() {
        let mut device = device();
        device.write(1, CTRL_MODE_GENERATE | CTRL_MODE_SINGLE_SHOT | CTRL_MODE_IMMEDIATE_INIT);
        assert!(device.timers[T2].is_single_shot());
        assert!(!device.timers[T2].is_deferred_init());
    }

    #[test]
    fn write_mode_compare_frequency_greater() {
        let mut device = device();
        device.write(1, CTRL_MODE_COMPARE | CTRL_MODE_FREQUENCY | CTRL_MODE_GREATER);
        assert!(device.timers[T2].is_frequency());
        assert!(device.timers[T2].is_compare_greater());
    }

    #[test]
    fn write_mode_generate_single_shot_deferred_init() {
        let mut device = device();
        device.write(1, CTRL_MODE_GENERATE | CTRL_MODE_SINGLE_SHOT | CTRL_MODE_DEFERRED_INIT);
        assert!(device.timers[T2].is_single_shot());
        assert!(device.timers[T2].is_deferred_init());
    }

    #[test]
    fn write_mode_compare_pulse_width_greater() {
        let mut device = device();
        device.write(1, CTRL_MODE_COMPARE | CTRL_MODE_PULSE_WIDTH | CTRL_MODE_GREATER);
        assert!(device.timers[T2].is_comparing());
        assert!(!device.timers[T2].is_frequency());
        assert!(device.timers[T2].is_compare_greater());
    }

    #[test]
    fn peek_offset_zero_is_zero() {
        let device = device();
        assert_eq!(device.peek(0), 0);
    }

    #[test]
    fn peek_status_register() {
        let mut device = device();
        device.timers[T1].irq_active = true;
        device.timers[T2].irq_active = true;
        device.timers[T3].irq_active = true;
        assert_eq!(device.read(1),
                   IRQ_COMPOSITE | IRQ_TIMER_1 | IRQ_TIMER_2 | IRQ_TIMER_3);
    }

    #[test]
    fn peek_counter_registers() {
        let mut device = device();
        for i in 0..3 {
            device.timers[i].counter = 0x55AA;
            device.lsb_buffer = 0;
            assert_eq!(device.peek((2 + 2 * i) as u16), 0x55);
            assert_eq!(device.peek((3 + 2 * i) as u16), 0xAA);
        }
    }

    #[test]
    fn prescaler_update() {
        let mut prescaler = Prescaler::new(2);
        assert!(!prescaler.update_has_carry_out(), "expected no carry out");
        assert!(prescaler.update_has_carry_out(), "expected carry out");
        assert_eq!(prescaler.count, prescaler.divisor);
    }

    #[test]
    fn timer_tick_in_generate_continuous_normal_count_mode() {
        let mut timer = Timer::new();
        timer.irq_enabled = true;
        assert!(!timer.output_state, "expected output state low");
        timer.mode = CTRL_MODE_GENERATE | CTRL_MODE_CONTINUOUS | CTRL_MODE_IMMEDIATE_INIT;
        timer.latch = 1;
        timer.init();
        // first tick should decrement counter
        timer.tick();
        assert_eq!(timer.counter, 0);
        // second tick should set output state high, signal interrupt, and reload counter from latch
        timer.tick();
        assert_eq!(timer.counter, timer.latch);
        assert!(timer.output_state, "expected output state high");
        assert!(timer.irq_active, "expected IRQ active");
    }

    #[test]
    fn timer_tick_in_generate_single_shot_normal_count_mode() {
        let mut timer = Timer::new();
        timer.irq_enabled = true;
        assert!(!timer.output_state, "expected output state low");
        timer.mode = CTRL_MODE_GENERATE | CTRL_MODE_SINGLE_SHOT | CTRL_MODE_IMMEDIATE_INIT;
        timer.latch = 1;
        timer.init();
        // first tick should decrement counter and set the output high
        timer.tick();
        assert_eq!(timer.counter, 0);
        assert!(timer.output_state, "expected output state high");
        // second tick should set output state low, signal interrupt; counter remains at zero
        timer.tick();
        assert!(!timer.output_state, "expected output state low");
        assert!(timer.irq_active, "expected IRQ active");
        assert_eq!(timer.counter, 0);
    }

    #[test]
    fn timer_tick_in_generate_continuous_dual8bit_count_mode() {
        let mut timer = Timer::new();
        timer.dual8bit_mode = true;
        timer.irq_enabled = true;
        timer.output_state = true;
        timer.mode = CTRL_MODE_GENERATE | CTRL_MODE_CONTINUOUS | CTRL_MODE_IMMEDIATE_INIT;
        timer.latch = 0x0101;
        timer.init();
        assert!(!timer.output_state, "expected output state low");
        assert_eq!(timer.counter, timer.latch);
        // first tick should zero counter LSB
        timer.tick();
        assert_eq!(timer.counter, 0x0100);
        // second tick should zero counter MSB, reload counter LSB, and set output state high
        timer.tick();
        assert_eq!(timer.counter, 0x0001);
        assert!(timer.output_state, "expected output state high");
        // third tick should zero counter LSB
        timer.tick();
        assert_eq!(timer.counter, 0x0000);
        // fourth tick should set output state low, signal interrupt, reload counter from latch
        timer.tick();
        assert!(!timer.output_state, "expected output state low");
        assert!(timer.irq_active, "expected IRQ active");
        assert_eq!(timer.counter, timer.latch);
    }

    #[test]
    fn timer_tick_in_generate_single_shot_dual8bit_count_mode() {
        let mut timer = Timer::new();
        timer.dual8bit_mode = true;
        timer.irq_enabled = true;
        timer.output_state = true;
        timer.mode = CTRL_MODE_GENERATE | CTRL_MODE_SINGLE_SHOT | CTRL_MODE_IMMEDIATE_INIT;
        timer.latch = 0x0101;
        timer.init();
        assert!(!timer.output_state, "expected output state low");
        assert_eq!(timer.counter, timer.latch);
        // first tick should zero counter LSB
        timer.tick();
        assert_eq!(timer.counter, 0x0100);
        // second tick should zero counter MSB, reload counter LSB, and set output state high
        timer.tick();
        assert_eq!(timer.counter, 0x0001);
        assert!(timer.output_state, "expected output state high");
        // third tick should zero counter LSB
        timer.tick();
        assert_eq!(timer.counter, 0x0000);
        // fourth tick should set output state low, signal interrupt; counter remains at zero
        timer.tick();
        assert!(!timer.output_state, "expected output state low");
        assert!(timer.irq_active, "expected IRQ active");
        assert_eq!(timer.counter, 0);
    }

    #[test]
    fn timer_tick_in_generate_mode_normal_count_with_latch_zero() {
        let mut timer = Timer::new();
        timer.mode = CTRL_MODE_GENERATE | CTRL_MODE_CONTINUOUS | CTRL_MODE_IMMEDIATE_INIT;
        timer.latch = 0;
        timer.irq_enabled = true;
        timer.init();
        assert!(!timer.is_zero(), "expected timer not zero");
        timer.tick();
        assert_eq!(timer.counter, 0xFFFF);
        let mut count = 1;
        while !timer.is_zero() {
            timer.tick();
            count += 1;
        }
        assert_eq!(count, 65536);
        // count is now N; should need N+1 to trigger IRQ
        assert!(!timer.irq_active, "expected IRQ not active");
        timer.tick();
        assert!(timer.irq_active, "expected IRQ active");
    }

    #[test]
    fn timer_tick_in_generate_mode_dual8bit_count_with_latch_zero() {
        let mut timer = Timer::new();
        timer.mode = CTRL_MODE_GENERATE | CTRL_MODE_CONTINUOUS | CTRL_MODE_IMMEDIATE_INIT;
        timer.dual8bit_mode = true;
        timer.latch = 0;
        timer.init();
        let output_state = timer.output_state;
        timer.tick();
        assert_ne!(timer.output_state, output_state);
        timer.tick();
        assert_eq!(timer.output_state, output_state);
        timer.tick();
        assert_ne!(timer.output_state, output_state);
    }

    #[test]
    fn timer_tick_in_generate_mode_single_shot_normal_count_deferred_init() {
        let mut timer = Timer::new();
        timer.mode = CTRL_MODE_GENERATE | CTRL_MODE_SINGLE_SHOT | CTRL_MODE_DEFERRED_INIT;
        timer.latch = 1;
        timer.gate_level = true;
        // when the timer is initialized in software, it counts down once
        timer.init();
        assert_eq!(timer.counter, 1);
        timer.tick();
        assert_eq!(timer.counter, 0);
        // it's single-shot, so it shouldn't reload
        timer.tick();
        assert_eq!(timer.counter, 0);
        // next tick clocks in the falling edge of the gate
        timer.gate_level = false;
        timer.tick();
        // need four ticks to recognize falling edge of gate; loop gets the first three
        for _ in 0..3 {
            timer.tick();
            assert_eq!(timer.counter, 0);
        }
        // falling edge of gate should be recognized on fourth tick
        // so timer should initialize and count down
        timer.tick();
        assert_eq!(timer.counter, 1);
        timer.tick();
        assert_eq!(timer.counter, 0);
        // since gate is edge-triggered, additional ticks won't initialize timer even
        // with gate held low
        for _ in 0..3 {
            timer.tick();
            assert_eq!(timer.counter, 0);
        }
        timer.gate_level = true;
        timer.tick();
        // next tick clocks in the falling edge of the gate
        timer.gate_level = false;
        timer.tick();
        // need four ticks to recognize falling edge of gate; loop gets the first three
        for _ in 0..3 {
            timer.tick();
            assert_eq!(timer.counter, 0);
        }
        // falling edge of gate should be recognized on fourth tick
        // so timer should initialize and count down
        timer.tick();
        assert_eq!(timer.counter, 1);
        timer.tick();
        assert_eq!(timer.counter, 0);
    }

    #[test]
    fn synchronizer_latency_edge_triggered() {
        let mut sync = Synchronizer::new(1, true, true);
        // put the edge in the pipeline
        assert!(!sync.sample(false));
        // edge emerges from the pipeline
        assert!(sync.sample(false));
        assert!(!sync.sample(true));
        assert!(!sync.sample(true));
        assert!(!sync.sample(true));
        assert!(!sync.sample(false));
        assert!(sync.sample(true));
    }

    #[test]
    fn synchronizer_latency_level_triggered() {
        let mut sync = Synchronizer::new(1, true, false);
        // first clock puts the new level in the pipeline
        assert!(sync.sample(false));
        // level emerges from the pipeline on the next clock
        assert!(!sync.sample(false));
    }

    fn timer_with_external_clock() -> Timer {
        let mut timer = Timer::new();
        timer.mode = CTRL_MODE_GENERATE | CTRL_MODE_SINGLE_SHOT | CTRL_MODE_DEFERRED_INIT;
        timer.external_clock = true;
        timer.latch = 1;
        timer
    }

    #[test]
    fn external_clock_updates_timer_after_pipeline_delay() {
        let mut timer = timer_with_external_clock();
        timer.init();
        // E clocks in the external clock edge
        timer.clock_level = false;
        timer.tick();
        timer.clock_level = true;
        // need four more E clocks to recognize the external clock edge
        timer.tick();
        assert_eq!(timer.counter, 1);
        timer.tick();
        assert_eq!(timer.counter, 1);
        timer.tick();
        assert_eq!(timer.counter, 1);
        timer.tick();
        // previous E clock should have decremented the counter
        assert_eq!(timer.counter, 0);
        // subsequent E clocks shouldn't change the timer
        timer.tick();
        assert_eq!(timer.counter, 0);
        timer.tick();
        assert_eq!(timer.counter, 0);
    }

    #[test]
    fn compare_frequency_lt_when_lt() {
        let mut timer = Timer::new();
        timer.irq_enabled = true;
        timer.set_control_register(CTRL_IRQ_ENABLE | CTRL_MODE_COMPARE | CTRL_MODE_FREQUENCY | CTRL_MODE_LESS);
        timer.latch = 3;
        timer.gate_level = true;  timer.tick();     // ?, ?, ?, H
        timer.gate_level = false; timer.tick();     // ?, ?, H, L
        timer.gate_level = true;  timer.tick();     // ?, H, L, H
        timer.gate_level = false; timer.tick();     // H, L, H, L
        timer.gate_level = true;  timer.tick();     // L, H, L, H
        assert_eq!(timer.compare_status, CompareStatus::Idle);
        timer.gate_level = true;  timer.tick();     // H, L, H, H (falling edge detected)
        // falling edge should init counter to start measurement
        assert_eq!(timer.compare_status, CompareStatus::Started);
        assert_eq!(timer.counter, 3);
        timer.gate_level = true;  timer.tick();     // L, H, H, H
        assert_eq!(timer.counter, 2);
        timer.gate_level = true;  timer.tick();     // H, H, H, H (falling edge detected)
        // not timeout => compare satisfied, raise interrupt
        assert_eq!(timer.compare_status, CompareStatus::Stopped);
        assert_eq!(timer.counter, 1);
        assert!(timer.irq_active());
        // timer should continue to decrement
        timer.gate_level = false; timer.tick();     // H, H, H, L
        assert_eq!(timer.counter, 0);
        timer.clear_irq();
        timer.gate_level = true;  timer.tick();     // H, H, L, H
        // even after IRQ cleared must continue to decrement
        // until the next negative edge of the gate is recognized
        assert_eq!(timer.counter, 0xFFFF);
        timer.gate_level = true;  timer.tick();     // H, L, H, H
        assert_eq!(timer.counter, 0xFFFE);
        timer.gate_level = true;  timer.tick();     // L, H, H, H
        assert_eq!(timer.counter, 0xFFFD);
        timer.gate_level = true;  timer.tick();     // H, H, H, H (falling edge detected)
        // falling edge should init counter to start measurement
        assert_eq!(timer.counter, 3);
        assert_eq!(timer.compare_status, CompareStatus::Started);
    }

    #[test]
    fn compare_frequency_lt_when_ge() {
        let mut timer = Timer::new();
        timer.irq_enabled = true;
        timer.set_control_register(CTRL_IRQ_ENABLE | CTRL_MODE_COMPARE | CTRL_MODE_FREQUENCY | CTRL_MODE_LESS);
        timer.latch = 3;
        timer.gate_level = true;  timer.tick();     // ?, ?, ?, H
        timer.gate_level = false; timer.tick();     // ?, ?, H, L
        timer.gate_level = true;  timer.tick();     // ?, H, L, H
        timer.gate_level = true;  timer.tick();     // H, L, H, H
        timer.gate_level = true;  timer.tick();     // L, H, H, H
        assert_eq!(timer.compare_status, CompareStatus::Idle);
        timer.gate_level = true;  timer.tick();     // H, H, H, H (falling edge detected)
        // falling edge should init counter to start measurement
        assert_eq!(timer.compare_status, CompareStatus::Started);
        assert_eq!(timer.counter, 3);
        timer.gate_level = false; timer.tick();     // H, H, H, L
        assert_eq!(timer.counter, 2);
        timer.gate_level = true;  timer.tick();     // H, H, L, H
        assert_eq!(timer.counter, 1);
        timer.gate_level = true;  timer.tick();     // H, L, H, H
        assert_eq!(timer.counter, 0);
        timer.gate_level = true;  timer.tick();     // L, H, H, H
        assert_eq!(timer.compare_status, CompareStatus::Idle);
        assert_eq!(timer.counter, 0xFFFF);
        assert!(!timer.irq_active());
        timer.gate_level = true;  timer.tick();     // H, H, H, H (falling edge detected)
        // falling edge should init counter to start measurement
        assert_eq!(timer.counter, 3);
        assert_eq!(timer.compare_status, CompareStatus::Started);
    }

    #[test]
    fn compare_frequency_gt_when_gt() {
        let mut timer = Timer::new();
        timer.irq_enabled = true;
        timer.set_control_register(CTRL_IRQ_ENABLE | CTRL_MODE_COMPARE | CTRL_MODE_FREQUENCY | CTRL_MODE_GREATER);
        timer.latch = 3;
        timer.gate_level = true;  timer.tick();     // ?, ?, ?, H
        timer.gate_level = false; timer.tick();     // ?, ?, H, L
        timer.gate_level = true;  timer.tick();     // ?, H, L, H
        timer.gate_level = true;  timer.tick();     // H, L, H, H
        timer.gate_level = true;  timer.tick();     // L, H, H, H
        assert_eq!(timer.compare_status, CompareStatus::Idle);
        timer.gate_level = true;  timer.tick();     // H, H, H, H (falling edge detected)
        // falling edge should init counter to start measurement
        assert_eq!(timer.compare_status, CompareStatus::Started);
        assert_eq!(timer.counter, 3);
        timer.gate_level = false; timer.tick();     // H, H, H, L
        assert_eq!(timer.counter, 2);
        timer.gate_level = true;  timer.tick();     // H, H, L, H
        assert_eq!(timer.counter, 1);
        timer.gate_level = false; timer.tick();     // H, L, H, L
        assert_eq!(timer.counter, 0);
        timer.gate_level = true;  timer.tick();     // L, H, L, H
        assert_eq!(timer.counter, 0xFFFF);
        assert_eq!(timer.compare_status, CompareStatus::Idle);
        assert!(timer.irq_active());
        timer.gate_level = true;  timer.tick();     // H, L, H, H (falling edge detected)
        // even after IRQ cleared must continue to decrement
        // until the next negative edge of the gate is recognized
        assert_eq!(timer.counter, 0xFFFE);
        timer.clear_irq();
        timer.gate_level = true;  timer.tick();     // L, H, H, H
        assert_eq!(timer.counter, 0xFFFD);
        timer.gate_level = true;  timer.tick();     // H, H, H, H (falling edge detected)
        // falling edge should init timer for measurement
        assert_eq!(timer.counter, 3);
        assert_eq!(timer.compare_status, CompareStatus::Started);
    }

    #[test]
    fn compare_frequency_gt_when_le() {
        let mut timer = Timer::new();
        timer.irq_enabled = true;
        timer.set_control_register(CTRL_IRQ_ENABLE | CTRL_MODE_COMPARE | CTRL_MODE_FREQUENCY | CTRL_MODE_GREATER);
        timer.latch = 3;
        timer.gate_level = true;  timer.tick();     // ?, ?, ?, H
        timer.gate_level = false; timer.tick();     // ?, ?, H, L
        timer.gate_level = true;  timer.tick();     // ?, H, L, H
        timer.gate_level = true;  timer.tick();     // H, L, H, H
        timer.gate_level = false;  timer.tick();    // L, H, H, L
        assert_eq!(timer.compare_status, CompareStatus::Idle);
        timer.gate_level = true;  timer.tick();     // H, H, L, H (falling edge detected)
        // falling edge should init counter to start measurement
        assert_eq!(timer.compare_status, CompareStatus::Started);
        assert_eq!(timer.counter, 3);
        timer.gate_level = false; timer.tick();     // H, L, H, L
        assert_eq!(timer.counter, 2);
        timer.gate_level = true;  timer.tick();     // L, H, L, H
        assert_eq!(timer.counter, 1);
        timer.gate_level = true;  timer.tick();     // H, L, H, H (falling edge detected)
        // falling edge before timeout should simply reset for another measurement
        assert_eq!(timer.counter, 3);
        assert_eq!(timer.compare_status, CompareStatus::Started);
        assert!(!timer.irq_active());
    }

    #[test]
    fn compare_pulse_width_lt_when_lt() {
        let mut timer = Timer::new();
        timer.irq_enabled = true;
        timer.set_control_register(CTRL_IRQ_ENABLE | CTRL_MODE_COMPARE | CTRL_MODE_PULSE_WIDTH | CTRL_MODE_LESS);
        timer.latch = 4;
        timer.gate_level = true;  timer.tick();     // ?, ?, ?, H
        timer.gate_level = false; timer.tick();     // ?, ?, H, L
        timer.gate_level = false; timer.tick();     // ?, H, L, L
        timer.gate_level = false; timer.tick();     // H, L, L, L
        timer.gate_level = true;  timer.tick();     // L, L, L, H
        assert_eq!(timer.compare_status, CompareStatus::Idle);
        timer.gate_level = true;  timer.tick();     // L, L, H, H (falling edge detected)
        // falling edge should init counter for measurement
        assert_eq!(timer.compare_status, CompareStatus::Started);
        assert_eq!(timer.counter, 4);
        timer.gate_level = false; timer.tick();     // L, H, H, L
        assert_eq!(timer.counter, 3);
        timer.gate_level = false; timer.tick();     // H, H, L, L
        assert_eq!(timer.counter, 2);
        timer.gate_level = false; timer.tick();     // H, L, L, L (rising edge detected)
        // not timeout => compare satisfied, raise interrupt
        assert_eq!(timer.counter, 1);
        assert_eq!(timer.compare_status, CompareStatus::Stopped);
        assert!(timer.irq_active());
        timer.gate_level = false; timer.tick();     // L, L, L, L
        // timer should continue to decrement
        timer.clear_irq();
        assert_eq!(timer.counter, 0);
        timer.gate_level = false; timer.tick();     // L, L, L, L (falling edge detected)
        // falling edge should init counter for measurement
        assert_eq!(timer.counter, 4);
        assert_eq!(timer.compare_status, CompareStatus::Started);
    }

    #[test]
    fn compare_pulse_width_lt_when_ge() {
        let mut timer = Timer::new();
        timer.irq_enabled = true;
        timer.set_control_register(CTRL_IRQ_ENABLE | CTRL_MODE_COMPARE | CTRL_MODE_PULSE_WIDTH | CTRL_MODE_LESS);
        timer.latch = 2;
        timer.gate_level = true;  timer.tick();     // ?, ?, ?, H
        timer.gate_level = false; timer.tick();     // ?, ?, H, L
        timer.gate_level = false; timer.tick();     // ?, H, L, L
        timer.gate_level = false; timer.tick();     // H, L, L, L
        timer.gate_level = false;  timer.tick();    // L, L, L, L
        assert_eq!(timer.compare_status, CompareStatus::Idle);
        timer.gate_level = true;  timer.tick();     // L, L, L, H (falling edge detected)
        // falling edge should init counter for measurement
        assert_eq!(timer.compare_status, CompareStatus::Started);
        assert_eq!(timer.counter, 2);
        timer.gate_level = false; timer.tick();     // L, L, H, L
        assert_eq!(timer.counter, 1);
        timer.gate_level = false; timer.tick();     // L, H, L, L
        assert_eq!(timer.counter, 0);
        timer.gate_level = false; timer.tick();     // H, L, L, L
        assert_eq!(timer.compare_status, CompareStatus::Idle);
        assert!(!timer.irq_active());
        assert_eq!(timer.counter, 0xFFFF);
        timer.gate_level = false; timer.tick();     // L, L, L, L (rising edge detected)
        // rising edge doesn't init counter
        assert_eq!(timer.counter, 0xFFFE);
        timer.gate_level = false; timer.tick();     // L, L, L, L (falling edge detected)
        // falling edge should init counter for measurement
        assert_eq!(timer.counter, 2);
        assert_eq!(timer.compare_status, CompareStatus::Started);
    }

    #[test]
    fn compare_pulse_width_gt_when_gt() {
        let mut timer = Timer::new();
        timer.irq_enabled = true;
        timer.set_control_register(CTRL_IRQ_ENABLE | CTRL_MODE_COMPARE | CTRL_MODE_PULSE_WIDTH | CTRL_MODE_GREATER);
        timer.latch = 3;
        timer.gate_level = true;  timer.tick();     // ?, ?, ?, H
        timer.gate_level = false; timer.tick();     // ?, ?, H, L
        timer.gate_level = false; timer.tick();     // ?, H, L, L
        timer.gate_level = false; timer.tick();     // H, L, L, L
        timer.gate_level = false; timer.tick();     // L, L, L, L
        assert_eq!(timer.compare_status, CompareStatus::Idle);
        timer.gate_level = false; timer.tick();     // L, L, L, L (falling edge detected)
        // falling edge should init counter for measurement
        assert_eq!(timer.compare_status, CompareStatus::Started);
        assert_eq!(timer.counter, 3);
        timer.gate_level = true;  timer.tick();     // L, L, L, H
        assert_eq!(timer.counter, 2);
        timer.gate_level = false; timer.tick();     // L, L, H, L
        assert_eq!(timer.counter, 1);
        timer.gate_level = true;  timer.tick();     // L, H, L, H
        assert_eq!(timer.counter, 0);
        timer.gate_level = true;  timer.tick();     // H, L, H, H (rising edge detected)
        assert_eq!(timer.counter, 0xFFFF);
        assert_eq!(timer.compare_status, CompareStatus::Idle);
        assert!(timer.irq_active());
        timer.gate_level = true;  timer.tick();     // L, H, H, H
        assert_eq!(timer.counter, 0xFFFE);
        timer.clear_irq();
        timer.gate_level = true;  timer.tick();     // H, H, H, H (falling edge detected)
        assert_eq!(timer.counter, 3);
        assert_eq!(timer.compare_status, CompareStatus::Started);
    }

    #[test]
    fn compare_pulse_width_gt_when_le() {
        let mut timer = Timer::new();
        timer.irq_enabled = true;
        timer.set_control_register(CTRL_IRQ_ENABLE | CTRL_MODE_COMPARE | CTRL_MODE_PULSE_WIDTH | CTRL_MODE_GREATER);
        timer.latch = 3;
        timer.gate_level = true;  timer.tick();     // ?, ?, ?, H
        timer.gate_level = false; timer.tick();     // ?, ?, H, L
        timer.gate_level = false; timer.tick();     // ?, H, L, L
        timer.gate_level = false; timer.tick();     // H, L, L, L
        timer.gate_level = true;  timer.tick();     // L, L, L, H
        assert_eq!(timer.compare_status, CompareStatus::Idle);
        timer.gate_level = true;  timer.tick();     // L, L, H, H (falling edge detected)
        // falling edge should init counter for measurement
        assert_eq!(timer.compare_status, CompareStatus::Started);
        assert_eq!(timer.counter, 3);
        timer.gate_level = true;  timer.tick();     // L, H, H, H
        assert_eq!(timer.counter, 2);
        timer.gate_level = false; timer.tick();     // H, H, H, L
        assert_eq!(timer.counter, 1);
        timer.gate_level = true;  timer.tick();     // H, H, L, H (rising edge detected)
        assert_eq!(timer.counter, 0);
        assert_eq!(timer.compare_status, CompareStatus::Idle);
        assert!(!timer.irq_active());
        timer.gate_level = true;  timer.tick();     // H, L, H, H
        assert_eq!(timer.counter, 0xFFFF);
        timer.gate_level = true;  timer.tick();     // L, H, H, H
        assert_eq!(timer.counter, 0xFFFE);
        timer.gate_level = true;  timer.tick();     // H, H, H, H (falling edge detected)
        assert_eq!(timer.counter, 3);
        assert_eq!(timer.compare_status, CompareStatus::Started);
    }

}
