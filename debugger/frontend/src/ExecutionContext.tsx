import { createContext, useCallback, useContext, useEffect, useState, ReactNode } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { ExecState } from "./DisassemblyPanel";
import { RegisterSnapshot } from "./RegisterPanel";

interface ExecutionContextValue {
  /** Most recent register snapshot, or null before the first fetch completes. */
  lastSnapshot: RegisterSnapshot | null;
  /** Current CPU execution state. */
  execState: ExecState;
  /** True once the CPU has halted on STP; disables the execution controls. Does
   * NOT cover WAI — WAI can be resumed via NMI/IRQ, so controls stay enabled. */
  cpuStopped: boolean;
  /** Called with the post-step register snapshot so RegisterPanel can update immediately. */
  onStep: (snap: RegisterSnapshot) => void;
  /** Called whenever the execution state changes. */
  onExecStateChange: (state: ExecState) => void;
  /** Called with the post-edit register snapshot so other panels can update. */
  onEdit: (snap: RegisterSnapshot) => void;
  /** Called with the post-reset register snapshot so other panels can update. */
  onReset: (snap: RegisterSnapshot) => void;
}

const ExecutionContext = createContext<ExecutionContextValue | null>(null);

/**
 * Provides the shared register snapshot and execution-state that Disassembly,
 * Register, CpuBus, Memory, and Watchpoint panels all need, plus the update
 * callbacks that keep them in sync with each other.
 */
export function ExecutionProvider({ children }: { children: ReactNode }) {
  const [lastSnapshot, setLastSnapshot] = useState<RegisterSnapshot | null>(null);
  const [execState, setExecState] = useState<ExecState>("stopped");

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

  const cpuStopped = Boolean(lastSnapshot?.cpu_stopped);

  return (
    <ExecutionContext.Provider
      value={{
        lastSnapshot,
        execState,
        cpuStopped,
        onStep: handleStep,
        onExecStateChange: handleExecStateChange,
        onEdit: handleSnapshotUpdate,
        onReset: handleSnapshotUpdate,
      }}
    >
      {children}
    </ExecutionContext.Provider>
  );
}

/** Returns the shared execution state and update callbacks. Must be used within an `ExecutionProvider`. */
export function useExecutionContext(): ExecutionContextValue {
  const ctx = useContext(ExecutionContext);
  if (!ctx) throw new Error("useExecutionContext must be used within an ExecutionProvider");
  return ctx;
}
