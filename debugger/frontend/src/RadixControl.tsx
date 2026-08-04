import { useCallback, useState } from "react";

/** The five numeric bases a data value can be displayed in. */
export type DataRadix = "hex" | "udec" | "sdec" | "oct" | "bin";

export const DATA_RADIX_CYCLE: DataRadix[] = ["hex", "udec", "sdec", "oct", "bin"];

const DATA_RADIX_LABEL: Record<DataRadix, string> = {
  hex:  "HEX",
  udec: "DEC",
  sdec: "±DEC",
  oct:  "OCT",
  bin:  "BIN",
};

/**
 * Formats `value` in `radix`.
 *
 * Pass `widthBits` for a fixed-width value (e.g. an 8-bit register): the
 * result is zero-padded to that width, and `sdec` sign-extends from its top
 * bit. Omit it for a variable-width value (e.g. a watch variable), which is
 * left unpadded and `sdec` is interpreted as a full 32-bit signed value.
 */
export function formatDataRadix(value: number, radix: DataRadix, widthBits?: number): string {
  const u = value >>> 0;
  switch (radix) {
    case "hex":  return u.toString(16).toUpperCase().padStart(widthBits ? Math.ceil(widthBits / 4) : 0, "0");
    case "udec": return u.toString(10);
    case "sdec": {
      const shift = 32 - (widthBits ?? 32);
      return ((u << shift) >> shift).toString(10);
    }
    case "oct":  return u.toString(8).padStart(widthBits ? Math.ceil(widthBits / 3) : 0, "0");
    case "bin":  return u.toString(2).padStart(widthBits ?? 0, "0");
  }
}

// --- editable-value parsing ---

/** Parses `rest` as an integer in `base` if it matches `charset` exactly, else null. */
function parseDigits(rest: string, charset: RegExp, base: number): number | null {
  return rest.length > 0 && charset.test(rest) ? parseInt(rest, base) : null;
}

const HEX_DIGITS = /^[0-9a-fA-F]+$/;
const OCT_DIGITS = /^[0-7]+$/;
const BIN_DIGITS = /^[01]+$/;
const DEC_DIGITS = /^-?[0-9]+$/;
const SIGNED_DEC = /^[+-][0-9]+$/;

/**
 * Parses an editable field's raw text into an integer.
 *
 * An explicit prefix always overrides `defaultRadix`: `$`/`0x` (hex),
 * `0o`/`0q` (octal), `0b` (binary), `0d`/`.` (decimal), or a bare leading
 * `+`/`-` (also decimal — no radix's unprefixed literal ever starts with a
 * sign, so this is unambiguous). With no prefix or sign, the text is parsed
 * in `defaultRadix`. Returns null if the text doesn't parse cleanly as an
 * integer.
 */
export function parseIntegerInput(raw: string, defaultRadix: DataRadix): number | null {
  const s = raw.trim();
  if (s === "") return null;

  if (s.startsWith("$")) return parseDigits(s.slice(1), HEX_DIGITS, 16);
  const lower = s.toLowerCase();
  if (lower.startsWith("0x")) return parseDigits(s.slice(2), HEX_DIGITS, 16);
  if (lower.startsWith("0o") || lower.startsWith("0q")) return parseDigits(s.slice(2), OCT_DIGITS, 8);
  if (lower.startsWith("0b")) return parseDigits(s.slice(2), BIN_DIGITS, 2);
  if (lower.startsWith("0d")) return parseDigits(s.slice(2), DEC_DIGITS, 10);
  if (s.startsWith(".")) return parseDigits(s.slice(1), DEC_DIGITS, 10);
  if (s.startsWith("-") || s.startsWith("+")) return parseDigits(s, SIGNED_DEC, 10);

  switch (defaultRadix) {
    case "hex":  return parseDigits(s, HEX_DIGITS, 16);
    case "udec": return parseDigits(s, DEC_DIGITS, 10);
    case "sdec": return parseDigits(s, DEC_DIGITS, 10);
    case "oct":  return parseDigits(s, OCT_DIGITS, 8);
    case "bin":  return parseDigits(s, BIN_DIGITS, 2);
  }
}

/**
 * Validates a parsed integer against a field's bit width and returns its
 * unsigned representation, or null if out of range.
 *
 * When `allowSigned` is set, accepts the union of the unsigned range
 * (0..2^width-1) and the signed two's-complement range (-2^(width-1)..-1),
 * so e.g. typing `-1` for an 8-bit field means 0xFF. Fields with no signed
 * display mode (e.g. PC) should pass `allowSigned: false` so only the
 * unsigned range is accepted.
 */
export function toUnsignedInRange(value: number, widthBits: number, allowSigned: boolean): number | null {
  if (!Number.isInteger(value)) return null;
  const max = 2 ** widthBits - 1;
  if (!allowSigned) {
    return value >= 0 && value <= max ? value : null;
  }
  const min = -(2 ** (widthBits - 1));
  if (value < min || value > max) return null;
  // Pure arithmetic two's complement (not `value & max`): for widthBits ==
  // 32, JS's bitwise operators coerce through signed Int32 and would flip
  // the sign of the top bit instead of masking it.
  return value >= 0 ? value : value + 2 ** widthBits;
}

/** Owns a `DataRadix` value and a handler that cycles it through `DATA_RADIX_CYCLE`. */
export function useDataRadix(initial: DataRadix = "hex"): [DataRadix, () => void] {
  const [radix, setRadix] = useState<DataRadix>(initial);
  const cycle = useCallback(() => {
    setRadix((r) => {
      const i = DATA_RADIX_CYCLE.indexOf(r);
      return DATA_RADIX_CYCLE[(i + 1) % DATA_RADIX_CYCLE.length];
    });
  }, []);
  return [radix, cycle];
}

interface RadixButtonProps {
  radix: DataRadix;
  onCycle: () => void;
  /** Set when the button lives inside another clickable element (e.g. a collapsible section header) whose own click handler must not also fire. */
  stopPropagation?: boolean;
}

/** A button showing `radix`'s label; clicking it invokes `onCycle`. */
export function RadixButton({ radix, onCycle, stopPropagation }: RadixButtonProps) {
  return (
    <button
      className="radix-btn"
      onClick={(e) => {
        if (stopPropagation) e.stopPropagation();
        onCycle();
      }}
      title="Cycle radix"
    >
      {DATA_RADIX_LABEL[radix]}
    </button>
  );
}
