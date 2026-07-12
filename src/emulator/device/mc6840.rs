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

use log::debug;
use crate::emulator::{DeviceId, ErrorSender, IoDevice, Transport, TransportError, PtmProtocolDecoder, PtmProtocolEncoder, PtmProtocolFormat, PtmProtocolMessage};

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

const CTRL_MODE_COMPARE: u8        = 0b00001000;
const CTRL_MODE_DEFERRED_LOAD: u8  = 0b00010000;
const CTRL_MODE_SINGLE_SHOT: u8    = 0b00100000;
const CTRL_MODE_PULSE_WIDTH: u8    = 0b00010000;
const CTRL_MODE_GREATER: u8        = 0b00100000;

const RESET_COUNT: u16 = 0xFFFF;

const IRQ_COMPOSITE: u8 = 0b10000000;

enum Measure {
    Frequency,
    PulseWidth,
}

enum Mode {
    Generating { single_shot: bool, deferred_load: bool },
    Comparing { measure: Measure, is_greater: bool },
}

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


struct Timer {
    latch: u16,
    carry_in: bool,
    counter: u16,
    mode: Mode,
    external_clock: bool,
    dual8bit_mode: bool,
    irq_enabled: bool,
    irq_active: bool,
    output_enabled: bool,
    output_state: bool,
    clock_input: bool,
    gate_input: bool,
}

impl Timer {

    fn new() -> Self {
        Timer {
            latch: RESET_COUNT,
            carry_in: false,
            counter: 0,
            mode: Mode::Generating { single_shot: false, deferred_load: false },
            external_clock: false,
            dual8bit_mode: false,
            irq_enabled: false,
            irq_active: false,
            output_enabled: false,
            output_state: false,
            clock_input: false,
            gate_input: true,
        }
    }

    fn is_zero(&self) -> bool {
        self.counter == 0 && (self.dual8bit_mode || !self.carry_in)
    }

    fn load(&mut self) {
        self.counter = self.latch;
        self.carry_in = self.counter == 0;
        if self.dual8bit_mode && self.latch != 0 {
            self.output_state = false;
        }
    }

    fn decrement(&mut self) {
        if self.gate_input {
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
                assert!(self.carry_in || self.counter > 0, "expected non-zero counter");
                self.counter = self.counter.wrapping_sub(1);
            }
        }
    }

    fn set_control_register(&mut self, value: u8) {
        self.mode = if value & CTRL_MODE_COMPARE == 0 {
            Mode::Generating {
                single_shot: value & CTRL_MODE_SINGLE_SHOT != 0,
                deferred_load: value & CTRL_MODE_DEFERRED_LOAD != 0,
            }
        } else {
            Mode::Comparing {
                measure: if value & CTRL_MODE_PULSE_WIDTH != 0 {
                    Measure::PulseWidth
                } else {
                    Measure::Frequency
                },
                is_greater: value & CTRL_MODE_GREATER != 0,
            }
        };

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

    fn tick(&mut self) {
        if self.is_zero() {
            self.irq_active = self.irq_enabled;
            let single_shot = matches!(self.mode, Mode::Generating { single_shot: true, .. });
            if !single_shot {
                self.load()
            }
            self.output_state = if self.dual8bit_mode && self.latch != 0 {
                false
            } else {
                !self.output_state
            }
        } else {
            self.decrement();
            self.carry_in = false;
        }
    }

}

struct TransportSlot {
    transport: Box<dyn Transport>,
    encoder: PtmProtocolEncoder,
    decoder: PtmProtocolDecoder,
    handshake_done: bool,
    last_connection_id: u64,
}

impl TransportSlot {
    fn new(transport: Box<dyn Transport>) -> Self {
        let last_connection_id = transport.connection_id();
        Self {
            transport,
            encoder: PtmProtocolEncoder::new(),
            decoder: PtmProtocolDecoder::new(),
            handshake_done: false,
            last_connection_id,
        }
    }

    /// Resets handshake and codec state for a new client session.
    fn reset(&mut self) {
        self.encoder = PtmProtocolEncoder::new();
        self.decoder = PtmProtocolDecoder::new();
        self.handshake_done = false;
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
        for i in 0..3 {
            state.clocks[i] = timers[i].clock_input;
            state.gates[i] = timers[i].gate_input;
            state.outputs[i] = timers[i].output_state & timers[i].output_enabled;
        }
        state
    }

}

pub struct Mc6840 {
    name: &'static str,
    address: u16,
    transports: Vec<TransportSlot>,
    error_sender: Option<ErrorSender>,
    device_id: Option<DeviceId>,

    latched_status: u8,
    lsb_buffer: u8,
    msb_buffer: u8,
    timers: [Timer; 3],
    cr1_enabled: bool,
    t3_prescaler: Prescaler,
    reset_active: bool,
}

impl Mc6840 {

    pub fn new(name: &'static str) -> Self {
        Mc6840 {
            name,
            address: 0,
            transports: Vec::new(),
            error_sender: None,
            device_id: None,
            latched_status: 0,
            lsb_buffer: 0,
            msb_buffer: 0,
            timers: [
                Timer::new(),   // T1
                Timer::new(),   // T2
                Timer::new(),   // T3
            ],
            cr1_enabled: false,
            t3_prescaler: Prescaler::new(1),
            reset_active: false,
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

    fn send_to_all(&mut self, message: PtmProtocolMessage) {
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

    fn send_state_update(&mut self, before: &AsyncIoState, after: &AsyncIoState) {
        if before.clocks != after.clocks {
            self.send_to_all(PtmProtocolMessage::ClockState { clocks: after.clocks });
        }
        if before.gates != after.gates {
            self.send_to_all(PtmProtocolMessage::GateState { gates: after.gates });
        }
        if before.outputs != after.outputs {
            self.send_to_all(PtmProtocolMessage::OutputState { outputs: after.outputs });
        }
    }

    fn send_state_dump(&mut self, idx: usize) {
        let mut clocks: [bool; 3] = [false, false, false];
        let mut gates: [bool; 3] = [false, false, false];
        let mut outputs: [bool; 3] = [false, false, false];
        for i in 0..3 {
            let timer = &self.timers[i];
            clocks[i] = timer.clock_input;
            gates[i] = timer.gate_input;
            outputs[i] = timer.output_state;
        }
        let messages = vec![
            PtmProtocolMessage::ClockState { clocks },
            PtmProtocolMessage::GateState { gates },
            PtmProtocolMessage::OutputState { outputs},
        ];
        for message in messages {
            let mut data = Vec::new();
            self.transports[idx].encoder.encode(message, &mut data);
            for b in data {
                if let Err(e) = self.transports[idx].transport.send(b) {
                    self.report_error(e);
                    return;
                }
            }
        }
    }

    fn send_state_to_all(&mut self) {
        for i in 0..self.transports.len() {
            if self.transports[i].handshake_done {
                self.send_state_dump(i);
            }
        }
    }

    fn apply_message(&mut self, message: PtmProtocolMessage) {
        // TODO
    }

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
                    if self.transports[i].decoder.format() == Some(PtmProtocolFormat::Binary) {
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

    fn tick_timers(&mut self) {
        for timer_id in 0..3 {
            if !self.timers[timer_id].external_clock {
                if timer_id != T3 || self.t3_prescaler.update_has_carry_out() {
                    self.timers[timer_id].tick();
                }
            }
        }
    }

    fn internal_reset(&mut self) {
        for i in 0..3 {
            self.timers[i].load();
            self.timers[i].irq_active = false;
            self.timers[i].output_state = false;
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
                    let divisor = if value & CTRL_T3_PRESCALE != 0 { 8 } else { 1 };
                    self.t3_prescaler = Prescaler::new(divisor);
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
                if let Mode::Generating { deferred_load, .. } = self.timers[timer_id].mode
                        && !deferred_load {
                    self.timers[timer_id].load();
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
        self.send_state_update(&before, &after);
    }

    fn reset(&mut self) {
        let address = self.address;
        let transports = std::mem::take(&mut self.transports);
        let error_sender = self.error_sender.take();
        let device_id = self.device_id;
        *self = Self::new(self.name);
        self.address = address;
        self.transports = transports;
        self.error_sender = error_sender;
        self.device_id = device_id;
        debug!("{} {} reset", self.name(), device_id.unwrap());
        self.send_state_to_all();
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
    const CTRL_MODE_IMMEDIATE_LOAD: u8 = 0;
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
        assert!(matches!(device.timers[T1].mode, Mode::Comparing { measure: Measure::PulseWidth, is_greater: true }),
                "expected measure pulse width in greater than mode");
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
        assert!(matches!(device.timers[T2].mode, Mode::Comparing { measure: Measure::PulseWidth, is_greater: true }),
                "expected measure pulse width in greater than mode");
        assert!(device.timers[T2].irq_enabled, "expected IRQ enabled");
        assert!(device.timers[T2].output_enabled, "expected output enabled");
    }

    #[test]
    fn write_t3_control_register() {
        let mut device = device();
        assert!(!device.cr1_enabled, "expected CR3 enabled");
        device.write(0, 0xff & !CTRL_T3_PRESCALE);
        assert_eq!(device.t3_prescaler.divisor, 1);
        assert!(device.timers[T3].external_clock, "expected external clock");
        assert!(matches!(device.timers[T3].mode, Mode::Comparing { measure: Measure::PulseWidth, is_greater: true }),
                "expected measure pulse width in greater than mode");
        assert!(device.timers[T3].irq_enabled, "expected IRQ enabled");
        assert!(device.timers[T3].output_enabled, "expected output enabled");
    }

    #[test]
    fn write_t3_control_register_with_prescaler() {
        let mut device = device();
        device.write(0, CTRL_T3_PRESCALE);
        assert_eq!(device.t3_prescaler.divisor, 8);
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
        device.write(1, CTRL_MODE_GENERATE | CTRL_MODE_CONTINUOUS | CTRL_MODE_IMMEDIATE_LOAD);
        device.write(2 + 2 * (T2 as u16), 0x55);
        device.write(3 + 2 * (T2 as u16), 0xAA);
        assert_eq!(device.timers[T2].latch, 0x55AA);
        assert_eq!(device.timers[T2].counter, 0x55AA);
    }

    #[test]
    fn write_latch_deferred_load() {
        let mut device = device();
        device.timers[T2].counter = 0;
        device.write(1, CTRL_MODE_GENERATE | CTRL_MODE_CONTINUOUS | CTRL_MODE_DEFERRED_LOAD);
        device.write(2 + 2 * (T2 as u16), 0x55);
        device.write(3 + 2 * (T2 as u16), 0xAA);
        assert_eq!(device.timers[T2].latch, 0x55AA);
        assert_eq!(device.timers[T2].counter, 0);
    }

    #[test]
    fn write_mode_generate_continuous_immediate_load() {
        let mut device = device();
        device.write(1, CTRL_MODE_GENERATE | CTRL_MODE_CONTINUOUS | CTRL_MODE_IMMEDIATE_LOAD);
        assert!(matches!(device.timers[T2].mode,
            Mode::Generating { deferred_load: false, single_shot: false }));
    }

    #[test]
    fn write_mode_compare_frequency_less() {
        let mut device = device();
        device.write(1, CTRL_MODE_COMPARE | CTRL_MODE_FREQUENCY | CTRL_MODE_LESS);
        assert!(matches!(device.timers[T2].mode,
            Mode::Comparing { measure: Measure::Frequency, is_greater: false }));
    }

    #[test]
    fn write_mode_generate_continuous_deferred_load() {
        let mut device = device();
        device.write(1, CTRL_MODE_GENERATE | CTRL_MODE_CONTINUOUS | CTRL_MODE_DEFERRED_LOAD);
        assert!(matches!(device.timers[T2].mode,
            Mode::Generating { deferred_load: true, single_shot: false }));
    }

    #[test]
    fn write_mode_compare_pulse_width_less() {
        let mut device = device();
        device.write(1, CTRL_MODE_COMPARE | CTRL_MODE_PULSE_WIDTH | CTRL_MODE_LESS);
        assert!(matches!(device.timers[T2].mode,
            Mode::Comparing { measure: Measure::PulseWidth, is_greater: false }));
    }

    #[test]
    fn write_mode_generate_single_shot_immediate_load() {
        let mut device = device();
        device.write(1, CTRL_MODE_GENERATE | CTRL_MODE_SINGLE_SHOT | CTRL_MODE_IMMEDIATE_LOAD);
        assert!(matches!(device.timers[T2].mode,
            Mode::Generating { deferred_load: false, single_shot: true }));
    }

    #[test]
    fn write_mode_compare_frequency_greater() {
        let mut device = device();
        device.write(1, CTRL_MODE_COMPARE | CTRL_MODE_FREQUENCY | CTRL_MODE_GREATER);
        assert!(matches!(device.timers[T2].mode,
            Mode::Comparing { measure: Measure::Frequency, is_greater: true }));
    }

    #[test]
    fn write_mode_generate_single_shot_deferred_load() {
        let mut device = device();
        device.write(1, CTRL_MODE_GENERATE | CTRL_MODE_SINGLE_SHOT | CTRL_MODE_DEFERRED_LOAD);
        assert!(matches!(device.timers[T2].mode,
            Mode::Generating { deferred_load: true, single_shot: true }));
    }

    #[test]
    fn write_mode_compare_pulse_width_greater() {
        let mut device = device();
        device.write(1, CTRL_MODE_COMPARE | CTRL_MODE_PULSE_WIDTH | CTRL_MODE_GREATER);
        assert!(matches!(device.timers[T2].mode,
            Mode::Comparing { measure: Measure::PulseWidth, is_greater: true }));
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
    fn timer_tick_in_generate_continuous_normal_count_mode() {
        let mut timer = Timer::new();
        timer.irq_enabled = true;
        assert!(!timer.output_state, "expected output state low");
        timer.mode = Mode::Generating { single_shot: false, deferred_load: false };
        timer.latch = 1;
        timer.load();
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
        timer.mode = Mode::Generating { single_shot: true, deferred_load: false };
        timer.latch = 1;
        timer.load();
        // first tick should decrement counter
        timer.tick();
        assert_eq!(timer.counter, 0);
        // second tick should set output state high, signal interrupt; counter remains at zero
        timer.tick();
        assert!(timer.output_state, "expected output state high");
        assert!(timer.irq_active, "expected IRQ active");
        assert_eq!(timer.counter, 0);
    }

    #[test]
    fn timer_tick_in_generate_continuous_dual8bit_count_mode() {
        let mut timer = Timer::new();
        timer.dual8bit_mode = true;
        timer.irq_enabled = true;
        timer.output_state = true;
        timer.mode = Mode::Generating { single_shot: false, deferred_load: false };
        timer.latch = 0x0101;
        timer.load();
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
        timer.mode = Mode::Generating { single_shot: true, deferred_load: false };
        timer.latch = 0x0101;
        timer.load();
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
        timer.mode = Mode::Generating { single_shot: false, deferred_load: false };
        timer.latch = 0;
        timer.irq_enabled = true;
        timer.load();
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
        timer.mode = Mode::Generating { single_shot: false, deferred_load: false };
        timer.dual8bit_mode = true;
        timer.latch = 0;
        timer.load();
        let output_state = timer.output_state;
        timer.tick();
        assert_ne!(timer.output_state, output_state);
        timer.tick();
        assert_eq!(timer.output_state, output_state);
        timer.tick();
        assert_ne!(timer.output_state, output_state);
    }

    #[test]
    fn prescaler_update() {
        let mut prescaler = Prescaler::new(2);
        assert!(!prescaler.update_has_carry_out(), "expected no carry out");
        assert!(prescaler.update_has_carry_out(), "expected carry out");
        assert_eq!(prescaler.count, prescaler.divisor);
    }

}
