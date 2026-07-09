# BusConfig: Multi-Region Devices and Conditional Chip-Select

Implements the design direction from [issue #139](https://github.com/ceharris/emma65-rust/issues/139).

## Context

`BusConfig`/`Bus` (`src/emulator/bus/mod.rs`) currently model every mapped device as
exactly one `Region::Device { range, id, device }` entry that owns its
`Box<dyn IoDevice>` directly. Two limitations follow from that:

1. A single device instance cannot be mapped at more than one address range — reusing
   a `DeviceId` in a second `.device()` call is rejected with
   `BusConfigError::DuplicateDeviceId`. This blocks devices like bank-switched memory,
   which need a data window (e.g. 16K at `0x8000`) plus a separate bank-select register
   (e.g. in the `0xff00` page).
2. `IoDevice` has no way to decline an access. Every address inside a mapped region is
   assumed to be handled by whatever occupies that region, with no mechanism for a
   device to conditionally fall through to what's underneath (real-hardware
   chip-select gated by device state).

A third, related problem surfaced while designing the fix for (1): `IoDevice::read`/
`write`/`peek` receive an `offset` normalized to whichever region matched
(`addr - matched_region.start`). A device mapped at two regions of different sizes
still sees offsets that both start at 0, so it has no way to tell which region a given
call belongs to from the offset value alone. `IoDevice` needs a way to work in terms of
the absolute bus address instead, at least for devices sophisticated enough to need it.

This plan covers the `BusConfig`/`Bus`/`IoDevice` mechanism that enables all of the
above. It does **not** cover implementing an actual bank-switched device module — see
"Out of scope."

## Goals

- A device can be registered once and mapped at more than one `AddressRange`.
- A device can decline a particular address at runtime; the bus falls through to the
  next most-specific region containing that address, walking through as many declined
  regions as exist (no fixed depth limit), ultimately reaching the existing
  unmapped-address policy if nothing claims it.
- A device that needs to distinguish between its own regions does so using the
  absolute bus address, which it can classify against address information it already
  possesses (or is given at construction) — not by inferring it from an ambiguous
  offset. `Bus` never computes a device-specific offset itself; that becomes the
  device's responsibility.
- Per-device lifecycle calls (`tick`, `reset`, `irq_active`, `take_nmi`) fire once per
  device, not once per region it occupies.
- Minimal, mechanical change for existing single-region, always-responding devices
  (`Console`, `Acia6551`, `Mc6850`, `Via6522`).
- No changes required to `DeviceModule` / `DeviceRegistry` / `DeviceSpec`
  (`src/emulator/config/`) beyond passing each device its own address at construction
  (which `instantiate()` already has on hand) — the builder already threads an owned
  `BusConfig` through `instantiate()`, so a future multi-region module can just call two
  builder methods in sequence.

## Design

### 1. Move device ownership out of `Region`

Replace the owning `Device` variant with a lightweight index reference, and move
ownership into a new `devices` field shared by `BusConfig`/`Bus`:

```rust
enum Region {
    Ram { range: AddressRange, data: Vec<u8> },
    Rom { range: AddressRange, data: Vec<u8>, write_policy: RomWritePolicy },
    Device { range: AddressRange, device_index: usize },
}

pub struct Bus {
    regions: Vec<Region>,
    devices: Vec<(DeviceId, Box<dyn IoDevice>)>,
    // ...unchanged fields
}

pub struct BusConfig {
    regions: Vec<Region>,
    devices: Vec<(DeviceId, Box<dyn IoDevice>)>,
    // ...unchanged fields
}
```

`device_index` is stable because devices are only ever appended during `BusConfig`
construction and the `Vec` moves as-is into `Bus` at `build()` — no removal, no
reordering.

### 2. `BusConfig::extend_device` — map an existing device at another range

`.device()` keeps its current signature and behavior (first registration, still
rejects a duplicate `DeviceId`), but now also pushes into `devices`:

```rust
pub fn device(
    mut self,
    range: AddressRange,
    id: DeviceId,
    device: Box<dyn IoDevice>,
) -> Result<Self, BusConfigError> {
    if self.devices.iter().any(|(existing, _)| *existing == id) {
        return Err(BusConfigError::DuplicateDeviceId(id));
    }
    debug_assert_eq!(
        device.base_address(), range.start,
        "device.base_address() must match the range it's registered at"
    );
    self.check_overlap(range)?;
    let device_index = self.devices.len();
    self.devices.push((id, device));
    self.regions.push(Region::Device { range, device_index });
    Ok(self)
}
```

The `debug_assert_eq!` is a safety net, not a hard error: it catches a device whose
`base_address()` disagrees with its registered range early, in debug builds, without
imposing a runtime cost (or a new `BusConfigError` variant) in release builds.

A new method maps an *already-registered* device at an additional range:

```rust
/// Maps an additional region for a device already registered via `.device()`.
///
/// Returns `BusConfigError::UnknownDeviceId` if `id` hasn't been registered yet.
pub fn extend_device(mut self, range: AddressRange, id: DeviceId) -> Result<Self, BusConfigError> {
    let device_index = self
        .devices
        .iter()
        .position(|(existing, _)| *existing == id)
        .ok_or(BusConfigError::UnknownDeviceId(id))?;
    self.check_overlap(range)?;
    self.regions.push(Region::Device { range, device_index });
    Ok(self)
}
```

`check_overlap` is unchanged — same-size overlap is still ambiguous regardless of
whether the colliding regions belong to the same device.

New error variant in `src/emulator/error.rs`:

```rust
#[error("device region references unknown device ID {0:?}; call .device() to register it first")]
UnknownDeviceId(DeviceId),
```

A future bank-switched RAM module would call, inside one `DeviceModule::instantiate`:

```rust
bus_config
    .device(window_range, id, Box::new(bank_switched_ram))?
    .extend_device(register_range, id)?
```

### 3. `IoDevice`: absolute addressing (`base_address`, `*_absolute`, `claims`)

Add a required `base_address()` method and three defaulted `*_absolute` methods that
translate an absolute address to the existing offset-based methods:

```rust
pub trait IoDevice: Send {
    /// The address at which this device is registered via `.device()`.
    ///
    /// Used by the default `*_absolute` methods to translate an absolute bus address
    /// into a range-relative offset. A device that overrides all three `*_absolute`
    /// methods (and `claims`) is free to return any value here, since nothing else
    /// consults it.
    fn base_address(&self) -> u16;

    /// Reads a byte from `offset` relative to `base_address()`, with side effects.
    fn read(&mut self, offset: u16) -> u8;
    /// Writes `value` to `offset` relative to `base_address()`.
    fn write(&mut self, offset: u16, value: u8);
    /// Reads a byte from `offset` relative to `base_address()`, without side effects.
    fn peek(&self, offset: u16) -> u8;

    /// Reads a byte at the absolute bus address `addr`, with side effects.
    ///
    /// Default implementation subtracts `base_address()` and delegates to `read()` —
    /// correct for any device mapped at a single region. A device mapped at more than
    /// one region overrides this directly, classifying `addr` against whatever address
    /// information it retains for its own regions.
    fn read_absolute(&mut self, addr: u16) -> u8 {
        self.read(addr - self.base_address())
    }
    /// Writes `value` at the absolute bus address `addr`. See `read_absolute`.
    fn write_absolute(&mut self, addr: u16, value: u8) {
        self.write(addr - self.base_address(), value)
    }
    /// Reads a byte at the absolute bus address `addr`, without side effects. See `read_absolute`.
    fn peek_absolute(&self, addr: u16) -> u8 {
        self.peek(addr - self.base_address())
    }

    /// Returns `true` if this device currently responds to `addr`, the absolute bus
    /// address. Consulted before dispatching `*_absolute`; declining causes the bus to
    /// fall through to the next most-specific region containing `addr`, or to the
    /// unmapped-address policy if none remain.
    ///
    /// Default implementation always claims (unconditional chip-select).
    fn claims(&self, _addr: u16) -> bool { true }

    // tick(), reset(), irq_active(), take_nmi(), name() unchanged
}
```

Existing devices need: a stored address (a new field), a constructor parameter to set
it, and a one-line `base_address()` impl. No other code changes — `read`/`write`/
`peek` keep their current bodies and are reached through the default `*_absolute`
wrappers. A multi-region device (e.g. bank-switched RAM) instead overrides
`read_absolute`/`write_absolute`/`peek_absolute`/`claims` directly, using its own
address knowledge to decide whether a call targets its data window or its bank-select
register, and never needs `read`/`write`/`peek` to do anything meaningful.

### 4. Bus dispatch always uses the absolute address for devices

`RegionMatch`/`PeekMatch`'s `Device` variant now carries the absolute address instead
of an offset — `Bus` never computes a device-specific offset:

```rust
enum PeekMatch<'a> {
    Ram { data: &'a Vec<u8>, offset: usize },
    Rom { data: &'a Vec<u8>, offset: usize },
    Device { device: &'a dyn IoDevice, addr: u16 },
}

enum RegionMatch<'a> {
    Ram { data: &'a mut Vec<u8>, offset: usize },
    Rom { data: &'a Vec<u8>, offset: usize, write_policy: RomWritePolicy },
    Device { device: &'a mut dyn IoDevice, addr: u16 },
}
```

`read()`/`write()`/`peek()` call the `*_absolute` methods:

```rust
pub fn read(&mut self, addr: u16) -> Result<u8, BusError> {
    let value = match self.find_region_mut(addr) {
        Some(RegionMatch::Ram { data, offset }) => Ok(data[offset]),
        Some(RegionMatch::Rom { data, offset, .. }) => Ok(data[offset]),
        Some(RegionMatch::Device { device, addr }) => Ok(device.read_absolute(addr)),
        None => /* unchanged */,
    }?;
    // ...unchanged
}
```

`find_region_index` becomes a loop that skips indices whose device has declined
(consulting `claims(addr)` via `self.devices[device_index]`), re-running the same
most-specific-match scan over the remaining candidates:

```rust
fn find_region_index(&self, addr: u16) -> Option<usize> {
    let mut skip: Vec<usize> = Vec::new();
    loop {
        let mut best_idx: Option<usize> = None;
        let mut best_size = u32::MAX;
        for (i, region) in self.regions.iter().enumerate() {
            if skip.contains(&i) {
                continue;
            }
            let range = region.range();
            if range.contains(addr) && range.len() < best_size {
                best_size = range.len();
                best_idx = Some(i);
            }
        }
        let idx = best_idx?;
        if let Region::Device { device_index, .. } = &self.regions[idx]
            && !self.devices[*device_index].1.claims(addr)
        {
            skip.push(idx);
            continue;
        }
        return Some(idx);
    }
}
```

Region count per `Bus` is small (a handful of devices at most), so the rescan-per-decline
approach is not worth optimizing further; it stays a plain linear scan like the rest of
this module.

`find_region_mut` resolves `Ram`/`Rom` offsets as today but hands devices the absolute
address; destructuring `self` keeps `regions` and `devices` as independent borrows:

```rust
fn find_region_mut(&mut self, addr: u16) -> Option<RegionMatch<'_>> {
    let idx = self.find_region_index(addr)?;
    let Bus { regions, devices, .. } = self;
    match &mut regions[idx] {
        Region::Ram { range, data } => {
            let offset = (addr - range.start) as usize;
            Some(RegionMatch::Ram { data, offset })
        }
        Region::Rom { range, data, write_policy } => {
            let offset = (addr - range.start) as usize;
            Some(RegionMatch::Rom { data, offset, write_policy: *write_policy })
        }
        Region::Device { device_index, .. } => Some(RegionMatch::Device {
            device: devices[*device_index].1.as_mut(),
            addr,
        }),
    }
}
```

`find_region` (used by `peek`) follows the same shape with `&self`/`PeekMatch`.

### 5. Per-device lifecycle calls iterate `devices`, not `regions`

`tick_devices`, `reset_devices`, `device_irq_states`, and `take_device_nmi` currently
filter `self.regions` for `Region::Device` entries, which would call a two-region
device's `tick`/`reset`/`irq_active`/`take_nmi` twice. Switch them to iterate
`self.devices` directly:

```rust
pub fn tick_devices(&mut self, cycles: u32) {
    for (_, device) in &mut self.devices {
        device.tick(cycles);
    }
}

pub fn reset_devices(&mut self) {
    for (_, device) in &mut self.devices {
        device.reset();
    }
}

pub fn device_irq_states(&self) -> Vec<(DeviceId, bool)> {
    self.devices.iter().map(|(id, device)| (*id, device.irq_active())).collect()
}

pub fn take_device_nmi(&mut self) -> bool {
    let mut any = false;
    for (_, device) in &mut self.devices {
        any |= device.take_nmi();
    }
    any
}
```

`load_rom` is untouched (it only matches `Region::Rom`). `BusConfig::build()` copies the
new `devices` field into `Bus` alongside the existing fields.

## Testing plan

Add to `src/emulator/bus/mod.rs`'s `#[cfg(test)]` module:

- `base_address_default_absolute_delegation` — a mock device implementing only
  `base_address()`, relying on the default `*_absolute` wrappers; verify `read`/`write`/
  `peek` are reached correctly through `Bus::read`/`write`/`peek`.
- `multi_region_device_overrides_absolute_methods` — a mock that overrides
  `read_absolute`/`write_absolute`/`peek_absolute`/`claims` directly (its `read`/`write`/
  `peek` can `unreachable!()` to prove they're never called), mapped at two ranges via
  `.device()` + `.extend_device()`; verify each range routes to the right logical
  resource using the absolute address.
- `extend_device_errors_for_unknown_device_id` — `.extend_device()` before any
  `.device()` call for that `id` returns `BusConfigError::UnknownDeviceId`.
- `extend_device_still_checks_overlap` — a second same-size region at the same address
  still triggers `AmbiguousOverlap`, whether or not it shares a `DeviceId` with an
  existing region.
- `claims_false_falls_through_to_underlying_region` — a mock device that conditionally
  declines; underlying ROM/RAM shows through when declined, device value shows when
  claimed.
- `claims_false_falls_through_to_unmapped_policy` — a declining device with nothing
  underneath; verify both `UnmappedPolicy::DefaultValue` and `UnmappedPolicy::Error`.
- `claims_walks_multiple_shadow_levels` — three nested regions (outer ROM, two
  independently-declining devices) confirm the walk isn't limited to one fallthrough.
- `tick_reset_irq_nmi_called_once_per_device` — a call-counting mock device mapped at
  two regions; assert each lifecycle method fires exactly once per `Bus`-level call,
  not once per region.
- Update the existing `MockDevice` test helper to add a `base_address()` impl (now
  required); confirm all existing tests (`most_specific_wins_device_shadows_rom`,
  `device_offset_translation`, `duplicate_device_id_error`,
  `peek_does_not_trigger_device_side_effects`, etc.) continue to pass.

## Migration / compatibility notes

- `IoDevice` gains one **required** method, `base_address()`. None of the four existing
  devices (`Console`, `Acia6551`, `Mc6850`, `Via6522`) currently store their own
  address, and each has a bare `new() -> Self` constructor — so each needs a new
  `address: u16` field, a constructor parameter, and a one-line `base_address()` impl.
  Every corresponding `*Module::instantiate` (`src/emulator/config/{console,acia6551,
  mc6850,via6522}.rs`) already has `address: u16` on hand and can pass it straight into
  the constructor call. Confirmed small and mechanical across all four.
- `read_absolute`/`write_absolute`/`peek_absolute` are defaulted — no change to
  existing devices' `read`/`write`/`peek` bodies.
- `Region` is a private enum, so its internal representation change is invisible outside
  `src/emulator/bus/mod.rs`.
- `BusConfigError` gains `UnknownDeviceId`; it is not `#[non_exhaustive]`, but no
  exhaustive match over it exists in this crate today (call sites use `matches!` against
  specific variants) — worth a final grep during implementation to confirm.
- No changes to `DeviceModule`, `DeviceRegistry`, or `DeviceSpec` beyond the constructor
  parameter changes above.

## Follow-up: rename `read`/`write`/`peek` to `read_relative`/`write_relative`/`peek_relative` — done

Once `read_absolute`/`write_absolute`/`peek_absolute` existed, the existing `read`/
`write`/`peek` methods were renamed to `read_relative`/`write_relative`/`peek_relative`,
making the two families of methods symmetric and self-explanatory for implementers
deciding which pair to use.

This was a large mechanical diff — existing unit tests called `.read(...)`/
`.write(...)`/`.peek(...)` directly on the concrete device types by name (hundreds of
call sites, mostly in `via6522.rs`'s test suite), and `console.rs` additionally mixed in
`Bus::read`/`Bus::write` calls that had to **not** be renamed. The user performed the
rename using their IDE's AST-based refactoring tool rather than scripted find/replace,
after the rest of this plan was implemented and merged as PR #140. All tests pass and a
leftover clippy warning surfaced by the rename has been fixed.

## Out of scope

- Implementing an actual bank-switched RAM/ROM device module. That's a follow-up story
  once this groundwork lands.
- Any `DeviceSpec`/TOML syntax for declaring a device's additional regions. A future
  bank-switched module can read whatever attributes it needs (e.g. a `register`
  attribute alongside the `type@address` window) and call `.device()` +
  `.extend_device()` itself — no config-layer changes are required by this plan.

## Open questions for review

None currently open — naming, the `base_address`/`*_absolute` split, and the
`debug_assert_eq!` safety net have all been confirmed.
