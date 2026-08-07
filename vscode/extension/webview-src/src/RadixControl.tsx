import { useCallback, useState } from "react";

/** The five numeric bases a data value can be displayed in. */
export type DataRadix = "hex" | "udec" | "sdec" | "oct" | "bin";

/** The full 5-option cycle: hex, unsigned dec, signed dec, octal, binary. */
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

/** Owns a `DataRadix` value and a handler that cycles it through `cycle`. */
export function useRadixCycle(cycle: DataRadix[], initial: DataRadix): [DataRadix, () => void] {
  const [radix, setRadix] = useState<DataRadix>(initial);
  const doCycle = useCallback(() => {
    setRadix((r) => {
      const i = cycle.indexOf(r);
      return cycle[(i + 1) % cycle.length];
    });
  }, [cycle]);
  return [radix, doCycle];
}

/** Owns a `DataRadix` value and a handler that cycles it through `DATA_RADIX_CYCLE`. */
export function useDataRadix(initial: DataRadix = "hex"): [DataRadix, () => void] {
  return useRadixCycle(DATA_RADIX_CYCLE, initial);
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
