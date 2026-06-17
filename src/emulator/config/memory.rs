use std::collections::HashMap;
use figment::value::{Dict, Value};
use figment::providers::Serialized;
use serde::Deserialize;

use crate::emulator::{AddressRange, BusConfig};
use super::{DeviceModule, DeviceModuleError, InstantiationContext};

// Type name used in registering RAM as a device
const RAM_DEVICE_TYPE: &str = "ram";

// Type name used in registering ROM as a device
const ROM_DEVICE_TYPE: &str = "rom";

/// RAM device module.
#[derive(Clone)]
pub struct RamModule;

/// ROM device module.
#[derive(Clone)]
pub struct RomModule;

#[derive(Deserialize)]
pub struct MemoryAttributes {
    size: u32,
    fill: Option<u8>,
    image: Option<std::path::PathBuf>,
}


impl MemoryAttributes {
    fn from_attributes(attributes: &HashMap<String, Value>) -> Result<Self, DeviceModuleError> {
        let attrs = Dict::from_iter(attributes.clone());
        figment::Figment::new()
            .merge(Serialized::defaults(attrs))
            .extract()
            .map_err(|e| DeviceModuleError::Config(format!("configuration error: {e}")))
    }
}

async fn read_image_file(filename: &std::path::Path) -> Result<Vec<u8>, DeviceModuleError> {
    tokio::fs::read(&filename).await
        .map_err(DeviceModuleError::Io)
}

impl DeviceModule for RamModule {
    fn name(&self) -> &'static str {
        RAM_DEVICE_TYPE
    }

    async fn instantiate(&self, bus_config: BusConfig, address: u16,
                         attributes: &HashMap<String, Value>, _context: &InstantiationContext)
                         -> Result<BusConfig, DeviceModuleError> {

        let config = MemoryAttributes::from_attributes(attributes)?;
        let range = AddressRange::new(address, address + (config.size - 1) as u16);
        if let Some(filename) = config.image {
            if config.fill.is_some() {
                return Err(DeviceModuleError::Config("Options 'image' and 'fill' are mutually exclusive".to_string()));
            }
            let data = read_image_file(&filename).await?;
            bus_config.ram_with_data(range, data).map_err(DeviceModuleError::BusConfig)
        } else if let Some(fill) = config.fill {
            bus_config.ram_with_fill(range, fill).map_err(DeviceModuleError::BusConfig)
        } else {
            bus_config.ram(range).map_err(DeviceModuleError::BusConfig)
        }
    }
}

impl DeviceModule for RomModule {
    fn name(&self) -> &'static str {
        ROM_DEVICE_TYPE
    }

    async fn instantiate(&self, bus_config: BusConfig, address: u16,
                         attributes: &HashMap<String, Value>, _context: &InstantiationContext)
                         -> Result<BusConfig, DeviceModuleError> {
        let config = MemoryAttributes::from_attributes(attributes)?;
        let range = AddressRange::new(address, address + (config.size - 1) as u16);
        if let Some(filename) = config.image {
            let data = read_image_file(&filename).await?;
            bus_config.rom(range, data).map_err(DeviceModuleError::BusConfig)
        }
        else {
            Err(DeviceModuleError::Config("ROM requires the 'image' attribute".to_string()))
        }
    }
}

