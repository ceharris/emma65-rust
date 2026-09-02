import { describe, expect, it } from "vitest";
import { formatCycles, splitSpeed } from "./StatusBar";

describe("splitSpeed", () => {
  it("splits a value and unit separated by a space", () => {
    expect(splitSpeed("1.8432 MHz")).toEqual(["1.8432", "MHz"]);
  });

  it("splits only on the last space, keeping multi-word values intact", () => {
    expect(splitSpeed("1 2 MHz")).toEqual(["1 2", "MHz"]);
  });

  it("returns the whole string as the value with an empty unit when there's no space", () => {
    expect(splitSpeed("0")).toEqual(["0", ""]);
  });
});

describe("formatCycles", () => {
  it("formats small numbers without separators", () => {
    expect(formatCycles(42)).toBe("42");
  });

  it("inserts comma thousands separators for large numbers", () => {
    expect(formatCycles(1234567)).toBe("1,234,567");
  });

  it("formats zero", () => {
    expect(formatCycles(0)).toBe("0");
  });
});
