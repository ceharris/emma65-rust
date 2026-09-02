import { describe, expect, it } from "vitest";
import type { ITheme } from "@xterm/xterm";
import { terminalKeyActionBytes, themeWithTextOverrides, type TerminalTextPreferences } from "./terminalPreferences";

const BASE_THEME: ITheme = {
  foreground: "#base-fg",
  background: "#base-bg",
  black: "#base-black",
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
