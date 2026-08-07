//! Built-in default device layout, used by any binary that wants a working machine when the
//! user hasn't configured one: 32K RAM, the given ROM image at `$8000`, a VIA, a PTM, two
//! ACIAs, an LFSR, and a console.
use super::{Config, CpuVariantSpec};

const DEFAULT_CLOCK_SPEED_HZ: u64 = 1_843_200;
const DEFAULT_CPU_VARIANT: CpuVariantSpec = CpuVariantSpec::Wdc6502;

/// Builds the built-in default [`Config`], writing `rom_bytes` to a tempfile referenced by the
/// ROM device's `image` attribute. The returned tempfile must be kept alive until the `Config`
/// is done building a session (its path is only read during device instantiation).
pub fn default_config(rom_bytes: &[u8]) -> (Config, tempfile::NamedTempFile) {
    let f = tempfile::Builder::new()
        .suffix(".bin")
        .tempfile()
        .expect("failed to create tempfile for default ROM");
    std::fs::write(f.path(), rom_bytes).expect("failed to write default ROM to tempfile");
    let rom_path = f.path().to_path_buf();
    let config = Config {
        cpu_variant_spec: Some(DEFAULT_CPU_VARIANT),
        clock_speed_hz: Some(DEFAULT_CLOCK_SPEED_HZ),
        devices: Some(vec![
            "ram@0x0000,size=32768,fill=0".parse().unwrap(),
            format!("rom@0x8000,size=32768,image={}", rom_path.display())
                .parse()
                .unwrap(),
            "via/6522@0xff80,transport=unix:~/.emma/sock/via6522".parse().unwrap(),
            "ptm/6840@0xff90,transport=unix:~/.emma/sock/mc6840".parse().unwrap(),
            "acia/6551@0xfff0,transport=pty:~/.emma/dev/ttyS0".parse().unwrap(),
            "acia/6850@0xfff4,transport=pty:~/.emma/dev/ttyS1".parse().unwrap(),
            "lfsr@0xfff6,mode=step".parse().unwrap(),
            "console@0xfff8,break=0x3".parse().unwrap(),
        ]),
    };
    (config, f)
}
