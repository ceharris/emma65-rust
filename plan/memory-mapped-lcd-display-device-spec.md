# Memory-Mapped LCD Display Device — Specification

## 1. Purpose and scope

This document specifies a memory-mapped device emulating a character LCD module built around a
Hitachi HD44780 (or compatible) Dot Matrix Liquid Crystal Display Controller/Driver, directly
interfaced on the 6502 bus (issue #554). It captures the *behavior and interface contract* of the
device: its configuration surface, its two-register bus interface, command execution timing, DDRAM/
CGRAM addressing, and the built-in character generator. It deliberately does not specify:

- The shape of Emma65's bus device trait(s), or how the device is instantiated/registered.
- The exact wire format of a transport protocol to an external companion process, or that
  companion process's own design — left to a follow-up document once this device-level contract
  is settled, mirroring `plan/char-display-external-protocol.md` and
  `plan/led-matrix-external-protocol.md`.
- A future mode where the device communicates over a transport as a VIA-protocol client rather
  than a direct bus interface (issue #554's stated future direction) — see §9.

## 2. Conceptual model

Unlike `CharDisplay` and `LedMatrix`, this device does **not** map its display memory directly
into the bus address space. A real HD44780 exposes exactly two logical registers (selected by the
RS pin) to its host bus, each readable and writable (via R/W): an **instruction/status register**
and a **data register**. All of the controller's actual state — 80 bytes of Display Data RAM
(DDRAM), 64 bytes of Character Generator RAM (CGRAM), the address counter, and a handful of
internal mode flags — is reached only indirectly through those two registers, exactly as on real
hardware. This device reproduces that interface faithfully rather than memory-mapping DDRAM/CGRAM
directly, which is why its bus footprint is only 2 bytes regardless of configured geometry.

Real hardware distinguishes register access by two pins, RS (register select) and R/W. On a bus
interface, both combine into ordinary byte-addressed read/write:

| RS | R/W | Bus operation | Meaning |
|---|---|---|---|
| 0 | write | Write instruction/status register | Issue a command (§4.2) |
| 0 | read | Read instruction/status register | Busy flag + address counter (§4.3) |
| 1 | write | Write data register | Write a byte to DDRAM or CGRAM at the current address (§4.4) |
| 1 | read | Read data register | Read a byte from DDRAM or CGRAM at the current address (§4.4) |

### 2.1 Command execution delay and the busy flag

Real HD44780 instructions are not instantaneous: internal RAM writes and the display-clear/home
operations each take a datasheet-specified amount of time, during which the controller reports
**busy** on the instruction register's high bit. A program is expected to either poll the busy
flag before issuing the next instruction or insert a delay at least as long. This device
reproduces that timing faithfully (approximately, per the datasheet, per issue #554) rather than
completing every command in zero simulated time, so 6502 programs written against real HD44780
timing assumptions behave the same way against this device. See §5.

### 2.2 Interface width: 8-bit and 4-bit modes

The HD44780's `Function Set` instruction's DL bit selects whether the host presents a full 8-bit
value per register access, or two 4-bit nibbles (high nibble first, then low nibble), both nibbles
carried in the same 4 high-order bit positions of successive byte-wide bus accesses — the "software
enabled 4-bit interface," commonly used in real designs to save data-line count even when, as here,
a full 8-bit bus is otherwise available. Both modes are equally valid ways to talk to this device;
the currently selected mode is internal controller state established by the most recently executed
`Function Set` (§4.2), defaulting to 8-bit at reset. See §6.

Unlike real hardware — where the controller's internal state immediately after power-on is not
reliably known, motivating the well-known "send 0x3 three times, then 0x2" defensive
initialization dance for entering 4-bit mode from an unknown state — this device's state is always
well-defined (8-bit mode, not mid-nibble) immediately after construction or `reset()`. That
defensive sequence still works correctly against this device (each 0x3/0x2 nibble/byte is
interpreted exactly as specified), it just isn't *required* the way it is against real, possibly
glitched hardware.

### 2.3 Display geometry

The physical character grid (rows × columns) is fixed at configuration time (§3) from a supported
set of real-world HD44780 module geometries. Unlike `CharDisplay`, geometry is not a free choice of
dimensions — an HD44780's 80-byte DDRAM and two-internal-line addressing model only support a
specific, real-world set of physical layouts (§7), and this device's job is to reproduce exactly how
a real module of the requested geometry maps DDRAM bytes onto visible character cells, quirks
included.

## 3. Configuration (locked at bus configuration time)

| Field | Type | Default | Notes |
|---|---|---|---|
| `base_address` | integer | — | Where the device's 2-byte register pair begins on the bus |
| `geometry` | one of `8x1`, `8x2`, `16x1`, `16x2`, `16x4`, `20x2`, `20x4`, `40x1`, `40x2` | `16x2` | Physical character grid (§7); fixed for the device's lifetime |
| `cgrom` | path | built-in default | Optional override for the built-in character generator ROM (§8.1) |
| `background` / `foreground` | RGB24 | blue background, white foreground | Cosmetic-only rendering colors (§8.3); not bus-addressable, not part of the HD44780's own behavior |

**Validation at configuration time:** `geometry` must be one of the nine listed values; any other
string is a configuration error naming the supported set.

## 4. Register semantics

### 4.1 Overview

Both registers occupy fixed offsets from `base_address`:

| Register | Offset | Access |
|---|---|---|
| Instruction/status | `0x0000` | W: issue instruction (§4.2). R: busy flag + address counter (§4.3) |
| Data | `0x0001` | W: write DDRAM/CGRAM at current address (§4.4). R: read DDRAM/CGRAM at current address (§4.4) |

Total mapped size is always 2 bytes, independent of configured geometry — geometry affects only how
DDRAM content is interpreted for rendering (§7), never the bus footprint.

Every register access, on either offset, is filtered first through the current 4-bit/8-bit
interface state (§6) before being interpreted as a full 8-bit instruction or data value.

### 4.2 Instruction register (write side)

| Instruction | Bit pattern | Effect | Timing class (§5) |
|---|---|---|---|
| Clear Display | `0000 0001` | DDRAM filled with `0x20` (space); address counter (DDRAM) set to 0; entry mode ID forced to increment | Long |
| Return Home | `0000 001-` | Address counter (DDRAM) set to 0; display shift offset (§7.4) reset to 0; DDRAM contents unchanged | Long |
| Entry Mode Set | `0000 01ID S` | Sets `ID` (address counter increment=1/decrement=0 direction) and `S` (accompany DDRAM writes with a display shift, §7.4) | Short |
| Display On/Off Control | `0000 1D C B` | Sets display-on (`D`), cursor-visible (`C`), cursor-blink (`B`) — all render-only, §8.3 | Short |
| Cursor or Display Shift | `0001 SC RL --` | `SC`=1 shifts the whole display (§7.4), `SC`=0 moves the cursor only; `RL`=1 right, `RL`=0 left. Does not touch DDRAM contents or the address counter's targeted RAM (only the cursor position / shift offset) | Short |
| Function Set | `001 DL N F --` | Sets the 4-bit/8-bit interface width `DL` (§6) and the font height `F` (§8.1); `N` is accepted and stored but has no effect on this device's addressing or rendering (§7.3) | Short |
| Set CGRAM Address | `01 AAAAAA` | Address counter := `AAAAAA` (6 bits); subsequent data accesses target CGRAM | Short |
| Set DDRAM Address | `1 AAAAAAA` | Address counter := `AAAAAAA` (7 bits); subsequent data accesses target DDRAM | Short |

Issuing any instruction while busy (§5) is ignored — the write is discarded and does not restart
or extend the busy period — mirroring real hardware's documented "do not access while busy"
contract by simply not letting an out-of-turn access have any effect, and logging a `Debug`-level
diagnostic so a misbehaving program's timing bug is observable without corrupting device state.

### 4.3 Instruction register (read side)

| Bit | Meaning |
|---|---|
| 7 | Busy flag (§5): 1 while a previously issued instruction or data access is still executing |
| 6:0 | Current address counter value — the DDRAM (0–79) or CGRAM (0–63) address last set, reflecting auto-increment/decrement from data accesses (§4.4) regardless of which RAM it currently targets |

Reading the instruction register is always permitted, including while busy (this is the read a
program uses to poll busy) and never itself becomes a source of busy time (Instant per real
hardware).

### 4.4 Data register

Effect depends on which RAM the address counter currently targets (most recent `Set CGRAM Address`
or `Set DDRAM Address`; DDRAM by default at reset):

- **Write**: stores the byte at the targeted RAM's current address; the address counter then
  moves by one position in the direction `ID` specifies (§4.2), wrapping (§4.4.1). If the target is
  DDRAM and entry mode's `S` bit is set, the display additionally shifts (§7.4) in the same
  direction as `ID`. Writing while busy is ignored (§4.2's rule applies identically here).
- **Read**: returns the byte at the targeted RAM's current address, then advances the address
  counter the same way a write does — except display shift (`S`) never accompanies a *read*,
  matching real hardware exactly. Reading while busy is ignored, returning 0 rather than the
  addressed byte's actual value (mirroring the instruction register's read-anytime exception not
  extending to the *data* register, which real hardware also gates on busy).

#### 4.4.1 Address counter wrap

The address counter always wraps within the RAM it currently targets — DDRAM wraps over its full
80-byte range (`0..80`), CGRAM over its full 64-byte range (`0..64`) — regardless of configured
geometry or the `N` bit (§7.3). Incrementing past the top wraps to 0; decrementing below 0 wraps to
the top.

## 5. Timing

Two timing classes, applied approximately per the HD44780 datasheet (issue #554 asks for
"approximately the same," not cycle-exact reproduction of the chip's internal oscillator):

| Class | Duration | Instructions |
|---|---|---|
| Short | 37 µs | Entry Mode Set, Display On/Off Control, Cursor/Display Shift, Function Set, Set CGRAM Address, Set DDRAM Address, data register write, data register read |
| Long | 1.52 ms | Clear Display, Return Home |

Each duration is converted to a whole number of CPU cycles from the emulator's configured clock
speed (or a nominal fallback when the CPU runs unthrottled, mirroring `CharDisplay`'s
`NOMINAL_CLOCK_HZ` approach) at the moment the instruction executes, and counted down as the CPU's
own `tick(cycles)` calls arrive — the same cycle-accounted style already used for `CharDisplay`'s
vsync cadence and `LedMatrix`'s auto-refresh cadence, applied here as a one-shot countdown per
instruction rather than a periodic cadence.

## 6. 4-bit / 8-bit register access

Interface width state (`DL`, from the most recent `Function Set`, defaulting to 8-bit at reset)
governs how a raw byte written to *either* register (instruction or data) is interpreted:

- **8-bit (`DL`=1)**: the written byte is the full instruction/data value directly.
- **4-bit (`DL`=0)**: two consecutive writes to the same register are required per logical byte.
  Each write's high nibble (bits 7:4) supplies four bits of the pending byte; the low nibble (bits
  3:0) is ignored, matching real hardware's convention of only wiring DB7:DB4. The first write of a
  pair supplies the high nibble of the resulting byte, the second write supplies the low nibble.
  Once both nibbles have arrived, the assembled byte is processed exactly as it would be in 8-bit
  mode (looked up as an instruction, or applied as data, respectively).

Nibble state is tracked **per register** (instruction vs. data), since a program can freely
interleave polling reads of the instruction register with in-progress nibble writes to the data
register (or vice versa) without one disturbing the other's pairing. A `Function Set` that changes
`DL` takes effect immediately for whichever access comes next; it does not retroactively affect a
nibble pairing already in progress on the *other* register at the moment it executes, since a
`Function Set` write is itself always a complete instruction (not a data access) by the time its
own DL takes effect.

Reads are not affected by nibble pairing — the instruction register's busy/address-counter read
(§4.3) and the data register's RAM read (§4.4) always return a full byte in one access, in both
4-bit and 8-bit mode, matching real hardware (a real HD44780 does present all 8 read bits on
DB7:DB0 in 8-bit mode, and repeats the same nibble read twice, high-then-low, when reading in
4-bit mode — see §9's note on the deferred 4-bit-mode read path, since only the write side is
observable by direct-bus firmware in the common write-mostly usage this device targets).

## 7. Display geometry and DDRAM addressing

### 7.1 Supported geometries

Each geometry maps to a fixed set of visible rows, each composed of one or more DDRAM segments
(a contiguous address range). Two segments in the same row (only for `16x1`, see below) render as
one continuous line of text with no visible seam.

| Geometry | Rows × Cols | Segments (per row, `start,count`) | `N` addressing style |
|---|---|---|---|
| `8x1` | 1×8 | `[(0x00,8)]` | single 80-byte line |
| `40x1` | 1×40 | `[(0x00,40)]` | single 80-byte line |
| `8x2` | 2×8 | `[(0x00,8)]`, `[(0x40,8)]` | two 40-byte lines |
| `16x2` | 2×16 | `[(0x00,16)]`, `[(0x40,16)]` | two 40-byte lines |
| `20x2` | 2×20 | `[(0x00,20)]`, `[(0x40,20)]` | two 40-byte lines |
| `40x2` | 2×40 | `[(0x00,40)]`, `[(0x40,40)]` | two 40-byte lines |
| `16x1` | 1×16 | `[(0x00,8), (0x40,8)]` | two 40-byte lines, split across one visible row |
| `16x4` | 4×16 | `[(0x00,16)]`, `[(0x40,16)]`, `[(0x10,16)]`, `[(0x50,16)]` | two 40-byte lines, each shared by two visible rows |
| `20x4` | 4×20 | `[(0x00,20)]`, `[(0x40,20)]`, `[(0x14,20)]`, `[(0x54,20)]` | two 40-byte lines, each shared by two visible rows |

`16x1` is a documented real-world quirk, not a simplification: many actual 16-column, single-row
HD44780 modules are wired to show the first 8 characters from DDRAM line 1 (`0x00`–`0x07`)
concatenated with the first 8 characters of DDRAM line 2 (`0x40`–`0x47`), rather than 16 contiguous
bytes of a single line. This device reproduces that mapping rather than a simpler contiguous
16-byte row, so 6502 programs written against real 16×1 module documentation address the display
correctly.

`16x4`/`20x4` reproduce the other well-known real quirk: with only two internal 40-byte DDRAM
lines available, four-row modules split each line's 40 bytes into two visible rows (first half /
second half), meaning rows 1 & 3 share one line and rows 2 & 4 share the other. This is why, on
real hardware and on this device, a `Cursor or Display Shift` (§7.4) visibly moves rows 1 and 3
together and rows 2 and 4 together, rather than each of the four rows independently.

### 7.2 Off-screen scroll room

A segment's `count` (visible width) may be smaller than the 40- or 80-byte line it belongs to —
this is true for every geometry above except `40x1`/`40x2`. The remaining bytes of that line are
still valid DDRAM, addressable and writable via the normal address counter, and become visible
through display/cursor shifting (§7.4) — reproducing the common real-world technique of writing a
longer message than fits on screen and scrolling it into view.

### 7.3 The `N` (number of lines) bit does not affect addressing

Unlike some simplified emulations, `Function Set`'s `N` bit is stored (for completeness of the
instruction's own bit pattern) but never consulted for address-counter wrapping (§4.4.1) or for
choosing how DDRAM maps to visible rows (§7.1, fixed entirely by the configured `geometry`) — on
real hardware, `N` reflects a module's physical row-driver wiring, which for an emulated device is
exactly what `geometry` already specifies. A firmware author does not need to (and cannot, in this
implementation) mismatch `N` against the configured geometry to any observable effect.

### 7.4 Display and cursor shift

The device maintains one shift offset per DDRAM line (40-byte lines: two independent offsets; the
single 80-byte line geometries: one offset), each initialized to 0 and reset to 0 by `Return Home`
and `Clear Display`. A `Cursor or Display Shift` instruction with `SC`=1, or a DDRAM data write
with entry mode's `S` bit set, adjusts **every** line's offset simultaneously by one position in
the direction given (`RL`/`ID` respectively) — matching real hardware's single shared shift
mechanism, which is why `16x4`/`20x4`'s paired rows (§7.1) shift together. Each line's offset wraps
modulo that line's own length (40, or 80 for the single-line geometries) independent of the
configured geometry's visible `count` — the extra off-screen bytes (§7.2) scroll into view exactly
as on real hardware.

Rendering row *r* (§8) reads, for each of that row's segments `(start, count)`, the `count` bytes
beginning at `(start + line_offset) mod line_length`, where `line_offset`/`line_length` are the
40-or-80-byte line that `start` belongs to.

`SC`=0 (cursor-only shift) instead moves only the cursor's rendered position (§8.3), leaving every
shift offset and all DDRAM contents untouched.

## 8. Character generation and rendering

### 8.1 CGROM (built-in font) and CGRAM (user-defined characters)

DDRAM byte values select a glyph to render at that cell:

- `0x00`–`0x0F`: selects one of the 8 (5×8 font, `F`=0) or 4 (5×10 font, `F`=1) CGRAM-defined
  characters (§8.2), using the low 3 (or low 2) bits of the DDRAM value — real hardware aliases
  the unused high bit(s) of this range onto the same custom characters, which this device
  reproduces rather than treating as a separate/blank range.
- `0x10`–`0x1F`: undefined/blank, matching the corresponding gap in the standard Hitachi character
  ROM tables.
- `0x20`–`0x7F`: standard ASCII, from CGROM (§8.1.1).
- `0x80`–`0x9F`: undefined/blank, matching the standard ROM table's gap.
- `0xA0`–`0xFF`: extended CGROM glyphs (§8.1.1) — Katakana/European characters depending on the
  configured ROM table.

#### 8.1.1 CGROM source

The built-in default reproduces one publicly documented Hitachi HD44780 character ROM table
(European/Western variant, "A02"), embedded the same way `CharDisplay` embeds its default font
(`include_bytes!`). The `cgrom` config attribute (§3) allows substituting an alternate table (e.g.
the Japanese "A00" variant) in the same binary layout, without needing a new build.

### 8.2 CGRAM addressing and the 5×8 / 5×10 font split

CGRAM is a flat 64-byte array, addressed exactly as the address counter addresses it (§4.4,
§4.4.1) — no special-casing beyond how the *font* (`F`, from `Function Set`) interprets which bytes
are meaningful:

- **`F`=0 (5×8, the common case)**: character index = `address >> 3` (0–7), row = `address & 0x7`
  (0–7). All 64 bytes are meaningful; 8 characters of 8 rows each.
- **`F`=1 (5×10)**: character index = `address >> 4` (0–3), row = `address & 0xF` (0–15), of which
  only rows 0–10 are rendered. Only 4 characters are usable; the remaining CGRAM bytes at rows
  11–15 of each group are still ordinarily readable/writable (nothing rejects those addresses) but
  are never rendered — reproducing the real chip's documented 16-byte-group-with-11-meaningful-rows
  layout rather than hiding the gap.

Each row's 5 low bits (of the low-order 5 bits in a byte; the top 3 bits are unused/unspecified,
matching real hardware) are the pixel pattern for that row, most-significant-of-the-5 on the left.

### 8.3 Rendering

Rendering (both the debugger panel and, later, an external companion process) composites the
currently visible rows (§7.1, §7.4) into a pixel grid: each character cell is its glyph's dot
pattern (5×8 or 5×10 per §8.2, from CGROM or CGRAM per §8.1) drawn in the configured `foreground`
color against the configured `background` color (§3) — both fixed, cosmetic-only settings with no
bus-addressable equivalent, since the HD44780 itself has no concept of color (a real module's
color comes from its physical backlight and LCD polarizer, neither part of the logical interface
this device emulates).

Display On/Off Control's `D` bit (§4.2), when clear, renders every cell blank regardless of DDRAM
contents — real hardware's "display off" blanks the segment/row drivers without altering DDRAM,
CGRAM, or the address counter, all of which resume being visible the instant `D` is set again.

The cursor (`C`/`B` bits, §4.2) renders at the DDRAM address counter's *current* position translated
back to a visible row/column via §7.1's segment table (only when that position falls within a
segment's currently visible window per §7.4 — an off-screen cursor position, from scrolling or from
being set past the visible window, renders no cursor at all) as an underline (`C`=1, `B`=0), or
alternates between an underline and a solid block at some implementation-chosen blink cadence when
blinking (`B`=1) is also enabled. A cursor position within CGRAM addressing (i.e., the address
counter currently targets CGRAM rather than DDRAM) renders no cursor, since the cursor is a DDRAM
concept.

## 9. Explicitly out of scope for this revision

- The bus device trait shape, instantiation, and registration mechanics.
- The external wire protocol to a companion process, and that companion process's own design —
  left to a follow-up document, per §1.
- The future VIA-protocol client transport mode (issue #554's "future enhancement") — the
  architecture should not preclude it, but no work toward it is specified here.
- 4-bit-mode *reads* of the data or instruction register split across two nibble reads. Real
  hardware does support this (repeating the same nibble twice, high then low), but firmware
  overwhelmingly only needs 4-bit *writes* (reading busy/data back is comparatively rare in 4-bit
  designs, which more commonly just insert a fixed delay instead of polling); full read-side nibble
  splitting can be added later without changing anything specified above for writes.
- Contrast/backlight control (a real module's Vo pin and LED backlight are outside the HD44780's
  own logical interface, and issue #554 does not ask for either).

## 10. Open questions for implementation

1. **CGROM source data**: the actual glyph bitmaps for the embedded default table (§8.1.1) must be
   authored/sourced from published HD44780 character ROM documentation during implementation; none
   are fixed by this document.
2. **Cursor blink cadence** (§8.3): left to implementation, since the HD44780 datasheet does not
   specify an exact blink rate either.
3. **`cgrom` override file format** (§8.1.1): left to implementation to define, analogous to how
   `plan/memory-mapped-display-device-spec.md` left `CharDisplay`'s `font=` file format to its own
   design phase.
