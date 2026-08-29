# Memory-Mapped LED Matrix Device — Specification

## 1. Purpose and scope

This document specifies a redesigned RGB LED matrix display device, replacing the current
register/blit-command `LedMatrix` (`src/emulator/device/led_matrix.rs`). The original design's
8-register command interface bottlenecks on 6502 bus-write bandwidth for anything that updates a
significant portion of the display. This redesign maps pixel memory directly into the address
space instead, following the same double-buffering approach already used by `CharDisplay`
(`doc/memory-mapped-display-device-spec.md`).

It captures the *behavior and interface contract* of the device: its configuration surface,
address-space layout, and read/write/swap semantics. It deliberately does not specify:

- The shape of Emma65's bus device trait(s).
- How the device is instantiated, registered, or wired into bus configuration.
- The exact wire format of the transport protocol to the external peripheral — that belongs in a
  follow-up document analogous to `doc/char-display-external-protocol.md`, once this device-level
  contract is settled.

## 2. Conceptual model

The device supports 1, 2, 4, or 8 attached 32×32 RGB LED matrices, fixed at configuration time.
Each matrix occupies 1,024 contiguous bytes of pixel memory, one byte per pixel, in row-major
order (byte `row * 32 + col`). A pixel byte is an index into a single, shared 256-entry color
palette — there is no per-matrix palette.

Each palette entry stores a 16-bit RGB565 color (5 bits red, 6 bits green, 5 bits blue), not a
24-bit RGB triple — this matches the color depth real RGB LED matrix driver hardware actually
uses, unlike `CharDisplay`'s full RGB24 palette. `CMD_PALETTE_WRITE`/`CMD_PALETTE_READ` (§4.2)
still exchange full 8-bit-per-channel byte values with the CPU for a simple command interface, but
each write is masked down to the entry's native 5/6/5 bit depth before being stored, and each read
reconstructs an 8-bit value by scaling the stored bits back up (§4.2.1) — a byte written and later
read back is not guaranteed to round-trip exactly, by design.

### 2.1 Default palette

The palette is not user-configurable at bus configuration time (unlike `CharDisplay`'s optional
`palette=` file) — the device always starts with one fixed, built-in 256-entry default, matching a
real RGB LED matrix driver's own default palette exactly so behavior is comparable to actual
hardware out of the box. It remains mutable at runtime via `CMD_PALETTE_WRITE` (§4.2) like any other
palette entry. Because the transport (§7) does not transfer palette contents at startup, this exact
scheme is specified here rather than left to the implementation — the device and the companion
process are two independent implementations that must construct *bit-identical* default palettes
without ever comparing notes over the wire.

All entries are built directly in RGB565 component space (5-bit red, 6-bit green, 5-bit blue —
§4.2.1's 8-bit byte interface is not involved in constructing the default), in this order:

- **`[0..7]`**: 8 primary/secondary colors at half intensity: black `(0,0,0)`, red `(15,0,0)`,
  green `(0,31,0)`, yellow `(15,31,0)`, blue `(0,0,15)`, magenta `(15,0,15)`, cyan `(0,31,15)`,
  white `(23,47,23)`.
- **`[8..15]`**: the same 8 colors at full intensity: gray `(7,15,7)`, bright red `(31,0,0)`,
  bright green `(0,63,0)`, bright yellow `(31,63,0)`, bright blue `(0,0,31)`, bright magenta
  `(31,0,31)`, bright cyan `(0,63,31)`, bright white `(31,63,31)`.
- **`[16..231]`**: a 6×6×6 RGB color cube. For `r`, `g`, `b` each ranging `0..=5` (`r` outermost,
  `b` innermost, filling indices in that nested order starting at 16): red = `round(r * 31 / 5)`,
  green = `round(g * 63 / 5)`, blue = `round(b * 31 / 5)`.
- **`[232..255]`**: a 24-step grayscale ramp. For `level` in `0..24`: red = blue =
  `round(level * 31 / 23)`, green = `round(level * 63 / 23)` — green is scaled independently
  because it has one more bit of range than red/blue, keeping the ramp visually neutral rather than
  green-tinted.

(`round` here rounds to the nearest integer; none of the above divisions land on an exact half, so
the tie-breaking rule doesn't matter.)

Like `CharDisplay`, the device is always double-buffered: there is no single-buffered mode,
since single-buffering would reintroduce the bandwidth problem this redesign exists to solve.
The CPU-addressable pixel memory has a single, fixed identity for the life of the device — the
CPU always reads back exactly what it last wrote, regardless of swap state (`doc/memory-mapped-
display-device-spec.md` §5.1's model, applied per-matrix here).

Unlike `CharDisplay`, this device has:

- No control/status registers and no IRQ. Swaps are synchronous (§5) — there is nothing
  asynchronous for a status bit or interrupt to report.
- A uniform command/data register pair (§4.2–4.3) instead of dedicated bitfield registers. Every
  operation — swap, auto-refresh configuration, power, brightness, palette read/write — is a
  command.
- Per-matrix dirty tracking (§5.3) driving auto-refresh, rather than an unconditional
  every-vsync resend.

## 3. Configuration (locked at bus configuration time)

| Field | Type | Default | Notes |
|---|---|---|---|
| `matrices` | integer | — (required) | Must be 1, 2, 4, or 8 |
| `base_address` | integer | — | Where the device's memory-mapped region begins on the bus |
| `frame_rate_hz` | integer | mirrors `CharDisplay`'s default | Drives the auto-refresh cadence (§6) |
| `transport` | transport spec | — | A single point-to-point `pipe:` transport to a spawned companion process, mirroring `CharDisplay`'s external-protocol transport requirements (`doc/char-display-external-protocol.md` §2) rather than the current `LedMatrix`'s multipoint tagged transport |

Derived, not separately configurable:

- `pixel_bytes = matrices * 1024`.

**Validation at configuration time:**
- `matrices` must be one of `{1, 2, 4, 8}`.

## 4. Bus-addressable memory map

| Region | Offset | Size | Access | Notes |
|---|---|---|---|---|
| Pixel memory | `0x0000` | `pixel_bytes` | R/W | Matrix *n* occupies `[n * 1024, n * 1024 + 1023]`; palette index per pixel, row-major |
| Command register | `pixel_bytes` | 1 byte | W | Selects/arms an operation (§4.2) |
| Data register | `pixel_bytes + 1` | 1 byte | R/W | Argument byte(s) for the armed operation (§4.3) |

Total mapped size is `pixel_bytes + 2`, scaling with the configured `matrices` count (1,024 to
8,192 bytes of pixel memory) rather than always reserving space for 8 matrices.

### 4.1 Pixel memory and dirty tracking

Every write to an address within a given matrix's 1,024-byte range marks that matrix **dirty**,
regardless of whether the written value differs from what was already there — no value
comparison is performed. Each matrix's dirty flag defaults to `true` at construction and after
`reset()`, so a matrix a program never touches still gets one initial swap+send rather than never
reaching the peripheral at all.

A matrix's dirty flag is cleared whenever that matrix is swapped (§5.2), whether the swap was
triggered explicitly (`CMD_SWAP`) or by auto-refresh (§6).

### 4.2 Command register

Write-only trigger. Writing a command code selects the operation and, for commands that take
more than zero argument bytes, arms a byte-sequence state machine on the data register (§4.3) —
the same shape as `CharDisplay`'s runtime palette-update sequence (`doc/memory-mapped-display-
device-spec.md` §4.4), generalized to every command rather than just palette writes. Re-issuing a
command at any point — including mid-sequence — resets the state machine, discarding whatever
partial sequence was in progress.

| Command | Data bytes (write) | Data bytes (read) | Effect |
|---|---|---|---|
| `CMD_SWAP` | 1 (matrix bitmask) | — | Swaps (§5.2) each matrix whose bit is set, immediately, regardless of its dirty flag; clears each swapped matrix's dirty flag |
| `CMD_SET_AUTOREFRESH` | 1 (matrix bitmask) | — | Replaces the persistent auto-refresh mask (§6) wholesale |
| `CMD_SET_POWER` | 1 (matrix power-state bitmask: bit *n* set = matrix *n* powered on) | — | Replaces the persistent power-state mask wholesale, mirroring `CMD_SET_AUTOREFRESH`; matrices whose bit is clear have their drivers turned off |
| `CMD_SET_BRIGHTNESS` | 1 (brightness level, 0–255) | — | Sets overall display brightness uniformly across all attached matrices — global, not per-matrix; no bitmask argument |
| `CMD_PALETTE_WRITE` | 4, consumed in order: `index`, `red`, `green`, `blue` | — | On the 4th byte, masks `red`/`green`/`blue` to RGB565 (§4.2.1) and sets `palette[index]` to the result; emits a palette-update message on the transport (§7) |
| `CMD_PALETTE_READ` | 1 (`index`) | 3, produced in order: `red`, `green`, `blue` | Arms a read sequence; each subsequent data-register read returns the next channel byte, scaled up from `palette[index]`'s stored RGB565 value (§4.2.1) |

The persistent power-state mask defaults to **all matrices powered on**, at construction and after
`reset()` — the same "works out of the box" rationale as auto-refresh's all-enabled default (§6).
Like `CMD_SET_AUTOREFRESH`, there is no way to toggle a single matrix's power state without writing
the full desired mask — a program that wants to change one matrix's power without disturbing the
others must already know (or track) the current mask itself, since neither command exposes a
readback.

Reading the command register is not meaningful (there are no persistent bitfields to reconstruct,
unlike `CharDisplay`'s control register) and returns 0.

### 4.2.1 Palette color masking and scaling

Palette entries are stored as RGB565 (§2), so the 8-bit `red`/`green`/`blue` bytes
`CMD_PALETTE_WRITE`/`CMD_PALETTE_READ` exchange with the CPU must be converted at the register
boundary:

- **Write (mask):** each incoming 8-bit component is truncated to the entry's native bit width by
  discarding its low-order bits — red and blue keep their top 5 bits, green keeps its top 6.
- **Read (scale):** each stored component is expanded back to 8 bits by replicating its high-order
  bits into the newly available low-order bits, so that a stored `0` reads back as `0x00` and a
  stored maximum value reads back as `0xFF` — not scaled by a naive left-shift alone, which would
  leave the result short of `0xFF` at the top of the range.

This is the same well-established RGB565↔RGB888 conversion used broadly for this exact purpose; it
is specified precisely here (rather than left to the implementation) because it is
CPU-observable — a `CMD_PALETTE_WRITE` byte and a subsequent `CMD_PALETTE_READ` of the same
channel are not guaranteed to be equal, and a program relying on an exact round-trip would be
relying on unspecified behavior otherwise.

### 4.3 Data register

Write and read behavior both depend on which command is currently armed (§4.2):

- **Write-sequence commands** (`CMD_SWAP`, `CMD_SET_AUTOREFRESH`, `CMD_SET_POWER`,
  `CMD_SET_BRIGHTNESS`, `CMD_PALETTE_WRITE`): each write advances the armed command's state
  machine by one byte; the command applies once its full argument sequence has been written.
- **Read-sequence commands** (`CMD_PALETTE_READ`): the single write arms the sequence; each
  subsequent *read* of the data register advances the state machine by one byte and returns it.
- Writes or reads with no command armed, or past a command's expected byte count, are ignored /
  return 0 — mirroring `CharDisplay`'s "writes ignored unless armed" default (`doc/memory-mapped-
  display-device-spec.md` §4.4).

## 5. Swap semantics

### 5.1 Model

As in `CharDisplay` (`doc/memory-mapped-display-device-spec.md` §5.1), each matrix has a
CPU-addressable buffer (§4's pixel memory) and a scanout buffer, not bus-addressable. A swap
copies the CPU-addressable buffer into the scanout buffer; the CPU-addressable buffer's identity
never changes, so the CPU always reads back what it last wrote regardless of swap state.

### 5.2 Swap triggering

Swaps are always synchronous — there is no deferred/vsync-gated swap mode as `CharDisplay` has.
A swap, whether from `CMD_SWAP` or auto-refresh (§6), performs the buffer copy and sends that
matrix's block over the transport (§7) within the same tick/bus-access that triggered it. There
is nothing for a status register or IRQ to report, which is why this device has neither.

### 5.3 Dirty flag interaction

See §4.1. `CMD_SWAP` swaps the requested matrices unconditionally (ignoring dirty state) and
clears their dirty flags. Auto-refresh (§6) only swaps matrices that are both in the auto-refresh
mask and currently dirty.

## 6. Auto-refresh

The device maintains a persistent auto-refresh bitmask, one bit per matrix, set via
`CMD_SET_AUTOREFRESH` (§4.2) and defaulting to **all matrices enabled** at construction and after
`reset()`.

Driven by the same cycle-accounted cadence approach as `CharDisplay`'s vsync tick (`frame_rate_hz`
config attribute → `cycles_per_frame` derived from clock speed → accumulated per `tick()` call):
on each cadence tick, every matrix that is both in the auto-refresh mask and dirty is swapped
(§5.1) and has its dirty flag cleared. Matrices not in the mask, or in the mask but not dirty, are
left untouched — no unconditional every-tick resend, unlike `CharDisplay`'s frame push.

`CMD_SWAP` remains available regardless of a matrix's auto-refresh membership, for programs that
want to force an immediate update outside the auto-refresh cadence.

## 7. Transport

A single point-to-point `pipe:` transport to a spawned companion process, mirroring
`CharDisplay`/`emma65-display`'s architecture (`doc/char-display-external-protocol.md`) rather
than the current `LedMatrix`'s multipoint tagged transport. At minimum the wire protocol needs:

- A one-time header (matrix count, `frame_rate_hz`) — no palette contents and no per-matrix
  dimensions, since both are fixed rather than configured (§2.1's default palette and the 32×32
  per-matrix size are constants every implementation already knows, not values one side needs to
  tell the other). The companion process independently constructs §2.1's default palette at
  startup rather than receiving it; only a subsequent `CMD_PALETTE_WRITE` (via the palette-update
  message below) ever changes what it renders after that.
- A per-swap block message identifying the target matrix plus its 1,024 pixel bytes, sent
  whenever §5.2 performs a swap (not bundled to any fixed per-vsync cadence, since auto-refresh
  is dirty-gated).
- A separate, small palette-update message (index + color), sent only when `CMD_PALETTE_WRITE`
  actually applies a change (§4.2) — not resent with every block, since even at RGB565's 2 bytes
  per entry a full 256-entry palette (512 bytes) would be a large overhead relative to a single
  matrix's 1,024-byte block. Whether the color is sent as the raw 16-bit RGB565 value or expanded
  to 8-bit-per-channel bytes (§4.2.1's scaling) is left to the follow-up wire-format document —
  either way, the companion process is responsible for rendering at whatever fidelity the
  transport carries, not for hiding RGB565's precision loss from the display.
- Messages for `CMD_SET_POWER` and `CMD_SET_BRIGHTNESS`, forwarded to the companion process.

The exact byte-level framing is left to a follow-up document, analogous to
`doc/char-display-external-protocol.md`.

## 8. Resolved implementation questions

This section originally raised three open questions; all are now settled and specified at the
locations noted below, kept here as a single reference point rather than removed outright:

1. **Command-register read value**: fixed at `0` (§4.2) — no echo of the last-armed command code,
   no other use. Reading the command register is never meaningful, since it holds no persistent
   bitfields to reconstruct (unlike `CharDisplay`'s control register).
2. **`CMD_SET_POWER` byte encoding**: a single-byte wholesale power-state mask (§4.2), mirroring
   `CMD_SET_AUTOREFRESH`'s existing wholesale-mask pattern.
3. **`CMD_SET_BRIGHTNESS` byte encoding**: a single global `0–255` level (§4.2), applied uniformly
   to all attached matrices — no bitmask, no per-matrix targeting.

## 9. Companion process

A new binary, analogous to `emma65-display` (`display/` crate) but distinct from it — not an
extension of the existing SDL2 character-display peripheral.
