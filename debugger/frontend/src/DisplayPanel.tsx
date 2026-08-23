import { useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";

/** `display::DisplayGeometryPayload`'s shape — fixed for the device's lifetime, so this is fetched once on mount rather than tracked as changing state. */
interface DisplayGeometry {
  columns: number;
  rows: number;
  pixel_width: number;
  pixel_height: number;
  frame_rate_hz: number;
}

/**
 * `display::DisplayFramePayload`'s shape — `pixels` arrives as a plain JSON number array (no
 * `serde_bytes` on the Rust side), same as Terminal's byte-stream events. `columns`/`rows` here
 * are the *cell* grid dimensions the frame was composited from (`CharDisplay::columns()`), not
 * the RGBA buffer's pixel dimensions — those come only from `DisplayGeometry` (glyphs are a
 * fixed 8x8, so pixel size = cells * 8, but this component reads `pixel_width`/`pixel_height`
 * from the geometry fetched on mount rather than hardcoding that factor).
 */
interface DisplayFramePayload {
  pixels: number[];
  columns: number;
  rows: number;
}

/**
 * The dock panel hosting the memory-mapped display device's composited output (Work Unit 5 of
 * the memory-mapped display device plan — see `doc/memory-mapped-display-device-plan.md`
 * design §11). A dumb blit target only: compositing (char/color RAM + palette + font -> RGBA)
 * happens entirely in the Rust backend (`emulator::device::display::compositing`, `display.rs`'s
 * bridge task), so this component does nothing but size a `<canvas>` from `get_display_geometry`
 * and `putImageData` whatever arrives on `"display-frame"`.
 *
 * No `dockPanelApi` prop, unlike `TerminalPanel` — the canvas has a fixed intrinsic pixel size
 * driven entirely by device configuration (columns/rows are fixed for the device's lifetime),
 * not a user-resizable content area the way Terminal's size-preset menu resizes its host. A
 * resized dock cell just scales the canvas via CSS (`.display-container`'s `object-fit: contain`
 * equivalent, done here as `max-width`/`max-height` with preserved aspect ratio), never
 * resampling the actual pixel buffer.
 *
 * Mounted in both the docked panel and the detached-Display window
 * (`display-detached.tsx`), same reuse shape as `TerminalPanel`/`terminal-detached.tsx`: no
 * device-specific state lives outside this component, so a detach/reattach cycle's unmount+
 * remount just re-fetches geometry and starts blitting fresh frames, with no state to carry
 * across the boundary.
 *
 * The canvas's intrinsic pixel buffer (`width`/`height` attributes) always matches the device's
 * native resolution — an 8x8 font at 40x25 cells is only 320x200 device pixels, a couple of
 * millimeters per glyph at a typical monitor's dot pitch, so displaying it 1:1 in CSS pixels on
 * a high-DPI host is illegible. A `ResizeObserver` on the container recomputes the largest whole-
 * number multiple of that native size that fits, and that integer scale is applied via the
 * canvas's CSS `width`/`height` (not its pixel buffer) so `image-rendering: pixelated` upscales
 * with crisp, uniform pixel edges rather than resampling the actual RGBA data.
 */
export default function DisplayPanel() {
  const containerRef = useRef<HTMLDivElement>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const [geometry, setGeometry] = useState<DisplayGeometry | null>(null);
  // Read by the frame listener below without retriggering its effect on every geometry fetch —
  // geometry is fixed for the device's lifetime (design §8), so it only ever transitions once,
  // from null to a value, and the listener must already be registered to not miss a frame that
  // arrives in the gap between mount and that fetch resolving.
  const geometryRef = useRef<DisplayGeometry | null>(null);
  const [scale, setScale] = useState(1);

  useEffect(() => {
    invoke<DisplayGeometry | null>("get_display_geometry")
      .then((g) => {
        geometryRef.current = g;
        setGeometry(g);
      })
      .catch((err) => console.error("get_display_geometry failed:", err));
  }, []);

  useEffect(() => {
    const container = containerRef.current;
    if (!container || !geometry) return;
    const { pixel_width, pixel_height } = geometry;
    const recomputeScale = () => {
      const fit = Math.min(container.clientWidth / pixel_width, container.clientHeight / pixel_height);
      setScale(Math.max(1, Math.floor(fit)));
    };
    recomputeScale();
    const observer = new ResizeObserver(recomputeScale);
    observer.observe(container);
    return () => observer.disconnect();
  }, [geometry]);

  useEffect(() => {
    const unlistenPromise = listen<DisplayFramePayload>("display-frame", (event) => {
      const canvas = canvasRef.current;
      const geometry = geometryRef.current;
      if (!canvas || !geometry) return;
      const { pixel_width, pixel_height } = geometry;
      const ctx = canvas.getContext("2d");
      if (!ctx) return;
      const imageData = new ImageData(new Uint8ClampedArray(event.payload.pixels), pixel_width, pixel_height);
      ctx.putImageData(imageData, 0, 0);
    });
    return () => {
      unlistenPromise.then((f) => f());
    };
  }, []);

  return (
    <div ref={containerRef} className="display-container">
      {geometry && (
        <canvas
          ref={canvasRef}
          width={geometry.pixel_width}
          height={geometry.pixel_height}
          style={{ width: geometry.pixel_width * scale, height: geometry.pixel_height * scale }}
          className="display-canvas"
        />
      )}
    </div>
  );
}
