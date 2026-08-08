use super::{DeviceModule, DeviceModuleError, ExpandedPathBuf, InstantiationContext, loader};
use crate::emulator::bus::{DeviceIdAllocator, symbol};
use crate::emulator::config::write_policy::WritePolicySpec;
use crate::emulator::device::{Finch, finch};
use crate::emulator::{AddressRange, BusConfig};
use figment::providers::Serialized;
use figment::value::{Dict, Value};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

const DEVICE_NAME: &str = "mem/finch";

/// Finch bank-switched MMU module.
#[derive(Clone)]
pub struct FinchModule;

/// Configuration attributes for the Finch bank-switched MMU module.
#[derive(Deserialize)]
pub struct FinchAttributes {
    #[serde(rename = "bank-registers", alias="banks")]
    bank_register_address: u16,
    #[serde(rename = "control-register", alias="ctrl")]
    control_register_address: u16,
    #[serde(rename = "write-policy", skip_serializing_if = "Option::is_none")]
    write_policy: Option<WritePolicySpec>,
    fill: Option<u8>,
    offset: Option<isize>,
    image: ExpandedPathBuf,
    labels: Option<ExpandedPathBuf>,
}

impl FinchAttributes {
    fn from_attributes(attributes: &HashMap<String, Value>) -> Result<Self, DeviceModuleError> {
        let attrs = Dict::from_iter(attributes.clone());
        figment::Figment::new()
            .merge(Serialized::defaults(attrs))
            .extract()
            .map_err(|e| DeviceModuleError::Config(format!("configuration error: {e}")))
    }
}

impl DeviceModule for FinchModule {

    fn name(&self) -> &'static str { DEVICE_NAME }

    async fn instantiate(&self, bus_config: BusConfig, _address: u16,
                         attributes: &HashMap<String, Value>, 
                         context: &InstantiationContext, 
                         id_allocator: Arc<Mutex<DeviceIdAllocator>>) -> Result<BusConfig, DeviceModuleError> {
        let config = FinchAttributes::from_attributes(attributes)?;
        let device_id = id_allocator.lock().unwrap().next(false);
        let offset = finch::ROM_START as isize + config.offset.unwrap_or(0);

        let bus_config = if let Some(filename) = config.labels {
            let table = symbol::load_vice_labels(filename)
                .await
                .map_err(DeviceModuleError::SymbolTable)?;
            bus_config.symbol_table(&table)
        } else {
            bus_config
        };

        let mut data = super::memory::make_buffer(finch::MEMORY_SIZE, config.fill);
        loader::load_image(&config.image, &mut data, offset).await.map_err(DeviceModuleError::Load)?;
        let device = {
            let mut dev = Finch::with_data(DEVICE_NAME, config.bank_register_address, config.control_register_address, data);
            if let Some(write_policy) = config.write_policy {
                dev.set_write_policy(write_policy.to_rom_write_policy());
            }
            if let Some(sender) = &context.error_sender {
                dev.set_error_sender(sender.clone(), device_id);
            }
            dev
        };

        bus_config.device(AddressRange::new(0, 0xFFFF), device_id, Box::new(device))
            .map_err(DeviceModuleError::BusConfig)
    }
}

