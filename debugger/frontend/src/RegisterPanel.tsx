import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import "./styles/registers.scss";

interface RegisterSnapshot {
  a: number;
  x: number;
  y: number;
  s: number;
  pc: number;
  p: number;
  changed_flags: number;
}

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

function flagDisplay(p: number, changed: number) {
  return FLAG_CHARS.map(({ label, bit }) => {
    const isSet = (p & bit) !== 0;
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
  });
}

interface Props {
  /** Incremented by the parent whenever a step completes. */
  refreshKey: number;
}

export default function RegisterPanel({ refreshKey }: Props) {
  const [snap, setSnap] = useState<RegisterSnapshot | null>(null);

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
  }, [refreshKey, fetchRegisters]);

  const hex2 = (v: number) => v.toString(16).toUpperCase().padStart(2, "0");
  const hex4 = (v: number) => v.toString(16).toUpperCase().padStart(4, "0");

  return (
    <div className="register-panel">
      <div className="panel-title">Registers</div>
      {snap === null ? (
        <span className="registers-empty">Waiting…</span>
      ) : (
        <table className="reg-table">
          <tbody>
            <tr>
              <td className="reg-name">A</td>
              <td className="reg-value">${hex2(snap.a)}</td>
            </tr>
            <tr>
              <td className="reg-name">X</td>
              <td className="reg-value">${hex2(snap.x)}</td>
            </tr>
            <tr>
              <td className="reg-name">Y</td>
              <td className="reg-value">${hex2(snap.y)}</td>
            </tr>
            <tr className="reg-separator" />
            <tr>
              <td className="reg-name">PC</td>
              <td className="reg-value">${hex4(snap.pc)}</td>
            </tr>
            <tr>
              <td className="reg-name">S</td>
              <td className="reg-value">${hex2(snap.s)}</td>
            </tr>
            <tr>
              <td className="reg-name">P</td>
              <td className="reg-value">${hex2(snap.p)}</td>
            </tr>
            <tr>
              <td className="reg-name" />
              <td className="reg-flags">
                {flagDisplay(snap.p, snap.changed_flags)}
              </td>
            </tr>
          </tbody>
        </table>
      )}
    </div>
  );
}
