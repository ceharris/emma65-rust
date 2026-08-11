import { useCallback } from "react";
import { AddPanelPositionOptions, DockviewReact, DockviewReadyEvent, DockviewTheme } from "dockview-react";
import "dockview-react/dist/styles/dockview.css";
import "../styles/dock-layout.scss";
import { useTheme } from "../ThemeContext";
import { MainPanelId, PANEL_TITLES, panelComponents } from "./panelRegistry";

/**
 * Theme hooks driven from the app's existing `--color-*` custom properties
 * (see `styles/dock-layout.scss`) rather than a shipped dockview theme —
 * confirmed feasible in issue #379's spike. `colorScheme` tracks the app's
 * resolved theme via `useTheme()`; unlike a detached window, the main
 * window already has that for free through `ThemeProvider`/React context.
 */
const EMMA65_DOCK_THEME_BASE: Omit<DockviewTheme, "colorScheme"> = {
  name: "emma65",
  className: "dockview-theme-emma65",
};

/**
 * Hardcoded default arrangement mirroring today's 3-column layout as
 * **splits, not tabs** — nothing is hidden behind a tab today, and the
 * default shouldn't regress that. No persistence yet (issue #382).
 */
function addDefaultLayout(api: DockviewReadyEvent["api"]) {
  const add = (id: MainPanelId, rest: { position?: AddPanelPositionOptions; initialWidth?: number }) =>
    api.addPanel({ id, component: id, title: PANEL_TITLES[id], ...rest });

  add("memory", { initialWidth: 640 });
  add("watchpoints", { position: { referencePanel: "memory", direction: "below" } });
  add("disassembly", { position: { referencePanel: "memory", direction: "right" } });
  add("registers", { position: { referencePanel: "disassembly", direction: "right" }, initialWidth: 220 });
  add("stack", { position: { referencePanel: "registers", direction: "below" } });
  add("cpu-bus", { position: { referencePanel: "stack", direction: "below" } });
}

/** Hosts the six main-window panels (Register/Disassembly/Memory/Stack/Watchpoint/CpuBus) in a dockview grid. */
export default function DockLayout() {
  const { resolvedTheme } = useTheme();

  const onReady = useCallback((event: DockviewReadyEvent) => {
    addDefaultLayout(event.api);
  }, []);

  return (
    <div className="dock-layout">
      <DockviewReact
        components={panelComponents}
        onReady={onReady}
        theme={{ ...EMMA65_DOCK_THEME_BASE, colorScheme: resolvedTheme }}
      />
    </div>
  );
}
