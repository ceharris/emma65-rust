use std::collections::HashMap;
use figment::value::{Tag, Value};
use tempfile::Builder;
use emma65::emulator::{
    BuildError, BusConfig, Config, CpuVariantSpec, DeviceModule, DeviceModuleError,
    DeviceRegistry, InstantiationContext, RamModule, RomModule,
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn attrs(pairs: &[(&str, Value)]) -> HashMap<String, Value> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
}

fn ctx() -> InstantiationContext {
    InstantiationContext { clock_hz: None, error_sender: None, console_transport: None }
}

fn config_with_devices(devices: Option<Vec<&str>>) -> Config {
    Config {
        cpu_variant_spec: None,
        clock_speed_hz: None,
        devices: devices.map(|specs| {
            specs.into_iter().map(|s| s.parse().unwrap()).collect()
        }),
    }
}

// ---------------------------------------------------------------------------
// Group A — Config::build end-to-end
// ---------------------------------------------------------------------------

#[tokio::test]
async fn build_with_no_devices() {
    let registry = DeviceRegistry::with_builtins();
    let config = config_with_devices(None);
    let session = config.build(&registry).await.unwrap();
    // CPU is present and ready (just verify it doesn't error)
    drop(session);
}

#[tokio::test]
async fn build_with_64kb_ram() {
    let registry = DeviceRegistry::with_builtins();
    let config = config_with_devices(Some(vec!["ram@0x0000,size=65536,fill=0"]));
    config.build(&registry).await.unwrap();
}

#[tokio::test]
async fn build_with_cmos65c02_variant() {
    let registry = DeviceRegistry::with_builtins();
    let config = Config {
        cpu_variant_spec: Some(CpuVariantSpec::Cmos6502),
        clock_speed_hz: None,
        devices: Some(vec!["ram@0x0000,size=65536,fill=0".parse().unwrap()]),
    };
    config.build(&registry).await.unwrap();
}

#[tokio::test]
async fn build_with_wdc65c02_variant() {
    let registry = DeviceRegistry::with_builtins();
    let config = Config {
        cpu_variant_spec: Some(CpuVariantSpec::Wdc6502),
        clock_speed_hz: None,
        devices: Some(vec!["ram@0x0000,size=65536,fill=0".parse().unwrap()]),
    };
    config.build(&registry).await.unwrap();
}

#[tokio::test]
async fn build_with_unknown_device_type() {
    let registry = DeviceRegistry::with_builtins();
    let config = config_with_devices(Some(vec!["no_such_thing@0x1000,size=256"]));
    let err = config.build(&registry).await.err().unwrap();
    assert!(matches!(
        err,
        BuildError::Device { ref module_name, .. } if module_name == "no_such_thing"
    ));
}

// ---------------------------------------------------------------------------
// Group B — RamModule instantiation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ram_module_plain() {
    let a = attrs(&[("size", Value::from(1024u32))]);
    RamModule.instantiate(BusConfig::new(), 0x2000, &a, &ctx()).await.unwrap();
}

#[tokio::test]
async fn ram_module_with_fill() {
    let a = attrs(&[("size", Value::from(1024u32)), ("fill", Value::from(0xABu32))]);
    let bus_config = RamModule.instantiate(BusConfig::new(), 0x2000, &a, &ctx()).await.unwrap();
    let mut bus = bus_config.build();
    assert_eq!(bus.read(0x2000).unwrap(), 0xAB);
    assert_eq!(bus.read(0x23FF).unwrap(), 0xAB);
}

#[tokio::test]
async fn ram_module_with_binary_image() {
    let data: [u8; 8] = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08];
    let f = Builder::new().suffix(".bin").tempfile().unwrap();
    tokio::fs::write(f.path(), &data).await.unwrap();
    let path_str = Value::String(Tag::Default, f.path().to_str().unwrap().to_string());
    let a = attrs(&[("size", Value::from(8u32)), ("image", path_str)]);
    let bus_config = RamModule.instantiate(BusConfig::new(), 0x3000, &a, &ctx()).await.unwrap();
    let mut bus = bus_config.build();
    for (i, &expected) in data.iter().enumerate() {
        assert_eq!(bus.read(0x3000 + i as u16).unwrap(), expected);
    }
}

#[tokio::test]
async fn ram_module_with_intel_hex_image() {
    // Minimal ihex: one data record at 0x0000, two bytes, then EOF
    let ihex = ":020000000102FB\n:00000001FF\n";
    let f = Builder::new().suffix(".hex").tempfile().unwrap();
    tokio::fs::write(f.path(), ihex.as_bytes()).await.unwrap();
    let path_str = Value::String(Tag::Default, f.path().to_str().unwrap().to_string());
    let a = attrs(&[
        ("size", Value::from(256u32)),
        ("fill", Value::from(0u32)),
        ("image", path_str),
    ]);
    let bus_config = RamModule.instantiate(BusConfig::new(), 0x0000, &a, &ctx()).await.unwrap();
    let mut bus = bus_config.build();
    assert_eq!(bus.read(0x0000).unwrap(), 0x01);
    assert_eq!(bus.read(0x0001).unwrap(), 0x02);
}

#[tokio::test]
async fn ram_module_missing_size() {
    let a = attrs(&[]);
    let err = RamModule.instantiate(BusConfig::new(), 0x2000, &a, &ctx()).await.err().unwrap();
    assert!(matches!(err, DeviceModuleError::Config(_)));
}

// ---------------------------------------------------------------------------
// Group C — RomModule instantiation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn rom_module_without_image() {
    let a = attrs(&[("size", Value::from(1024u32))]);
    let err = RomModule.instantiate(BusConfig::new(), 0x8000, &a, &ctx()).await.err().unwrap();
    assert!(matches!(err, DeviceModuleError::Config(_)));
}

#[tokio::test]
async fn rom_module_with_binary_image() {
    let data: [u8; 8] = [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF, 0x11, 0x22];
    let f = Builder::new().suffix(".bin").tempfile().unwrap();
    tokio::fs::write(f.path(), &data).await.unwrap();
    let path_str = Value::String(Tag::Default, f.path().to_str().unwrap().to_string());
    let a = attrs(&[("size", Value::from(8u32)), ("image", path_str)]);
    let bus_config = RomModule.instantiate(BusConfig::new(), 0x8000, &a, &ctx()).await.unwrap();
    let mut bus = bus_config.build();
    for (i, &expected) in data.iter().enumerate() {
        assert_eq!(bus.read(0x8000 + i as u16).unwrap(), expected);
    }
}

// ---------------------------------------------------------------------------
// Group D — DeviceRegistry dispatch
// ---------------------------------------------------------------------------

#[tokio::test]
async fn registry_instantiates_ram() {
    let registry = DeviceRegistry::with_builtins();
    let a = attrs(&[("size", Value::from(64u32)), ("fill", Value::from(0u32))]);
    registry.instantiate("ram", BusConfig::new(), 0x1000, &a, &ctx()).await.unwrap();
}

#[tokio::test]
async fn registry_instantiates_rom() {
    let data: [u8; 64] = [0x42; 64];
    let f = Builder::new().suffix(".bin").tempfile().unwrap();
    tokio::fs::write(f.path(), &data).await.unwrap();
    let path_str = Value::String(Tag::Default, f.path().to_str().unwrap().to_string());
    let registry = DeviceRegistry::with_builtins();
    let a = attrs(&[("size", Value::from(64u32)), ("image", path_str)]);
    registry.instantiate("rom", BusConfig::new(), 0x8000, &a, &ctx()).await.unwrap();
}

#[tokio::test]
async fn registry_unknown_module_name() {
    let registry = DeviceRegistry::with_builtins();
    let a = attrs(&[]);
    let err = registry.instantiate("bogus", BusConfig::new(), 0x1000, &a, &ctx()).await.err().unwrap();
    assert!(matches!(err, DeviceModuleError::Config(s) if s.contains("bogus")));
}
