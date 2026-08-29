import { useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";

/** `led_matrix::LedMatrixGeometryPayload`'s shape — fixed for the device's lifetime, so this is
 * fetched once on mount rather than tracked as changing state. `null` when no `display/matrix`
 * device is configured for the active profile. */
interface LedMatrixGeometry {
  matrices: number;
}

/**
 * `led_matrix::LedMatrixFramePayload`'s shape — one matrix's composited frame, pushed only when
 * that matrix actually swaps (design doc §10), never a whole-device vsync push. `pixels` arrives
 * base64-encoded for the same reason as `DisplayFramePayload::pixels` — see `DisplayPanel.tsx`'s
 * identical `decodeBase64` helper below.
 */
interface LedMatrixFramePayload {
  matrix_index: number;
  pixels: string;
}

/** Every matrix is a fixed 32x32 RGBA framebuffer (spec §2) — unlike `CharDisplay`'s
 * columns/rows, there's nothing device-specific to compute here. */
const MATRIX_SIZE = 32;

/** Floor the shared LED pitch (on-screen center-to-center spacing) never drops below, however
 * cramped the dock cell is. Unlike the old fixed-bitmap rendering this floor doesn't need to be an
 * integer — LEDs are drawn as vector circles, not upscaled pixels — it just keeps a matrix
 * legible (rather than a smear of overlapping dots) when the dock cell or detached window is very
 * small. `.led-matrix-container`'s `overflow: auto` is what lets the row scroll horizontally on a
 * many-matrix config too narrow to fit this floor for every matrix at once, rather than silently
 * clipping them. */
const MIN_PITCH_PX = 6;

/** LED radius as a fraction of pitch, modeling a real hobbyist RGB LED matrix panel (Adafruit
 * product #2026: 32x32, 2.5-3.0mm round LEDs on a 5mm pitch, ~160mm square board). Diameter is
 * 50-60% of pitch on that reference panel (2.5/5 to 3.0/5); 0.28 sits at the midpoint. Centering
 * each LED at `(i + 0.5) * pitch` (see `drawMatrix`) also reproduces that panel's ~2.5mm edge
 * margin (half a pitch) for free — a 32-unit-wide grid at that pitch is exactly the board's
 * 160mm width, so no separate margin constant is needed. */
const LED_RADIUS_RATIO = 0.28;

/** Near-black PCB substrate color drawn behind the LEDs. */
const PCB_BACKGROUND_COLOR = "#0a0a0a";

/** Color for an unlit LED (RGB 0,0,0). Deliberately distinct from `PCB_BACKGROUND_COLOR` — a
 * real unlit RGB LED reads as a dim dark dot against the board, not as invisible, and rendering it
 * as true black would make a dark frame indistinguishable from the board itself. */
const UNLIT_LED_COLOR = "rgb(30, 30, 34)";

/** Decodes a base64 string into raw bytes — see `DisplayPanel.tsx` for why this beats
 * `JSON.parse`-ing a plain number array per frame. */
function decodeBase64(base64: string): Uint8ClampedArray<ArrayBuffer> {
  const binary = atob(base64);
  const bytes = new Uint8ClampedArray(binary.length);
  for (let i = 0; i < binary.length; i++) {
    bytes[i] = binary.charCodeAt(i);
  }
  return bytes;
}

/** Draws one LED. Deliberately isolated from `drawMatrix`'s grid-walking loop so a later
 * enhancement (e.g. a radial-gradient glow for lit LEDs) only has to change this function. */
function drawLed(ctx: CanvasRenderingContext2D, cx: number, cy: number, radius: number, color: string) {
  ctx.beginPath();
  ctx.arc(cx, cy, radius, 0, Math.PI * 2);
  ctx.fillStyle = color;
  ctx.fill();
}

/** Renders one matrix's already-decoded RGBA pixel buffer into its canvas as a grid of round LEDs
 * on a PCB-colored background, at the given on-screen `pitch` (center-to-center LED spacing, in
 * CSS pixels). Called both when a fresh frame arrives and when `pitch` changes (a resize), so a
 * resize can redraw already-known pixel data immediately rather than waiting on the next swap. */
function drawMatrix(canvas: HTMLCanvasElement, pixels: Uint8ClampedArray, pitch: number) {
  const ctx = canvas.getContext("2d");
  if (!ctx) return;

  const dpr = window.devicePixelRatio || 1;
  const sizePx = MATRIX_SIZE * pitch;
  canvas.width = Math.round(sizePx * dpr);
  canvas.height = Math.round(sizePx * dpr);
  canvas.style.width = `${sizePx}px`;
  canvas.style.height = `${sizePx}px`;
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);

  ctx.fillStyle = PCB_BACKGROUND_COLOR;
  ctx.fillRect(0, 0, sizePx, sizePx);

  const radius = pitch * LED_RADIUS_RATIO;
  for (let row = 0; row < MATRIX_SIZE; row++) {
    for (let col = 0; col < MATRIX_SIZE; col++) {
      const offset = (row * MATRIX_SIZE + col) * 4;
      const r = pixels[offset];
      const g = pixels[offset + 1];
      const b = pixels[offset + 2];
      const color = r === 0 && g === 0 && b === 0 ? UNLIT_LED_COLOR : `rgb(${r}, ${g}, ${b})`;
      drawLed(ctx, (col + 0.5) * pitch, (row + 0.5) * pitch, radius, color);
    }
  }
}

/**
 * The dock panel hosting the memory-mapped LED matrix device's composited output (memory-mapped
 * LED matrix device plan, Work Unit 5 — design §11/§12). One independent `<canvas>` per matrix,
 * left to right by index, updated only when its own `matrix_index`'s `led-matrix-frame` event
 * arrives — design §10 pushes one frame per matrix actually swapped (whether by `CMD_SWAP` or
 * auto-refresh), not a whole-device push every vsync the way `DisplayPanel` gets. There's no
 * font/palette compositing here at all (that's already done server-side by
 * `led_matrix::compositing`) and no keyboard input, so this component is a pure per-matrix
 * render target — simpler than `DisplayPanel`, which also owns keyboard focus/forwarding.
 *
 * Mounted in both the docked panel and the detached-LED-Matrix window
 * (`led-matrix-detached.tsx`), reusing `DisplayPanel`/`display-detached.tsx`'s pattern with one
 * important asymmetry: the *docked* panel really is a fresh React mount each time (`DockLayout.tsx`
 * destroys and recreates that dock tab on detach/reattach), but the detached window is a
 * statically-declared Tauri window merely shown/hidden thereafter — this component mounts there
 * exactly once for the app's whole lifetime, not once per detach.
 *
 * That asymmetry matters because, unlike `DisplayPanel`'s per-vsync whole-grid push, a matrix's
 * frame here is only ever sent when it actually swaps (design §10) — so a mount with nothing yet
 * painted would otherwise show blank canvases until some unrelated later write touches each
 * matrix again. The fetch-on-mount effect below (`get_led_matrix_frames`,
 * `led_matrix.rs`'s `LedMatrixFrameCache`) covers every case that's a genuine fresh mount — initial
 * docked mount, and the docked panel reappearing after a reattach — but does nothing for the
 * detached window on its second or later detach, since that mount already happened long ago. The
 * backend instead replays the cache as ordinary `led-matrix-frame` events straight to the detached
 * window every time it becomes the target (`led_matrix.rs`'s `show_detached_led_matrix`), landing
 * on this component's already-registered live listener below rather than needing it to re-fetch
 * anything.
 *
 * Each matrix is rendered as a grid of round LEDs on a PCB-colored background (`drawMatrix`),
 * modeling a real hobbyist RGB LED matrix panel's proportions rather than the raw pixel buffer's
 * square cells — see `LED_RADIUS_RATIO`'s doc comment. Matrices are laid out with zero gap between
 * them, since real matrix boards mount edge-to-edge flush, giving the appearance of one
 * contiguous board rather than several separate ones. All canvases share one on-screen `pitch`
 * (LED center-to-center spacing, in CSS pixels): a `ResizeObserver` on the container recomputes
 * the largest pitch that fits every matrix side by side, never smaller than `MIN_PITCH_PX`. Unlike
 * the previous bitmap-blit rendering, pitch is continuous (not floored to an integer multiple)
 * since there's no pixel grid to keep aligned to whole multiples of — canvases resize smoothly
 * rather than snapping between steps.
 */
export default function LedMatrixPanel() {
  const containerRef = useRef<HTMLDivElement>(null);
  const canvasRefs = useRef<(HTMLCanvasElement | null)[]>([]);
  // Last-decoded pixel buffer per matrix, so a pitch change (resize) can redraw already-known
  // frames immediately rather than waiting for the next `led-matrix-frame` event.
  const pixelBuffersRef = useRef<(Uint8ClampedArray | null)[]>([]);
  // Mirrors `pitch` state for the frame-event listener below, which is registered once (empty
  // deps, so a frame arriving between mount and the geometry fetch resolving is never missed) and
  // so can't close over state that changes later.
  const pitchRef = useRef(MIN_PITCH_PX);
  const [geometry, setGeometry] = useState<LedMatrixGeometry | null>(null);
  const [pitch, setPitch] = useState(MIN_PITCH_PX);

  useEffect(() => {
    invoke<LedMatrixGeometry | null>("get_led_matrix_geometry")
      .then(setGeometry)
      .catch((err) => console.error("get_led_matrix_geometry failed:", err));
  }, []);

  useEffect(() => {
    pitchRef.current = pitch;
    for (let i = 0; i < pixelBuffersRef.current.length; i++) {
      const canvas = canvasRefs.current[i];
      const pixels = pixelBuffersRef.current[i];
      if (canvas && pixels) drawMatrix(canvas, pixels, pitch);
    }
  }, [pitch]);

  useEffect(() => {
    const container = containerRef.current;
    if (!container || !geometry) return;
    const recomputePitch = () => {
      // Real matrix boards mount edge-to-edge flush (no bezel gap), so the row's canvases are
      // laid out with zero spacing between them — the fit computation divides the container's
      // full width across all matrices rather than reserving room for gaps.
      const fitWidth = container.clientWidth / (geometry.matrices * MATRIX_SIZE);
      const fitHeight = container.clientHeight / MATRIX_SIZE;
      setPitch(Math.max(MIN_PITCH_PX, Math.min(fitWidth, fitHeight)));
    };
    recomputePitch();
    const observer = new ResizeObserver(recomputePitch);
    observer.observe(container);
    return () => observer.disconnect();
  }, [geometry]);

  // Replays each matrix's last delivered frame once the canvases exist to paint into — runs after
  // `geometry` settles (not on the initial, geometry-less render), since `canvasRefs.current` is
  // only populated for matrices the geometry-driven render below has actually created. React
  // attaches refs during commit, before effects run, so by the time this effect fires every
  // canvas for `geometry.matrices` is already in place.
  useEffect(() => {
    if (!geometry) return;
    invoke<LedMatrixFramePayload[]>("get_led_matrix_frames")
      .then((frames) => {
        for (const frame of frames) {
          const canvas = canvasRefs.current[frame.matrix_index];
          const pixels = decodeBase64(frame.pixels);
          pixelBuffersRef.current[frame.matrix_index] = pixels;
          if (canvas) drawMatrix(canvas, pixels, pitchRef.current);
        }
      })
      .catch((err) => console.error("get_led_matrix_frames failed:", err));
  }, [geometry]);

  // Registered once, before `geometry` is even known, the same as `DisplayPanel`'s frame listener
  // — so a frame that arrives in the gap between mount and the geometry fetch resolving is never
  // missed; `canvasRefs.current[matrix_index]` is simply `undefined` until that matrix's canvas
  // exists, in which case the frame is silently dropped rather than queued.
  useEffect(() => {
    const unlistenPromise = listen<LedMatrixFramePayload>("led-matrix-frame", (event) => {
      const canvas = canvasRefs.current[event.payload.matrix_index];
      const pixels = decodeBase64(event.payload.pixels);
      pixelBuffersRef.current[event.payload.matrix_index] = pixels;
      if (canvas) drawMatrix(canvas, pixels, pitchRef.current);
    });
    return () => {
      unlistenPromise.then((f) => f());
    };
  }, []);

  return (
    <div ref={containerRef} className="led-matrix-container">
      {geometry &&
        Array.from({ length: geometry.matrices }, (_, i) => (
          <canvas
            key={i}
            ref={(el) => {
              canvasRefs.current[i] = el;
            }}
            className="led-matrix-canvas"
          />
        ))}
    </div>
  );
}
