//! `emma65-display` — an external peripheral process that renders `CharDisplay`'s
//! composited output in an SDL2 window.
//!
//! Spawned by the emulator itself via a `display/char` device's `transport = "pipe:..."`
//! attribute (see `doc/char-display-external-protocol.md`); this binary is never run
//! standalone against a live `emma65` process any other way; its own stdin *is* the pipe. It
//! reads the one-time header,
//! then one fixed-size frame per vsync, decoding each with [`protocol`] and compositing pixels
//! with the same `emma65::emulator::device::display::compositing::composite` the debugger's
//! in-process display panel uses — no rendering logic is duplicated here. It also captures
//! keystrokes from the SDL2 window and writes them back over the same pipe (its own stdout) per
//! `doc/char-display-external-protocol.md` §6 — the first thing that gives the plain `emma65`
//! CLI keyboard input at all (see `doc/display-keyboard-integration-plan.md`).

mod protocol;

use std::io::{self, Read, Write};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use clap::Parser;
use emma65::emulator::device::display::compositing::composite;
use sdl2::event::Event;
use sdl2::keyboard::{Keycode, Mod};
use sdl2::pixels::PixelFormatEnum;

use protocol::{Frame, Header};

#[derive(Parser)]
#[command(about = "SDL2 external peripheral for emma65's memory-mapped character display")]
struct Args {
    /// Initial window scale factor (integer multiple of the native cells*8 pixel size); the
    /// window remains resizable afterward and SDL2 letterboxes/scales to fit.
    #[arg(long, default_value_t = 3)]
    scale: u32,
}

/// Reads into `buf` until it is full, `Ok(false)` on a clean EOF with nothing read yet, or an
/// error if the stream closes mid-message (a protocol desync, since every message here has a
/// size fixed in advance — see `doc/char-display-external-protocol.md` §3).
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

/// Spawns a thread that reads frames from stdin and forwards them over a bounded (capacity 1)
/// channel — backpressure naturally throttles reading if the render loop falls behind, rather
/// than buffering stale frames. The thread (and thus the channel) ends on a clean EOF or any
/// read/decode error, the latter logged to stderr first; either way the main loop treats a
/// closed channel as "time to exit".
fn spawn_frame_reader(frame_len: usize, columns: u32, rows: u32) -> mpsc::Receiver<Frame> {
    let (tx, rx) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let stdin = io::stdin();
        let mut lock = stdin.lock();
        let mut buf = vec![0u8; frame_len];
        loop {
            match read_exact_or_eof(&mut lock, &mut buf) {
                Ok(true) => {
                    let frame = protocol::decode_frame(&buf, columns, rows);
                    if tx.send(frame).is_err() {
                        break; // receiver gone (window closed); stop reading
                    }
                }
                Ok(false) => break, // clean EOF: emulator exited or transport shut down
                Err(e) => {
                    eprintln!("emma65-display: {e}");
                    break;
                }
            }
        }
    });
    rx
}

/// Encodes an SDL2 keyboard event into a single wire byte per
/// `doc/char-display-external-protocol.md` §6, mirroring `keyboardByteForEvent` in
/// `debugger/frontend/src/DisplayPanel.tsx`. `Event::TextInput` handles ordinary printable
/// characters (shift/layout-correct without a keycode table); `Event::KeyDown` handles
/// `Return`/`Backspace`/`Tab`/`Escape`/`Ctrl+<letter>`, none of which also fire `TextInput` in
/// SDL2, so there's no double-send to guard against. Returns `None` for anything else
/// (modifier-only presses, non-ASCII IME input, unmapped keys).
fn keystroke_byte(event: &Event) -> Option<u8> {
    match event {
        Event::TextInput { text, .. } => text.bytes().next().filter(u8::is_ascii),
        Event::KeyDown { keycode: Some(keycode), keymod, .. }
            if keymod.intersects(Mod::LCTRLMOD | Mod::RCTRLMOD) =>
        {
            match keycode.into_i32() {
                code @ 97..=122 => Some((code - 97 + 1) as u8), // Ctrl+A..Z -> 0x01..0x1a
                _ => None,
            }
        }
        Event::KeyDown { keycode: Some(keycode), .. } => match *keycode {
            Keycode::RETURN => Some(0x0d),
            Keycode::BACKSPACE => Some(0x08),
            Keycode::TAB => Some(0x09),
            Keycode::ESCAPE => Some(0x1b),
            _ => None,
        },
        _ => None,
    }
}

fn main() {
    let args = Args::parse();

    let header = match read_header(&mut io::stdin().lock()) {
        Ok(header) => header,
        Err(e) => {
            eprintln!("emma65-display: failed to read header: {e}");
            std::process::exit(1);
        }
    };

    eprintln!(
        "emma65-display: connected, {}x{} cells @ {} Hz",
        header.columns, header.rows, header.frame_rate_hz
    );

    let pixel_width = header.columns * 8;
    let pixel_height = header.rows * 8;
    let frame_len = header.frame_len();

    let rx = spawn_frame_reader(frame_len, header.columns, header.rows);

    let sdl_context = sdl2::init().expect("SDL2 init failed");
    let video = sdl_context.video().expect("SDL2 video subsystem init failed");
    let window = video
        .window(
            &format!("emma65 display ({}x{} cells)", header.columns, header.rows),
            pixel_width * args.scale.max(1),
            pixel_height * args.scale.max(1),
        )
        .resizable()
        .position_centered()
        .build()
        .expect("failed to create SDL2 window");

    video.text_input().start();

    let mut canvas = window.into_canvas().build().expect("failed to create SDL2 canvas");
    canvas.set_logical_size(pixel_width, pixel_height).expect("failed to set logical render size");
    let texture_creator = canvas.texture_creator();
    let mut texture = texture_creator
        .create_texture_streaming(PixelFormatEnum::RGBA32, pixel_width, pixel_height)
        .expect("failed to create SDL2 texture");

    let mut event_pump = sdl_context.event_pump().expect("failed to obtain SDL2 event pump");
    let pitch = (pixel_width * 4) as usize;
    let mut stdout = io::stdout();

    'running: loop {
        for event in event_pump.poll_iter() {
            if let Event::Quit { .. } = event {
                break 'running;
            }
            if let Some(byte) = keystroke_byte(&event)
                && stdout.write_all(&[byte]).is_err()
            {
                // The emulator side closed the pipe (process exited); nothing left to drive.
                break 'running;
            }
        }

        match rx.recv_timeout(Duration::from_millis(10)) {
            Ok(frame) => {
                let pixels = composite(&frame.char_ram, &frame.color_ram, header.columns, header.rows, &frame.palette, &header.font);
                texture.update(None, &pixels, pitch).expect("failed to update SDL2 texture");
                canvas.clear();
                canvas.copy(&texture, None, None).expect("failed to blit SDL2 texture");
                canvas.present();
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break 'running,
        }
    }
}
