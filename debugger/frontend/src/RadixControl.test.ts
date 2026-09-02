import { act, renderHook } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import {
  ADDR_RADIX_CYCLE,
  DATA_RADIX_CYCLE,
  STACK_RADIX_CYCLE,
  formatDataRadix,
  useDataRadix,
  useRadixCycle,
} from "./RadixControl";

describe("formatDataRadix", () => {
  it("formats hex, zero-padded to widthBits when given", () => {
    expect(formatDataRadix(0x0a, "hex", 8)).toBe("0A");
    expect(formatDataRadix(0xff, "hex")).toBe("FF");
  });

  it("formats udec as an unsigned decimal string", () => {
    expect(formatDataRadix(255, "udec")).toBe("255");
    expect(formatDataRadix(0, "udec")).toBe("0");
  });

  it("formats sdec sign-extended from widthBits' top bit when given", () => {
    expect(formatDataRadix(0xff, "sdec", 8)).toBe("-1");
    expect(formatDataRadix(0x7f, "sdec", 8)).toBe("127");
  });

  it("formats sdec as a full 32-bit signed value when widthBits is omitted", () => {
    expect(formatDataRadix(0xffffffff, "sdec")).toBe("-1");
    expect(formatDataRadix(1, "sdec")).toBe("1");
  });

  it("formats oct, zero-padded to widthBits when given", () => {
    expect(formatDataRadix(8, "oct", 8)).toBe("010");
    expect(formatDataRadix(8, "oct")).toBe("10");
  });

  it("formats bin, zero-padded to widthBits when given", () => {
    expect(formatDataRadix(5, "bin", 8)).toBe("00000101");
    expect(formatDataRadix(5, "bin")).toBe("101");
  });
});

describe("useRadixCycle", () => {
  it("cycles through the given radix list and wraps back to the start", () => {
    const { result } = renderHook(() => useRadixCycle(ADDR_RADIX_CYCLE, "hex"));

    expect(result.current[0]).toBe("hex");
    act(() => result.current[1]());
    expect(result.current[0]).toBe("udec");
    act(() => result.current[1]());
    expect(result.current[0]).toBe("oct");
    act(() => result.current[1]());
    expect(result.current[0]).toBe("hex");
  });

  it("starts from the given initial radix, not always the first entry", () => {
    const { result } = renderHook(() => useRadixCycle(STACK_RADIX_CYCLE, "sdec"));
    expect(result.current[0]).toBe("sdec");
  });
});

describe("useDataRadix", () => {
  it("defaults to hex and cycles through the full 5-option DATA_RADIX_CYCLE", () => {
    const { result } = renderHook(() => useDataRadix());

    expect(result.current[0]).toBe("hex");
    for (const expected of DATA_RADIX_CYCLE.slice(1)) {
      act(() => result.current[1]());
      expect(result.current[0]).toBe(expected);
    }
    act(() => result.current[1]());
    expect(result.current[0]).toBe(DATA_RADIX_CYCLE[0]);
  });

  it("accepts an explicit initial radix", () => {
    const { result } = renderHook(() => useDataRadix("oct"));
    expect(result.current[0]).toBe("oct");
  });
});
