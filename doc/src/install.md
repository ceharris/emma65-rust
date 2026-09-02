# Install

## Rust toolchain

Install Rust via [rustup](https://www.rust-lang.org/tools/install):

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.org | sh
```

This installs the latest stable toolchain; Emma65 uses the 2024 edition, which
requires Rust 1.85 or newer. Verify with `rustc --version` and
`cargo --version`, and see the [rustup book](https://rust-lang.github.io/rustup/)
for updating an existing installation.

## System libraries

The plain `emma65` and `emma65-tracer` binaries have no system library
dependencies beyond Rust itself. Building the rest of the workspace needs
additional development packages: `emma65-display` and `emma65-led-matrix`
need the SDL2 libraries (`emma65-led-matrix` also needs SDL2_gfx), and
`emma65-debugger` needs Tauri's Linux dependencies (WebKitGTK, GTK,
libayatana-appindicator, librsvg).

### Ubuntu Linux

```bash
sudo apt-get update
sudo apt-get install -y \
  build-essential \
  libsdl2-dev \
  libsdl2-gfx-dev \
  libwebkit2gtk-4.1-dev \
  libssl-dev \
  libayatana-appindicator3-dev \
  librsvg2-dev
```

### Fedora Linux

```bash
sudo dnf install -y \
  gcc gcc-c++ make \
  SDL2-devel \
  SDL2_gfx-devel \
  webkit2gtk4.1-devel \
  openssl-devel \
  libappindicator-gtk3-devel \
  librsvg2-devel
```

## Build and install

Build the whole workspace in release mode:

```bash
cargo build --release --workspace
```

Or build only what you need — each crate's system library requirement is
independent of the others (see above):

```bash
cargo build --release              # emma65 + emma65-tracer only
cargo build --release -p emma65-display
cargo build --release -p emma65-led-matrix
cargo build --release -p emma65-debugger
```

Install the binaries onto your `PATH` (`cargo install` has no `--workspace`
flag, so each workspace member is installed with its own invocation — they
all land in the same place, `~/.cargo/bin` by default):

```bash
cargo install --path .            # emma65, emma65-tracer
cargo install --path display      # emma65-display
cargo install --path led-matrix   # emma65-led-matrix
```

`emma65-debugger` isn't installed this way; build it as a packaged desktop
app with `cargo tauri build` instead (see [The Debugger](the-debugger.md)). On
Linux this produces installable packages under
`target/release/bundle/` — a `.deb` and a `.rpm`:

```bash
sudo apt install ./target/release/bundle/deb/emma65-debugger_*.deb    # Ubuntu
sudo dnf install ./target/release/bundle/rpm/emma65-debugger-*.rpm    # Fedora
```

as well as a self-contained `.AppImage` under
`target/release/bundle/appimage/` that needs no install step — `chmod +x` it
and run it directly (or use a tool like AppImageLauncher to add it to your
desktop menu).
