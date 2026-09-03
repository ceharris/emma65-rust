import { useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";

/**
 * `lcd_display::LcdDisplayGeometryPayload`'s shape -- the character grid an `display/lcd` device
 * was configured with. Used only to gate whether this panel has anything to show at all; unlike
 * `DisplayGeometry`/`LedMatrixGeometry` it carries no pixel dimensions, since those depend on the
 * configured font (5x8 vs 5x10, spec §8.2), which this payload doesn't report -- the canvas is
 * instead sized directly from a frame's own pixel buffer (see `paintFrame`).
 */
interface LcdDisplayGeometry {
  columns: number;
  rows: number;
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

/** Fixed glyph cell width in pixels (spec §8.2), regardless of font. */
const CELL_WIDTH_PX = 5;

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

/** A frame's pixel dimensions, in device pixels. */
interface LcdFrameDims {
  widthPx: number;
  heightPx: number;
}

/**
 * Sizes `canvas` to `payload`'s implied pixel dimensions and blits it. Cell height (8 vs 10 rows,
 * depending on the configured font) isn't reported by any payload the frontend sees, so it's
 * derived here from the decoded buffer's own length instead of a hardcoded constant --
 * `heightPx = pixels.length / 4 / widthPx`, exact since `compositing::composite` always produces
 * a `width_px * height_px * 4`-byte buffer with no padding.
 */
function paintFrame(canvas: HTMLCanvasElement, payload: LcdDisplayFramePayload): LcdFrameDims {
  const pixels = decodeBase64(payload.pixels);
  const widthPx = payload.columns * CELL_WIDTH_PX;
  const heightPx = widthPx > 0 ? pixels.length / 4 / widthPx : 0;
  if (canvas.width !== widthPx || canvas.height !== heightPx) {
    canvas.width = widthPx;
    canvas.height = heightPx;
  }
  if (widthPx > 0 && heightPx > 0) {
    const ctx = canvas.getContext("2d");
    ctx?.putImageData(new ImageData(pixels, widthPx, heightPx), 0, 0);
  }
  return { widthPx, heightPx };
}

/**
 * The dock panel hosting the memory-mapped LCD display device's composited output (memory-mapped
 * LCD display device plan, Work Unit 5 -- design §10). A dumb blit target only, same contract as
 * `DisplayPanel`/`LedMatrixPanel`: compositing (DDRAM/CGRAM + CGROM + palette -> RGBA) happens
 * entirely in the Rust backend (`emulator::device::lcd_display::compositing`, `lcd_display.rs`'s
 * bridge task). Unlike `DisplayPanel`, there's no keyboard input to forward -- an LCD display has
 * no input capability, like `LedMatrixPanel`.
 *
 * Mounted in both the docked panel and the detached-LCD-Display window
 * (`lcd-display-detached.tsx`), same reuse shape as `DisplayPanel`/`display-detached.tsx`: no
 * device-specific state lives outside this component, so a detach/reattach cycle's unmount+remount
 * just re-fetches geometry/the cached frame and starts blitting fresh frames.
 *
 * A frame is only ever pushed on a render-affecting register write (design doc §7), never on a
 * periodic vsync, so a freshly-mounted panel could otherwise sit blank until some unrelated later
 * write happens. `get_lcd_display_frame` (backed by `lcd_display::LcdDisplayFrameCache`) covers
 * that by fetching the last delivered frame on mount and painting it immediately, mirroring
 * `LedMatrixPanel`'s `get_led_matrix_frames` fetch -- until either that fetch or the first live
 * `"lcd-display-frame"` event lands, nothing has been composited yet this session and the panel
 * shows an empty canvas, the same as an un-initialized real HD44780 module.
 *
 * The canvas's intrinsic pixel size comes from a frame's own payload rather than from
 * `get_lcd_display_geometry` (see `LcdDisplayGeometry`'s doc comment for why); geometry is fetched
 * only to know whether a `display/lcd` device is configured at all, gating whether this panel
 * renders a canvas in the first place. A `ResizeObserver` on the container recomputes the largest
 * whole-number on-screen multiple of the frame's native pixel size that fits, the same crisp-
 * upscale approach as `DisplayPanel`.
 */
export default function LcdDisplayPanel() {
  const containerRef = useRef<HTMLDivElement>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const [geometry, setGeometry] = useState<LcdDisplayGeometry | null>(null);
  const [dims, setDims] = useState<LcdFrameDims | null>(null);
  const [scale, setScale] = useState(1);

  useEffect(() => {
    invoke<LcdDisplayGeometry | null>("get_lcd_display_geometry")
      .then(setGeometry)
      .catch((err) => console.error("get_lcd_display_geometry failed:", err));
  }, []);

  // Paints the last delivered frame, if any, once the canvas exists to paint into -- runs after
  // `geometry` settles (not on the initial, geometry-less render), since the canvas below is only
  // rendered once `geometry` is known. Mirrors `LedMatrixPanel`'s analogous cache-replay effect.
  useEffect(() => {
    if (!geometry) return;
    invoke<LcdDisplayFramePayload | null>("get_lcd_display_frame")
      .then((frame) => {
        const canvas = canvasRef.current;
        if (!frame || !canvas) return;
        setDims(paintFrame(canvas, frame));
      })
      .catch((err) => console.error("get_lcd_display_frame failed:", err));
  }, [geometry]);

  // Registered once, before `geometry` is even known, the same as `DisplayPanel`'s frame listener
  // -- so a frame that arrives in the gap between mount and the geometry fetch resolving is never
  // missed; the canvas ref is simply null until `geometry` resolves and renders it, in which case
  // the frame is silently dropped rather than queued.
  useEffect(() => {
    const unlistenPromise = listen<LcdDisplayFramePayload>("lcd-display-frame", (event) => {
      const canvas = canvasRef.current;
      if (!canvas) return;
      setDims(paintFrame(canvas, event.payload));
    });
    return () => {
      unlistenPromise.then((f) => f());
    };
  }, []);

  useEffect(() => {
    const container = containerRef.current;
    if (!container || !dims || dims.widthPx === 0 || dims.heightPx === 0) return;
    const recomputeScale = () => {
      const fit = Math.min(container.clientWidth / dims.widthPx, container.clientHeight / dims.heightPx);
      setScale(Math.max(1, Math.floor(fit)));
    };
    recomputeScale();
    const observer = new ResizeObserver(recomputeScale);
    observer.observe(container);
    return () => observer.disconnect();
  }, [dims]);

  return (
    <div ref={containerRef} className="lcd-display-container">
      {geometry && (
        <canvas
          ref={canvasRef}
          style={dims ? { width: dims.widthPx * scale, height: dims.heightPx * scale } : undefined}
          className="lcd-display-canvas"
        />
      )}
    </div>
  );
}
