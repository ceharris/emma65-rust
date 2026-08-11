import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useExecutionContext } from "./ExecutionContext";
import { ADDR_RADIX_CYCLE, DataRadix, formatDataRadix, RadixButton, useDataRadix, useRadixCycle } from "./RadixControl";
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
  /** True when the CPU executed STP and is now halted. */
  cpu_stopped: boolean;
  /** True when the CPU executed WAI and is now halted awaiting an interrupt. */
  cpu_waiting: boolean;
  /** True when this snapshot resulted from hitting a breakpoint. */
  breakpoint_hit: boolean;
}

/** Returns the character in single quotes if `value` is a printable ASCII byte (0x20–0x7E), otherwise null. */
function printableAscii(value: number): string | null {
  return value >= 0x20 && value <= 0x7e ? `'${String.fromCharCode(value)}'` : null;
}

// --- register editing ---

/** Register field names as sent to the `set_register` Tauri command. */
type RegisterField = "a" | "x" | "y" | "s" | "pc" | "p";

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
 * Parses a register edit field's raw text into an integer.
 *
 * An explicit prefix always overrides the register's current display radix:
 * `$`/`0x` (hex), `0o`/`0q` (octal), `0b` (binary), `0d`/`.` (decimal), or a
 * bare leading `+`/`-` (also decimal — no radix's unprefixed literal ever
 * starts with a sign, so this is unambiguous). With no prefix or sign, the
 * text is parsed in `defaultRadix` (the register's current display radix).
 * Returns null if the text doesn't parse cleanly as an integer.
 */
function parseRegisterInput(raw: string, defaultRadix: DataRadix): number | null {
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
 * Validates a parsed integer against a register's bit width and returns its
 * unsigned representation, or null if out of range.
 *
 * Byte/word fields (`allowSigned`) accept the union of the unsigned range
 * (0..2^width-1) and the signed two's-complement range (-2^(width-1)..-1),
 * so e.g. typing `-1` for an 8-bit register means 0xFF. PC has no signed
 * display mode, so only the unsigned range is accepted for it.
 */
function toUnsignedInRange(value: number, widthBits: number, allowSigned: boolean): number | null {
  if (!Number.isInteger(value)) return null;
  const max = (1 << widthBits) - 1;
  if (!allowSigned) {
    return value >= 0 && value <= max ? value : null;
  }
  const min = -(1 << (widthBits - 1));
  return value >= min && value <= max ? value & max : null;
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

export default function RegisterPanel() {
  const { lastSnapshot: snapFromParent, execState, onEdit } = useExecutionContext();
  const [snap, setSnap] = useState<RegisterSnapshot | null>(null);
  const [dataRadix, cycleDataRadix] = useDataRadix("hex");
  const [addrRadix, cycleAddrRadix] = useRadixCycle(ADDR_RADIX_CYCLE, "hex");
  const [editingTarget, setEditingTarget] = useState<RegisterField | "flags" | null>(null);
  const [editValue, setEditValue] = useState("");
  const [editInvalid, setEditInvalid] = useState(false);
  const [editFlags, setEditFlags] = useState(0);

  const isEditable = execState === "stopped";

  useEffect(() => {
    if (snapFromParent !== null) {
      setSnap(snapFromParent);
      // A fresh snapshot (e.g. from Reset) invalidates any in-progress edit.
      setEditingTarget(null);
      setEditInvalid(false);
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

  const beginEdit = useCallback((field: RegisterField, currentText: string) => {
    if (!isEditable) return;
    setEditingTarget(field);
    setEditValue(currentText);
    setEditInvalid(false);
  }, [isEditable]);

  const cancelEdit = useCallback(() => {
    setEditingTarget(null);
    setEditInvalid(false);
  }, []);

  const commitEdit = useCallback(async (
    field: RegisterField,
    radix: DataRadix,
    widthBits: number,
    allowSigned: boolean,
  ) => {
    const parsed = parseRegisterInput(editValue, radix);
    const value = parsed === null ? null : toUnsignedInRange(parsed, widthBits, allowSigned);
    if (value === null) {
      setEditInvalid(true);
      return;
    }
    try {
      const result = await invoke<RegisterSnapshot>("set_register", { field, value });
      onEdit(result);
      setEditingTarget(null);
      setEditInvalid(false);
    } catch (e) {
      console.error("set_register failed:", e);
      setEditInvalid(true);
    }
  }, [editValue, onEdit]);

  const beginFlagsEdit = useCallback((currentP: number) => {
    if (!isEditable) return;
    setEditingTarget("flags");
    setEditFlags(currentP);
  }, [isEditable]);

  const cancelFlagsEdit = useCallback(() => {
    setEditingTarget(null);
  }, []);

  const toggleFlagBit = useCallback((bit: number) => {
    setEditFlags((f) => f ^ bit);
  }, []);

  const commitFlagsEdit = useCallback(async () => {
    try {
      const result = await invoke<RegisterSnapshot>("set_register", { field: "p", value: editFlags });
      onEdit(result);
      setEditingTarget(null);
    } catch (e) {
      console.error("set_register failed:", e);
      setEditingTarget(null);
    }
  }, [editFlags, onEdit]);

  /** Renders a register's value cell: an inline edit input when `field` is
   *  being edited, otherwise the formatted display text (double-clickable
   *  to start editing when the CPU is stopped). */
  const renderRegisterValue = (
    field: RegisterField,
    displayText: string,
    radix: DataRadix,
    widthBits: number,
    allowSigned: boolean,
  ) => {
    if (editingTarget === field) {
      return (
        <input
          className={`reg-edit-input${editInvalid ? " invalid" : ""}`}
          autoFocus
          value={editValue}
          onChange={(e) => setEditValue(e.target.value)}
          onKeyDown={(e) => {
            e.stopPropagation();
            if (e.key === "Enter") {
              e.preventDefault();
              commitEdit(field, radix, widthBits, allowSigned);
            } else if (e.key === "Escape") {
              e.preventDefault();
              cancelEdit();
            }
          }}
          onBlur={cancelEdit}
        />
      );
    }
    return (
      <span
        className={isEditable ? "reg-editable" : ""}
        onDoubleClick={() => beginEdit(field, displayText)}
        title={isEditable ? "Double-click to edit" : undefined}
      >
        {displayText}
      </span>
    );
  };

  /** Renders the P-register flag characters: individually toggleable (click
   *  a flag to flip it, except the unused "-" position) while editing,
   *  otherwise the plain display (double-clickable to start editing). */
  const renderFlags = (p: number, changed: number) => {
    if (editingTarget === "flags") {
      return (
        <div
          className="reg-flags-edit"
          tabIndex={0}
          autoFocus
          onKeyDown={(e) => {
            e.stopPropagation();
            if (e.key === "Enter") {
              e.preventDefault();
              commitFlagsEdit();
            } else if (e.key === "Escape") {
              e.preventDefault();
              cancelFlagsEdit();
            }
          }}
          onBlur={cancelFlagsEdit}
        >
          {FLAG_CHARS.map(({ label, bit }) => {
            const isSet = (editFlags & bit) !== 0;
            const toggleable = label !== "-";
            return (
              <span
                key={label}
                className={[
                  "flag-char",
                  isSet ? "flag-set" : "flag-clear",
                  toggleable ? "flag-toggleable" : "",
                ]
                  .filter(Boolean)
                  .join(" ")}
                onClick={toggleable ? () => toggleFlagBit(bit) : undefined}
              >
                {isSet ? label : "-"}
              </span>
            );
          })}
        </div>
      );
    }
    return (
      <span
        className={isEditable ? "reg-editable" : ""}
        onDoubleClick={() => beginFlagsEdit(p)}
        title={isEditable ? "Double-click to edit flags" : undefined}
      >
        <FlagDisplay p={p} changed={changed} />
      </span>
    );
  };

  return (
    <div className="register-panel">
      {snap === null ? (
        <span className="registers-empty">Waiting…</span>
      ) : (
        <table className="reg-table">
          <tbody>
            <tr className="reg-group-header">
              <td />
              <td>
                <RadixButton radix={dataRadix} onCycle={cycleDataRadix} />
              </td>
            </tr>
            <tr>
              <td className="reg-name">A</td>
              <td className="reg-value">
                {renderRegisterValue("a", formatDataRadix(snap.a, dataRadix, 8), dataRadix, 8, true)}
                {editingTarget !== "a" && printableAscii(snap.a) !== null && (
                  <span className="reg-ascii">{printableAscii(snap.a)}</span>
                )}
              </td>
            </tr>
            <tr>
              <td className="reg-name">X</td>
              <td className="reg-value">{renderRegisterValue("x", formatDataRadix(snap.x, dataRadix, 8), dataRadix, 8, true)}</td>
            </tr>
            <tr>
              <td className="reg-name">Y</td>
              <td className="reg-value">{renderRegisterValue("y", formatDataRadix(snap.y, dataRadix, 8), dataRadix, 8, true)}</td>
            </tr>
            <tr className="reg-separator" />
            <tr className="reg-group-header">
              <td />
              <td>
                <RadixButton radix={addrRadix} onCycle={cycleAddrRadix} />
              </td>
            </tr>
            <tr>
              <td className="reg-name">PC</td>
              <td className="reg-value">{renderRegisterValue("pc", formatDataRadix(snap.pc, addrRadix, 16), addrRadix, 16, false)}</td>
            </tr>
            <tr>
              <td className="reg-name">S</td>
              <td className="reg-value">{renderRegisterValue("s", formatDataRadix(snap.s, addrRadix, 8), addrRadix, 8, true)}</td>
            </tr>
            <tr>
              <td className="reg-name">P</td>
              <td className="reg-value">{renderRegisterValue("p", formatDataRadix(snap.p, addrRadix, 8), addrRadix, 8, true)}</td>
            </tr>
            <tr>
              <td className="reg-name" />
              <td className="reg-flags">
                {renderFlags(snap.p, snap.changed_flags)}
              </td>
            </tr>
          </tbody>
        </table>
      )}
    </div>
  );
}
