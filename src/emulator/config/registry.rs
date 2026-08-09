use super::{ConsoleModule, DeviceModule, DeviceModuleError, FinchModule, LfsrModule, Mc6840Module, Mc6850Module, PhoebeModule, PicFinchModule, R6551Module, RamModule, RomModule, Via6522Module, VireoModule};
use crate::emulator::bus::DeviceIdAllocator;
use crate::emulator::config::led_matrix::LedMatrixModule;
use crate::emulator::transport::{ChannelRelay, Transport, TransportError, TransportReporter};
use crate::emulator::{BusConfig, DeviceEvent, DeviceId, ErrorSender};
use figment::value::Value;
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

/// A shareable slot holding an optional pre-built transport, suitable for
/// one-time consumption by a device module that runs after the slot is
/// filled. The transport is paired with its inbound `ChannelRelay<u8>` and a
/// `TransportReporter` constructed via `TransportReporter::pending` — the
/// caller that fills this slot builds the transport before any `DeviceId`
/// exists, so the reporter starts unbound and the device module that
/// consumes the slot binds it once the device's id is allocated.
pub type TransportSlot = Arc<Mutex<Option<(Box<dyn Transport>, ChannelRelay<u8>, TransportReporter)>>>;

/// A context of application attributes that may be used by device modules during instantiation.
#[derive(Clone)]
pub struct InstantiationContext {
    /// Configured clock speed of the CPU (None signifies no throttling).
    pub clock_hz: Option<u64>,
    /// An error sender that can be cloned into any device that needs it.
    pub error_sender: Option<ErrorSender>,
    /// A pre-created transport, relay, and reporter to inject into the
    /// console device.
    ///
    /// When present, the console device module uses this transport instead of
    /// constructing one from a `TransportSpec`. The slot's contents are taken
    /// (consumed) on first use, leaving `None` in its place.
    pub console_transport: Option<TransportSlot>,
}

impl InstantiationContext {
    /// Returns a callback suitable for [`TransportSpec::to_transport_with_reporter`](super::TransportSpec::to_transport_with_reporter) that
    /// reports child-process exit as a [`DeviceEvent::TransportError`] for the given device.
    pub fn pipe_exit_reporter(&self, device_id: DeviceId) -> impl FnOnce(std::io::Error) + Send + 'static {
        let sender = self.error_sender.clone();
        move |e: std::io::Error| {
            if let Some(sender) = sender {
                let _ = sender.send(DeviceEvent::TransportError {
                    device: device_id,
                    error: TransportError::Io(e),
                });
            }
        }
    }
}

type InstantiateFn = Box<
    dyn Fn(BusConfig, u16, &HashMap<String, Value>, &InstantiationContext, Arc<Mutex<DeviceIdAllocator>>)
        -> Pin<Box<dyn Future<Output = Result<BusConfig, DeviceModuleError>> + Send>> + Send + Sync
>;

/// A registry of devices that can be configured and added to a [`BusConfig`].
pub struct DeviceRegistry {
    modules: HashMap<String, InstantiateFn>,
}

impl Default for DeviceRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl DeviceRegistry {

    /// Constructs a new instance with an empty modules map.
    pub fn new() -> Self {
        DeviceRegistry {
            modules: HashMap::new(),
        }
    }

    /// Creates a registry containing all the built-in device modules.
    pub fn with_builtins() -> Self {
        let mut r = Self::new();
        r.register(RamModule);
        r.register(RomModule);
        r.register(ConsoleModule);
        r.register(FinchModule);
        r.register(LedMatrixModule);
        r.register(LfsrModule);
        r.register(R6551Module);
        r.register(Mc6840Module);
        r.register(Mc6850Module);
        r.register(PhoebeModule);
        r.register(PicFinchModule);
        r.register(Via6522Module);
        r.register(VireoModule);
        r
    }

    /// Captures the specified [`DeviceModule`] and assigns it a name.
    /// An instance of the corresponding device can be configured and attached to a bus
    /// configuration using the [`DeviceRegistry::instantiate`] method.
    pub fn register<M>(&mut self, module: M)
    where
        M: DeviceModule + Send + Sync + Clone + 'static,
    {
        let name = module.name().to_string();
        self.modules.insert(name, Box::new(move |bus_config, address, attrs, context, id_allocator| {
            let m = module.clone();
            let a = attrs.clone();
            let c = context.clone();
            Box::pin(async move {
                m.instantiate(bus_config, address, &a, &c, id_allocator).await
            })
        }));
    }

    /// Instantiates a registered device type, configures it according to the given attributes,
    /// and attaches it to the given bus configuration.
    /// # Arguments
    /// * name - name of a registered device type
    /// * bus_config - the bus configuration to which the device instance will be attached
    /// * address - starting address at which the device will be mapped
    /// * attributes - configuration attributes for the device
    pub async fn instantiate(&self, name: &str, bus_config: BusConfig, address: u16,
                             attributes: &HashMap<String, Value>,
                             context: &InstantiationContext,
                             id_allocator: Arc<Mutex<DeviceIdAllocator>>)
                             -> Result<BusConfig, DeviceModuleError> {
        let f = self.modules.get(name)
            .ok_or_else(|| DeviceModuleError::Config(format!("unknown device type: {name}")))?;
        f(bus_config, address, attributes, context, id_allocator).await
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emulator::bus::DeviceIdAllocator;

    #[derive(Clone)]
    struct MockModule {
        name: &'static str,
        tag: Option<&'static str>,
    }

    impl MockModule {
        fn from_name(name: &'static str) -> Self {
            MockModule {
                name,
                tag: None,
            }
        }

        fn from_name_and_tag(name: &'static str, tag: &'static str) -> Self {
            MockModule {
                name,
                tag: Some(tag),
            }
        }
    }

    impl DeviceModule for MockModule {
        fn name(&self) -> &'static str {
            self.name
        }

        async fn instantiate(&self, _bus_config: BusConfig, _address: u16,
                             _attributes: &HashMap<String, Value>, _context: &InstantiationContext,
                             _id_allocator: Arc<Mutex<DeviceIdAllocator>>)
                -> Result<BusConfig, DeviceModuleError> {
            Err(DeviceModuleError::Config(self.tag.unwrap_or(self.name).to_string()))
        }
    }

    #[tokio::test]
    async fn instantiate_unknown_device_type() {
        let registry = DeviceRegistry::default();
        let bus_config = BusConfig::new();
        let context = InstantiationContext { clock_hz: None, error_sender: None, console_transport: None };
        let id_allocator = Arc::new(Mutex::new(DeviceIdAllocator::new()));
        let attributes: HashMap<String, Value> = HashMap::new();
        let err = registry.instantiate("foobar", bus_config, 0x55aa, &attributes, &context, id_allocator)
            .await.err().unwrap();
        assert!(matches!(err, DeviceModuleError::Config(s) if s.contains("foobar")))
    }

    #[tokio::test]
    async fn instantiate_routes_to_correct_module() {
        let mut registry = DeviceRegistry::default();
        let attributes: HashMap<String, Value> = HashMap::new();
        let context = InstantiationContext { clock_hz: None, error_sender: None, console_transport: None };
        let id_allocator = Arc::new(Mutex::new(DeviceIdAllocator::new()));
        registry.register(MockModule::from_name("alpha"));
        registry.register(MockModule::from_name("beta"));
        let err_a = registry.instantiate("alpha", BusConfig::new(), 0x55aa, &attributes, &context, id_allocator.clone())
            .await.err().unwrap();
        let err_b = registry.instantiate("beta", BusConfig::new(), 0x55aa, &attributes, &context, id_allocator.clone())
            .await.err().unwrap();
        assert!(matches!(err_a, DeviceModuleError::Config(s) if s == "alpha"));
        assert!(matches!(err_b, DeviceModuleError::Config(s) if s == "beta"));
    }

    #[tokio::test]
    async fn register_replaces_existing_module() {
        let mut registry = DeviceRegistry::default();
        let attributes: HashMap<String, Value> = HashMap::new();
        registry.register(MockModule::from_name_and_tag("alpha", "alpha1"));
        registry.register(MockModule::from_name_and_tag("alpha", "alpha2"));
        let context = InstantiationContext { clock_hz: None, error_sender: None, console_transport: None };
        let id_allocator = Arc::new(Mutex::new(DeviceIdAllocator::new()));
        let err_a = registry.instantiate("alpha", BusConfig::new(), 0x55aa, &attributes, &context, id_allocator)
            .await.err().unwrap();
        assert!(matches!(err_a, DeviceModuleError::Config(s) if s == "alpha2"));
    }

    #[tokio::test]
    async fn with_builtins_has_ram_module() {
        let registry = DeviceRegistry::with_builtins();
        let context = InstantiationContext { clock_hz: None, error_sender: None, console_transport: None };
        let id_allocator = Arc::new(Mutex::new(DeviceIdAllocator::new()));
        let mut attributes: HashMap<String, Value> = HashMap::new();
        attributes.insert("size".to_string(), Value::from(65536));
        let bus_config = registry.instantiate("ram", BusConfig::new(), 0, &attributes, &context, id_allocator).await.unwrap();
        let mut bus = bus_config.build();
        bus.write(0, 0x55).unwrap();
        assert_eq!(bus.read(0).unwrap(), 0x55);
        bus.write(0xffff, 0xaa).unwrap();
        assert_eq!(bus.read(0xffff).unwrap(), 0xaa);
    }

}