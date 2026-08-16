/**
 * Shared terminal font-resolution and sizing math, used by `TerminalPanel.tsx`
 * for both the docked and detached hosts. Currently holds the font
 * validation/fallback logic from issue #462's Work Unit 1; the cell-metrics
 * and pixel/grid conversion helpers from Work Unit 2 land here too, so both
 * the resize-on-container-change path and the size-preset menu share one
 * source of truth.
 */

import type { Terminal } from "@xterm/xterm";
import { LogicalSize } from "@tauri-apps/api/dpi";
import { getCurrentWindow } from "@tauri-apps/api/window";

/** A resolved, ready-to-use terminal font — never `null`/`undefined` fields. */
export interface ResolvedTerminalFont {
  fontFamily: string;
  fontSize: number;
}

/**
 * Font size used whenever `terminal_font_size` isn't in play — either because
 * the family fell back to the platform default, or because the family
 * validated but no size was configured. Matches the terminal's pre-#462
 * hardcoded size, so leaving these preferences unconfigured is a no-op for
 * existing users.
 */
const DEFAULT_FONT_SIZE = 14;

/**
 * Reports whether `fontFamily` renders as a genuinely monospace font, by
 * comparing the canvas advance width of a narrow glyph ("i") against wide
 * glyphs ("M", "W") — a font that merely claims to be monospace (or a
 * `font-family` list that falls through to a proportional font because the
 * named family isn't actually installed) will show unequal advance widths.
 * `probeSize` only affects measurement precision, not the result — any
 * reasonable size works since the ratio being compared is scale-invariant.
 */
export function isMonospaceFont(fontFamily: string, probeSize = 32): boolean {
  const canvas = document.createElement("canvas");
  const ctx = canvas.getContext("2d");
  if (!ctx) return false;
  ctx.font = `${probeSize}px ${fontFamily}`;
  const narrow = ctx.measureText("i").width;
  const wide1 = ctx.measureText("M").width;
  const wide2 = ctx.measureText("W").width;
  return narrow === wide1 && wide1 === wide2;
}

/**
 * Resolves the terminal's configured font family/size (from
 * `get_terminal_font`) against `fallbackFamily` (the platform default
 * monospace font, e.g. the `--font-mono` CSS var). Falls back the family to
 * `fallbackFamily` when `configuredFamily` is unset or fails the
 * `isMonospaceFont` probe; falls back the size to `DEFAULT_FONT_SIZE`
 * whenever `configuredSize` is unset, independent of whether the family
 * itself fell back.
 */
export function resolveTerminalFont(
  configuredFamily: string | null | undefined,
  configuredSize: number | null | undefined,
  fallbackFamily: string,
): ResolvedTerminalFont {
  const fontFamily =
    configuredFamily && isMonospaceFont(configuredFamily) ? configuredFamily : fallbackFamily;
  return { fontFamily, fontSize: configuredSize ?? DEFAULT_FONT_SIZE };
}

/** Rendered glyph size, in CSS px, plus the scrollbar gutter reserved alongside it. */
export interface CellMetrics {
  cellWidth: number;
  cellHeight: number;
  /** Width reserved for the overview-ruler/scrollbar gutter; 0 when scrollback is disabled. */
  gutter: number;
}

/**
 * Reads the terminal's actual rendered cell size, in CSS px, via the same
 * private render-service path `@xterm/addon-fit`'s own `proposeDimensions()`
 * uses internally (`term._core._renderService.dimensions.css.cell`) — xterm.js
 * doesn't expose rendered glyph metrics through any public API. Returns
 * `null` before the terminal has ever painted (cell size not yet measured) or
 * once `term.dispose()` has torn down its render service, mirroring
 * `proposeDimensions()`'s own zero-size guard.
 */
export function measureCell(term: Terminal): CellMetrics | null {
  const cell = (
    term as unknown as {
      _core?: { _renderService?: { dimensions?: { css?: { cell?: { width: number; height: number } } } } };
    }
  )._core?._renderService?.dimensions?.css?.cell;
  if (!cell || cell.width === 0 || cell.height === 0) return null;
  // Matches `addon-fit`'s own `proposeDimensions()` gutter logic exactly, so
  // a size this module proposes and the one `addon-fit` later measures back
  // agree on the same grid.
  const gutter = term.options.scrollback === 0 ? 0 : term.options.overviewRuler?.width || 14;
  return { cellWidth: cell.width, cellHeight: cell.height, gutter };
}

/** A container size, in CSS px. */
export interface PixelSize {
  width: number;
  height: number;
}

/** A fixed VT220-style terminal grid size. */
export interface TerminalSizePreset {
  cols: number;
  rows: number;
}

/** The four standard VT220 grid sizes named in issue #462, in the order the size-preset menu lists them. */
export const TERMINAL_SIZE_PRESETS: TerminalSizePreset[] = [
  { cols: 80, rows: 24 },
  { cols: 132, rows: 24 },
  { cols: 80, rows: 43 },
  { cols: 132, rows: 43 },
];

/**
 * Inverse of `FitAddon.proposeDimensions()`: given a target grid, returns the
 * CSS-px size of `term.element`'s parent (the same element
 * `proposeDimensions()` measures, and what the docked panel's `setSize()` or
 * the detached window's container should be resized to) needed to display
 * that grid without clipping the last row/column.
 *
 * Rounds the result up (`Math.ceil`) rather than computing the exact product,
 * so that any fractional-pixel loss when the container is later laid out at
 * an integer CSS size can never leave `proposeDimensions()` measuring one
 * fewer column or row than requested — the opposite direction of rounding
 * error is harmless (at most one extra sliver of padding), the clipped case
 * is not.
 *
 * Returns `null` if `term` isn't attached to the DOM yet or hasn't painted,
 * mirroring `measureCell`'s guard.
 */
export function pixelSizeForGrid(term: Terminal, cols: number, rows: number): PixelSize | null {
  const metrics = measureCell(term);
  if (!metrics || !term.element) return null;
  const style = window.getComputedStyle(term.element);
  const paddingX = parseInt(style.paddingLeft, 10) + parseInt(style.paddingRight, 10);
  const paddingY = parseInt(style.paddingTop, 10) + parseInt(style.paddingBottom, 10);
  return {
    width: Math.ceil(cols * metrics.cellWidth + metrics.gutter + paddingX),
    height: Math.ceil(rows * metrics.cellHeight + paddingY),
  };
}

/**
 * Converts a CSS-px container size (e.g. from `pixelSizeForGrid`) to a Tauri
 * `LogicalSize` for the detached-Terminal window's `setSize()`. Only needed
 * for the detached OS-window path — the docked path stays entirely in CSS/
 * logical px within the same webview (dockview's own `panel.api.setSize()`)
 * and needs no scale conversion.
 *
 * `scaleFactorOverride`, when given (the `--scale-factor` CLI flag, read via
 * `get_terminal_scale_factor_override`), is used in place of
 * `getCurrentWindow().scaleFactor()` — an escape hatch for hosts where scale
 * auto-detection misreports the effective display scale, the same class of
 * bug `doc/terminal-sizing-plan.md` (June 2026) hit under GNOME text scaling
 * before `main.rs` started forcing `GDK_BACKEND=x11`.
 */
export async function logicalSizeForCssPixels(
  cssWidth: number,
  cssHeight: number,
  scaleFactorOverride?: number | null,
): Promise<LogicalSize> {
  const scaleFactor = scaleFactorOverride ?? (await getCurrentWindow().scaleFactor());
  return new LogicalSize(cssWidth * scaleFactor, cssHeight * scaleFactor);
}
