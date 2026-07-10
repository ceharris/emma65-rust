use std::collections::HashMap;
use figment::value::{Dict, Value};
use figment::providers::Serialized;
use clap::Parser;
use serde::Deserialize;

use crate::emulator::device::{Phoebe, phoebe};
use crate::emulator::{AddressRange, BusConfig, DeviceId};
use crate::emulator::config::write_policy::WritePolicySpec;
use super::{loader, DeviceModule, DeviceModuleError, ExpandedPathBuf, InstantiationContext};

const DEVICE_NAME: &str = "rom/phoebe";

/// Phoebe bank-switched ROM module.
#[derive(Clone)]
pub struct PhoebeModule;

/// Configuration attributes for the Phoebe bank-switched ROM module.
#[derive(Parser, Deserialize)]
pub struct PhoebeAttributes {
    /// Address of the bank selection register.
    #[serde(rename = "bank-register")]
    bank_register_address: u16,
    /// Write policy
    #[serde(rename = "write-policy", skip_serializing_if = "Option::is_none")]
    #[clap(long = "write-policy")]
    write_policy: Option<WritePolicySpec>,
    /// Value used to fill unused space in ROM
    fill: Option<u8>,
    /// Offset to apply to load records in the ROM image
    offset: Option<u16>,
    /// Path to an image to load into the ROM
    image: ExpandedPathBuf,
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
        let range = AddressRange::new(address, address.wrapping_add(phoebe::REGION_SIZE).wrapping_sub(1));
        let device_id = DeviceId(address as u32);
        let fill = config.fill.unwrap_or(0xff);
        let offset = config.offset.unwrap_or(0) as usize;
        let mut data = vec![fill; phoebe::MEMORY_SIZE as usize];
        loader::load_image(&config.image, &mut data, offset).await.map_err(DeviceModuleError::Load)?;
        let device = {
            let mut dev = Phoebe::with_data(DEVICE_NAME, range, config.bank_register_address, data);
            if let Some(write_policy) = config.write_policy {
                dev.set_write_policy(write_policy.to_rom_write_policy());
            }
            if let Some(sender) = &context.error_sender {
                dev.set_error_sender(sender.clone(), device_id);
            }
            dev
        };

        bus_config.device(range, device_id, Box::new(device))
            .map_err(DeviceModuleError::BusConfig)
    }
}

