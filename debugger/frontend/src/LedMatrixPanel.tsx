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

/** Must match `.led-matrix-container`'s `gap` in `styles/global.scss` — the scale computation
 * below needs to know exactly how much horizontal space the row's gaps consume to fit the
 * canvases against the container's actual width. */
const MATRIX_GAP_PX = 8;

/** Floor the shared per-pixel scale never drops below, however cramped the dock cell is.
 * `DisplayPanel`'s equivalent fit-to-container computation floors at a bare 1x because its native
 * 320x200 canvas is already legible even unscaled; this panel's native 32x32 canvas is not — at
 * 1x a whole matrix is a 32px square, too small to read a written test pattern pixel by pixel
 * (the actual motivation for this panel during Work Unit 6's manual verification). 8 keeps every
 * emulated pixel a clearly separable on-screen square regardless of how little room the dock cell
 * or detached window happens to have; `.led-matrix-container`'s `overflow: auto` is what lets the
 * row scroll horizontally on a many-matrix config too narrow to fit this floor for every matrix
 * at once, rather than silently clipping them. */
const MIN_SCALE = 8;

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

/**
 * The dock panel hosting the memory-mapped LED matrix device's composited output (memory-mapped
 * LED matrix device plan, Work Unit 5 — design §11/§12). One independent `<canvas>` per matrix,
 * left to right by index, updated only when its own `matrix_index`'s `led-matrix-frame` event
 * arrives — design §10 pushes one frame per matrix actually swapped (whether by `CMD_SWAP` or
 * auto-refresh), not a whole-device push every vsync the way `DisplayPanel` gets. There's no
 * font/palette compositing here at all (that's already done server-side by
 * `led_matrix::compositing`) and no keyboard input, so this component is a pure multi-canvas blit
 * target — simpler than `DisplayPanel`, which also owns keyboard focus/forwarding.
 *
 * Mounted in both the docked panel and the detached-LED-Matrix window
 * (`led-matrix-detached.tsx`), same reuse shape as `DisplayPanel`/`display-detached.tsx`: no
 * device-specific state lives outside this component, so a detach/reattach cycle's unmount+remount
 * just re-fetches geometry and starts blitting fresh frames, with no state to carry across the
 * boundary.
 *
 * All canvases share one integer CSS scale — `DisplayPanel`'s single-canvas scale computation
 * generalized to a row: a `ResizeObserver` on the container recomputes the largest whole-number
 * multiple of `MATRIX_SIZE` that fits every matrix side by side (accounting for the row's gaps),
 * applied via each canvas's CSS `width`/`height` (never resampling the underlying pixel buffer) so
 * `image-rendering: pixelated` upscales with crisp, uniform pixel edges. Never smaller than
 * `MIN_SCALE`, even if that overflows the container — see `MIN_SCALE`'s own doc comment.
 */
export default function LedMatrixPanel() {
  const containerRef = useRef<HTMLDivElement>(null);
  const canvasRefs = useRef<(HTMLCanvasElement | null)[]>([]);
  const [geometry, setGeometry] = useState<LedMatrixGeometry | null>(null);
  const [scale, setScale] = useState(1);

  useEffect(() => {
    invoke<LedMatrixGeometry | null>("get_led_matrix_geometry")
      .then(setGeometry)
      .catch((err) => console.error("get_led_matrix_geometry failed:", err));
  }, []);

  useEffect(() => {
    const container = containerRef.current;
    if (!container || !geometry) return;
    const recomputeScale = () => {
      const totalGap = MATRIX_GAP_PX * Math.max(0, geometry.matrices - 1);
      const fitWidth = (container.clientWidth - totalGap) / (geometry.matrices * MATRIX_SIZE);
      const fitHeight = container.clientHeight / MATRIX_SIZE;
      setScale(Math.max(MIN_SCALE, Math.floor(Math.min(fitWidth, fitHeight))));
    };
    recomputeScale();
    const observer = new ResizeObserver(recomputeScale);
    observer.observe(container);
    return () => observer.disconnect();
  }, [geometry]);

  // Registered once, before `geometry` is even known, the same as `DisplayPanel`'s frame listener
  // — so a frame that arrives in the gap between mount and the geometry fetch resolving is never
  // missed; `canvasRefs.current[matrix_index]` is simply `undefined` until that matrix's canvas
  // exists, in which case the frame is silently dropped rather than queued.
  useEffect(() => {
    const unlistenPromise = listen<LedMatrixFramePayload>("led-matrix-frame", (event) => {
      const canvas = canvasRefs.current[event.payload.matrix_index];
      if (!canvas) return;
      const ctx = canvas.getContext("2d");
      if (!ctx) return;
      const imageData = new ImageData(decodeBase64(event.payload.pixels), MATRIX_SIZE, MATRIX_SIZE);
      ctx.putImageData(imageData, 0, 0);
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
            width={MATRIX_SIZE}
            height={MATRIX_SIZE}
            style={{ width: MATRIX_SIZE * scale, height: MATRIX_SIZE * scale }}
            className="led-matrix-canvas"
          />
        ))}
    </div>
  );
}
