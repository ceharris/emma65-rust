use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use figment::value::Value;

use super::{DeviceModule, DeviceModuleError, InstantiationContext};
use crate::emulator::bus::DeviceIdAllocator;
use crate::emulator::device::PicFinch;
use crate::emulator::{AddressRange, BusConfig};

/// Number of bytes this device occupies in the bus address space: just the IER register.
/// The 16-byte vector table at `0xFFE0`-`0xFFEF` is expected to be backed by ROM and is
/// not claimed by the PIC itself — see [`PicFinch`]'s module docs.
const BUS_SIZE: u16 = 1;

/// Device module that registers a [`PicFinch`] as a bus device and installs its
/// [`VectorResolver`](crate::emulator::VectorResolver) on the `Cpu` being built. Accepts
/// no device-specific attributes.
#[derive(Clone)]
pub struct PicFinchModule;

impl DeviceModule for PicFinchModule {
    fn name(&self) -> &'static str {
        "pic/finch"
    }

    async fn instantiate(&self, bus_config: BusConfig, address: u16,
                         _attributes: &HashMap<String, Value>, context: &InstantiationContext,
                         id_allocator: Arc<Mutex<DeviceIdAllocator>>)
            -> Result<BusConfig, DeviceModuleError> {

        let device_id = id_allocator.lock().unwrap().next_available();
        let mut device = PicFinch::new(self.name()).with_address(address);
        if let Some(sender) = &context.log_sender {
            device.set_log_sender(sender.clone());
        }
        let resolver = device.vector_resolver();

        let bus_config = bus_config.device(
            AddressRange::new(address, address + (BUS_SIZE - 1)),
            device_id, Box::new(device))
            .map_err(DeviceModuleError::BusConfig)?;

        bus_config.vector_resolver(Box::new(resolver))
            .map_err(DeviceModuleError::BusConfig)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emulator::cpu::IRQ_VECTOR;
    use crate::emulator::{Cpu, CpuVariant, IrqSource};

    fn context() -> InstantiationContext {
        InstantiationContext { clock_hz: None, error_sender: None, console_transport: None, keyboard_transport: None, log_sender: None, display_frame_sink: None, display_geometry_sink: None, led_matrix_frame_sink: None, led_matrix_geometry_sink: None }
    }

    #[tokio::test]
    async fn instantiate_registers_ier_as_a_bus_device() {
        let module = PicFinchModule;
        let attributes: HashMap<String, Value> = HashMap::new();
        let id_allocator = Arc::new(Mutex::new(DeviceIdAllocator::new()));
        let bus_config = module.instantiate(BusConfig::new(), 0xFFFF, &attributes, &context(), id_allocator)
            .await.unwrap();
        let mut bus = bus_config.build();
        bus.write(0xFFFF, !(1 << 2) & 0x7F).unwrap(); // disable all but slot 2
        assert_eq!(bus.read(0xFFFF).unwrap(), 0x80 | (1 << 2));
    }

    #[tokio::test]
    async fn instantiate_installs_a_vector_resolver_that_tracks_ier_writes() {
        let module = PicFinchModule;
        let attributes: HashMap<String, Value> = HashMap::new();
        let id_allocator = Arc::new(Mutex::new(DeviceIdAllocator::new()));
        // RAM everywhere except the PIC's own IER byte at 0xFFFF, so the vector table
        // (backed by ROM in a real system) can actually store the bytes we write.
        let bus_config = BusConfig::new().ram(AddressRange::new(0x0000, 0xFFFE)).unwrap();
        let mut bus_config = module.instantiate(bus_config, 0xFFFF, &attributes, &context(), id_allocator)
            .await.unwrap();
        let resolver = bus_config.take_vector_resolver();
        let bus = bus_config.build();
        let mut builder = Cpu::builder(CpuVariant::Wdc65C02).bus(bus);
        if let Some(resolver) = resolver {
            builder = builder.vector_resolver(resolver);
        }
        let mut cpu = builder.build().unwrap();

        cpu.bus_mut().write(0xFFFF, 0x80 | (1 << 3)).unwrap(); // enable slot 3
        cpu.bus_mut().write(0xFFE6, 0x00).unwrap(); // slot 3 vector low byte
        cpu.bus_mut().write(0xFFE7, 0x04).unwrap(); // slot 3 vector high byte
        cpu.interrupts_mut().assert_irq(IrqSource(3));
        cpu.registers_mut().p.remove(crate::emulator::StatusRegister::I);

        cpu.step(None, true);
        assert_eq!(cpu.registers().pc, 0x0400);
        let _ = IRQ_VECTOR; // not fetched when a PIC resolver is installed
    }
}
