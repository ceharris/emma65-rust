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
| `CMD_SET_POWER` | 1 (matrix bitmask + on/off — exact encoding TBD, §8) | — | Turns the addressed matrices' drivers on or off |
| `CMD_SET_BRIGHTNESS` | 1 (brightness level) | — | Sets overall display brightness (global vs. per-matrix: TBD, §8) |
| `CMD_PALETTE_WRITE` | 4, consumed in order: `index`, `red`, `green`, `blue` | — | On the 4th byte, sets `palette[index] = RGB(red, green, blue)` and emits a palette-update message on the transport (§7) |
| `CMD_PALETTE_READ` | 1 (`index`) | 3, produced in order: `red`, `green`, `blue` | Arms a read sequence; each subsequent data-register read returns the next channel byte |

Reading the command register is not meaningful (there are no persistent bitfields to reconstruct,
unlike `CharDisplay`'s control register) and returns 0.

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

- A one-time header (matrix count, 32×32 fixed per-matrix dimensions, palette length, initial
  palette contents, `frame_rate_hz`).
- A per-swap block message identifying the target matrix plus its 1,024 pixel bytes, sent
  whenever §5.2 performs a swap (not bundled to any fixed per-vsync cadence, since auto-refresh
  is dirty-gated).
- A separate, small palette-update message (index + RGB), sent only when `CMD_PALETTE_WRITE`
  actually applies a change (§4.2) — not resent with every block, since a full 256-entry palette
  (768 bytes) would be a large overhead relative to a single matrix's 1,024-byte block.
- Messages for `CMD_SET_POWER` and `CMD_SET_BRIGHTNESS`, forwarded to the companion process.

The exact byte-level framing is left to a follow-up document, analogous to
`doc/char-display-external-protocol.md`.

## 8. Open questions for implementation

1. **`CMD_SET_POWER` and `CMD_SET_BRIGHTNESS` byte encoding**: whether these are addressed
   per-matrix (a bitmask byte, consistent with `CMD_SWAP`/`CMD_SET_AUTOREFRESH`, needing 2
   argument bytes for power's mask + on/off) or apply globally to all attached matrices with a
   single argument byte. No strong preference either way — left to be decided during
   implementation.
2. **Command-register read value**: specified here as always 0 (§4.2); confirm no other use is
   wanted (e.g. echoing the last-armed command code).

## 9. Companion process

A new binary, analogous to `emma65-display` (`display/` crate) but distinct from it — not an
extension of the existing SDL2 character-display peripheral.
