use super::{DeviceModule, DeviceModuleError, InstantiationContext, LedMatrixGeometry, TransportSpec, TransportSpecFormat};
use crate::emulator::bus::DeviceIdAllocator;
use crate::emulator::device::display::DEFAULT_FRAME_RATE_HZ;
use crate::emulator::device::led_matrix::compositing::default_palette;
use crate::emulator::device::led_matrix::{LedMatrix, PIXELS_PER_MATRIX};
use crate::emulator::{AddressRange, BusConfig, IoDevice};
use figment::providers::Serialized;
use figment::value::{Dict, Value};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

// Matrix counts this device accepts (spec §3, design doc §2).
const VALID_MATRIX_COUNTS: [u32; 4] = [1, 2, 4, 8];

/// RGB LED matrix display adapter module (`display/matrix`).
///
/// Not IRQ-capable (design doc §1: "No control/status registers and no IRQ" -- swaps are always
/// synchronous), so device IDs come from [`DeviceIdAllocator::next_available`] rather than
/// [`DeviceIdAllocator::for_irq`].
///
/// Pixel memory and the command/data register pair are two disjoint bus ranges rather than one
/// contiguous region: pixel memory is sized from `arrangement` and based at the device's
/// `address`, while the register pair is based at the separately configured, required
/// `register-address` (data register immediately follows at `register-address + 1`). This keeps
/// pixel memory -- likely to be placed at a 1-KiB/N-KiB-aligned boundary -- free of the
/// fragmentation two extra register bytes tacked onto its end would otherwise cause. The two
/// ranges are mapped onto the same `DeviceId` via `BusConfig::extend_device`, mirroring how
/// `CharDisplay`'s optional keyboard sub-range is wired (`config::display`).
#[derive(Clone)]
pub struct LedMatrixModule;

#[derive(Deserialize)]
struct LedMatrixAttributes {
    /// Physical arrangement of the daisy-chained matrices (design doc §2.2), `COLSxROWS` (e.g.
    /// `"2x1"`), determining both the matrix count (`columns * rows`) and how bus addresses map
    /// onto the composed canvas -- see `led_matrix::mod`'s "Arrangement-aware pixel addressing"
    /// doc comment. There is deliberately no separate `matrix-count` attribute: since a matrix
    /// count alone doesn't say how the matrices are wired, requiring both invited the two to
    /// silently disagree; `arrangement` alone is both necessary and sufficient.
    arrangement: String,
    #[serde(rename = "register-address")]
    register_address: u16,
    frame_rate_hz: Option<u32>,
    transport: Option<TransportSpecFormat>,
}

/// Parses `arrangement`'s `COLSxROWS` syntax into `(columns, rows)`. This is the single source of
/// truth for both bus addressing (design doc §2.2) and on-screen layout: the resulting `columns`
/// flows to the debugger via `LedMatrixGeometry::columns` and to the external companion binary via
/// the wire protocol header's `columns` field (`plan/led-matrix-external-protocol.md` §4), so
/// neither host has its own independent arrangement to configure.
fn parse_arrangement(spec: &str) -> Result<(u32, u32), String> {
    let (cols_str, rows_str) = spec.split_once('x')
        .ok_or_else(|| format!("display/matrix: arrangement must be COLSxROWS, got {spec:?}"))?;
    let columns: u32 = cols_str.parse()
        .map_err(|_| format!("display/matrix: invalid column count in arrangement {spec:?}"))?;
    let rows: u32 = rows_str.parse()
        .map_err(|_| format!("display/matrix: invalid row count in arrangement {spec:?}"))?;
    if columns == 0 || rows == 0 {
        return Err(format!("display/matrix: arrangement columns and rows must both be at least 1, got {spec:?}"));
    }
    Ok((columns, rows))
}

impl DeviceModule for LedMatrixModule {

    fn name(&self) -> &'static str {
        "display/matrix"
    }

    async fn instantiate(&self, bus_config: BusConfig, address: u16,
                         attributes: &HashMap<String, Value>, context: &InstantiationContext,
                         id_allocator: Arc<Mutex<DeviceIdAllocator>>)
            -> Result<BusConfig, DeviceModuleError> {

        let attrs = Dict::from_iter(attributes.clone());
        let config: LedMatrixAttributes = figment::Figment::new()
            .merge(Serialized::defaults(attrs))
            .extract()
            .map_err(|e| DeviceModuleError::Config(format!("configuration error: {e}")))?;

        let (cols, rows) = parse_arrangement(&config.arrangement).map_err(DeviceModuleError::Config)?;
        let matrix_count = cols * rows;

        if !VALID_MATRIX_COUNTS.contains(&matrix_count) {
            return Err(DeviceModuleError::Config(format!(
                "display/matrix: arrangement {:?} implies {matrix_count} matrices, but matrix count must be one of {VALID_MATRIX_COUNTS:?}",
                config.arrangement)));
        }

        let frame_rate_hz = config.frame_rate_hz.unwrap_or(DEFAULT_FRAME_RATE_HZ);

        let transport_spec = config.transport
            .map(TransportSpec::try_from)
            .transpose()
            .map_err(DeviceModuleError::Config)?;
        // The external protocol's per-message sends (`plan/led-matrix-external-protocol.md`) rely
        // on `Transport::send_bytes`'s all-or-nothing contract, which only `PipeTransport`
        // provides (see `config::display`'s identical restriction) -- reject any other kind
        // rather than silently desyncing the stream on the first dropped message.
        if let Some(spec) = &transport_spec
            && !matches!(spec, TransportSpec::Pipe { .. })
        {
            return Err(DeviceModuleError::Config(
                "display/matrix requires a pipe transport; \
                 tcp/unix/pty transports don't support the atomic bulk-send this protocol needs"
                    .to_string()));
        }

        let device_id = id_allocator.lock().unwrap().next_available();

        let pixel_bytes = matrix_count * PIXELS_PER_MATRIX as u32;
        let pixel_range = AddressRange::new(address, address + (pixel_bytes as u16 - 1));
        let register_range = AddressRange::new(config.register_address, config.register_address + 1);

        let mut device = LedMatrix::new(
            self.name(),
            pixel_range,
            register_range,
            matrix_count,
            cols,
            context.clock_hz,
            frame_rate_hz,
            default_palette(),
        );

        if let Some(sender) = &context.log_sender {
            device.set_log_sender(sender.clone());
        }

        // Both slots (design doc §10) are consumed the same way `display_frame_sink`/
        // `display_geometry_sink` are: present only when a host (the debugger) wants to receive
        // this device's output, absent (a no-op here) for the plain `emma65` CLI.
        if let Some(slot) = &context.led_matrix_geometry_sink {
            *slot.lock().unwrap() = Some(LedMatrixGeometry { matrices: matrix_count, columns: cols });
        }
        if let Some(slot) = &context.led_matrix_frame_sink
            && let Some(sender) = slot.lock().unwrap().take()
        {
            device.attach_frame_sink(sender);
        }

        if let Some(transport_spec) = transport_spec {
            // Size the pipe's ring to hold every matrix's block message at once, not just one --
            // a single `CMD_SWAP` bitmask (or one auto-refresh tick) can swap every configured
            // matrix in the same synchronous call, and `swap_matrix` sends one block message
            // (1 tag + 1 index + 1024 pixel bytes = 1026 bytes) per swap with no yield in
            // between. A ring sized for exactly one message leaves zero room for the next send
            // the instant the first message lands, so it races the background pipe-writer task's
            // drain -- `send_bytes`'s atomic push then silently drops the whole message on loss,
            // observed as some matrices in a multi-matrix swap never updating on the peripheral
            // (protocol module stays private, so its consts aren't importable here; hand-computed
            // per the design doc's message layout).
            let capacity = matrix_count as usize * 1026;

            let (transport, _relay) = transport_spec
                .to_transport_with_reporter_and_capacity(
                    context.transport_reporter(device.identity()),
                    context.pipe_exit_reporter(device.identity()),
                    Some(capacity))
                .await
                .map_err(DeviceModuleError::Transport)?;
            device.attach_external_transport(transport);
        }

        let bus_config = bus_config.device(pixel_range, device_id, Box::new(device))
            .map_err(DeviceModuleError::BusConfig)?;

        bus_config.extend_device(register_range, device_id)
            .map_err(DeviceModuleError::BusConfig)
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> InstantiationContext {
        InstantiationContext {
            clock_hz: None,
            error_sender: None,
            console_transport: None,
            keyboard_transport: None,
            log_sender: None,
            display_frame_sink: None,
            display_geometry_sink: None,
            led_matrix_frame_sink: None,
            led_matrix_geometry_sink: None,
        }
    }

    // Comfortably clear of every pixel range this file's tests build (up to 8 * 1024 = 0x2000
    // bytes starting at 0x8000), so the default doesn't accidentally overlap and mask a real
    // validation failure with an unrelated overlap error.
    const REGISTER_ADDRESS: u16 = 0xA000;

    /// Builds attributes for a device with `matrix_count` matrices in a single column -- the
    /// simplest arrangement that reaches a given count, for tests that don't care about layout.
    fn attributes(matrix_count: u32) -> HashMap<String, Value> {
        attributes_with_arrangement(&format!("1x{matrix_count}"))
    }

    fn attributes_with_arrangement(arrangement: &str) -> HashMap<String, Value> {
        let mut attributes = HashMap::new();
        attributes.insert("arrangement".to_string(), Value::from(arrangement));
        attributes.insert("register-address".to_string(), Value::from(REGISTER_ADDRESS));
        attributes
    }

    #[tokio::test]
    async fn instantiate_with_valid_matrix_count_succeeds() {
        let id_allocator = Arc::new(Mutex::new(DeviceIdAllocator::new()));
        let result = LedMatrixModule.instantiate(
            BusConfig::new(), 0x8000, &attributes(4), &context(), id_allocator).await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn instantiate_without_arrangement_fails() {
        let id_allocator = Arc::new(Mutex::new(DeviceIdAllocator::new()));
        let result = LedMatrixModule.instantiate(
            BusConfig::new(), 0x8000, &HashMap::new(), &context(), id_allocator).await;

        assert!(matches!(result, Err(DeviceModuleError::Config(_))));
    }

    #[tokio::test]
    async fn instantiate_with_arrangement_implying_invalid_matrix_count_fails() {
        let id_allocator = Arc::new(Mutex::new(DeviceIdAllocator::new()));
        let result = LedMatrixModule.instantiate(
            BusConfig::new(), 0x8000, &attributes(3), &context(), id_allocator).await;

        match result {
            Err(DeviceModuleError::Config(message)) => assert!(message.contains("matrix count")),
            Err(other) => panic!("expected DeviceModuleError::Config, got a different error variant: {other}"),
            Ok(_) => panic!("expected DeviceModuleError::Config, got Ok"),
        }
    }

    #[tokio::test]
    async fn instantiate_without_register_address_fails() {
        let id_allocator = Arc::new(Mutex::new(DeviceIdAllocator::new()));
        let mut attributes = HashMap::new();
        attributes.insert("arrangement".to_string(), Value::from("1x4"));
        let result = LedMatrixModule.instantiate(
            BusConfig::new(), 0x8000, &attributes, &context(), id_allocator).await;

        assert!(matches!(result, Err(DeviceModuleError::Config(_))));
    }

    #[tokio::test]
    async fn pixel_range_sized_from_matrix_count_and_registers_placed_separately() {
        let id_allocator = Arc::new(Mutex::new(DeviceIdAllocator::new()));
        let bus_config = LedMatrixModule.instantiate(
            BusConfig::new(), 0x8000, &attributes(2), &context(), id_allocator).await.unwrap();
        let mut bus = bus_config.build();

        let pixel_bytes = 2 * PIXELS_PER_MATRIX as u16;
        // The last pixel byte of the second matrix must round-trip, proving the device's claimed
        // pixel range covers all `matrix_count * PIXELS_PER_MATRIX` bytes, not just the first
        // matrix's -- and that the gap up to `register-address` is not part of the device.
        bus.write(0x8000 + pixel_bytes - 1, 0x42).unwrap();
        assert_eq!(bus.read(0x8000 + pixel_bytes - 1).unwrap(), 0x42);
        // Command and data registers live at the separately configured `register-address`, not
        // immediately after pixel memory.
        assert!(bus.write(REGISTER_ADDRESS, 0).is_ok());
        assert!(bus.write(REGISTER_ADDRESS + 1, 0).is_ok());
    }

    #[tokio::test]
    async fn instantiate_without_transport_attribute_succeeds() {
        let id_allocator = Arc::new(Mutex::new(DeviceIdAllocator::new()));
        let result = LedMatrixModule.instantiate(
            BusConfig::new(), 0x8000, &attributes(4), &context(), id_allocator).await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn rejects_non_pipe_transport_spec() {
        let mut attributes = attributes(4);
        attributes.insert("transport".to_string(), Value::from("unix:/tmp/emma65_test_led_matrix.sock"));
        let id_allocator = Arc::new(Mutex::new(DeviceIdAllocator::new()));

        let result = LedMatrixModule.instantiate(
            BusConfig::new(), 0x8000, &attributes, &context(), id_allocator).await;

        match result {
            Err(DeviceModuleError::Config(message)) => assert!(message.contains("pipe transport")),
            Err(other) => panic!("expected DeviceModuleError::Config, got a different error variant: {other}"),
            Ok(_) => panic!("expected DeviceModuleError::Config, got Ok"),
        }
    }

    #[tokio::test]
    async fn attaches_pipe_transport_and_sends_header_immediately() {
        let mut attributes = attributes(4);
        attributes.insert("transport".to_string(), Value::from("pipe:/usr/bin/cat"));
        let id_allocator = Arc::new(Mutex::new(DeviceIdAllocator::new()));

        let result = LedMatrixModule.instantiate(
            BusConfig::new(), 0x8000, &attributes, &context(), id_allocator).await;

        // End-to-end smoke test with a real spawned child: confirms the computed ring capacity
        // is accepted by `PipeTransport::spawn_with_capacity` and `attach_external_transport`'s
        // immediate header send doesn't panic against a live pipe.
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn transport_capacity_holds_every_matrix_block_message_from_one_swap_burst() {
        // Regression test for a real bug: a single `CMD_SWAP` bitmask (or one auto-refresh tick)
        // can swap every configured matrix in the same synchronous call, and `swap_matrix` sends
        // one 1026-byte block message per swap with no yield in between (design doc §10). A ring
        // sized for exactly one message (the original bug) leaves zero room for the next `send_bytes`
        // the instant the first message lands, so it races the background pipe-writer task's drain
        // and silently drops the loser -- observed live as some matrices in a multi-matrix swap
        // never updating on `emma65-led-matrix`. This spawns a real `cat` child (same as production)
        // with the config module's actual capacity formula and fires `matrix_count` full-size block
        // messages back-to-back with no `.await` between them -- the exact shape of one `CMD_SWAP`
        // burst -- asserting every single one is accepted (`send_bytes` returns `true`), not just
        // the first and last.
        let matrix_count = 8u32;
        let capacity = matrix_count as usize * 1026;
        let (sender, _receiver) = crate::emulator::device_event_channel();
        let reporter = crate::emulator::TransportReporter::pending(Some(sender));

        let spec = TransportSpec::Pipe { command: vec!["/usr/bin/cat".to_string()] };
        let (mut transport, _relay) = spec
            .to_transport_with_reporter_and_capacity(reporter, |_| {}, Some(capacity))
            .await
            .unwrap();

        for matrix_index in 0..matrix_count as u8 {
            let mut block = vec![matrix_index; PIXELS_PER_MATRIX + 2];
            block[0] = 1; // MSG_BLOCK tag
            block[1] = matrix_index;
            assert!(
                transport.send_bytes(&block),
                "block message for matrix {matrix_index} was dropped -- ring capacity too small for a full swap burst"
            );
        }
    }

    #[tokio::test]
    async fn instantiate_with_valid_arrangement_succeeds() {
        let id_allocator = Arc::new(Mutex::new(DeviceIdAllocator::new()));

        let result = LedMatrixModule.instantiate(
            BusConfig::new(), 0x8000, &attributes_with_arrangement("2x2"), &context(), id_allocator).await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn instantiate_with_malformed_arrangement_fails() {
        let id_allocator = Arc::new(Mutex::new(DeviceIdAllocator::new()));

        let result = LedMatrixModule.instantiate(
            BusConfig::new(), 0x8000, &attributes_with_arrangement("not-an-arrangement"), &context(), id_allocator).await;

        assert!(matches!(result, Err(DeviceModuleError::Config(_))));
    }

    #[tokio::test]
    async fn single_column_arrangement_matches_the_original_contiguous_layout() {
        // With a "1xN" arrangement, matrix 1's pixels start at byte offset `PIXELS_PER_MATRIX`,
        // exactly like the original one-matrix-per-1024-contiguous-bytes layout.
        let id_allocator = Arc::new(Mutex::new(DeviceIdAllocator::new()));
        let bus_config = LedMatrixModule.instantiate(
            BusConfig::new(), 0x8000, &attributes_with_arrangement("1x2"), &context(), id_allocator).await.unwrap();
        let mut bus = bus_config.build();

        bus.write(0x8000, 0x11).unwrap();
        bus.write(0x8000 + PIXELS_PER_MATRIX as u16, 0x22).unwrap();

        assert_eq!(bus.read(0x8000).unwrap(), 0x11);
        assert_eq!(bus.read(0x8000 + PIXELS_PER_MATRIX as u16).unwrap(), 0x22);
    }

    #[tokio::test]
    async fn device_id_is_not_irq_capable() {
        let id_allocator = Arc::new(Mutex::new(DeviceIdAllocator::new()));
        let _bus_config = LedMatrixModule.instantiate(
            BusConfig::new(), 0x8000, &attributes(1), &context(), id_allocator.clone()).await.unwrap();

        // A plain next_available() id falls outside the IRQ bitmask range, so every IRQ line
        // (including 0) must remain unclaimed.
        assert!(id_allocator.lock().unwrap().for_irq(0).is_ok());
    }
}
