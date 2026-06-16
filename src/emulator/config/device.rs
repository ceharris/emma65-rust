use std::collections::HashMap;
use std::str::FromStr;
use figment::value::{Tag, Value};
use serde::{Deserialize, Serialize};

use crate::emulator::{BusConfig, BusConfigError};


/// A configuration spec for a pluggable device module.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceSpec {
    /// Address at which the device is to be mapped on the bus.
    address: u16,
    /// Device module name used to identify the device to be instantiated.
    #[serde(rename = "type")]
    type_name: String,
    /// Additional device-specific attributes.
    #[serde(flatten)]
    attributes: HashMap<String, figment::value::Value>,
}

impl FromStr for DeviceSpec {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(parse_spec(s)?)
    }
}

/// A pluggable device module.
pub trait DeviceModule {
    /// Gets the name of this device module.
    fn name(&self) -> &'static  str;
    /// Instantiates the device represented by this module.
    /// # Arguments
    /// * bus_config - bus configuration builder
    /// * address - address at which the device will be mapped on the bus
    /// * attributes - device configuration attributes
    fn instantiate(&self, bus_config: BusConfig, address: u16, 
                   attributes: &HashMap<String, Value>) -> Result<BusConfig, BusConfigError>;
}

fn parse_prefixed_u16(s: &str) -> Result<u16, std::num::ParseIntError> {
    if let Some(hex_str) = s.strip_prefix("0x") {
        u16::from_str_radix(hex_str, 16)
    } else if let Some(hex_str) = s.strip_prefix("0X") {
        u16::from_str_radix(hex_str, 16)
    } else if let Some(oct_str) = s.strip_prefix("0o") {
        u16::from_str_radix(oct_str, 8)
    } else if let Some(oct_str) = s.strip_prefix("0O") {
        u16::from_str_radix(oct_str, 8)
    } else if let Some(bin_str) = s.strip_prefix("0b") {
        u16::from_str_radix(bin_str, 2)
    } else if let Some(bin_str) = s.strip_prefix("0B") {
        u16::from_str_radix(bin_str, 2)
    } else {
        s.parse::<u16>() // Fallback to standard base-10
    }
}

fn parse_suffixed_u16(s: &str) -> Result<u16, std::num::ParseIntError> {
    if let Some(k_str) = s.strip_suffix("K") {
        Ok(k_str.parse::<u16>()? * 1024)
    } else if let Some(k_str) = s.strip_suffix("k") {
        Ok(k_str.parse::<u16>()? * 1024)
    } else {
        s.parse::<u16>()
    }
}

fn parse_device_mapping(s: &str) -> Result<(String, u16), String> {
    let parts: Vec<&str> = s.splitn(2, '@').collect();
    if parts.len() == 2 {
        let device_type = parts[0].to_string();
        let address = parts[1];
        if device_type.is_empty() {
            return Err("Device type is required on the left-hand side of '@'".to_string())
        }
        if address.is_empty() {
            return Err("Address is required on the right-hand side of '@'".to_string())
        }
        match parse_prefixed_u16(address) {
            Ok(address) => Ok((device_type, address)),
            Err(error) => Err(error.to_string())
        }
    } else {
        Err("Device type and address are required (e.g. console@0xfff8)".to_string())
    }
}

fn parse_attributes(s: &str) -> Result<HashMap<String, figment::value::Value>, String> {
    let mut attributes = HashMap::new();
    for pair in s.split(',') {
        if pair.is_empty() {
            continue;
        }
        let mut parts = pair.splitn(2,'=');
        let key = parts.next().ok_or("missing attribute name")?.to_string();
        if key.is_empty() {
            return Err("Attribute name is required on the left-hand side of '='".to_string());
        }
        let val_str = parts.next().ok_or(format!("Missing attribute value for '{}'", key))?;
        if val_str.is_empty() {
            return Err("Attribute value is required on the right-hand side of '='".to_string());
        }

        let value = if let Ok(b) = val_str.parse::<bool>() {
            Value::from(b)
        } else if let Ok(i) = parse_prefixed_u16(val_str) {
            Value::from(i)
        } else if let Ok(i) = parse_suffixed_u16(val_str) {
            Value::from(i)
        } else if let Ok(i) = val_str.parse::<u32>() {
            Value::from(i)
        } else {
            // Fallback to a string. Note that Figment values require a Tag for
            // tracking configuration metadata. Tag::Default works perfectly here.
            Value::String(Tag::Default, val_str.to_string())
        };

        attributes.insert(key, value);
    }
    Ok(attributes)
}

fn parse_spec(s: &str) -> Result<DeviceSpec, String> {
    let parts: Vec<&str> = s.splitn(2,',').collect();
    let (type_name, address) = parse_device_mapping(parts[0])?;
    let attributes = if parts.len() == 2 {
        parse_attributes(parts[1])?
    }
    else {
        HashMap::new()
    };
    Ok(DeviceSpec {
        address,
        type_name,
        attributes,
    })
}


#[cfg(test)]

mod tests {

    use super::*;

    #[test]
    fn parse_prefixed_u16_hex() {
        assert_eq!(parse_prefixed_u16("0xdead").unwrap(), 0xdead);
        assert_eq!(parse_prefixed_u16("0XDEAD").unwrap(), 0xdead);
    }

    #[test]
    fn parse_prefixed_u16_octal() {
        assert_eq!(parse_prefixed_u16("0o777").unwrap(), 0o777);
        assert_eq!(parse_prefixed_u16("0O777").unwrap(), 0o777);
    }

    #[test]
    fn parse_prefixed_u16_binary() {
        assert_eq!(parse_prefixed_u16("0b10100101").unwrap(), 0b10100101);
        assert_eq!(parse_prefixed_u16("0B10100101").unwrap(), 0b10100101);
    }

    #[test]
    fn parse_prefixed_u16_decimal() {
        assert_eq!(parse_prefixed_u16("65535").unwrap(), 65535);
    }

    #[test]
    fn parse_suffixed_u16_kilobytes() {
        assert_eq!(parse_suffixed_u16("16K").unwrap(), 16384);
        assert_eq!(parse_suffixed_u16("16k").unwrap(), 16384);
    }

    #[test]
    fn parse_suffixed_u16_bytes() {
        assert_eq!(parse_suffixed_u16("16384").unwrap(), 16384)
    }

    #[test]
    fn parse_device_mapping_with_type_and_address() {
        match parse_device_mapping("console@0xfff8") {
            Ok((device_type, address)) => {
                assert_eq!(device_type, "console");
                assert_eq!(address, 0xfff8);
            }
            _ => panic!("expected valid device mapping")
        }
    }

    #[test]
    #[should_panic(expected = "required")]
    fn parse_device_mapping_without_delimiter() {
        parse_device_mapping("console").unwrap();
    }

    #[test]
    #[should_panic(expected = "Device type is required")]
    fn parse_device_mapping_with_empty_type() {
        parse_device_mapping("@0xfff8").unwrap();
    }

    #[test]
    #[should_panic(expected = "Address is required")]
    fn parse_device_mapping_with_empty_address() {
        parse_device_mapping("console@").unwrap();
    }

    #[test]
    fn parse_one_attribute_with_string_value() {
        match parse_attributes("name=value") {
            Ok(map) => {
                assert_eq!(map.get("name").unwrap().as_str().unwrap(), "value");
            }
            _ => panic!("expected mapped attribute")
        }
    }

    #[test]
    fn parse_one_attribute_with_bool_value() {
        match parse_attributes("name=true") {
            Ok(map) => {
                assert_eq!(map.get("name").unwrap().to_bool().unwrap(), true);
            }
            _ => panic!("expected mapped attribute")
        }
    }

    #[test]
    fn parse_one_attribute_with_u32_value() {
        match parse_attributes("name=1843200") {
            Ok(map) => {
                assert_eq!(map.get("name").unwrap().to_num().unwrap().to_u32().unwrap(), 1_843_200);
            }
            _ => panic!("expected mapped attribute")
        }
    }

    #[test]
    fn parse_one_attribute_with_prefixed_u16_value() {
        match parse_attributes("name=0x7fff") {
            Ok(map) => {
                assert_eq!(map.get("name").unwrap().to_num().unwrap().to_u32().unwrap(), 0x7fff);
            }
            _ => panic!("expected mapped attribute")
        }
    }

    #[test]
    fn parse_one_attribute_with_suffixed_u16_value() {
        match parse_attributes("name=48K") {
            Ok(map) => {
                assert_eq!(map.get("name").unwrap().to_num().unwrap().to_u32().unwrap(), 48 * 1024);
            }
            _ => panic!("expected mapped attribute")
        }
    }

    #[test]
    fn parse_two_attributes() {
        match parse_attributes("name1=value1,name2=value2") {
            Ok(map) => {
                assert_eq!(map.get("name1").unwrap().as_str().unwrap(), "value1");
                assert_eq!(map.get("name2").unwrap().as_str().unwrap(), "value2");
            }
            _ => panic!("expected mapped attributes")
        }
    }

    #[test]
    #[should_panic(expected = "name is required")]
    fn parse_attribute_with_empty_name() {
        parse_attributes("=value").unwrap();
    }

    #[test]
    #[should_panic(expected = "value is required")]
    fn parse_attribute_with_empty_value() {
        parse_attributes("name=").unwrap();
    }

    #[test]
    fn parse_spec_without_attributes() {
        match parse_spec("console@0xfff8") {
            Ok(device_spec) => {
                assert_eq!(device_spec.type_name, "console");
                assert_eq!(device_spec.address, 0xfff8);
                assert!(device_spec.attributes.is_empty());
            }
            _ => panic!("expected device spec")
        }
    }

    #[test]
    fn parse_spec_with_attributes() {
        match parse_spec("console@0xfff8,transport=pty:.emma/dev/ttyS0") {
            Ok(device_spec) => {
                assert_eq!(device_spec.type_name, "console");
                assert_eq!(device_spec.address, 0xfff8);
                assert_eq!(device_spec.attributes.get("transport").unwrap().as_str().unwrap(),
                           "pty:.emma/dev/ttyS0".to_string());
            }
            _ => panic!("expected device spec")
        }
    }

}