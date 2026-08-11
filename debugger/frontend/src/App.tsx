import { useCallback, useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import ClearRecentDialog from "./ClearRecentDialog";
import CpuBusPanel from "./CpuBusPanel";
import DisassemblyPanel, { ExecState } from "./DisassemblyPanel";
import ExitConfirmDialog from "./ExitConfirmDialog";
import MemoryPanel from "./MemoryPanel";
import NewProfileDialog from "./NewProfileDialog";
import RegisterPanel, { RegisterSnapshot } from "./RegisterPanel";
import StackPanel from "./StackPanel";
import ThemeSelector from "./ThemeSelector";
import WatchpointPanel from "./WatchpointPanel";
import { useAppKeyBindings } from "./useAppKeyBindings";

interface SessionStatus {
  message: string;
  ok: boolean;
}

export default function App() {
  const [status, setStatus] = useState<SessionStatus | null>(null);
  const [lastSnapshot, setLastSnapshot] = useState<RegisterSnapshot | null>(null);
  const [execState, setExecState] = useState<ExecState>("stopped");

  useAppKeyBindings();

  // Ctrl+O/Ctrl+Q: open an existing profile / quit the app. Handled here (not
  // in the shared APP_KEY_BINDINGS array) since App.tsx only mounts in the
  // main window and both actions — like New Profile's Ctrl+N — are
  // main-window-only (issue #351: Ctrl+Q previously fired from every window,
  // including as a surprise exit while typing in the Terminal window, where
  // Ctrl+Q is conventionally XON).
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (!(e.ctrlKey || e.metaKey)) return;
      if (e.key === "o") {
        e.preventDefault();
        invoke("open_profile").catch((err) => console.error("open_profile failed:", err));
      } else if (e.key === "q") {
        e.preventDefault();
        invoke("quit").catch((err) => console.error("quit failed:", err));
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, []);

  // Phase 0 spike only (issue #379): Ctrl+Shift+D opens the throwaway
  // dockview spike window. Not added to APP_KEY_BINDINGS since it's
  // main-window-only and has no native menu accelerator to double-fire
  // against. Remove this block along with the rest of the spike code once
  // the write-up lands.
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.ctrlKey && e.shiftKey && e.code === "KeyD") {
        e.preventDefault();
        invoke("toggle_dockview_spike_window").catch((err) =>
          console.error("toggle_dockview_spike_window failed:", err)
        );
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

  useEffect(() => {
    const unlistenTick = listen("debugger-running-tick", () => {
      invoke<RegisterSnapshot>("get_registers")
        .then((snap) => setLastSnapshot(snap))
        .catch(() => {});
    });
    return () => { unlistenTick.then((f) => f()); };
  }, []);

  const handleExecStateChange = useCallback((state: ExecState) => {
    setExecState(state);
  }, []);

  // Shared by Reset (CpuBusPanel) and register edits (RegisterPanel) — both
  // just need lastSnapshot to reflect the command's returned snapshot.
  const handleSnapshotUpdate = useCallback((snap: RegisterSnapshot) => {
    setLastSnapshot(snap);
  }, []);

  // True once the CPU has halted on STP; cleared again on Reset. WAI does NOT
  // set this — unlike STP, WAI can be resumed by triggering NMI or asserting
  // IRQ, so Run/Step/Auto-Step stay enabled to let the user continue from there.
  const cpuStopped = Boolean(lastSnapshot?.cpu_stopped);

  if (status === null || !status.ok) {
    return (
      <>
        <NewProfileDialog />
        <ClearRecentDialog />
        <ExitConfirmDialog />
        <div className="app-splash">
          {status === null ? (
            <span className="status-pending">Initializing…</span>
          ) : (
            <span className="status-error">{status.message}</span>
          )}
        </div>
      </>
    );
  }

  return (
    <>
      <NewProfileDialog />
      <ClearRecentDialog />
      <ExitConfirmDialog />
      <div className="app-shell">
        <header className="app-toolbar">
          <ThemeSelector />
        </header>
        <div className="app-layout">
          <div className="col col-left">
            <MemoryPanel execState={execState} />
            <WatchpointPanel execState={execState} />
          </div>
          <div className="col col-center">
            <DisassemblyPanel onStep={handleStep} onExecStateChange={handleExecStateChange} cpuStopped={cpuStopped} />
          </div>
          <div className="col col-right">
            <RegisterPanel snapshot={lastSnapshot} execState={execState} onEdit={handleSnapshotUpdate} />
            <StackPanel />
            <CpuBusPanel execState={execState} onReset={handleSnapshotUpdate} />
          </div>
        </div>
      </div>
    </>
  );
}
