import { IDockviewPanelProps } from "dockview-react";
import BreakpointPanel from "../BreakpointPanel";
import DisassemblyPanel from "../DisassemblyPanel";
import LogPanel from "../LogPanel";
import MemoryPanel from "../MemoryPanel";
import RegisterPanel from "../RegisterPanel";
import RunControlsPanel from "../RunControlsPanel";
import StackPanel from "../StackPanel";
import TerminalPanel from "../TerminalPanel";
import TracePanel from "../TracePanel";
import WatchpointPanel from "../WatchpointPanel";

/** Panel ids for the main window's dockview panels, used as both dockview panel ids and component-registry keys. */
export type MainPanelId =
  | "registers"
  | "disassembly"
  | "memory"
  | "stack"
  | "watchpoints"
  | "breakpoints"
  | "run-controls"
  | "trace"
  | "log"
  | "terminal";

/** Tab/group title dockview displays for each main-window panel. */
export const PANEL_TITLES: Record<MainPanelId, string> = {
  registers: "Registers",
  disassembly: "Disassembly",
  memory: "Memory",
  stack: "Stack",
  watchpoints: "Watchpoints",
  breakpoints: "Breakpoints",
  "run-controls": "Run Controls",
  trace: "Trace",
  log: "Log",
  terminal: "Terminal",
};

/** dockview component-id -> renderer map for the main window's dockview panels. */
export const panelComponents: Record<MainPanelId, React.FC<IDockviewPanelProps>> = {
  registers: () => <RegisterPanel />,
  disassembly: () => <DisassemblyPanel />,
  memory: () => <MemoryPanel />,
  stack: () => <StackPanel />,
  watchpoints: () => <WatchpointPanel />,
  breakpoints: () => <BreakpointPanel />,
  "run-controls": () => <RunControlsPanel />,
  trace: () => <TracePanel />,
  log: () => <LogPanel />,
  terminal: () => <TerminalPanel />,
};
