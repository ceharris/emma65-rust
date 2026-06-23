import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import "./styles/disassembly.scss";

interface DisassembledRow {
  addr: number;
  bytes: string[];
  mnemonic: string;
  operand: string;
  is_valid: boolean;
}

interface Props {
  /** Called after a step completes so the register panel can refresh. */
  onStep: () => void;
}

const ROW_COUNT = 24;

export default function DisassemblyPanel({ onStep }: Props) {
  const [rows, setRows] = useState<DisassembledRow[]>([]);
  const [currentPc, setCurrentPc] = useState<number | null>(null);
  const [stepping, setStepping] = useState(false);
  const currentPcRef = useRef<number | null>(null);

  const fetchDisassembly = useCallback(async (pc: number) => {
    try {
      const result = await invoke<DisassembledRow[]>("get_disassembly", {
        addr: pc,
        count: ROW_COUNT,
      });
      setRows(result);
      setCurrentPc(pc);
      currentPcRef.current = pc;
    } catch (e) {
      console.error("get_disassembly failed:", e);
    }
  }, []);

  useEffect(() => {
    const unlistenPromise = listen<number>("debugger-halted", (event) => {
      fetchDisassembly(event.payload);
    });

    return () => { unlistenPromise.then((f) => f()); };
  }, [fetchDisassembly]);

  const stepInto = useCallback(async () => {
    if (stepping) return;
    setStepping(true);
    try {
      await invoke("step_into");
      onStep();
    } catch (e) {
      console.error("step_into failed:", e);
    } finally {
      setStepping(false);
    }
  }, [stepping, onStep]);

  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.key === "F11" && !e.shiftKey) {
        e.preventDefault();
        stepInto();
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [stepInto]);

  const formatAddr = (addr: number) =>
    addr.toString(16).toUpperCase().padStart(4, "0");

  const formatBytes = (bytes: string[]) =>
    bytes.map((b) => b.padStart(2, "0")).join(" ").padEnd(8, " ");

  return (
    <div className="disassembly-panel">
      <div className="disassembly-header">
        <span className="panel-title">Disassembly</span>
        <div className="exec-controls">
          <button
            className="exec-btn step-into-btn"
            onClick={stepInto}
            disabled={stepping}
            title="Step Into (F11)"
          >
            Step Into
          </button>
        </div>
      </div>
      <div className="disassembly-body">
        {rows.length === 0 ? (
          <span className="disassembly-empty">Waiting for session…</span>
        ) : (
          rows.map((row) => (
            <div
              key={row.addr}
              className={[
                "disasm-row",
                row.addr === currentPc ? "current-pc" : "",
                row.is_valid ? "" : "invalid-op",
              ]
                .filter(Boolean)
                .join(" ")}
            >
              <span className="disasm-gutter" />
              <span className="disasm-addr">{formatAddr(row.addr)}</span>
              <span className="disasm-bytes">{formatBytes(row.bytes)}</span>
              <span className="disasm-mnemonic">{row.mnemonic}</span>
              {row.operand && (
                <span className="disasm-operand">{row.operand}</span>
              )}
            </div>
          ))
        )}
      </div>
    </div>
  );
}
