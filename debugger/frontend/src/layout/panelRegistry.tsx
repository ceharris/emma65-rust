import { IDockviewPanelProps } from "dockview-react";
import AssemblerPanel from "../AssemblerPanel";
import BreakpointPanel from "../BreakpointPanel";
import DisassemblyPanel from "../DisassemblyPanel";
import DisplayPanel from "../DisplayPanel";
import LogPanel from "../LogPanel";
import MemoryPanel from "../MemoryPanel";
import RegisterPanel from "../RegisterPanel";
import RunControlsPanel from "../RunControlsPanel";
import StackPanel from "../StackPanel";
import SymbolsPanel from "../SymbolsPanel";
import TerminalPanel from "../TerminalPanel";
import TracePanel from "../TracePanel";
import WatchpointPanel from "../WatchpointPanel";

/** Panel ids for the main window's dockview panels, used as both dockview panel ids and component-registry keys. */
export type MainPanelId =
  | "registers"
  | "disassembly"
  | "memory"
  | "display"
  | "stack"
  | "symbols"
  | "watchpoints"
  | "breakpoints"
  | "run-controls"
  | "trace"
  | "log"
  | "terminal"
  | "assembler";

/** Tab/group title dockview displays for each main-window panel. */
export const PANEL_TITLES: Record<MainPanelId, string> = {
  registers: "Registers",
  disassembly: "Disassembly",
  memory: "Memory",
  display: "Display",
  stack: "Stack",
  symbols: "Symbols",
  watchpoints: "Watchpoints",
  breakpoints: "Breakpoints",
  "run-controls": "Run Controls",
  trace: "Trace",
  log: "Log",
  terminal: "Terminal",
  assembler: "Assembler",
};

/** dockview component-id -> renderer map for the main window's dockview panels. */
export const panelComponents: Record<MainPanelId, React.FC<IDockviewPanelProps>> = {
  registers: () => <RegisterPanel />,
  disassembly: () => <DisassemblyPanel />,
  memory: () => <MemoryPanel />,
  display: () => <DisplayPanel />,
  stack: () => <StackPanel />,
  symbols: () => <SymbolsPanel />,
  watchpoints: () => <WatchpointPanel />,
  breakpoints: () => <BreakpointPanel />,
  "run-controls": () => <RunControlsPanel />,
  trace: () => <TracePanel />,
  log: () => <LogPanel />,
  // Threads the dockview panel API down so the size-preset menu (issue #462
  // Work Unit 4) can resize this panel via `dockPanelApi.setSize()` — the
  // detached-window host (`terminal-detached.tsx`) renders `TerminalPanel`
  // with no such prop, which is how it tells docked and detached apart.
  terminal: ({ api }) => <TerminalPanel dockPanelApi={api} />,
  // Threads the dockview panel API down so the tab title can reflect the
  // currently open source file (issue #474 debugger integration, Unit 4
  // follow-up) — same pattern as Terminal's dockPanelApi above.
  assembler: ({ api }) => <AssemblerPanel dockPanelApi={api} />,
};
