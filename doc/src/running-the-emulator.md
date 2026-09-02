# Running the Emulator

## Default configuration

When launched with no devices configured, the emulator runs with a built-in
[TaliForth 2](https://github.com/SamCoVT/TaliForth2) ROM and a full set of
peripherals:

- 32 KB RAM at `0x0000`–`0x7FFF`
- TaliForth ROM at `0x8000`–`0xFFFF`
- VIA at `0xFF80` on a Unix-domain socket (`~/.emma/sock/via6522`)
- MC6840 PTM at `0xFF90` on a Unix-domain socket (`~/.emma/sock/mc6840`)
- R6551 ACIA at `0xFFF0` on a pseudo-terminal (`~/.emma/dev/ttyS0`)
- MC6850 ACIA at `0xFFF4` on a pseudo-terminal (`~/.emma/dev/ttyS1`)
- LFSR at `0xFFF6` in step mode
- Console device at `0xFFF8`–`0xFFF9`, connected to the process's own
  standard input and output
- WDC 65C02 variant at 1.8432 MHz

Interact with the Forth interpreter via standard input and output.

## TOML configuration file

Use `--config <file>` to load a TOML configuration file. Top-level keys map
directly to emulator fields — there is no `[emulator]` wrapper:

```toml
cpu-variant = "WDC65C02"   # or "65C02" (CMOS only, default)
clock-speed-hz = 1843200   # omit for unlimited throughput

[[devices]]
type = "ram"
address = 0x0000
size = 32768               # or "32K"

[[devices]]
type = "rom"
address = 0x8000
size = 32768
image = "~/roms/my.bin"    # .bin, .rom, .hex, .ihx, .ihex, .s19, .srec

[[devices]]
type = "console"
address = 0xFFF8
transport = { pty = { path = "~/.emma/dev/ttyS0" } }
```

## CLI flags

All config values can also be set from the command line. CLI takes precedence
over TOML, which takes precedence over environment variables.

```
emma65 --cpu-variant WDC65C02 \
       --clock-speed-hz 1843200 \
       --device ram@0x0000,size=32768,fill=0 \
       --device rom@0x8000,size=32768,image=~/roms/my.bin \
       --device console@0xFFF8,transport=pty:~/.emma/dev/ttyS0
```

Device shorthand format: `type@address[,key=value,...]`

- Address: decimal, `0x` hex, `0o` octal, or `0b` binary
- Size: bytes, or `K`/`k` suffix for kibibytes (e.g. `32K`)
- Paths support `~/` tilde expansion

## Environment variables

Any config key can be set with the `EMMA65_` prefix, using `_` in place of
`-`:

```
EMMA65_CPU_VARIANT=WDC65C02
EMMA65_CLOCK_SPEED_HZ=1843200
```

## Built-in device types

| Type            | Registers | Key attributes                                                                     |
|-----------------|:---------:|-------------------------------------------------------------------------------------|
| `ram`           |     —     | `size` (required), `fill` (optional byte), `image` (optional path)                  |
| `rom`           |     —     | `size` (required), `image` (required path), `fill` (optional byte)                  |
| `console`       |     2     | `transport` (optional), `break` (optional byte: break-key code)                     |
| `acia/6551`     |     4     | `transport` (optional), `with-tdre-bug` (bool), `with-overrun` (bool)               |
| `acia/6850`     |     2     | `transport` (optional)                                                              |
| `via/6522`      |    16     | `transport` (optional), `protocol` (`ascii` or `binary`, optional)                  |
| `ptm/6840`      |     8     | `transport` (optional), `protocol` (`ascii` or `binary`, optional)                  |
| `display/matrix`| variable  | `arrangement` (required `COLSxROWS`; `columns * rows` must be 1, 2, 4, or 8), `register-address` (required), `frame_rate_hz`, `transport` (optional, `pipe:` only) |
| `display`  |  variable | `columns`, `rows` (optional, default 40×25), `palette`, `font` (optional paths), `double-buffered` (bool), `frame-rate-hz`, `transport` (optional, `pipe:` only) |
| `lfsr`          |     2     | `taps` (optional u16), `mode` (`continuous` or `step`, optional)                    |
| `mem/finch`     |     2     | `bank-registers`, `control-register` (required addresses), `image` (required path), `write-policy`, `fill`, `offset`, `labels` (all optional) |
| `mem/phoebe`    |     1     | `control-register` (required address), `image` (required path), `write-policy`, `fill`, `ram-fill`, `offset`, `labels` (all optional) |
| `mem/vireo`     |     1     | `control-register` (required address), `image` (required path), `write-policy`, `fill`, `ram-fill`, `offset`, `labels` (all optional) |

`mem/finch`, `mem/phoebe`, and `mem/vireo` each occupy the entire 64 KB
address space rather than a fixed-size register window; their register count
above is the count of dedicated MMU/bank-control registers, placed at the
configurable addresses shown, not a contiguous block.

`display`'s register window is `2 * columns * rows + 2` bytes (char RAM + 
color RAM + a control register + a status/data register), so it grows with
the configured grid size rather than being fixed.

`display/matrix`'s pixel memory is `columns * rows * 1024` bytes (from its
`arrangement`), based at `address`; its command and data registers are a
separate 2-byte range based at `register-address` rather than immediately
following pixel memory, so the two can be placed independently on the bus
(e.g. keeping pixel memory aligned to a 1 KiB/N KiB boundary).

Transport shorthand values for CLI and TOML string form:
`pipe:/path/to/exe,arg1,arg2`, `tcp:PORT`, `tcp:IP:PORT`, `unix:PATH`, `pty`,
`pty:SYMLINK_PATH`
