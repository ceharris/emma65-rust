import { describe, expect, it } from "vitest";
import type { ITheme } from "@xterm/xterm";
import {
  ANSI_PALETTE_FIELDS,
  ANSI_PRESET_COLORS,
  terminalKeyActionBytes,
  themeWithCursorOverrides,
  themeWithTextOverrides,
  type TerminalCursorPreferences,
  type TerminalTextPreferences,
} from "./terminalPreferences";

const BASE_THEME: ITheme = {
  foreground: "#base-fg",
  background: "#base-bg",
  black: "#base-black",
  cursor: "#base-cursor",
  cursorAccent: "#base-cursor-accent",
};

const UNSET_CURSOR_PREFS: TerminalCursorPreferences = {
  active_shape: "block",
  inactive_shape: "outline",
  blink: true,
  color: null,
  accent_color: null,
};

const UNSET_TEXT_PREFS: TerminalTextPreferences = {
  font_family: null,
  font_size: null,
  scrollback: 1000,
  foreground: null,
  background: null,
  black: null,
  red: null,
  green: null,
  yellow: null,
  blue: null,
  magenta: null,
  cyan: null,
  white: null,
  bright_black: null,
  bright_red: null,
  bright_green: null,
  bright_yellow: null,
  bright_blue: null,
  bright_magenta: null,
  bright_cyan: null,
  bright_white: null,
};

describe("themeWithTextOverrides", () => {
  it("keeps the base theme's values when every override is unset", () => {
    expect(themeWithTextOverrides(BASE_THEME, UNSET_TEXT_PREFS)).toEqual(BASE_THEME);
  });

  it("overrides foreground, background, and palette fields that are set", () => {
    const prefs: TerminalTextPreferences = {
      ...UNSET_TEXT_PREFS,
      foreground: "#custom-fg",
      red: "#custom-red",
    };

    const theme = themeWithTextOverrides(BASE_THEME, prefs);

    expect(theme.foreground).toBe("#custom-fg");
    expect(theme.background).toBe(BASE_THEME.background);
    expect(theme.red).toBe("#custom-red");
    expect(theme.black).toBe(BASE_THEME.black);
  });
});

describe("terminalKeyActionBytes", () => {
  it("returns the ASCII Backspace control code for bs", () => {
    expect(terminalKeyActionBytes("bs")).toEqual([0x08]);
  });

  it("returns the ASCII Delete control code for del", () => {
    expect(terminalKeyActionBytes("del")).toEqual([0x7f]);
  });

  it("returns the ANSI DCH escape sequence for dch", () => {
    expect(terminalKeyActionBytes("dch")).toEqual([0x1b, 0x5b, 0x50]);
  });
});

describe("themeWithCursorOverrides", () => {
  it("keeps the base theme's cursor/cursorAccent when both overrides are unset", () => {
    expect(themeWithCursorOverrides(BASE_THEME, UNSET_CURSOR_PREFS)).toEqual(BASE_THEME);
  });

  it("overrides cursor and accent_color independently when set", () => {
    const theme = themeWithCursorOverrides(BASE_THEME, {
      ...UNSET_CURSOR_PREFS,
      color: "#custom-cursor",
    });

    expect(theme.cursor).toBe("#custom-cursor");
    expect(theme.cursorAccent).toBe(BASE_THEME.cursorAccent);
  });

  it("overrides both cursor and accent_color when both are set", () => {
    const theme = themeWithCursorOverrides(BASE_THEME, {
      ...UNSET_CURSOR_PREFS,
      color: "#custom-cursor",
      accent_color: "#custom-accent",
    });

    expect(theme.cursor).toBe("#custom-cursor");
    expect(theme.cursorAccent).toBe("#custom-accent");
  });
});

describe("ANSI_PALETTE_FIELDS", () => {
  it("has 16 entries, one per TerminalTextPreferences palette field", () => {
    expect(ANSI_PALETTE_FIELDS).toHaveLength(16);
  });

  it("gives every field a non-empty label and a valid #rrggbb defaultHex", () => {
    for (const entry of ANSI_PALETTE_FIELDS) {
      expect(entry.label.length).toBeGreaterThan(0);
      expect(entry.defaultHex).toMatch(/^#[0-9a-f]{6}$/);
    }
  });

  it("has no duplicate field or themeKey entries", () => {
    const fields = ANSI_PALETTE_FIELDS.map((e) => e.field);
    const themeKeys = ANSI_PALETTE_FIELDS.map((e) => e.themeKey);
    expect(new Set(fields).size).toBe(fields.length);
    expect(new Set(themeKeys).size).toBe(themeKeys.length);
  });
});

describe("ANSI_PRESET_COLORS", () => {
  it("has 16 entries matching ANSI_PALETTE_FIELDS' labels", () => {
    expect(ANSI_PRESET_COLORS).toHaveLength(16);
    expect(ANSI_PRESET_COLORS.map((c) => c.label)).toEqual(ANSI_PALETTE_FIELDS.map((f) => f.label));
  });

  it("gives every preset a valid #rrggbb hex value", () => {
    for (const entry of ANSI_PRESET_COLORS) {
      expect(entry.hex).toMatch(/^#[0-9a-f]{6}$/);
    }
  });
});
