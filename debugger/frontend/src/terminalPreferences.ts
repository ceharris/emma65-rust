/**
 * TypeScript mirror of `debugger/src-tauri/src/preferences.rs`'s
 * `TerminalPreferences` struct tree (issue #467), plus the shared bits its
 * Text tab needs: the 16-ANSI-palette field list (also used by the
 * color-swatch popover's palette grid) and the theme-merge helper
 * `TerminalPanel.tsx` uses to apply Text-tab overrides on top of its
 * existing light/dark base theme.
 */

import type { ITheme } from "@xterm/xterm";

/** Text-tab terminal preferences — font, 16-color ANSI palette, scrollback. */
export interface TerminalTextPreferences {
  font_family: string | null;
  font_size: number | null;
  scrollback: number;
  foreground: string | null;
  background: string | null;
  black: string | null;
  red: string | null;
  green: string | null;
  yellow: string | null;
  blue: string | null;
  magenta: string | null;
  cyan: string | null;
  white: string | null;
  bright_black: string | null;
  bright_red: string | null;
  bright_green: string | null;
  bright_yellow: string | null;
  bright_blue: string | null;
  bright_magenta: string | null;
  bright_cyan: string | null;
  bright_white: string | null;
}

export type CursorShape = "block" | "underline" | "bar";
export type CursorInactiveShape = "outline" | "block" | "underline" | "bar" | "none";

/** Cursor-tab terminal preferences — not yet editable (issue #467 Work Unit 3), round-tripped as-is. */
export interface TerminalCursorPreferences {
  active_shape: CursorShape;
  inactive_shape: CursorInactiveShape;
  blink: boolean;
  color: string | null;
  accent_color: string | null;
}

export type TerminalKeyAction = "bs" | "del" | "dch";

/** Compatibility-tab terminal preferences — what the Backspace and Delete keys send. */
export interface TerminalCompatibilityPreferences {
  backspace_key: TerminalKeyAction;
  delete_key: TerminalKeyAction;
}

export interface TerminalPreferences {
  text: TerminalTextPreferences;
  cursor: TerminalCursorPreferences;
  compatibility: TerminalCompatibilityPreferences;
}

/**
 * Ordered [field, label, xterm ITheme key, built-in default hex] entries for
 * the 16 ANSI palette fields on `TerminalTextPreferences` — drives the
 * Preferences dialog's palette swatch grid (`defaultHex` is what the
 * color-swatch popover previews when a slot is unset),
 * `themeWithTextOverrides`'s field-by-field merge, and the palette grid's
 * default color preview.
 *
 * `defaultHex` values are `@xterm/xterm`'s own built-in `DEFAULT_ANSI_COLORS`
 * (verified directly against the installed package's bundle) — what's
 * actually rendered for a slot left unset, independent of light/dark theme.
 */
export const ANSI_PALETTE_FIELDS: { field: keyof TerminalTextPreferences; label: string; themeKey: keyof ITheme; defaultHex: string }[] = [
  { field: "black", label: "Black", themeKey: "black", defaultHex: "#2e3436" },
  { field: "red", label: "Red", themeKey: "red", defaultHex: "#cc0000" },
  { field: "green", label: "Green", themeKey: "green", defaultHex: "#4e9a06" },
  { field: "yellow", label: "Yellow", themeKey: "yellow", defaultHex: "#c4a000" },
  { field: "blue", label: "Blue", themeKey: "blue", defaultHex: "#3465a4" },
  { field: "magenta", label: "Magenta", themeKey: "magenta", defaultHex: "#75507b" },
  { field: "cyan", label: "Cyan", themeKey: "cyan", defaultHex: "#06989a" },
  { field: "white", label: "White", themeKey: "white", defaultHex: "#d3d7cf" },
  { field: "bright_black", label: "Bright Black", themeKey: "brightBlack", defaultHex: "#555753" },
  { field: "bright_red", label: "Bright Red", themeKey: "brightRed", defaultHex: "#ef2929" },
  { field: "bright_green", label: "Bright Green", themeKey: "brightGreen", defaultHex: "#8ae234" },
  { field: "bright_yellow", label: "Bright Yellow", themeKey: "brightYellow", defaultHex: "#fce94f" },
  { field: "bright_blue", label: "Bright Blue", themeKey: "brightBlue", defaultHex: "#729fcf" },
  { field: "bright_magenta", label: "Bright Magenta", themeKey: "brightMagenta", defaultHex: "#ad7fa8" },
  { field: "bright_cyan", label: "Bright Cyan", themeKey: "brightCyan", defaultHex: "#34e2e2" },
  { field: "bright_white", label: "Bright White", themeKey: "brightWhite", defaultHex: "#eeeeec" },
];

/**
 * Standard ANSI reference colors (VS Code's default terminal palette), used
 * as one-click presets in the color-swatch popover — independent of
 * whatever the user has configured as their own palette, per the terminal-
 * preferences plan doc's "Color chooser" decision.
 */
export const ANSI_PRESET_COLORS: { label: string; hex: string }[] = [
  { label: "Black", hex: "#000000" },
  { label: "Red", hex: "#cd3131" },
  { label: "Green", hex: "#0dbc79" },
  { label: "Yellow", hex: "#e5e510" },
  { label: "Blue", hex: "#2472c8" },
  { label: "Magenta", hex: "#bc3fbc" },
  { label: "Cyan", hex: "#11a8cd" },
  { label: "White", hex: "#e5e5e5" },
  { label: "Bright Black", hex: "#666666" },
  { label: "Bright Red", hex: "#f14c4c" },
  { label: "Bright Green", hex: "#23d18b" },
  { label: "Bright Yellow", hex: "#f5f543" },
  { label: "Bright Blue", hex: "#3b8eea" },
  { label: "Bright Magenta", hex: "#d670d6" },
  { label: "Bright Cyan", hex: "#29b8db" },
  { label: "Bright White", hex: "#ffffff" },
];

/**
 * Merges `text`'s foreground/background/16-palette overrides onto `base`
 * (the current light/dark theme), matching `TerminalPanel.tsx`'s existing
 * `XTERM_DARK_THEME`/`XTERM_LIGHT_THEME` fallback pattern: an unset (`null`)
 * field keeps `base`'s value. Cursor/cursor-accent colors are left
 * untouched — those are issue #467 Work Unit 3's concern.
 */
export function themeWithTextOverrides(base: ITheme, text: TerminalTextPreferences): ITheme {
  const theme: ITheme = {
    ...base,
    foreground: text.foreground ?? base.foreground,
    background: text.background ?? base.background,
  };
  for (const { field, themeKey } of ANSI_PALETTE_FIELDS) {
    const value = text[field] as string | null;
    if (value != null) (theme as Record<string, string>)[themeKey] = value;
  }
  return theme;
}

/**
 * `@xterm/xterm`'s own built-in default for `ITheme.cursorAccent` (verified
 * directly against the installed package's bundle: an unset `cursorAccent`
 * falls back to a hardcoded opaque black, independent of `background` or any
 * other theme field) — what a Cursor-tab accent-color swatch actually
 * previews when left unset, since neither `XTERM_DARK_THEME` nor
 * `XTERM_LIGHT_THEME` sets `cursorAccent` itself. `cursor` gets the same
 * hardcoded-default treatment when unset, but both base themes already set
 * it explicitly, so `TerminalPanel.tsx` previews that value directly instead
 * of needing an equivalent `DEFAULT_CURSOR_COLOR` constant here.
 */
export const DEFAULT_CURSOR_ACCENT_COLOR = "#000000";

/**
 * Merges `cursor`'s color/accent-color overrides onto `theme` (already
 * merged with any Text-tab overrides via `themeWithTextOverrides`), matching
 * that function's unset-field-keeps-base-value pattern: `null` leaves
 * `theme`'s existing `cursor`/`cursorAccent` (the current light/dark base
 * theme's values) untouched.
 */
export function themeWithCursorOverrides(theme: ITheme, cursor: TerminalCursorPreferences): ITheme {
  return {
    ...theme,
    cursor: cursor.color ?? theme.cursor,
    cursorAccent: cursor.accent_color ?? theme.cursorAccent,
  };
}

/**
 * The bytes a compatibility-remapped Backspace/Delete key should send: the
 * ASCII Backspace control code (`\x08`), the ASCII Delete control code
 * (`\x7f`), or the literal ANSI Delete Character (DCH) escape sequence
 * `CSI P` (`\x1b[P`) — per the terminal-preferences plan doc's Compatibility
 * tab section, deliberately not the VT220 Delete keycode `\x1b[3~`.
 */
export function terminalKeyActionBytes(action: TerminalKeyAction): number[] {
  switch (action) {
    case "bs":
      return [0x08];
    case "del":
      return [0x7f];
    case "dch":
      return [0x1b, 0x5b, 0x50]; // ESC [ P
  }
}
