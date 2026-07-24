use figment::providers::Serialized;
use figment::value::{Dict, Value};
use serde::Deserialize;
use std::collections::HashMap;

use super::{DeviceModule, DeviceModuleError, ExpandedPathBuf, InstantiationContext, loader};
use crate::emulator::config::write_policy::WritePolicySpec;
use crate::emulator::device::{Phoebe, phoebe};
use crate::emulator::{AddressRange, BusConfig, DeviceId};

const DEVICE_NAME: &str = "mem/phoebe";

/// Phoebe bank-switched memory module.
#[derive(Clone)]
pub struct PhoebeModule;

/// Configuration attributes for the Phoebe bank-switched memory module.
#[derive(Deserialize)]
pub struct PhoebeAttributes {
    /// Address of the bank selection register.
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
    /// Value used to fill RAM
    ram_fill: Option<u8>,
}

impl PhoebeAttributes {
    fn from_attributes(attributes: &HashMap<String, Value>) -> Result<Self, DeviceModuleError> {
        let attrs = Dict::from_iter(attributes.clone());
        figment::Figment::new()
            .merge(Serialized::defaults(attrs))
            .extract()
            .map_err(|e| DeviceModuleError::Config(format!("configuration error: {e}")))
    }
}

impl DeviceModule for PhoebeModule {

    fn name(&self) -> &'static str { DEVICE_NAME }

    async fn instantiate(&self, bus_config: BusConfig, address: u16,
                         attributes: &HashMap<String, Value>, context: &InstantiationContext)
                         -> Result<BusConfig, DeviceModuleError> {
        let config = PhoebeAttributes::from_attributes(attributes)?;
        let device_id = DeviceId(address as u32);
        let offset = config.offset.unwrap_or(0) as usize;
        let mut rom_data = super::memory::make_buffer(phoebe::ROM_SIZE, config.fill);
        let ram_data = super::memory::make_buffer(phoebe::ROM_SIZE, config.ram_fill);
        loader::load_image(&config.image, &mut rom_data, offset).await.map_err(DeviceModuleError::Load)?;
        let device = {
            let mut dev = Phoebe::with_data(DEVICE_NAME, config.control_register_address, rom_data, ram_data);
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

