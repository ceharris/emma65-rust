import { useCallback, useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import CpuBusPanel from "./CpuBusPanel";
import DisassemblyPanel, { ExecState } from "./DisassemblyPanel";
import MemoryPanel from "./MemoryPanel";
import RegisterPanel, { RegisterSnapshot } from "./RegisterPanel";
import StackPanel from "./StackPanel";

interface SessionStatus {
  message: string;
  ok: boolean;
}

export default function App() {
  const [status, setStatus] = useState<SessionStatus | null>(null);
  const [lastSnapshot, setLastSnapshot] = useState<RegisterSnapshot | null>(null);
  const [execState, setExecState] = useState<ExecState>("stopped");

  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.key === "q" && (e.ctrlKey || e.metaKey)) {
        e.preventDefault();
        invoke("quit");
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, []);

  useEffect(() => {
    const unlistenPromise = listen<SessionStatus>("session-status", (event) => {
      setStatus(event.payload);
    });

    invoke<SessionStatus | null>("get_session_status").then((current) => {
      if (current !== null) {
        setStatus(current);
      }
    });

    return () => { unlistenPromise.then((f) => f()); };
  }, []);

  const handleStep = useCallback((snap: RegisterSnapshot) => {
    setLastSnapshot(snap);
  }, []);

  const handleExecStateChange = useCallback((state: ExecState) => {
    setExecState(state);
  }, []);

  const handleReset = useCallback((snap: RegisterSnapshot) => {
    setLastSnapshot(snap);
  }, []);

  if (status === null || !status.ok) {
    return (
      <div className="app-splash">
        {status === null ? (
          <span className="status-pending">Initializing…</span>
        ) : (
          <span className="status-error">{status.message}</span>
        )}
      </div>
    );
  }

  return (
    <div className="app-layout">
      <div className="col col-left">
        <MemoryPanel />
        {/* Watchpoints — story 12 */}
      </div>
      <div className="col col-center">
        <DisassemblyPanel onStep={handleStep} onExecStateChange={handleExecStateChange} />
      </div>
      <div className="col col-right">
        <RegisterPanel snapshot={lastSnapshot} />
        <StackPanel />
        <CpuBusPanel execState={execState} onReset={handleReset} />
      </div>
    </div>
  );
}
