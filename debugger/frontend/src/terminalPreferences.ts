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

/** Compatibility-tab terminal preferences — not yet editable (issue #467 Work Unit 4), round-tripped as-is. */
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
 * Ordered [field, label, xterm ITheme key] triples for the 16 ANSI palette
 * fields on `TerminalTextPreferences` — drives both the Preferences dialog's
 * palette swatch grid and `themeWithTextOverrides`'s field-by-field merge.
 */
export const ANSI_PALETTE_FIELDS: { field: keyof TerminalTextPreferences; label: string; themeKey: keyof ITheme }[] = [
  { field: "black", label: "Black", themeKey: "black" },
  { field: "red", label: "Red", themeKey: "red" },
  { field: "green", label: "Green", themeKey: "green" },
  { field: "yellow", label: "Yellow", themeKey: "yellow" },
  { field: "blue", label: "Blue", themeKey: "blue" },
  { field: "magenta", label: "Magenta", themeKey: "magenta" },
  { field: "cyan", label: "Cyan", themeKey: "cyan" },
  { field: "white", label: "White", themeKey: "white" },
  { field: "bright_black", label: "Bright Black", themeKey: "brightBlack" },
  { field: "bright_red", label: "Bright Red", themeKey: "brightRed" },
  { field: "bright_green", label: "Bright Green", themeKey: "brightGreen" },
  { field: "bright_yellow", label: "Bright Yellow", themeKey: "brightYellow" },
  { field: "bright_blue", label: "Bright Blue", themeKey: "brightBlue" },
  { field: "bright_magenta", label: "Bright Magenta", themeKey: "brightMagenta" },
  { field: "bright_cyan", label: "Bright Cyan", themeKey: "brightCyan" },
  { field: "bright_white", label: "Bright White", themeKey: "brightWhite" },
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
