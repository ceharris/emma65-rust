import { useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";

/**
 * `lcd_display::LcdDisplayGeometryPayload`'s shape -- the character grid an `display/lcd` device
 * was configured with, plus its configuration-time-fixed background/foreground colors (spec §3,
 * §8.3). Unlike `DisplayGeometry`/`LedMatrixGeometry` it carries no pixel dimensions, since those
 * depend on the configured font (5x8 vs 5x10, spec §8.2), which this payload doesn't report --
 * cell pixel height is instead derived per frame from that frame's own pixel buffer (see
 * `decodeFrame`).
 */
interface LcdDisplayGeometry {
  columns: number;
  rows: number;
  background: [number, number, number];
  foreground: [number, number, number];
}

/**
 * `lcd_display::LcdDisplayFramePayload`'s shape. `pixels` arrives base64-encoded for the same
 * reason as `DisplayFramePayload::pixels` -- see `DisplayPanel.tsx`'s identical `decodeBase64`.
 */
interface LcdDisplayFramePayload {
  pixels: string;
  columns: number;
  rows: number;
}

/** Fixed glyph cell width in dots (spec §8.2), regardless of font. */
const DOTS_PER_CELL_WIDTH = 5;

/** Floor the on-screen dot pitch (center-to-center spacing) never drops below, mirroring
 * `LedMatrixPanel.tsx`'s `MIN_PITCH_PX` -- dots are drawn as vector squares, not upscaled pixels,
 * so this doesn't need to be an integer; it just keeps the grid legible rather than a smear when
 * the dock cell or detached window is very small. */
const MIN_PITCH_PX = 6;

/** Fraction of `pitch` a dot actually covers; the remainder is the gap between adjacent dots
 * within a cell (issue #569: "there's typically a very small gap between pixels"). Dots are plain
 * squares, not rounded rects (issue #593 follow-up): rounding every dot's corners independently of
 * its neighbors made adjacent same-color dots in a glyph stroke merge into a pinched "hourglass"
 * shape at the seam instead of a clean rectangle, which read as some dots looking
 * smaller/raggeder than others. */
const DOT_FILL_RATIO = 0.75;

/** Extra gap between adjacent character cells, in whole dot pitches, so neighboring glyphs don't
 * read as joined (issue #569: "a gap of about one pixel width between each character cell").
 * Applied both between columns and between rows, matching a real module's cell-separator
 * lines. */
const CELL_GAP_PITCHES = 1;

/** How far an "off" dot is blended from `background` toward `foreground`, simulating the
 * always-somewhat-visible contrast between a real module's backlight and its unlit segments
 * (issue #569) -- rather than rendering "off" as flat, invisible background. Halved from an
 * original 0.15 (issue #595): review across all 8 polarity/backlight presets found unlit dots
 * still read as too bright at 0.15. */
const OFF_DOT_BLEND = 0.075;

/** Width of the black plastic bezel drawn around the viewing window, in whole dot pitches
 * (issue #579: "[v]irtually all common LCD display components in the market have a black bezel
 * area around the display itself"). Deliberately just the bezel -- unlike `LedMatrixPanel.tsx`'s
 * `PCB_BACKGROUND_COLOR`, no surrounding circuit board is drawn (issue #579's reference photo
 * shows exposed PCB at the board's edges, which this simulation doesn't represent). */
const BEZEL_PITCHES = 3;

/** Near-black bezel color -- matches `LedMatrixPanel.tsx`'s `PCB_BACKGROUND_COLOR` tone for a
 * consistent "plastic/PCB black" across peripheral panels, even though this is a different part
 * (a bezel, not a substrate). */
const BEZEL_COLOR = "#0a0a0a";

/** Decodes a base64 string into raw bytes -- see `DisplayPanel.tsx` for why this beats
 * `JSON.parse`-ing a plain number array per frame. */
function decodeBase64(base64: string): Uint8ClampedArray<ArrayBuffer> {
  const binary = atob(base64);
  const bytes = new Uint8ClampedArray(binary.length);
  for (let i = 0; i < binary.length; i++) {
    bytes[i] = binary.charCodeAt(i);
  }
  return bytes;
}

/** A decoded frame plus the per-cell dot grid dimensions derived from it. */
interface LcdFrame {
  pixels: Uint8ClampedArray;
  /** Raw pixel-buffer width, in dots (`columns * DOTS_PER_CELL_WIDTH`). */
  widthDots: number;
  /** Glyph cell height in dots -- 8 or 10 depending on the active font (spec §8.2), derived from
   * the buffer's own length since no payload reports it directly. */
  cellHeightDots: number;
}

/** Decodes `payload.pixels` and derives its dot-grid dimensions, mirroring
 * `LcdDisplayPanel.tsx`'s previous `paintFrame`: `heightDots = pixels.length / 4 / widthDots /
 * rows` is exact since `compositing::composite` always produces a `width_px * height_px * 4`-byte
 * buffer with no padding. */
function decodeFrame(payload: LcdDisplayFramePayload): LcdFrame {
  const pixels = decodeBase64(payload.pixels);
  const widthDots = payload.columns * DOTS_PER_CELL_WIDTH;
  const heightDots = widthDots > 0 ? pixels.length / 4 / widthDots : 0;
  const cellHeightDots = payload.rows > 0 ? heightDots / payload.rows : 0;
  return { pixels, widthDots, cellHeightDots };
}

/** Linearly blends `from` toward `to` by `t` (0..1), per channel, rounded to the nearest
 * integer. */
function blendColor(from: [number, number, number], to: [number, number, number], t: number): string {
  const r = Math.round(from[0] + (to[0] - from[0]) * t);
  const g = Math.round(from[1] + (to[1] - from[1]) * t);
  const b = Math.round(from[2] + (to[2] - from[2]) * t);
  return `rgb(${r}, ${g}, ${b})`;
}

/** Draws one dot as a plain square centered at `(cx, cy)` with the given full side length (before
 * `DOT_FILL_RATIO` is applied by the caller). Isolated from `drawFrame`'s grid-walking loop the
 * same way `LedMatrixPanel.tsx`'s `drawLed` is. */
function drawDot(ctx: CanvasRenderingContext2D, cx: number, cy: number, size: number, color: string) {
  const half = size / 2;
  ctx.fillStyle = color;
  ctx.fillRect(cx - half, cy - half, size, size);
}

/** Renders one decoded frame into `canvas` as a dot-matrix grid at the given on-screen `pitch`
 * (dot center-to-center spacing, in CSS pixels), against `geometry`'s configured colors, framed
 * by a `BEZEL_PITCHES`-wide black bezel (issue #579). Called both when a fresh frame arrives and
 * when `pitch` changes (a resize), so a resize can redraw already-known pixel data immediately
 * rather than waiting on the next frame.
 *
 * Dots are positioned and sized in *device* pixels, rounded to whole pixels, rather than at the
 * raw (generally fractional) CSS-pixel pitch (issue #593): since `pitch` is a continuous value
 * chosen to fill the container, drawing anti-aliased dots at its exact fractional position would
 * put each dot's edges at a different sub-pixel phase than its neighbors, so each dot's
 * anti-aliasing picks up a different amount of edge coverage -- visible as uneven dot brightness,
 * worse the fewer pixels each dot spans. Rounding every dot to the same integer device-pixel grid
 * makes each one a plain translation of the same shape, so their AA coverage -- and thus apparent
 * brightness -- is identical. */
function drawFrame(canvas: HTMLCanvasElement, frame: LcdFrame, geometry: LcdDisplayGeometry, pitch: number) {
  const ctx = canvas.getContext("2d");
  if (!ctx || frame.widthDots === 0 || frame.cellHeightDots === 0) return;

  const totalDotsWide = geometry.columns * DOTS_PER_CELL_WIDTH + (geometry.columns - 1) * CELL_GAP_PITCHES;
  const totalDotsHigh = geometry.rows * frame.cellHeightDots + (geometry.rows - 1) * CELL_GAP_PITCHES;
  const bezelPx = BEZEL_PITCHES * pitch;
  const viewportWidthPx = totalDotsWide * pitch;
  const viewportHeightPx = totalDotsHigh * pitch;
  const widthPx = viewportWidthPx + 2 * bezelPx;
  const heightPx = viewportHeightPx + 2 * bezelPx;

  const dpr = window.devicePixelRatio || 1;
  canvas.width = Math.round(widthPx * dpr);
  canvas.height = Math.round(heightPx * dpr);
  canvas.style.width = `${widthPx}px`;
  canvas.style.height = `${heightPx}px`;
  // Draw directly in device pixels (no dpr transform) so dot geometry below can be rounded to
  // whole device pixels rather than whole CSS pixels.
  ctx.setTransform(1, 0, 0, 1, 0, 0);

  const pitchDevice = pitch * dpr;
  const bezelPxDevice = bezelPx * dpr;
  const viewportWidthPxDevice = viewportWidthPx * dpr;
  const viewportHeightPxDevice = viewportHeightPx * dpr;

  ctx.fillStyle = BEZEL_COLOR;
  ctx.fillRect(0, 0, canvas.width, canvas.height);

  const backgroundColor = `rgb(${geometry.background.join(", ")})`;
  const offColor = blendColor(geometry.background, geometry.foreground, OFF_DOT_BLEND);
  ctx.fillStyle = backgroundColor;
  ctx.fillRect(bezelPxDevice, bezelPxDevice, viewportWidthPxDevice, viewportHeightPxDevice);

  const dotSize = Math.max(1, Math.round(pitchDevice * DOT_FILL_RATIO));
  for (let row = 0; row < geometry.rows; row++) {
    for (let dotRow = 0; dotRow < frame.cellHeightDots; dotRow++) {
      const rawY = row * frame.cellHeightDots + dotRow;
      const cy = Math.round(bezelPxDevice + (row * (frame.cellHeightDots + CELL_GAP_PITCHES) + dotRow + 0.5) * pitchDevice);
      for (let col = 0; col < geometry.columns; col++) {
        for (let dotCol = 0; dotCol < DOTS_PER_CELL_WIDTH; dotCol++) {
          const rawX = col * DOTS_PER_CELL_WIDTH + dotCol;
          const offset = (rawY * frame.widthDots + rawX) * 4;
          const r = frame.pixels[offset];
          const g = frame.pixels[offset + 1];
          const b = frame.pixels[offset + 2];
          const isBackground = r === geometry.background[0] && g === geometry.background[1] && b === geometry.background[2];
          const color = isBackground ? offColor : `rgb(${r}, ${g}, ${b})`;
          const cx = Math.round(bezelPxDevice + (col * (DOTS_PER_CELL_WIDTH + CELL_GAP_PITCHES) + dotCol + 0.5) * pitchDevice);
          drawDot(ctx, cx, cy, dotSize, color);
        }
      }
    }
  }
}

/**
 * The dock panel hosting the memory-mapped LCD display device's composited output (memory-mapped
 * LCD display device plan, Work Unit 5 -- design §10; dot-matrix cosmetics per issue #569; black
 * bezel and yellow-green/black default colors per issue #579).
 * Compositing (DDRAM/CGRAM + CGROM + palette -> RGBA) happens entirely in the Rust backend
 * (`emulator::device::lcd_display::compositing`, `lcd_display.rs`'s bridge task), which emits a
 * flat 1-pixel-per-dot buffer; this component is responsible for the cosmetic dot-matrix
 * rendering on top of it (rounded dots, inter-dot and inter-cell gaps, a dimly-visible "off"
 * state, a bezel) the same way `LedMatrixPanel.tsx` draws its raw per-LED buffer as round LEDs on
 * a PCB -- neither cosmetic layer lives in the shared library, since each renderer (this panel and
 * the SDL2 companion, `emma65-lcd-display`, mirroring `led-matrix`'s) draws with its own native
 * primitives at its own resolution rather than blitting a backend-rasterized bitmap. Unlike
 * `DisplayPanel`, there's no keyboard input to forward -- an LCD display has no input capability,
 * like `LedMatrixPanel`.
 *
 * Mounted in both the docked panel and the detached-LCD-Display window
 * (`lcd-display-detached.tsx`), same reuse shape as `DisplayPanel`/`display-detached.tsx`: no
 * device-specific state lives outside this component, so a detach/reattach cycle's unmount+remount
 * just re-fetches geometry/the cached frame and starts drawing fresh frames.
 *
 * A frame is only ever pushed on a render-affecting register write (design doc §7), never on a
 * periodic vsync, so a freshly-mounted panel could otherwise sit blank until some unrelated later
 * write happens. `get_lcd_display_frame` (backed by `lcd_display::LcdDisplayFrameCache`) covers
 * that by fetching the last delivered frame on mount and painting it immediately, mirroring
 * `LedMatrixPanel`'s `get_led_matrix_frames` fetch -- until either that fetch or the first live
 * `"lcd-display-frame"` event lands, nothing has been composited yet this session and the panel
 * shows an empty canvas, the same as an un-initialized real HD44780 module.
 *
 * A `ResizeObserver` on the container recomputes the largest on-screen dot pitch that fits the
 * full grid (glyph cells plus inter-cell gaps), never smaller than `MIN_PITCH_PX` -- continuous
 * rather than floored to an integer multiple, the same departure from the old pixelated-blit
 * approach `LedMatrixPanel`'s pitch already made, since there's no pixel grid to keep aligned to
 * whole multiples of when every dot is drawn as its own vector shape.
 */
export default function LcdDisplayPanel() {
  const containerRef = useRef<HTMLDivElement>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const frameRef = useRef<LcdFrame | null>(null);
  const pitchRef = useRef(MIN_PITCH_PX);
  const [geometry, setGeometry] = useState<LcdDisplayGeometry | null>(null);
  const [pitch, setPitch] = useState(MIN_PITCH_PX);

  useEffect(() => {
    invoke<LcdDisplayGeometry | null>("get_lcd_display_geometry")
      .then(setGeometry)
      .catch((err) => console.error("get_lcd_display_geometry failed:", err));
  }, []);

  useEffect(() => {
    pitchRef.current = pitch;
    const canvas = canvasRef.current;
    const frame = frameRef.current;
    if (canvas && frame && geometry) drawFrame(canvas, frame, geometry, pitch);
  }, [pitch, geometry]);

  // Paints the last delivered frame, if any, once the canvas exists to paint into -- runs after
  // `geometry` settles (not on the initial, geometry-less render), since the canvas below is only
  // rendered once `geometry` is known. Mirrors `LedMatrixPanel`'s analogous cache-replay effect.
  useEffect(() => {
    if (!geometry) return;
    invoke<LcdDisplayFramePayload | null>("get_lcd_display_frame")
      .then((payload) => {
        const canvas = canvasRef.current;
        if (!payload || !canvas) return;
        const frame = decodeFrame(payload);
        frameRef.current = frame;
        drawFrame(canvas, frame, geometry, pitchRef.current);
      })
      .catch((err) => console.error("get_lcd_display_frame failed:", err));
  }, [geometry]);

  // Registered once, before `geometry` is even known, the same as `DisplayPanel`'s frame listener
  // -- so a frame that arrives in the gap between mount and the geometry fetch resolving is never
  // missed; the canvas ref is simply null until `geometry` resolves and renders it, in which case
  // the frame is silently dropped rather than queued.
  useEffect(() => {
    const unlistenPromise = listen<LcdDisplayFramePayload>("lcd-display-frame", (event) => {
      const frame = decodeFrame(event.payload);
      frameRef.current = frame;
      const canvas = canvasRef.current;
      if (canvas && geometry) drawFrame(canvas, frame, geometry, pitchRef.current);
    });
    return () => {
      unlistenPromise.then((f) => f());
    };
  }, [geometry]);

  useEffect(() => {
    const container = containerRef.current;
    if (!container || !geometry) return;
    const recomputePitch = () => {
      const cellHeightDots = frameRef.current?.cellHeightDots || 8;
      const totalDotsWide = geometry.columns * DOTS_PER_CELL_WIDTH + (geometry.columns - 1) * CELL_GAP_PITCHES + 2 * BEZEL_PITCHES;
      const totalDotsHigh = geometry.rows * cellHeightDots + (geometry.rows - 1) * CELL_GAP_PITCHES + 2 * BEZEL_PITCHES;
      const fit = Math.min(container.clientWidth / totalDotsWide, container.clientHeight / totalDotsHigh);
      setPitch(Math.max(MIN_PITCH_PX, fit));
    };
    recomputePitch();
    const observer = new ResizeObserver(recomputePitch);
    observer.observe(container);
    return () => observer.disconnect();
  }, [geometry]);

  return (
    <div ref={containerRef} className="lcd-display-container">
      {geometry && <canvas ref={canvasRef} className="lcd-display-canvas" />}
    </div>
  );
}
