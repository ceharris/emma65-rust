import { IDockviewPanelProps } from "dockview-react";
import CpuBusPanel from "../CpuBusPanel";
import DisassemblyPanel from "../DisassemblyPanel";
import LogPanel from "../LogPanel";
import MemoryPanel from "../MemoryPanel";
import RegisterPanel from "../RegisterPanel";
import StackPanel from "../StackPanel";
import TracePanel from "../TracePanel";
import WatchpointPanel from "../WatchpointPanel";

/** Panel ids for the main window's dockview panels, used as both dockview panel ids and component-registry keys. */
export type MainPanelId =
  | "registers"
  | "disassembly"
  | "memory"
  | "stack"
  | "watchpoints"
  | "cpu-bus"
  | "trace"
  | "log";

/** Tab/group title dockview displays for each main-window panel. */
export const PANEL_TITLES: Record<MainPanelId, string> = {
  registers: "Registers",
  disassembly: "Disassembly",
  memory: "Memory",
  stack: "Stack",
  watchpoints: "Watchpoints",
  "cpu-bus": "CPU and Bus",
  trace: "Trace",
  log: "Log",
};

/** dockview component-id -> renderer map for the main window's dockview panels. */
export const panelComponents: Record<MainPanelId, React.FC<IDockviewPanelProps>> = {
  registers: () => <RegisterPanel />,
  disassembly: () => <DisassemblyPanel />,
  memory: () => <MemoryPanel />,
  stack: () => <StackPanel />,
  watchpoints: () => <WatchpointPanel />,
  "cpu-bus": () => <CpuBusPanel />,
  trace: () => <TracePanel />,
  log: () => <LogPanel />,
};
