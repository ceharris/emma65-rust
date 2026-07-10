use std::path::PathBuf;
use tempfile::NamedTempFile;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn emulator_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_emma65"))
}

/// 32 KB ROM image: every byte is STP (0xDB), with the reset vector at
/// 0x7FFC/0x7FFD pointing to 0x8000 (the start of the ROM on the bus).
/// Must be used with --cpu-variant WDC65C02, as STP is a WDC-only opcode.
fn make_stp_rom() -> Vec<u8> {
    let mut rom = vec![0xDBu8; 32768];
    rom[0x7FFC] = 0x00; // reset vector lo → 0x8000
    rom[0x7FFD] = 0x80; // reset vector hi
    rom
}

fn write_rom_tempfile() -> NamedTempFile {
    let f = tempfile::Builder::new().suffix(".bin").tempfile().unwrap();
    std::fs::write(f.path(), make_stp_rom()).unwrap();
    f
}

/// CLI device args that map 32 KB RAM at 0x0000 and a ROM image at 0x8000.
fn device_args(rom_path: &std::path::Path) -> Vec<String> {
    vec![
        "--cpu-variant".to_string(),
        "WDC65C02".to_string(),
        "--device".to_string(),
        "ram@0x0000,size=32768,fill=0".to_string(),
        "--device".to_string(),
        format!("rom@0x8000,size=32768,image={}", rom_path.display()),
    ]
}

// ---------------------------------------------------------------------------
// Group E — emulator binary subprocess tests
// ---------------------------------------------------------------------------

#[test]
fn run_with_cli_args() {
    let rom = write_rom_tempfile();
    let output = std::process::Command::new(emulator_bin())
        .args(device_args(rom.path()))
        .output()
        .unwrap();
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
}

#[test]
fn run_with_toml_config() {
    let rom = write_rom_tempfile();
    let toml = format!(
        r#"
cpu-variant = "WDC65C02"

[[devices]]
type = "ram"
address = 0
size = 32768

[[devices]]
type = "rom"
address = 32768
size = 32768
image = "{}"
"#,
        rom.path().display()
    );
    let cfg = tempfile::Builder::new().suffix(".toml").tempfile().unwrap();
    std::fs::write(cfg.path(), toml.as_bytes()).unwrap();
    let output = std::process::Command::new(emulator_bin())
        .args(["--config", cfg.path().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
}

#[test]
fn run_with_cli_overrides_toml() {
    // TOML contains an unknown device type; CLI --device flags should replace it entirely.
    let rom = write_rom_tempfile();
    let toml = r#"
[[devices]]
type = "bogus"
address = 4096
size = 256
"#;
    let cfg = tempfile::Builder::new().suffix(".toml").tempfile().unwrap();
    std::fs::write(cfg.path(), toml.as_bytes()).unwrap();
    let mut args = vec!["--config".to_string(), cfg.path().to_str().unwrap().to_string()];
    args.extend(device_args(rom.path()));
    let output = std::process::Command::new(emulator_bin())
        .args(&args)
        .output()
        .unwrap();
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
}

#[test]
fn run_with_env_var_cpu_variant() {
    let rom = write_rom_tempfile();
    // Supply devices via CLI but let the env var set the CPU variant.
    let output = std::process::Command::new(emulator_bin())
        .env("EMMA65_CPU_VARIANT", "WDC65C02")
        .args([
            "--device", "ram@0x0000,size=32768,fill=0",
            "--device", &format!("rom@0x8000,size=32768,image={}", rom.path().display()),
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
}

#[test]
fn run_with_invalid_env_var_cpu_variant() {
    let rom = write_rom_tempfile();
    let output = std::process::Command::new(emulator_bin())
        .env("EMMA65_CPU_VARIANT", "NOT_A_VARIANT")
        .args([
            "--device", "ram@0x0000,size=32768,fill=0",
            "--device", &format!("rom@0x8000,size=32768,image={}", rom.path().display()),
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("NOT_A_VARIANT"),
        "expected variant name in stderr, got: {stderr}"
    );
}

#[test]
fn run_with_unknown_device_type() {
    let output = std::process::Command::new(emulator_bin())
        .args([
            "--cpu-variant", "WDC65C02",
            "--device", "bogus@0x1000,size=256",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("bogus"),
        "expected device type name in stderr, got: {stderr}"
    );
}

#[test]
fn run_with_no_config_uses_default() {
    // Launch with no arguments; the binary should apply the built-in default config.
    // TaliForth runs a REPL and never halts on its own, so we kill the process after a
    // short delay and check that it didn't exit immediately with an error.
    let mut child = std::process::Command::new(emulator_bin())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(500));
    // If it already exited it was a startup error — capture status.
    match child.try_wait().unwrap() {
        Some(status) => {
            // Collect stderr to report the error, then fail.
            let output = child.wait_with_output().unwrap();
            let stderr = String::from_utf8_lossy(&output.stderr);
            panic!("binary exited unexpectedly with {status}: {stderr}");
        }
        None => {
            // Still running — good.
            child.kill().unwrap();
            child.wait().unwrap();
        }
    }
}

#[test]
fn run_with_missing_rom_image() {
    // Point the ROM image attribute at a path that doesn't exist.
    let output = std::process::Command::new(emulator_bin())
        .args([
            "--cpu-variant", "WDC65C02",
            "--device", "ram@0x0000,size=32768,fill=0",
            "--device", "rom@0x8000,size=32768,image=/nonexistent/path/rom.bin",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
}
