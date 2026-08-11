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

// MemoryPanel always renders a fixed 16-row page (mouse wheel pages through
// memory rather than scrolling the list), so unlike the other stacked
// panels it can't shrink and scroll internally — dockview's default 50/50
// "below" split leaves it too short, clipping the last several rows behind
// the Watchpoints panel underneath. ~30px header (font-size-btn buttons +
// padding + border) + 16 rows at font-size-mono/line-height 1.6 (~22px each)
// + body padding, plus headroom for cross-platform font-metric variance.
const MEMORY_PANEL_DEFAULT_HEIGHT = 420;

/**
 * Hardcoded default arrangement mirroring today's 3-column layout as
 * **splits, not tabs** — nothing is hidden behind a tab today, and the
 * default shouldn't regress that. No persistence yet (issue #382).
 *
 * `position: {referencePanel, direction}` splits relative to the *group*
 * containing that panel, not the whole row/column it happens to sit in —
 * so the top-level left/center/right row must be established first (memory,
 * disassembly, registers as horizontal siblings of the root). Only then can
 * watchpoints/stack/cpu-bus be added "below" their column's top panel,
 * which nests a new column *inside* that row cell rather than splitting the
 * grid's root. Adding a "below" split before the row exists instead nests
 * the next "right" split inside that same cell, collapsing all three
 * columns' heights down to just the top row.
 */
function addDefaultLayout(api: DockviewReadyEvent["api"]) {
  const add = (id: MainPanelId, rest: { position?: AddPanelPositionOptions; initialWidth?: number }) =>
    api.addPanel({ id, component: id, title: PANEL_TITLES[id], ...rest });

  add("memory", { initialWidth: 640 });
  add("disassembly", { position: { referencePanel: "memory", direction: "right" } });
  add("registers", { position: { referencePanel: "disassembly", direction: "right" }, initialWidth: 220 });
  add("watchpoints", { position: { referencePanel: "memory", direction: "below" } });
  add("stack", { position: { referencePanel: "registers", direction: "below" } });
  add("cpu-bus", { position: { referencePanel: "stack", direction: "below" } });

  // Reserve Memory's full page height directly rather than sizing
  // Watchpoints (dockview gives the sibling whichever space is left over).
  api.getPanel("memory")?.api.setSize({ height: MEMORY_PANEL_DEFAULT_HEIGHT });
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
