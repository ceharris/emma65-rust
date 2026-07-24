use figment::providers::Serialized;
use figment::value::{Dict, Value};
use serde::Deserialize;
use std::collections::HashMap;

use super::{DeviceModule, DeviceModuleError, ExpandedPathBuf, InstantiationContext, loader};
use crate::emulator::config::write_policy::WritePolicySpec;
use crate::emulator::device::{Finch, finch};
use crate::emulator::{AddressRange, BusConfig, DeviceId};

const DEVICE_NAME: &str = "mem/finch";

/// Finch bank-switched MMU module.
#[derive(Clone)]
pub struct FinchModule;

/// Configuration attributes for the Finch bank-switched MMU module.
#[derive(Deserialize)]
pub struct FinchAttributes {
    /// Base address of the bank selection registers.
    #[serde(rename = "bank-registers", alias="banks")]
    bank_register_address: u16,
    /// Address of the control register.
    #[serde(rename = "control-register", alias="ctrl")]
    control_register_address: u16,
    /// Write policy
    #[serde(rename = "write-policy", skip_serializing_if = "Option::is_none")]
    write_policy: Option<WritePolicySpec>,
    /// Value used to fill unused space in ROM
    fill: Option<u8>,
    /// Offset to apply to load records in the ROM image
    offset: Option<u16>,
    /// Path to an image to load into the ROM
    image: ExpandedPathBuf,
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

    async fn instantiate(&self, bus_config: BusConfig, address: u16,
                         attributes: &HashMap<String, Value>, context: &InstantiationContext)
                         -> Result<BusConfig, DeviceModuleError> {
        let config = FinchAttributes::from_attributes(attributes)?;
        let device_id = DeviceId(address as u32);
        let offset = finch::ROM_START + config.offset.unwrap_or(0) as usize;
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

