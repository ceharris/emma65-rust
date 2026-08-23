use super::{ConsoleModule, DeviceModule, DeviceModuleError, FinchModule, LfsrModule, Mc6840Module, Mc6850Module, PhoebeModule, PicFinchModule, R6551Module, RamModule, RomModule, Via6522Module, VireoModule};
use crate::emulator::bus::DeviceIdAllocator;
use crate::emulator::config::display::CharDisplayModule;
use crate::emulator::config::keyboard::KeyboardModule;
use crate::emulator::config::led_matrix::LedMatrixModule;
use crate::emulator::device::display::DisplayFrame;
use crate::emulator::transport::{ChannelRelay, Transport, TransportError, TransportReporter};
use crate::emulator::{BusConfig, DeviceEvent, ErrorSender, LogSender};
use figment::value::Value;
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

/// A shareable slot holding an optional pre-built transport, suitable for
/// one-time consumption by a device module that runs after the slot is
/// filled. The transport is paired with its inbound `ChannelRelay<u8>` and a
/// `TransportReporter` constructed via `TransportReporter::pending` — the
/// caller that fills this slot builds the transport before any `DeviceId`
/// exists, so the reporter starts unbound and the device module that
/// consumes the slot binds it once the device's id is allocated.
pub type TransportSlot = Arc<Mutex<Option<(Box<dyn Transport>, ChannelRelay<u8>, TransportReporter)>>>;

/// A shareable slot a display device module fills with the sending half of its composited-frame
/// push channel during instantiation, mirroring [`TransportSlot`]'s shape: the caller (the
/// debugger's session setup code) creates the channel and stashes the sender here before bus
/// construction runs, then keeps the receiver for its own bridge task -- see design doc §9.
pub type DisplayFrameSlot = Arc<Mutex<Option<mpsc::Sender<DisplayFrame>>>>;

/// A shareable slot a display device module fills with its fixed pixel/cell geometry during
/// instantiation. Unlike [`DisplayFrameSlot`], nothing is "taken" from this slot -- it's simply
/// set once, since (unlike a frame push channel) a plain value can be read any number of times
/// without being consumed.
pub type DisplayGeometrySlot = Arc<Mutex<Option<DisplayGeometry>>>;

/// A display device's fixed pixel/cell geometry, known entirely from its configuration
/// attributes (columns, rows, frame rate) and unaffected by anything the CPU does afterward
/// (design doc §9). Handed to the debugger via [`InstantiationContext::display_geometry_sink`]
/// so its display panel can size its canvas on mount, before any frame has been composited.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DisplayGeometry {
    /// Grid width in cells.
    pub columns: u32,
    /// Grid height in cells.
    pub rows: u32,
    /// Composited frame width in pixels (`columns * 8`, the fixed glyph width -- design doc §7).
    pub pixel_width: u32,
    /// Composited frame height in pixels (`rows * 8`, the fixed glyph height -- design doc §7).
    pub pixel_height: u32,
    /// Configured vsync cadence in Hz (design doc §6).
    pub frame_rate_hz: u32,
}

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
    /// A pre-created transport, relay, and reporter to inject into the
    /// keyboard device, mirroring [`InstantiationContext::console_transport`]. Unlike the console,
    /// the keyboard device module exposes no `transport=` attribute of its own -- this slot is the
    /// only way it ever receives input.
    pub keyboard_transport: Option<TransportSlot>,
    /// Shared sender for diagnostic messages (e.g. device `reset()`), cloned into any device
    /// module that calls `set_log_sender`. `None` means no file sink is configured; devices keep
    /// their own default `log`-crate-backed sender in that case.
    pub log_sender: Option<LogSender>,
    /// A pre-created slot for a display device to hand back the sending half of its
    /// composited-frame push channel (design doc §9). `None` when no host wants to receive
    /// frames (e.g. the plain `emma65` CLI) -- a display device configured in that case simply
    /// composites nothing.
    pub display_frame_sink: Option<DisplayFrameSlot>,
    /// A pre-created slot for a display device to report its fixed pixel/cell geometry (design
    /// doc §9), read by the debugger's `get_display_geometry` command.
    pub display_geometry_sink: Option<DisplayGeometrySlot>,
}

impl InstantiationContext {
    /// Returns a callback suitable for [`TransportSpec::to_transport_with_reporter`](super::TransportSpec::to_transport_with_reporter) that
    /// reports child-process exit as a [`DeviceEvent::TransportError`] for the device identified
    /// by `identity` (as returned by [`IoDevice::identity`](crate::emulator::IoDevice::identity)).
    pub fn pipe_exit_reporter(&self, identity: impl Into<String>) -> impl FnOnce(std::io::Error) + Send + 'static {
        let sender = self.error_sender.clone();
        let device = identity.into();
        move |e: std::io::Error| {
            if let Some(sender) = sender {
                let _ = sender.send(DeviceEvent::TransportError {
                    device,
                    error: TransportError::Io(e),
                });
            }
        }
    }

    /// Returns a [`TransportReporter`] bound to `identity` (as returned by
    /// [`IoDevice::identity`](crate::emulator::IoDevice::identity)), backed by this
    /// context's `error_sender` (a silent no-op reporter if none is
    /// configured) — the reporter a `DeviceModule::instantiate` passes into
    /// transport construction so connect/disconnect/error events actually
    /// reach the host.
    pub fn transport_reporter(&self, identity: impl Into<String>) -> TransportReporter {
        let reporter = TransportReporter::pending(self.error_sender.clone());
        reporter.bind(identity);
        reporter
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
        r.register(KeyboardModule);
        r.register(FinchModule);
        r.register(LedMatrixModule);
        r.register(CharDisplayModule);
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

    #[test]
    fn transport_reporter_is_bound_and_reports_through_error_sender() {
        let (sender, mut receiver) = crate::emulator::device_event_channel();
        let context = InstantiationContext { clock_hz: None, error_sender: Some(sender), console_transport: None, keyboard_transport: None, log_sender: None, display_frame_sink: None, display_geometry_sink: None };

        let reporter = context.transport_reporter("test-device");
        reporter.report_connected(None);

        match receiver.try_recv() {
            Ok(DeviceEvent::TransportConnected { device, .. }) => assert_eq!(device, "test-device"),
            other => panic!("expected TransportConnected, got {other:?}"),
        }
    }

    #[test]
    fn transport_reporter_is_a_silent_no_op_when_no_error_sender_is_configured() {
        let context = InstantiationContext { clock_hz: None, error_sender: None, console_transport: None, keyboard_transport: None, log_sender: None, display_frame_sink: None, display_geometry_sink: None };

        let reporter = context.transport_reporter("test-device");
        // Must not panic with no error sender configured.
        reporter.report_connected(None);
    }

    #[tokio::test]
    async fn instantiate_unknown_device_type() {
        let registry = DeviceRegistry::default();
        let bus_config = BusConfig::new();
        let context = InstantiationContext { clock_hz: None, error_sender: None, console_transport: None, keyboard_transport: None, log_sender: None, display_frame_sink: None, display_geometry_sink: None };
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
        let context = InstantiationContext { clock_hz: None, error_sender: None, console_transport: None, keyboard_transport: None, log_sender: None, display_frame_sink: None, display_geometry_sink: None };
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
        let context = InstantiationContext { clock_hz: None, error_sender: None, console_transport: None, keyboard_transport: None, log_sender: None, display_frame_sink: None, display_geometry_sink: None };
        let id_allocator = Arc::new(Mutex::new(DeviceIdAllocator::new()));
        let err_a = registry.instantiate("alpha", BusConfig::new(), 0x55aa, &attributes, &context, id_allocator)
            .await.err().unwrap();
        assert!(matches!(err_a, DeviceModuleError::Config(s) if s == "alpha2"));
    }

    #[tokio::test]
    async fn with_builtins_has_ram_module() {
        let registry = DeviceRegistry::with_builtins();
        let context = InstantiationContext { clock_hz: None, error_sender: None, console_transport: None, keyboard_transport: None, log_sender: None, display_frame_sink: None, display_geometry_sink: None };
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