//! `emma65-led-matrix` — an external peripheral process that renders `LedMatrix`'s per-matrix
//! composited output in an SDL2 window.
//!
//! Spawned by the emulator itself via a `display/matrix` device's `transport = "pipe:..."`
//! attribute (see `doc/led-matrix-external-protocol.md`); this binary is never run standalone
//! against a live `emma65` process any other way — its own stdin *is* the pipe. It reads the
//! one-time header, then a background thread decodes the tagged message stream that follows
//! (§5) into [`protocol::Message`]s. Each matrix's most recently received raw pixel indices are
//! retained and recomposited against the current palette on every redraw (§7: palette changes
//! aren't re-sent per matrix, so there is nothing to special-case — recompositing from raw state
//! at render time naturally picks up the latest palette), via the same
//! `emma65::emulator::device::led_matrix::compositing::composite_matrix` the debugger's
//! in-process `LedMatrixPanel` uses — no rendering logic is duplicated here. Unlike
//! `CharDisplay`'s peripheral, there is no keyboard forwarding (the LED matrix has no input
//! capability) and no in-app arrangement menu (SDL2 has no context-menu widget) — the physical
//! layout comes from the header's `columns` field (§4), mirroring the device's own configured
//! arrangement rather than an independently-chosen value.
//!
//! The main loop redraws unconditionally at a fixed ~30Hz cadence rather than only when a new
//! message arrives: unlike `CharDisplay`'s continuous per-vsync pushes, `LedMatrix` messages are
//! sporadic (only on a matrix swap or an actual palette write), so idle gaps of a second or more
//! between messages are normal. A message-triggered-only redraw left the window showing stale
//! (blank) content through those gaps — the previously presented frame doesn't reliably survive
//! that long without a fresh `present()`, at least under some window manager/driver
//! combinations. Continuous redraw sidesteps the issue entirely and also matches how a real LED
//! matrix panel works: rows are driven by a persistence-of-vision refresh, not presented once and
//! left static.

mod protocol;

use std::io::{self, Read};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use clap::Parser;
use emma65::emulator::device::led_matrix::PIXELS_PER_MATRIX;
use emma65::emulator::device::led_matrix::compositing::{Rgb565, composite_matrix, default_palette};
use sdl2::event::Event;
use sdl2::gfx::primitives::DrawRenderer;
use sdl2::pixels::Color;
use sdl2::rect::Rect;

use protocol::{Header, Message};

/// Every matrix is a fixed 32x32 framebuffer (spec §2) — unlike `CharDisplay`'s columns/rows,
/// there's nothing device-specific to compute here.
const MATRIX_SIZE: u32 = 32;

/// LED radius as a fraction of pitch — matches `LedMatrixPanel.tsx`'s `LED_RADIUS_RATIO`, which
/// models a real hobbyist RGB LED matrix panel's proportions (see that file's doc comment for the
/// reference hardware this is based on).
const LED_RADIUS_RATIO: f64 = 0.3;

/// Near-black PCB substrate color drawn behind the LEDs — matches
/// `LedMatrixPanel.tsx`'s `PCB_BACKGROUND_COLOR`.
const PCB_BACKGROUND_COLOR: Color = Color::RGB(0x0a, 0x0a, 0x0a);

/// Color for an unlit LED (RGB 0,0,0) — matches `LedMatrixPanel.tsx`'s `UNLIT_LED_COLOR`.
/// Deliberately distinct from `PCB_BACKGROUND_COLOR`, the same reasoning as that file's.
const UNLIT_LED_COLOR: Color = Color::RGB(30, 30, 34);

/// Converts an SDL2 [`Color`] into the `u32` SDL2_gfx's `*Color` entry points (e.g.
/// `filledCircleColor`) actually expect. `sdl2::gfx`'s blanket `ToColor for Color` impl packs
/// the color big-endian as `0xRRGGBBAA` (`u32::from_be_bytes`), but the underlying C function
/// reinterprets that `u32` as a raw 4-byte array in the host's *native* order
/// (`Uint8 *c = (Uint8 *)&color`) and passes `c[0..4]` through as r,g,b,a. On this little-endian
/// host that reverses every channel — red ends up in alpha — so any color with a low or zero red
/// component (e.g. bright green, `Rgb565`'s palette index 10) is drawn as nearly or fully
/// transparent instead of the intended color. `ToColor for u32` is the one impl that passes its
/// value through unchanged, so packing the bytes in native order here sidesteps the broken
/// blanket impl entirely.
fn gfx_color(color: Color) -> u32 {
    u32::from_ne_bytes([color.r, color.g, color.b, color.a])
}

#[derive(Parser)]
#[command(about = "SDL2 external peripheral for emma65's memory-mapped LED matrix display")]
struct Args {
    /// Initial on-screen LED center-to-center spacing, in pixels. The window remains resizable
    /// afterward; SDL2 letterboxes/scales its fixed logical size to fit.
    #[arg(long, default_value_t = 12)]
    pitch: u32,
}

/// A physical layout of matrices into a grid, `matrix_index` placed row-major (index `i` at
/// `(i % columns, i / columns)`) — mirrors the device's own configured arrangement, threaded
/// through the header's `columns` field (spec §4) rather than chosen independently by this
/// binary.
#[derive(Clone, Copy)]
struct Arrangement {
    columns: u32,
    rows: u32,
}

/// Derives the on-screen arrangement from the header's `matrix_count`/`columns` fields — row
/// count is `matrix_count / columns`, which always divides evenly since the device's config
/// module validates that invariant before ever sending a header.
fn arrangement_from_header(header: &Header) -> Arrangement {
    Arrangement { columns: header.columns as u32, rows: header.matrix_count as u32 / header.columns as u32 }
}

/// Reads into `buf` until it is full, `Ok(false)` on a clean EOF with nothing read yet, or an
/// error if the stream closes mid-message (a protocol desync, since every message here has a
/// size fixed in advance — see `doc/led-matrix-external-protocol.md` §3).
fn read_exact_or_eof<R: Read>(reader: &mut R, buf: &mut [u8]) -> io::Result<bool> {
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) if filled == 0 => return Ok(false),
            Ok(0) => return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "stream closed mid-message")),
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(true)
}

/// Reads one header from stdin, blocking until it arrives (spec §4: sent immediately when the
/// emulator attaches the transport, so this is effectively instantaneous after spawn).
fn read_header<R: Read>(reader: &mut R) -> io::Result<Header> {
    let mut buf = vec![0u8; protocol::HEADER_LEN];
    if !read_exact_or_eof(reader, &mut buf)? {
        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "stream closed before sending a header"));
    }
    protocol::decode_header(&buf).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Spawns a thread that reads tagged messages from stdin and forwards them over a bounded
/// channel — a full channel applies backpressure to the reader (never dropping a message; unlike
/// `CharDisplay`'s per-vsync frames, every block and palette message here carries state that
/// isn't resent, so none can be safely discarded). The thread (and thus the channel) ends on a
/// clean EOF or any read/decode error, the latter logged to stderr first; either way the main
/// loop treats a closed channel as "time to exit".
fn spawn_message_reader() -> mpsc::Receiver<Message> {
    let (tx, rx) = mpsc::sync_channel(64);
    thread::spawn(move || {
        let stdin = io::stdin();
        let mut lock = stdin.lock();
        let mut tag_buf = [0u8; 1];
        loop {
            match read_exact_or_eof(&mut lock, &mut tag_buf) {
                Ok(true) => {
                    let tag = tag_buf[0];
                    let Some(len) = protocol::body_len(tag) else {
                        eprintln!("emma65-led-matrix: unrecognized message tag {tag}, stream desynced");
                        break;
                    };
                    let mut body = vec![0u8; len];
                    match read_exact_or_eof(&mut lock, &mut body) {
                        Ok(true) => match protocol::decode_message(tag, &body) {
                            Ok(message) => {
                                if tx.send(message).is_err() {
                                    break; // receiver gone (window closed); stop reading
                                }
                            }
                            Err(e) => {
                                eprintln!("emma65-led-matrix: {e}");
                                break;
                            }
                        },
                        Ok(false) => break, // stream closed immediately after the tag byte, before any body bytes; treat as shutdown
                        Err(e) => {
                            eprintln!("emma65-led-matrix: {e}");
                            break;
                        }
                    }
                }
                Ok(false) => break, // clean EOF: emulator exited or transport shut down
                Err(e) => {
                    eprintln!("emma65-led-matrix: {e}");
                    break;
                }
            }
        }
    });
    rx
}

/// Applies one decoded message to per-matrix raw pixel state, the shared palette, or the
/// power/brightness state. Recompositing happens lazily at render time (see [`render`]), so none
/// of these need special per-matrix fan-out here — the next redraw naturally recomposites every
/// matrix's retained raw pixels against the updated palette/power/brightness state (spec §7).
fn apply_message(
    matrices: &mut [[u8; PIXELS_PER_MATRIX]],
    palette: &mut [Rgb565],
    power_mask: &mut u8,
    brightness: &mut u8,
    message: Message,
) {
    match message {
        Message::Block { matrix_index, pixels } => {
            if let Some(slot) = matrices.get_mut(matrix_index as usize) {
                slot.copy_from_slice(&pixels);
            }
        }
        Message::Palette { index, color } => {
            if let Some(slot) = palette.get_mut(index as usize) {
                *slot = color;
            }
        }
        Message::Power { mask } => *power_mask = mask,
        Message::Brightness { level } => *brightness = level,
    }
}

/// Renders every matrix's current raw pixels (recomposited against `palette`) as a grid of round
/// LEDs on a PCB-colored background, flush against each other per `arrangement` — no gap between
/// matrices, since real matrix boards mount edge-to-edge (`LedMatrixPanel.tsx`'s `drawMatrix`).
fn render(
    canvas: &mut sdl2::render::Canvas<sdl2::video::Window>,
    matrices: &[[u8; PIXELS_PER_MATRIX]],
    palette: &[Rgb565],
    power_mask: u8,
    brightness: u8,
    arrangement: Arrangement,
    pitch: u32,
) -> Result<(), String> {
    canvas.set_draw_color(Color::RGB(0, 0, 0));
    canvas.clear();

    let matrix_size_px = MATRIX_SIZE * pitch;
    let radius = ((pitch as f64) * LED_RADIUS_RATIO).round().max(1.0) as i16;

    for (index, pixels) in matrices.iter().enumerate() {
        let col = index as u32 % arrangement.columns;
        let row = index as u32 / arrangement.columns;
        let base_x = col * matrix_size_px;
        let base_y = row * matrix_size_px;

        canvas.set_draw_color(PCB_BACKGROUND_COLOR);
        canvas.fill_rect(Rect::new(base_x as i32, base_y as i32, matrix_size_px, matrix_size_px))?;

        let power_on = power_mask & (1u8 << index as u32) != 0;
        let rgba = composite_matrix(pixels, palette, power_on, brightness);
        for r in 0..MATRIX_SIZE {
            for c in 0..MATRIX_SIZE {
                let offset = ((r * MATRIX_SIZE + c) * 4) as usize;
                let (red, green, blue) = (rgba[offset], rgba[offset + 1], rgba[offset + 2]);
                let color = if red == 0 && green == 0 && blue == 0 { UNLIT_LED_COLOR } else { Color::RGB(red, green, blue) };
                let cx = (base_x + c * pitch + pitch / 2) as i16;
                let cy = (base_y + r * pitch + pitch / 2) as i16;
                canvas.filled_circle(cx, cy, radius, gfx_color(color))?;
            }
        }
    }

    canvas.present();
    Ok(())
}

fn main() {
    let args = Args::parse();

    let header = match read_header(&mut io::stdin().lock()) {
        Ok(header) => header,
        Err(e) => {
            eprintln!("emma65-led-matrix: failed to read header: {e}");
            std::process::exit(1);
        }
    };

    let arrangement = arrangement_from_header(&header);

    let mut matrices = vec![[0u8; PIXELS_PER_MATRIX]; header.matrix_count as usize];
    let mut palette = default_palette();
    let mut power_mask: u8 = 0xFF;
    let mut brightness: u8 = 0xFF;

    let pixel_width = arrangement.columns * MATRIX_SIZE * args.pitch;
    let pixel_height = arrangement.rows * MATRIX_SIZE * args.pitch;

    let rx = spawn_message_reader();

    let sdl_context = sdl2::init().expect("SDL2 init failed");
    let video = sdl_context.video().expect("SDL2 video subsystem init failed");
    let window = video
        .window(
            &format!("emma65 LED matrix - {}x{}", arrangement.columns, arrangement.rows),
            pixel_width,
            pixel_height,
        )
        .resizable()
        .position_centered()
        .build()
        .expect("failed to create SDL2 window");

    let mut canvas = window.into_canvas().build().expect("failed to create SDL2 canvas");
    canvas.set_logical_size(pixel_width, pixel_height).expect("failed to set logical render size");

    let mut event_pump = sdl_context.event_pump().expect("failed to obtain SDL2 event pump");

    'running: loop {
        for event in event_pump.poll_iter() {
            if let Event::Quit { .. } = event {
                break 'running;
            }
        }

        match rx.recv_timeout(Duration::from_millis(33)) {
            Ok(message) => {
                apply_message(&mut matrices, &mut palette, &mut power_mask, &mut brightness, message);
                while let Ok(message) = rx.try_recv() {
                    apply_message(&mut matrices, &mut palette, &mut power_mask, &mut brightness, message);
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break 'running,
        }

        render(&mut canvas, &matrices, &palette, power_mask, brightness, arrangement, args.pitch)
            .expect("render failed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sdl2::pixels::PixelFormatEnum;
    use sdl2::surface::Surface;

    /// Reads back the `(r, g, b, a)` bytes at `(x, y)` from an `RGBA32` surface. `RGBA32` (unlike
    /// the endianness-dependent bit-packed `RGBA8888`, which this test used at first and got
    /// bitten by the exact class of bug it's meant to catch) is one of SDL's format *aliases*
    /// that's defined to keep memory byte order `[r, g, b, a]` consistent across host
    /// endianness — it resolves to a different concrete bit-packed format per platform
    /// specifically to guarantee that.
    fn pixel_at(canvas: &sdl2::render::Canvas<Surface>, x: u32, y: u32) -> (u8, u8, u8, u8) {
        let surface = canvas.surface();
        let pitch = surface.pitch() as usize;
        let bytes = surface.without_lock().expect("surface must not be locked");
        let offset = y as usize * pitch + x as usize * 4;
        (bytes[offset], bytes[offset + 1], bytes[offset + 2], bytes[offset + 3])
    }

    /// Regression test for the SDL2_gfx color byte-order mismatch `gfx_color` works around:
    /// `sdl2::gfx`'s blanket `ToColor for Color` packs the color big-endian as `0xRRGGBBAA`, but
    /// SDL2_gfx's C entry points reinterpret that `u32` as raw native-order bytes, reversing every
    /// channel on a little-endian host (red ends up in alpha) — so a raw `Color` with red == 0
    /// (e.g. bright green) draws as fully transparent instead of the intended color. Uses a
    /// surface-backed canvas (`Surface::into_canvas`), which needs no video subsystem or display,
    /// so this runs in any environment.
    #[test]
    fn gfx_color_draws_the_actual_requested_color() {
        let surface = Surface::new(4, 4, PixelFormatEnum::RGBA32).unwrap();
        let mut canvas = surface.into_canvas().unwrap();
        canvas.set_draw_color(Color::RGBA(10, 10, 10, 255));
        canvas.clear();

        canvas.filled_circle(2, 2, 1, gfx_color(Color::RGB(0, 255, 0))).unwrap();

        assert_eq!(pixel_at(&canvas, 2, 2), (0, 255, 0, 255));
    }

    #[test]
    fn apply_message_power_sets_power_mask() {
        let mut matrices = [[0u8; PIXELS_PER_MATRIX]; 1];
        let mut palette = default_palette();
        let mut power_mask = 0xFFu8;
        let mut brightness = 0xFFu8;

        apply_message(&mut matrices, &mut palette, &mut power_mask, &mut brightness, Message::Power { mask: 0b0110 });

        assert_eq!(power_mask, 0b0110);
    }

    #[test]
    fn apply_message_brightness_sets_brightness() {
        let mut matrices = [[0u8; PIXELS_PER_MATRIX]; 1];
        let mut palette = default_palette();
        let mut power_mask = 0xFFu8;
        let mut brightness = 0xFFu8;

        apply_message(&mut matrices, &mut palette, &mut power_mask, &mut brightness, Message::Brightness { level: 0x7F });

        assert_eq!(brightness, 0x7F);
    }
}
