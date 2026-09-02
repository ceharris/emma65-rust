import { afterEach, describe, expect, it, vi } from "vitest";
import type { Terminal } from "@xterm/xterm";
import { TERMINAL_SIZE_PRESETS, isMonospaceFont, pixelSizeForGrid } from "./terminalSizing";

describe("TERMINAL_SIZE_PRESETS", () => {
  it("lists the four standard VT220 grid sizes in menu order", () => {
    expect(TERMINAL_SIZE_PRESETS).toEqual([
      { cols: 80, rows: 24 },
      { cols: 132, rows: 24 },
      { cols: 80, rows: 43 },
      { cols: 132, rows: 43 },
    ]);
  });
});

describe("isMonospaceFont", () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  function stubMeasureText(widthsByText: Record<string, number>) {
    vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue({
      font: "",
      measureText: (text: string) => ({ width: widthsByText[text] ?? 0 }),
    } as unknown as CanvasRenderingContext2D);
  }

  it("returns true when narrow and wide glyphs share the same advance width", () => {
    stubMeasureText({ i: 10, M: 10, W: 10 });
    expect(isMonospaceFont("Courier New")).toBe(true);
  });

  it("returns false when glyph advance widths differ", () => {
    stubMeasureText({ i: 4, M: 10, W: 12 });
    expect(isMonospaceFont("Arial")).toBe(false);
  });

  it("returns false when the canvas can't produce a 2d context", () => {
    vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
    expect(isMonospaceFont("Courier New")).toBe(false);
  });
});

/** Builds a fake `Terminal`-shaped object exposing just what `measureCell`/`pixelSizeForGrid` read. */
function makeFakeTerminal(options: {
  cell?: { width: number; height: number } | null;
  scrollback?: number;
  overviewRulerWidth?: number;
  padding?: { left: number; right: number; top: number; bottom: number };
  noElement?: boolean;
}): Terminal {
  const padding = options.padding ?? { left: 0, right: 0, top: 0, bottom: 0 };
  const element = document.createElement("div");
  element.style.paddingLeft = `${padding.left}px`;
  element.style.paddingRight = `${padding.right}px`;
  element.style.paddingTop = `${padding.top}px`;
  element.style.paddingBottom = `${padding.bottom}px`;

  return {
    element: options.noElement ? null : element,
    options: {
      scrollback: options.scrollback ?? 1000,
      overviewRuler: options.overviewRulerWidth != null ? { width: options.overviewRulerWidth } : undefined,
    },
    _core: {
      _renderService: {
        dimensions: {
          css: {
            cell: options.cell === undefined ? { width: 9, height: 17 } : options.cell,
          },
        },
      },
    },
  } as unknown as Terminal;
}

describe("pixelSizeForGrid", () => {
  it("computes width/height from cell size, gutter, and padding", () => {
    const term = makeFakeTerminal({
      cell: { width: 9, height: 17 },
      scrollback: 1000,
      overviewRulerWidth: 14,
      padding: { left: 2, right: 3, top: 1, bottom: 4 },
    });

    expect(pixelSizeForGrid(term, 80, 24)).toEqual({
      width: Math.ceil(80 * 9 + 14 + 2 + 3),
      height: Math.ceil(24 * 17 + 1 + 4),
    });
  });

  it("uses a zero gutter when scrollback is disabled", () => {
    const term = makeFakeTerminal({ cell: { width: 9, height: 17 }, scrollback: 0 });
    expect(pixelSizeForGrid(term, 80, 24)?.width).toBe(80 * 9);
  });

  it("falls back to a 14px gutter when overviewRuler width is unset", () => {
    const term = makeFakeTerminal({ cell: { width: 9, height: 17 }, scrollback: 1000 });
    expect(pixelSizeForGrid(term, 80, 24)?.width).toBe(80 * 9 + 14);
  });

  it("returns null when the terminal hasn't painted yet (zero cell size)", () => {
    const term = makeFakeTerminal({ cell: { width: 0, height: 0 } });
    expect(pixelSizeForGrid(term, 80, 24)).toBeNull();
  });

  it("returns null when the render service hasn't measured cell metrics at all", () => {
    const term = makeFakeTerminal({ cell: null });
    expect(pixelSizeForGrid(term, 80, 24)).toBeNull();
  });

  it("returns null when the terminal isn't attached to the DOM", () => {
    const term = makeFakeTerminal({ cell: { width: 9, height: 17 }, noElement: true });
    expect(pixelSizeForGrid(term, 80, 24)).toBeNull();
  });
});
