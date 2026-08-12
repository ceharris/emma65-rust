import { useCallback, useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
  AddPanelPositionOptions,
  DockviewIDisposable,
  DockviewReact,
  DockviewReadyEvent,
  DockviewTheme,
  IDockviewHeaderActionsProps,
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
 * `layout::get_dock_layout`'s return shape (see `layout.rs`): dockview's own
 * serialized arrangement plus the "Terminal is detached to its own window"
 * flag persisted alongside it (issue #385) — snake_case to match the Rust
 * struct's field names directly, same convention `CpuBusPanel.tsx` follows
 * for `effective_speed`.
 */
interface DockLayoutData {
  dockview: SerializedDockview | null;
  terminal_detached: boolean;
}

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

// Trace/Log form a VS Code-style Output/Problems bottom dock, tabbed against
// each other. Given as a plain height (not width): both scroll internally,
// so this just trades off default vertical space against the panels above.
const BOTTOM_GROUP_DEFAULT_HEIGHT = 260;

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
 *
 * `terminalDetached` skips adding the "terminal" panel — reachable even on a
 * brand-new profile with no saved dockview arrangement yet, since the
 * terminal-detached flag (like the rest of `layout.json`, per #342) isn't
 * profile-scoped: Terminal can already be detached from a previous profile
 * when this profile builds its very first default layout.
 */
function addDefaultLayout(api: DockviewReadyEvent["api"], terminalDetached: boolean) {
  const add = (
    id: MainPanelId,
    rest: { position?: AddPanelPositionOptions; initialWidth?: number; initialHeight?: number },
  ) => api.addPanel({ id, component: id, title: PANEL_TITLES[id], ...rest });

  add("memory", { initialWidth: 640 });
  add("disassembly", { position: { referencePanel: "memory", direction: "right" } });
  add("registers", { position: { referencePanel: "disassembly", direction: "right" }, initialWidth: 220 });
  add("watchpoints", { position: { referencePanel: "memory", direction: "below" } });
  add("stack", { position: { referencePanel: "registers", direction: "below" } });
  add("cpu-bus", { position: { referencePanel: "stack", direction: "below" } });

  // No referencePanel: an AbsolutePosition split (dockview-core's
  // `orthogonalize`) applies to the grid's root rather than to one panel's
  // own cell, so this spans the full width below the three-column row above
  // — unlike the "below" splits just above, which nest inside their
  // column's own cell precisely because they *do* reference a panel there.
  add("trace", { position: { direction: "below" }, initialHeight: BOTTOM_GROUP_DEFAULT_HEIGHT });
  add("log", { position: { referencePanel: "trace" } });
  if (!terminalDetached) add("terminal", { position: { referencePanel: "trace" } });

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
 * Adds Trace/Log/Terminal as the bottom tabbed group if a just-restored
 * layout is missing any of them — i.e. it was persisted before #383/#384
 * introduced them. Returns whether anything was added, so the caller knows
 * whether to re-persist.
 *
 * `api.fromJSON` doesn't error just because the saved JSON has fewer panels
 * than `panelComponents` now registers — it happily restores a valid subset
 * — so restoring an old layout otherwise leaves a since-added panel
 * permanently missing rather than falling back to `addDefaultLayout`, which
 * is the only other place that adds them. Any future addition to this
 * bottom group needs the same kind of reconciliation here.
 *
 * `terminalDetached` skips adding "terminal" even when it's absent — a
 * missing "terminal" panel is only a stale-layout bug when Terminal isn't
 * currently detached; when it is, `restoreLayout` is the one place that
 * knows to leave it out.
 */
function addMissingBottomPanels(api: DockviewReadyEvent["api"], terminalDetached: boolean): boolean {
  const hasTrace = api.getPanel("trace") !== undefined;
  const hasLog = api.getPanel("log") !== undefined;
  const hasTerminal = api.getPanel("terminal") !== undefined;
  if (!hasTrace) {
    api.addPanel({
      id: "trace",
      component: "trace",
      title: PANEL_TITLES.trace,
      position: { direction: "below" },
      initialHeight: BOTTOM_GROUP_DEFAULT_HEIGHT,
    });
  }
  if (!hasLog) {
    api.addPanel({
      id: "log",
      component: "log",
      title: PANEL_TITLES.log,
      // Tabs alongside Trace if this restore just added it above; otherwise
      // Trace was already present in the restored layout, so tab there.
      position: { referencePanel: "trace" },
    });
  }
  if (!hasTerminal && !terminalDetached) {
    api.addPanel({
      id: "terminal",
      component: "terminal",
      title: PANEL_TITLES.terminal,
      position: { referencePanel: "trace" },
    });
  }
  return !hasTrace || !hasLog || (!hasTerminal && !terminalDetached);
}

/**
 * Restores the persisted layout on mount via `get_dock_layout`, falling back
 * to the hardcoded default (and re-persisting it) if none was saved yet or
 * the saved layout fails to deserialize — e.g. after a dockview version
 * upgrade changes its internal schema. A layout that restores successfully
 * but predates a since-added panel gets that panel patched in and
 * re-persisted too (see `addMissingBottomPanels`).
 *
 * The terminal-detached flag returned alongside the dockview arrangement is
 * authoritative over whatever the arrangement itself happens to contain —
 * `detach_terminal`/`reattach_terminal` (`terminal.rs`) persist the flag and
 * the arrangement as two separate writes, so a crash between them can leave
 * a restored arrangement with a stale "terminal" panel despite the flag
 * saying detached (or vice versa isn't possible: `addMissingBottomPanels`
 * already treats "flag false, panel missing" as a reconciliation case). Any
 * such mismatch is corrected here before the panel actions render.
 */
async function restoreLayout(api: DockviewReadyEvent["api"]) {
  let restored = false;
  let terminalDetached = false;
  try {
    const saved = await invoke<DockLayoutData>("get_dock_layout");
    terminalDetached = saved.terminal_detached;
    if (saved.dockview) {
      api.fromJSON(saved.dockview);
      restored = true;
    }
  } catch (err) {
    console.error("Failed to restore persisted dock layout, falling back to default:", err);
  }
  if (!restored) {
    addDefaultLayout(api, terminalDetached);
    persistLayout(api);
    return;
  }
  if (terminalDetached) {
    api.getPanel("terminal")?.api.close();
  }
  if (addMissingBottomPanels(api, terminalDetached)) {
    persistLayout(api);
  }
}

/**
 * Renders a "Detach" action in the tab bar of whichever group's active
 * panel is "terminal" — dockview calls this once per group, passing that
 * group's own `activePanel`/`containerApi`, so groups without a "terminal"
 * tab active render nothing. Calls the `detach_terminal` command (shows the
 * detached window, retargets the console bridge, persists the flag — see
 * `terminal.rs`) and only then closes the dock panel, deliberately after
 * the new window/target is fully in place (issue #385's `emit_to`-retarget
 * race mitigation).
 */
function TerminalTabActions({ activePanel, containerApi }: IDockviewHeaderActionsProps) {
  if (activePanel?.id !== "terminal") return null;
  const handleDetach = () => {
    invoke("detach_terminal")
      .then(() => containerApi.getPanel("terminal")?.api.close())
      .catch((err) => console.error("detach_terminal failed:", err));
  };
  return (
    <button className="dock-tab-action" onClick={handleDetach} title="Detach Terminal to its own window">
      <i className="codicon codicon-link-external" />
    </button>
  );
}

/** Hosts the main window's dockview panels (Register/Disassembly/Memory/Stack/Watchpoint/CpuBus/Trace/Log/Terminal) in a dockview grid. */
export default function DockLayout() {
  const { resolvedTheme } = useTheme();
  const layoutChangeSubscriptionRef = useRef<DockviewIDisposable | null>(null);
  const persistTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const apiRef = useRef<DockviewReadyEvent["api"] | null>(null);

  useEffect(
    () => () => {
      layoutChangeSubscriptionRef.current?.dispose();
      if (persistTimerRef.current !== null) clearTimeout(persistTimerRef.current);
    },
    [],
  );

  // Trace/Log no longer have their own window to show/hide, so
  // Ctrl+Shift+Y/L and the Window-menu "Reveal Trace"/"Reveal Log" items
  // just need their dock tab brought to the front — reachable from any
  // window via the native menu accelerator (main window only) or the
  // JS-level binding in useAppKeyBindings.ts, via the `reveal-panel` event,
  // targeted at this window specifically since it's the only one hosting a
  // dockview instance. Ctrl+Shift+T still reaches Terminal's dock tab this
  // same way when it's docked (a harmless no-op while detached, since no
  // "terminal" panel exists to activate) — the Window > Terminal menu item's
  // own accelerator was repurposed to detach/attach instead (see `lib.rs`'s
  // `on_menu_event`), so this event is Terminal's only remaining "bring
  // its tab to the front" path.
  useEffect(() => {
    const unlistenPromise = listen<MainPanelId>("reveal-panel", (event) => {
      apiRef.current?.getPanel(event.payload)?.api.setActive();
    });
    return () => { unlistenPromise.then((f) => f()); };
  }, []);

  // Rust-driven detach/reattach (the Window > Terminal menu item, and the
  // detached window's native close button) has no JS handler of its own
  // already in place to add/remove the "terminal" dock panel — the dock
  // tab's own Detach button (`TerminalTabActions` below) does that inline
  // since it's already running in this component, but the menu/close paths
  // instead emit these two events for the same effect.
  useEffect(() => {
    const unlistenPromise = listen("terminal-detach-requested", () => {
      apiRef.current?.getPanel("terminal")?.api.close();
    });
    return () => { unlistenPromise.then((f) => f()); };
  }, []);

  useEffect(() => {
    const unlistenPromise = listen("terminal-reattached", () => {
      const api = apiRef.current;
      if (!api || api.getPanel("terminal")) return;
      api.addPanel({ id: "terminal", component: "terminal", title: PANEL_TITLES.terminal, position: { referencePanel: "trace" } });
    });
    return () => { unlistenPromise.then((f) => f()); };
  }, []);

  const onReady = useCallback((event: DockviewReadyEvent) => {
    apiRef.current = event.api;
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
        rightHeaderActionsComponent={TerminalTabActions}
        onReady={onReady}
        theme={{ ...EMMA65_DOCK_THEME_BASE, colorScheme: resolvedTheme }}
      />
    </div>
  );
}
