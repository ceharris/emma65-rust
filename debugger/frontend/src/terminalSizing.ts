/**
 * Shared terminal font-resolution and sizing math, used by `TerminalPanel.tsx`
 * for both the docked and detached hosts. Currently holds the font
 * validation/fallback logic from issue #462's Work Unit 1; the cell-metrics
 * and pixel/grid conversion helpers from Work Unit 2 land here too, so both
 * the resize-on-container-change path and the size-preset menu share one
 * source of truth.
 */

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
