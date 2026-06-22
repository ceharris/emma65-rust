# Hex Loader Implementation Plan

## Goal

Add support for loading Intel Hex (`.hex`, `.ihx`) and Motorola S-Record (`.s19`)
files into RAM or ROM memory devices, in addition to the existing binary (`.bin`)
support.

## Return type

`load_image` and all internal format functions return `Result<Option<u16>, LoadError>`.
The `Option<u16>` is a start/entry address when the file contains one:

- `load_binary` → always `Ok(None)`
- `load_intel_hex` → `Ok(Some(addr))` if a type `05` Start Linear Address record was
  seen (lower 16 bits of its 4-byte field); `Ok(None)` if only a type `01` EOF was seen.
  Type `05` may appear anywhere before type `01`; capture it and continue until `01`.
  Missing type `01` is a format error.
- `load_srec` → `Ok(Some(addr))` from the 2-byte address in the `S9` record, which
  also terminates parsing. Missing `S9` is a format error.

Callers in `memory.rs` discard the `Option<u16>` with `?` for now.

## Preamble handling

Both formats allow an optional preamble before the first record marker:

- **Intel Hex**: if the first byte is not `:`, consume bytes until after the first LF
  (optionally preceded by CR), then look for `:`. Repeat until `:` is found.
- **S-Record**: if the first byte is not `S`, consume bytes until after the first LF,
  then look for `S` as the first non-preamble byte. Repeat until `S` is found.

Once the first record marker is found, the file must consist exclusively of legitimate
records (strict parsing).

## Inter-record whitespace

Between records (trailing after checksum, surrounding newlines, leading before the
next marker), the following bytes are silently ignored:

- CR (`\r`), LF (`\n`), space (` `), tab (`\t`), NUL (`\0`)

NUL characters are included to handle hex files produced by teletype-era tools that
pad after newlines with ASCII NUL. NUL inside a record (within hex digit sequences)
is a format error.

## Trailing record content

After a valid checksum:

- Whitespace (CR, LF, space, tab, NUL) → ignore
- Any other byte → format error

## Step 1 — Create `src/emulator/config/loader.rs`

```rust
pub enum LoadError {
    UnknownFormat(String),
    Format(String),
    OutOfBounds { address: u32, size: usize },
    Io(std::io::Error),
}

pub async fn load_image(
    path: &Path,
    mem: &mut Vec<u8>,
    offset: u32,
) -> Result<Option<u16>, LoadError>
```

Dispatches on the lowercased file extension:

| Extension     | Handler             |
|---------------|---------------------|
| `.bin`        | `load_binary`       |
| `.hex`, `.ihx`| `load_intel_hex`    |
| `.s19`        | `load_srec`         |
| other         | `UnknownFormat` error |

Reads the file with `tokio::fs::read` and passes the bytes to the appropriate parser.

### `load_binary`

Validates `data.len() == mem.len()` (error otherwise), then copies `data` into `mem`.
The `offset` parameter is unused. Returns `Ok(None)`.

### `load_intel_hex`

Parse records line by line:

1. Preamble scan (see above).
2. For each record:
   - `:` marker (already consumed or expected at start of line)
   - `LL` (1 byte = 2 hex chars) → byte count
   - `AAAA` (2 bytes = 4 hex chars) → 16-bit address
   - `TT` (1 byte = 2 hex chars) → record type
   - `LL` data bytes (2 hex chars each)
   - `CC` (1 byte = 2 hex chars) → checksum
   - Verify checksum: two's complement of the sum of all preceding record bytes
     must equal `CC`.
3. Record types:
   - `00` (Data): `target = address.checked_sub(offset as u16)` (underflow →
     `OutOfBounds`); check `target as usize + len <= mem.len()`; write bytes.
   - `01` (EOF): return `Ok(start_address)` where `start_address` is `Some(addr)`
     if a type `05` was seen, else `None`. Missing `01` is a format error.
   - `05` (Start Linear Address): record the lower 16 bits of the 4-byte address
     field; continue parsing.
   - Other types: format error.
4. After the checksum, ignore inter-record whitespace; non-whitespace is a format error.

### `load_srec`

Parse records line by line:

1. Preamble scan (see above).
2. For each record:
   - `S` marker (already consumed or expected at start of line)
   - Type digit character
   - `LL` (1 byte = 2 hex chars) → byte count covering address + data + checksum
   - `AAAA` (2 bytes = 4 hex chars) → 16-bit address (for S0, S1, S5, S9)
   - Data bytes: count = `LL - 3`
   - `CC` (1 byte = 2 hex chars) → checksum
   - Verify checksum: one's complement of the sum of all preceding record bytes
     (starting from `LL`) must equal `CC`.
3. Record types:
   - `S0` (Header): parse but ignore content.
   - `S1` (Data): same offset/bounds logic as Intel Hex type `00`; write bytes.
   - `S5` (Count): parse but ignore.
   - `S9` (Start Address): return `Ok(Some(address))`. Terminates parsing.
   - Other types: format error.
4. File ending without `S9` is a format error.
5. After the checksum, ignore inter-record whitespace; non-whitespace is a format error.

## Step 2 — Add `DeviceModuleError::Load` in `src/emulator/config/device.rs`

```rust
pub enum DeviceModuleError {
    BusConfig(BusConfigError),
    Transport(TransportError),
    Config(String),
    Io(std::io::Error),
    Load(loader::LoadError),   // new
}
```

Add a `Display` arm and `From<LoadError>` conversion (or `.map_err`).

## Step 3 — Modify `src/emulator/config/memory.rs`

Replace `read_image_file` with a buffer-initialization helper:

```rust
fn make_buffer(size: usize, fill: Option<u8>) -> Vec<u8> {
    match fill {
        Some(v) => vec![v; size],
        None    => (0..size).map(|_| rand::random::<u8>()).collect(),
    }
}
```

`RamModule::instantiate` — new logic:
- No `image`: keep existing `ram()` / `ram_with_fill()` paths unchanged.
- `image` present: initialize buffer with `make_buffer(config.size, config.fill)`,
  call `loader::load_image(&filename, &mut data, address as u32).await?`,
  call `bus_config.ram_with_data(range, data)`. Remove the old `fill`/`image`
  mutual-exclusion check — they are compatible for hex files.

`RomModule::instantiate` — new logic:
- Require `image` (unchanged).
- Initialize buffer with `make_buffer(config.size, config.fill)` (caller can use
  `fill=0xff` for EEPROM default).
- Call `loader::load_image(&filename, &mut data, address as u32).await?`,
  then `bus_config.rom(range, data)`.

## Step 4 — Wire up in `src/emulator/config/mod.rs`

Add `mod loader;` (private — only used by `memory.rs`).

## Address offset semantics

The `offset` passed to `load_image` is `address as u32` — the bus-mapped start
address of the segment. A hex record at address `0xc000` loaded into a 32K segment
mapped at `0x8000` is stored at `mem[0xc000 - 0x8000]` = `mem[0x4000]`.

Bounds check uses `checked_sub` to catch addresses below the segment start (underflow),
then verifies `offset + data_len <= mem.len()`.

## Tests

Unit tests in `loader.rs` covering:
- Normal data records for both formats
- Preamble skipping
- Checksum validation errors
- Out-of-bounds record addresses
- Address underflow (record address below segment offset)
- EOF/S9 termination and returned start address
- NUL bytes between records
- Non-whitespace trailing content error
- Missing EOF (`01`) / missing S9 terminator errors