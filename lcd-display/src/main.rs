//! `emma65-lcd-display` — an external peripheral process that renders `LcdDisplay`'s composited
//! output in an SDL2 window.
//!
//! Spawned by the emulator itself via a `display/lcd` device's `transport = "pipe:..."`
//! attribute (see `plan/lcd-display-external-protocol.md`); this binary is never run standalone
//! against a live `emma65` process any other way — its own stdin *is* the pipe. It reads the
//! one-time header, then a background thread decodes each self-describing frame that follows
//! (§5) into raw RGBA pixels. Unlike `emma65-led-matrix`/`emma65-display`, no compositing logic
//! runs here at all — `LcdDisplay`'s frame sink already produces a fully composited flat buffer
//! (background/foreground baked in, no palette concept) — but the *cosmetic* dot-matrix rendering
//! (square dots, inter-dot/inter-cell gaps, a dimly-blended "off" state) is deliberately not
//! shared with `LcdDisplayPanel.tsx` (see that file's doc comment and spec §6), so this binary
//! ports that same cosmetic treatment to SDL2 with its own native primitives, the same split
//! `emma65-led-matrix` uses for its round-LED rendering. Unlike `CharDisplay`'s peripheral, there
//! is no keyboard forwarding — an LCD display has no input capability, like `LedMatrix`.
//!
//! Per spec §7 there is no "replay the last frame" mechanism, so a freshly attached peripheral
//! shows a blank grid (in `background`, at an assumed 5x8 font) until its first frame arrives.

mod protocol;

use std::io::{self, Read};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use clap::Parser;
use emma65::emulator::device::lcd_display::compositing::Rgb24;
use sdl2::event::Event;
use sdl2::pixels::Color;
use sdl2::rect::Rect;

use protocol::Header;

/// Fixed glyph cell width in dots (spec §8.2 of the device spec), regardless of font.
const DOTS_PER_CELL_WIDTH: u32 = 5;

/// Glyph cell height assumed before the first frame arrives (spec §7: "should render a blank grid
/// ... until its first frame arrives") — matches `LcdDisplayPanel.tsx`'s `recomputePitch` fallback.
const DEFAULT_CELL_HEIGHT_DOTS: u32 = 8;

/// Extra gap between adjacent character cells, in whole dot pitches — matches
/// `LcdDisplayPanel.tsx`'s `CELL_GAP_PITCHES` (issue #569).
const CELL_GAP_PITCHES: u32 = 1;

/// Fraction of the on-screen dot pitch a dot actually covers — matches `LcdDisplayPanel.tsx`'s
/// `DOT_FILL_RATIO`. Dots are plain squares, not rounded rects (issue #593 follow-up): rounding
/// every dot's corners independently of its neighbors made adjacent same-color dots in a glyph
/// stroke merge into a pinched "hourglass" shape at the seam instead of a clean rectangle, which
/// read as some dots looking smaller/raggeder than others.
const DOT_FILL_RATIO: f64 = 0.75;

/// How far an "off" dot is blended from `background` toward `foreground` — matches
/// `LcdDisplayPanel.tsx`'s `OFF_DOT_BLEND`.
const OFF_DOT_BLEND: f64 = 0.15;

/// Width of the black plastic bezel drawn around the viewing window, in whole dot pitches —
/// matches `LcdDisplayPanel.tsx`'s `BEZEL_PITCHES` (issue #579). Deliberately just the bezel, no
/// surrounding PCB — see that constant's doc comment.
const BEZEL_PITCHES: u32 = 3;

/// Near-black bezel color — matches `LcdDisplayPanel.tsx`'s `BEZEL_COLOR` and
/// `led-matrix/src/main.rs`'s `PCB_BACKGROUND_COLOR` tone.
const BEZEL_COLOR: Color = Color::RGB(0x0a, 0x0a, 0x0a);

/// Linearly blends `from` toward `to` by `t` (0..1), per channel, rounded to the nearest integer
/// — mirrors `LcdDisplayPanel.tsx`'s `blendColor`.
fn blend_color(from: Rgb24, to: Rgb24, t: f64) -> Color {
    let blend = |a: u8, b: u8| (a as f64 + (b as f64 - a as f64) * t).round() as u8;
    Color::RGB(blend(from.r, to.r), blend(from.g, to.g), blend(from.b, to.b))
}

#[derive(Parser)]
#[command(about = "SDL2 external peripheral for emma65's memory-mapped LCD display")]
struct Args {
    /// Initial on-screen dot center-to-center spacing, in pixels. The window remains resizable
    /// afterward; SDL2 letterboxes/scales its fixed logical size to fit.
    #[arg(long, default_value_t = 12)]
    pitch: u32,
}

/// The most recently received frame's raw RGBA pixels plus its pixel dimensions (spec §5) —
/// `None` until the first frame arrives (spec §7).
struct Frame {
    width_px: u32,
    height_px: u32,
    pixels: Vec<u8>,
}

/// Reads into `buf` until it is full, `Ok(false)` on a clean EOF with nothing read yet, or an
/// error if the stream closes mid-message (a protocol desync, since every message here has a
/// size fixed in advance once its prefix is known — see `plan/lcd-display-external-protocol.md`
/// §3).
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

/// Reads one frame from `reader`: first the dimension prefix (spec §5), then exactly
/// `width_px * height_px * 4` further bytes of raw RGBA — a frame's total size can't be known
/// until the prefix is decoded, unlike `CharDisplay`'s/`LedMatrix`'s fixed-size messages.
fn read_frame<R: Read>(reader: &mut R) -> io::Result<Option<Frame>> {
    let mut dims_buf = [0u8; protocol::FRAME_DIMENSIONS_LEN];
    if !read_exact_or_eof(reader, &mut dims_buf)? {
        return Ok(None);
    }
    let (width_px, height_px) = protocol::decode_frame_dimensions(&dims_buf);
    let mut pixels = vec![0u8; width_px as usize * height_px as usize * 4];
    if !read_exact_or_eof(reader, &mut pixels)? {
        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "stream closed before sending a frame's pixels"));
    }
    Ok(Some(Frame { width_px: width_px as u32, height_px: height_px as u32, pixels }))
}

/// Spawns a thread that reads frames from stdin and forwards them over a bounded (capacity 1)
/// channel — backpressure naturally throttles reading if the render loop falls behind, rather
/// than buffering stale frames, same policy as `emma65-display`'s `spawn_frame_reader`. The
/// thread (and thus the channel) ends on a clean EOF or any read/decode error, the latter logged
/// to stderr first; either way the main loop treats a closed channel as "time to exit".
fn spawn_frame_reader() -> mpsc::Receiver<Frame> {
    let (tx, rx) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let stdin = io::stdin();
        let mut lock = stdin.lock();
        loop {
            match read_frame(&mut lock) {
                Ok(Some(frame)) => {
                    if tx.send(frame).is_err() {
                        break; // receiver gone (window closed); stop reading
                    }
                }
                Ok(None) => break, // clean EOF: emulator exited or transport shut down
                Err(e) => {
                    eprintln!("emma65-lcd-display: {e}");
                    break;
                }
            }
        }
    });
    rx
}

/// Derives the on-screen pixel size of the full grid (glyph cells plus inter-cell gaps, framed by
/// the bezel) for a given `cell_height_dots`, at `pitch` pixels per dot — mirrors
/// `LcdDisplayPanel.tsx`'s `drawFrame` sizing math.
fn window_size(header: &Header, cell_height_dots: u32, pitch: u32) -> (u32, u32) {
    let total_dots_wide = header.columns as u32 * DOTS_PER_CELL_WIDTH + (header.columns as u32 - 1) * CELL_GAP_PITCHES;
    let total_dots_high = header.rows as u32 * cell_height_dots + (header.rows as u32 - 1) * CELL_GAP_PITCHES;
    let bezel_px = 2 * BEZEL_PITCHES * pitch;
    (total_dots_wide * pitch + bezel_px, total_dots_high * pitch + bezel_px)
}

/// Renders the current `frame` (or a blank `background` grid before the first one arrives, per
/// spec §7) as a dot-matrix grid at `pitch` pixels per dot, framed by a `BEZEL_PITCHES`-wide black
/// bezel (issue #579), against `header`'s configured colors — mirrors `LcdDisplayPanel.tsx`'s
/// `drawFrame`/`drawDot`.
fn render<T: sdl2::render::RenderTarget>(
    canvas: &mut sdl2::render::Canvas<T>,
    header: &Header,
    frame: Option<&Frame>,
    pitch: u32,
) -> Result<(), String> {
    let background = Color::RGB(header.background.r, header.background.g, header.background.b);
    let off_color = blend_color(header.background, header.foreground, OFF_DOT_BLEND);
    let bezel_px = BEZEL_PITCHES * pitch;

    canvas.set_draw_color(BEZEL_COLOR);
    canvas.clear();

    let total_dots_wide = header.columns as u32 * DOTS_PER_CELL_WIDTH + (header.columns as u32 - 1) * CELL_GAP_PITCHES;

    let Some(frame) = frame else {
        let total_dots_high = header.rows as u32 * DEFAULT_CELL_HEIGHT_DOTS + (header.rows as u32 - 1) * CELL_GAP_PITCHES;
        canvas.set_draw_color(background);
        canvas.fill_rect(Rect::new(bezel_px as i32, bezel_px as i32, total_dots_wide * pitch, total_dots_high * pitch))?;
        canvas.present();
        return Ok(());
    };

    let cell_height_dots = if header.rows == 0 { 0 } else { frame.height_px / header.rows as u32 };
    if cell_height_dots == 0 {
        canvas.present();
        return Ok(());
    }

    let total_dots_high = header.rows as u32 * cell_height_dots + (header.rows as u32 - 1) * CELL_GAP_PITCHES;
    canvas.set_draw_color(background);
    canvas.fill_rect(Rect::new(bezel_px as i32, bezel_px as i32, total_dots_wide * pitch, total_dots_high * pitch))?;

    let dot_size = (pitch as f64 * DOT_FILL_RATIO).round().max(1.0) as u32;
    let half = (dot_size / 2) as i32;

    for row in 0..header.rows as u32 {
        for dot_row in 0..cell_height_dots {
            let raw_y = row * cell_height_dots + dot_row;
            let cy = (bezel_px + (row * (cell_height_dots + CELL_GAP_PITCHES) + dot_row) * pitch + pitch / 2) as i32;
            for col in 0..header.columns as u32 {
                for dot_col in 0..DOTS_PER_CELL_WIDTH {
                    let raw_x = col * DOTS_PER_CELL_WIDTH + dot_col;
                    let offset = ((raw_y * frame.width_px + raw_x) * 4) as usize;
                    let (r, g, b) = (frame.pixels[offset], frame.pixels[offset + 1], frame.pixels[offset + 2]);
                    let is_background = r == header.background.r && g == header.background.g && b == header.background.b;
                    let color = if is_background { off_color } else { Color::RGB(r, g, b) };
                    let cx = (bezel_px + (col * (DOTS_PER_CELL_WIDTH + CELL_GAP_PITCHES) + dot_col) * pitch + pitch / 2) as i32;
                    canvas.set_draw_color(color);
                    canvas.fill_rect(Rect::new(cx - half, cy - half, dot_size, dot_size))?;
                }
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
            eprintln!("emma65-lcd-display: failed to read header: {e}");
            std::process::exit(1);
        }
    };

    let (pixel_width, pixel_height) = window_size(&header, DEFAULT_CELL_HEIGHT_DOTS, args.pitch);

    let rx = spawn_frame_reader();

    let sdl_context = sdl2::init().expect("SDL2 init failed");
    let video = sdl_context.video().expect("SDL2 video subsystem init failed");
    let window = video
        .window(&format!("emma65 LCD display - {}x{}", header.columns, header.rows), pixel_width, pixel_height)
        .resizable()
        .position_centered()
        .build()
        .expect("failed to create SDL2 window");

    let mut canvas = window.into_canvas().build().expect("failed to create SDL2 canvas");
    canvas.set_logical_size(pixel_width, pixel_height).expect("failed to set logical render size");

    let mut event_pump = sdl_context.event_pump().expect("failed to obtain SDL2 event pump");

    let mut current_frame: Option<Frame> = None;
    let mut logical_size = (pixel_width, pixel_height);

    'running: loop {
        for event in event_pump.poll_iter() {
            if let Event::Quit { .. } = event {
                break 'running;
            }
        }

        match rx.recv_timeout(Duration::from_millis(10)) {
            Ok(frame) => current_frame = Some(frame),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break 'running,
        }

        let cell_height_dots = current_frame
            .as_ref()
            .filter(|_| header.rows != 0)
            .map(|frame| frame.height_px / header.rows as u32)
            .unwrap_or(DEFAULT_CELL_HEIGHT_DOTS);
        let wanted_size = window_size(&header, cell_height_dots, args.pitch);
        if wanted_size != logical_size {
            canvas.window_mut().set_size(wanted_size.0, wanted_size.1).expect("failed to resize SDL2 window");
            canvas.set_logical_size(wanted_size.0, wanted_size.1).expect("failed to set logical render size");
            logical_size = wanted_size;
        }

        render(&mut canvas, &header, current_frame.as_ref(), args.pitch).expect("render failed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sdl2::pixels::PixelFormatEnum;
    use sdl2::surface::Surface;

    fn sample_header() -> Header {
        Header { columns: 2, rows: 1, background: Rgb24::new(0, 0, 0), foreground: Rgb24::new(255, 255, 255) }
    }

    /// Reads back the `(r, g, b, a)` bytes at `(x, y)` from an `RGBA32` surface — same approach as
    /// `led-matrix/src/main.rs`'s `pixel_at`, for the same byte-order reasons.
    fn pixel_at(canvas: &sdl2::render::Canvas<Surface>, x: u32, y: u32) -> (u8, u8, u8, u8) {
        let surface = canvas.surface();
        let pitch = surface.pitch() as usize;
        let bytes = surface.without_lock().expect("surface must not be locked");
        let offset = y as usize * pitch + x as usize * 4;
        (bytes[offset], bytes[offset + 1], bytes[offset + 2], bytes[offset + 3])
    }

    #[test]
    fn window_size_accounts_for_cell_gaps_and_bezel() {
        let header = sample_header();
        let (width, height) = window_size(&header, 8, 10);

        // 2 columns * 5 dots + 1 gap = 11 dots wide; 1 row * 8 dots + 0 gaps = 8 dots high; plus a
        // BEZEL_PITCHES-wide bezel on every side.
        assert_eq!(width, 11 * 10 + 2 * BEZEL_PITCHES * 10);
        assert_eq!(height, 8 * 10 + 2 * BEZEL_PITCHES * 10);
    }

    #[test]
    fn blend_color_interpolates_channels() {
        let from = Rgb24::new(0, 0, 0);
        let to = Rgb24::new(255, 255, 255);

        let color = blend_color(from, to, 0.5);

        assert_eq!(color, Color::RGB(128, 128, 128));
    }

    #[test]
    fn render_before_first_frame_fills_bezel_and_background() {
        let header = sample_header();
        let surface = Surface::new(64, 64, PixelFormatEnum::RGBA32).unwrap();
        let mut canvas = surface.into_canvas().unwrap();

        render(&mut canvas, &header, None, 10).unwrap();

        // A pixel within the bezel margin (BEZEL_PITCHES * pitch == 30px wide) reads as the bezel
        // color; `sample_header`'s background is a distinct near-black (0, 0, 0), unlike the
        // bezel's (0x0a, 0x0a, 0x0a), so the two are still distinguishable.
        assert_eq!(pixel_at(&canvas, 5, 5), (0x0a, 0x0a, 0x0a, 255));
        // A pixel inside the viewport (past the 30px bezel margin) reads as the background color.
        assert_eq!(pixel_at(&canvas, 35, 35), (0, 0, 0, 255));
    }

    #[test]
    fn render_draws_foreground_dot_where_frame_pixel_is_not_background() {
        let header = sample_header();
        let width_px = 10u32; // 2 columns * 5 dots
        let height_px = 8u32; // 1 row * 8 dots (5x8 font)
        let mut pixels = vec![0u8; (width_px * height_px * 4) as usize];
        // Mark the dot at raw (2, 1) -- the middle column of an 'A' glyph's row 1 -- as foreground.
        let offset = ((width_px + 2) * 4) as usize;
        pixels[offset..offset + 4].copy_from_slice(&[255, 255, 255, 255]);
        let frame = Frame { width_px, height_px, pixels };

        let surface = Surface::new(200, 200, PixelFormatEnum::RGBA32).unwrap();
        let mut canvas = surface.into_canvas().unwrap();
        let pitch = 10u32;

        render(&mut canvas, &header, Some(&frame), pitch).unwrap();

        let bezel_px = BEZEL_PITCHES * pitch;
        let cy = bezel_px + pitch + pitch / 2;
        let cx = bezel_px + 2 * pitch + pitch / 2;
        assert_eq!(pixel_at(&canvas, cx, cy), (255, 255, 255, 255));
    }
}
