import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import "./styles/registers.scss";

export interface RegisterSnapshot {
  a: number;
  x: number;
  y: number;
  s: number;
  pc: number;
  /** Processor status byte. */
  p: number;
  /** Bitmask of P-register bits that changed on the most recent step (0 on initial load). */
  changed_flags: number;
}

// --- radix cycling ---

type DataRadix = "hex" | "udec" | "sdec" | "oct" | "bin" ;
type AddrRadix = "hex" | "udec" | "oct";

const DATA_RADIX_CYCLE: DataRadix[] = ["hex", "udec", "sdec", "oct", "bin"];
const ADDR_RADIX_CYCLE: AddrRadix[] = ["hex", "udec", "oct"];

const DATA_RADIX_LABEL: Record<DataRadix, string> = {
  hex:  "HEX",
  udec: "DEC",
  sdec: "±DEC",
  oct:  "OCT",
  bin:  "BIN",
};

const ADDR_RADIX_LABEL: Record<AddrRadix, string> = {
  hex:  "HEX",
  udec: "DEC",
  oct:  "OCT",
};

function formatData(value: number, radix: DataRadix): string {
  switch (radix) {
    case "hex":  return value.toString(16).toUpperCase().padStart(2, "0");
    case "udec": return value.toString(10);
    case "sdec": return ((value << 24) >> 24).toString(10);
    case "oct":  return value.toString(8).padStart(3, "0");
    case "bin":  return value.toString(2).padStart(8, "0");
  }
}

/** Returns the character in single quotes if `value` is a printable ASCII byte (0x20–0x7E), otherwise null. */
function printableAscii(value: number): string | null {
  return value >= 0x20 && value <= 0x7e ? `'${String.fromCharCode(value)}'` : null;
}

function formatAddr(value: number, radix: AddrRadix, byteWidth: number): string {
  switch (radix) {
    case "hex":  return value.toString(16).toUpperCase().padStart(byteWidth * 2, "0");
    case "udec": return value.toString(10);
    case "oct":  return value.toString(8).padStart(byteWidth === 2 ? 6 : 3, "0");
  }
}

// --- flag display ---

/** Flag bit positions in the P register, ordered N V - B D I Z C (bit 7 → bit 0). */
const FLAG_CHARS = [
  { label: "N", bit: 0x80 },
  { label: "V", bit: 0x40 },
  { label: "-", bit: 0x20 },
  { label: "B", bit: 0x10 },
  { label: "D", bit: 0x08 },
  { label: "I", bit: 0x04 },
  { label: "Z", bit: 0x02 },
  { label: "C", bit: 0x01 },
];

function FlagDisplay({ p, changed }: { p: number; changed: number }) {
  return (
    <>
      {FLAG_CHARS.map(({ label, bit }) => {
        const isSet = (p & bit) !== 0;
        // The UNUSED bit (0x20, displayed as "-") never changes meaningfully.
        const didChange = label !== "-" && (changed & bit) !== 0;
        return (
          <span
            key={label}
            className={[
              "flag-char",
              isSet ? "flag-set" : "flag-clear",
              didChange ? "flag-changed" : "",
            ]
              .filter(Boolean)
              .join(" ")}
          >
            {isSet ? label : "-"}
          </span>
        );
      })}
    </>
  );
}

// --- component ---

interface Props {
  /**
   * When provided (after a step_into call), the panel renders this snapshot
   * immediately without a round-trip to get_registers.
   */
  snapshot: RegisterSnapshot | null;
}

export default function RegisterPanel({ snapshot: snapFromParent }: Props) {
  const [snap, setSnap] = useState<RegisterSnapshot | null>(null);
  const [dataRadix, setDataRadix] = useState<DataRadix>("hex");
  const [addrRadix, setAddrRadix] = useState<AddrRadix>("hex");

  useEffect(() => {
    if (snapFromParent !== null) {
      setSnap(snapFromParent);
    }
  }, [snapFromParent]);

  const fetchRegisters = useCallback(async () => {
    try {
      const result = await invoke<RegisterSnapshot>("get_registers");
      setSnap(result);
    } catch (e) {
      console.error("get_registers failed:", e);
    }
  }, []);

  useEffect(() => {
    fetchRegisters();
  }, [fetchRegisters]);

  const cycleDataRadix = useCallback(() => {
    setDataRadix((r) => {
      const i = DATA_RADIX_CYCLE.indexOf(r);
      return DATA_RADIX_CYCLE[(i + 1) % DATA_RADIX_CYCLE.length];
    });
  }, []);

  const cycleAddrRadix = useCallback(() => {
    setAddrRadix((r) => {
      const i = ADDR_RADIX_CYCLE.indexOf(r);
      return ADDR_RADIX_CYCLE[(i + 1) % ADDR_RADIX_CYCLE.length];
    });
  }, []);

  return (
    <div className="register-panel">
      <div className="panel-title">Registers</div>
      {snap === null ? (
        <span className="registers-empty">Waiting…</span>
      ) : (
        <table className="reg-table">
          <tbody>
            <tr className="reg-group-header">
              <td />
              <td>
                <button className="radix-btn" onClick={cycleDataRadix} title="Cycle radix">
                  {DATA_RADIX_LABEL[dataRadix]}
                </button>
              </td>
            </tr>
            <tr>
              <td className="reg-name">A</td>
              <td className="reg-value">
                {formatData(snap.a, dataRadix)}
                {printableAscii(snap.a) !== null && (
                  <span className="reg-ascii">{printableAscii(snap.a)}</span>
                )}
              </td>
            </tr>
            <tr>
              <td className="reg-name">X</td>
              <td className="reg-value">{formatData(snap.x, dataRadix)}</td>
            </tr>
            <tr>
              <td className="reg-name">Y</td>
              <td className="reg-value">{formatData(snap.y, dataRadix)}</td>
            </tr>
            <tr className="reg-separator" />
            <tr className="reg-group-header">
              <td />
              <td>
                <button className="radix-btn" onClick={cycleAddrRadix} title="Cycle radix">
                  {ADDR_RADIX_LABEL[addrRadix]}
                </button>
              </td>
            </tr>
            <tr>
              <td className="reg-name">PC</td>
              <td className="reg-value">{formatAddr(snap.pc, addrRadix, 2)}</td>
            </tr>
            <tr>
              <td className="reg-name">S</td>
              <td className="reg-value">{formatAddr(snap.s, addrRadix, 1)}</td>
            </tr>
            <tr>
              <td className="reg-name">P</td>
              <td className="reg-value">{formatAddr(snap.p, addrRadix, 1)}</td>
            </tr>
            <tr>
              <td className="reg-name" />
              <td className="reg-flags">
                <FlagDisplay p={snap.p} changed={snap.changed_flags} />
              </td>
            </tr>
          </tbody>
        </table>
      )}
    </div>
  );
}
