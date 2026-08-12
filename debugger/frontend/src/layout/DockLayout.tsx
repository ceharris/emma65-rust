import { useCallback, useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  AddPanelPositionOptions,
  DockviewIDisposable,
  DockviewReact,
  DockviewReadyEvent,
  DockviewTheme,
  SerializedDockview,
} from "dockview-react";
import "dockview-react/dist/styles/dockview.css";
import "../styles/dock-layout.scss";
import { useTheme } from "../ThemeContext";
import { MainPanelId, PANEL_TITLES, panelComponents } from "./panelRegistry";

// Debounces persisting the layout while the user is actively dragging/resizing
// panels — onDidLayoutChange fires on every intermediate frame of a drag.
const LAYOUT_PERSIST_DEBOUNCE_MS = 500;

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

// RegisterPanel's two-column layout (header + 3 rows on each side): ~25px
// header row + 3 rows at ~20px + table/panel padding, with headroom.
const REGISTERS_PANEL_DEFAULT_HEIGHT = 150;

// StackPanel always renders a fixed 8-row page (VISIBLE_PAIRS in
// StackPanel.tsx), so like Memory it can't shrink and scroll internally.
// ~30px header + 8 rows at font-size-mono/line-height 1.7 (~23px each) +
// body padding, with headroom for cross-platform font-metric variance.
const STACK_PANEL_DEFAULT_HEIGHT = 260;

/**
 * Hardcoded default arrangement mirroring today's 3-column layout as
 * **splits, not tabs** — nothing is hidden behind a tab today, and the
 * default shouldn't regress that. Used on first run and as the fallback
 * whenever a persisted layout (issue #382) is missing or fails to restore.
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

  // Registers/Stack/CpuBus form one flat 3-way vertical split (see the
  // ordering note above). dockview's resizeView sets the target's size
  // exactly, then redistributes the delta proportionally across the *other*
  // views in that split — so a setSize call perturbs whatever was set by an
  // earlier call, but leaves nothing after it untouched. Stack must come
  // last: it can't shrink and scroll (fixed 8-row page, like Memory), so its
  // size has to land exactly on target, whereas Registers degrades
  // gracefully via its own overflow-y: auto if the earlier call gets
  // nudged. CpuBus is left unset and simply takes what's left over.
  api.getPanel("registers")?.api.setSize({ height: REGISTERS_PANEL_DEFAULT_HEIGHT });
  api.getPanel("stack")?.api.setSize({ height: STACK_PANEL_DEFAULT_HEIGHT });
}

/**
 * Persists the current layout to `~/.emma/debugger/config/layout.json` via
 * the `set_dock_layout` command. `api.toJSON()` returns dockview's own
 * serialization format; the Rust side stores it as opaque JSON and never
 * parses its internal schema (see `layout.rs`).
 */
function persistLayout(api: DockviewReadyEvent["api"]) {
  invoke("set_dock_layout", { layout: api.toJSON() }).catch((err) => console.error("set_dock_layout failed:", err));
}

/**
 * Restores the persisted layout on mount via `get_dock_layout`, falling back
 * to the hardcoded default (and re-persisting it) if none was saved yet or
 * the saved layout fails to deserialize — e.g. after a dockview version
 * upgrade changes its internal schema.
 */
async function restoreLayout(api: DockviewReadyEvent["api"]) {
  let restored = false;
  try {
    const saved = await invoke<SerializedDockview | null>("get_dock_layout");
    if (saved) {
      api.fromJSON(saved);
      restored = true;
    }
  } catch (err) {
    console.error("Failed to restore persisted dock layout, falling back to default:", err);
  }
  if (!restored) {
    addDefaultLayout(api);
    persistLayout(api);
  }
}

/** Hosts the six main-window panels (Register/Disassembly/Memory/Stack/Watchpoint/CpuBus) in a dockview grid. */
export default function DockLayout() {
  const { resolvedTheme } = useTheme();
  const layoutChangeSubscriptionRef = useRef<DockviewIDisposable | null>(null);
  const persistTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(
    () => () => {
      layoutChangeSubscriptionRef.current?.dispose();
      if (persistTimerRef.current !== null) clearTimeout(persistTimerRef.current);
    },
    [],
  );

  const onReady = useCallback((event: DockviewReadyEvent) => {
    restoreLayout(event.api);
    layoutChangeSubscriptionRef.current = event.api.onDidLayoutChange(() => {
      if (persistTimerRef.current !== null) clearTimeout(persistTimerRef.current);
      persistTimerRef.current = setTimeout(() => {
        persistTimerRef.current = null;
        persistLayout(event.api);
      }, LAYOUT_PERSIST_DEBOUNCE_MS);
    });
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
