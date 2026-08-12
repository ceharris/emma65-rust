import { useCallback, useEffect, useMemo, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
  AddPanelPositionOptions,
  AnchoredBox,
  AnchorPosition,
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
import { RUN_CONTROLS_COLLAPSED_HEIGHT } from "../RunControlsPanel";

// Debounces persisting the layout while the user is actively dragging/resizing
// panels — onDidLayoutChange fires on every intermediate frame of a drag.
const LAYOUT_PERSIST_DEBOUNCE_MS = 500;

/**
 * `layout::get_dock_layout`'s return shape (see `layout.rs`): dockview's own
 * serialized arrangement, the "Terminal is detached to its own window" flag
 * (issue #385), and the per-panel last-known-position map (issue #393), all
 * persisted alongside each other — snake_case to match the Rust struct's
 * field names directly, same convention `CpuBusPanel.tsx` follows for
 * `effective_speed`.
 */
interface DockLayoutData {
  dockview: SerializedDockview | null;
  terminal_detached: boolean;
  panel_positions: Partial<Record<MainPanelId, PanelPosition>> | null;
}

/**
 * A dock group (and tab index within it) some panel occupied at some point
 * in the past. Used two ways: `terminalPositionRef` below remembers exactly
 * where Terminal was immediately before a detach, so reattach can put it
 * back (in-memory only — a fresh session has no "previous" position to
 * restore anyway); `lastPanelPositionRef` (see `recordPanelPositions`)
 * continuously tracks every panel's most recent position and is persisted
 * alongside the dockview arrangement itself, so the View menu (issue #393)
 * can restore a panel whose dock tab was closed even across an app restart.
 * `group_id` values from a restored session remain valid to look up via
 * `api.getGroup` after `fromJSON` restores the same arrangement, since
 * dockview's serialized format embeds each group's id and reconstructs it
 * exactly on restore. Terminal is always docked, so `terminalPositionRef`
 * only ever holds this shape; `lastPanelPositionRef` covers every panel and
 * so must also allow the floating shape below (issue #395's Run Controls
 * panel).
 */
interface DockedPanelPosition {
  group_id: string;
  index?: number;
}

/**
 * Where a floating panel last sat, in dockview's own anchored-box format
 * (`AnchoredBox` — one of the four corner shapes plus a size). No `group_id`
 * to look up: a closed floating group's window is gone entirely, unlike a
 * docked group which can persist empty.
 */
interface FloatingPanelPosition {
  kind: "floating";
  bounds: AnchoredBox;
}

/** Undiscriminated docked shape kept exactly as before (issue #393) so old persisted JSON keeps parsing as-is; `kind` distinguishes the new floating variant. */
type PanelPosition = DockedPanelPosition | FloatingPanelPosition;

/** The `floating` shape `api.addPanel` accepts — either a fresh `x`/`y` guess or a remembered corner-anchored position, both paired with a size. */
type FloatingBounds = { x: number; y: number; width: number; height: number } | { position: AnchorPosition; width: number; height: number };

/**
 * Default floating position/size for the Run Controls panel (issue #395)
 * when it has no remembered position — first run, or after a restore whose
 * persisted layout predates this panel. Dockview's `FloatingGroupService`
 * clamps on-screen at runtime, so an imperfect guess self-corrects.
 *
 * `width` is well above what the button row alone needs (~275px) because a
 * `width: 300` default was observed rendering far narrower in practice —
 * dockview's own "N hidden tabs" overflow indicator appeared on a group
 * with only one tab, meaning the *actual* rendered box, not just what's
 * visible on screen, was narrower than requested. Root cause unconfirmed
 * (measured screenshots suggest something close to a fixed ~0.57x width
 * attenuation, not reproduced for height); this value asks for enough
 * headroom to land close to the intended size even under that attenuation.
 */
const RUN_CONTROLS_DEFAULT_BOUNDS: FloatingBounds = { x: 460, y: 40, width: 700, height: RUN_CONTROLS_COLLAPSED_HEIGHT };

/**
 * Records the "terminal" panel's current group/index into `positionRef`
 * before closing it, so a later reattach (`positionForReattach` below) can
 * restore it there. Shared by the dock tab's own Detach button and the
 * Window-menu/native-close-driven detach path (`terminal-detach-requested`),
 * since both ultimately just close the same panel.
 */
function closeTerminalPanel(api: DockviewReadyEvent["api"] | null, positionRef: React.MutableRefObject<DockedPanelPosition | null>) {
  const panel = api?.getPanel("terminal");
  if (!panel) return;
  positionRef.current = { group_id: panel.group.id, index: panel.group.panels.indexOf(panel) };
  panel.api.close();
}

/**
 * Resolves where to re-add the "terminal" panel on reattach: the remembered
 * pre-detach group/index if that group still exists (it won't if Terminal
 * was the last panel in it — dockview removes emptied groups), otherwise the
 * same default bottom-group tab position `addMissingBottomPanels` uses.
 */
function positionForReattach(api: DockviewReadyEvent["api"], remembered: DockedPanelPosition | null): AddPanelPositionOptions {
  const groupStillExists = remembered !== null && api.getGroup(remembered.group_id) !== undefined;
  return groupStillExists
    ? { referenceGroup: remembered.group_id, index: remembered.index }
    : { referencePanel: "trace" };
}

/**
 * Default position to re-add a *docked* panel that's missing and has no
 * remembered position (see `recordPanelPositions`/`resolveRevealPosition`
 * below) — mirrors `addDefaultLayout`'s placements so a dismissed-then-
 * restored panel lands roughly where a fresh profile would put it. Memory
 * and Trace are absent: they're the two structural roots `addDefaultLayout`
 * adds first (Memory with no position at all, Trace via an
 * `AbsolutePosition` split), so `resolveRevealPosition` special-cases both
 * instead. Run Controls (issue #395) is also absent — it's floating, not
 * docked, so it falls back to `RUN_CONTROLS_DEFAULT_BOUNDS` instead of an
 * entry here.
 */
const DEFAULT_PANEL_POSITION: Partial<Record<MainPanelId, { referencePanel: MainPanelId; direction?: "right" | "below" }>> = {
  disassembly: { referencePanel: "memory", direction: "right" },
  registers: { referencePanel: "disassembly", direction: "right" },
  watchpoints: { referencePanel: "memory", direction: "below" },
  stack: { referencePanel: "registers", direction: "below" },
  "cpu-bus": { referencePanel: "stack", direction: "below" },
  log: { referencePanel: "trace" },
  terminal: { referencePanel: "trace" },
};

/**
 * Finds the remembered on-screen bounds of the floating window hosting
 * `groupId`, by scanning `json.floatingGroups` for the entry whose
 * legacy single-group `data.id` matches. Doesn't handle the nested-`grid`
 * form (multiple groups dragged into one floating window) — an edge case
 * for a panel meant to be used alone, so it degrades to "no remembered
 * bounds" rather than a crash.
 */
function findFloatingBounds(json: SerializedDockview, groupId: string): AnchoredBox | null {
  for (const fg of json.floatingGroups ?? []) {
    if (fg.data?.id === groupId) return fg.position;
  }
  return null;
}

/**
 * Snapshots every currently-present main panel's position into `ref`,
 * called on every `onDidLayoutChange` (drag, resize, add, remove, move —
 * see `onReady` below). Since this only ever writes an entry for a panel
 * that still exists, a panel's entry simply stops updating (rather than
 * being cleared) once it's removed — leaving `ref` holding each panel's
 * *last* known position, which is exactly what the View menu's restore
 * (`resolveRevealPosition`) needs.
 *
 * Floating panels (issue #395) need one `api.toJSON()` call to read their
 * on-screen bounds back out, so the floating check runs first and that call
 * is skipped entirely in the common all-docked case — this function runs on
 * every drag frame.
 */
function recordPanelPositions(api: DockviewReadyEvent["api"], ref: React.MutableRefObject<Partial<Record<MainPanelId, PanelPosition>>>) {
  const floatingIds: MainPanelId[] = [];
  for (const id of Object.keys(PANEL_TITLES) as MainPanelId[]) {
    const panel = api.getPanel(id);
    if (!panel) continue;
    if (panel.group.api.location.type === "floating") {
      floatingIds.push(id);
    } else {
      ref.current[id] = { group_id: panel.group.id, index: panel.group.panels.indexOf(panel) };
    }
  }
  if (floatingIds.length === 0) return;
  const json = api.toJSON();
  for (const id of floatingIds) {
    const panel = api.getPanel(id);
    if (!panel) continue;
    const bounds = findFloatingBounds(json, panel.group.id);
    if (bounds) ref.current[id] = { kind: "floating", bounds };
  }
}

/**
 * Resolves where to re-add a panel that a View menu click (issue #393) found
 * missing from the dock: its last recorded position if that group/window
 * still exists, else Trace's usual full-width bottom-group split (for
 * "trace" itself), the floating default (for "run-controls"), or the
 * corresponding `DEFAULT_PANEL_POSITION` entry (for anything else, provided
 * its reference panel is actually present — otherwise dockview has no cell
 * to split relative to, so the panel is added as a new group instead, which
 * is what an absent `position` produces).
 */
function resolveRevealPosition(
  api: DockviewReadyEvent["api"],
  id: MainPanelId,
  lastPositions: Partial<Record<MainPanelId, PanelPosition>>,
): { position?: AddPanelPositionOptions; initialHeight?: number; floating?: FloatingBounds } {
  const remembered = lastPositions[id];
  if (remembered) {
    if ("kind" in remembered) {
      const { width, height, ...position } = remembered.bounds;
      return { floating: { position, width, height } };
    }
    if (api.getGroup(remembered.group_id)) {
      return { position: { referenceGroup: remembered.group_id, index: remembered.index } };
    }
  }
  if (id === "trace") {
    return { position: { direction: "below" }, initialHeight: BOTTOM_GROUP_DEFAULT_HEIGHT };
  }
  if (id === "run-controls") {
    return { floating: RUN_CONTROLS_DEFAULT_BOUNDS };
  }
  const fallback = DEFAULT_PANEL_POSITION[id];
  if (fallback && api.getPanel(fallback.referencePanel)) {
    return { position: fallback };
  }
  return {};
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

  // Floating, not docked — see RUN_CONTROLS_DEFAULT_BOUNDS.
  api.addPanel({
    id: "run-controls",
    component: "run-controls",
    title: PANEL_TITLES["run-controls"],
    floating: RUN_CONTROLS_DEFAULT_BOUNDS,
  });

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
 * Persists the current layout, plus `lastPositions` (the per-panel
 * last-known-position map — see `recordPanelPositions`), to
 * `~/.emma/debugger/config/layout.json` via the `set_dock_layout` command.
 * `api.toJSON()` returns dockview's own serialization format; the Rust side
 * stores both as opaque JSON and never parses their internal shape (see
 * `layout.rs`).
 */
function persistLayout(api: DockviewReadyEvent["api"], lastPositions: Partial<Record<MainPanelId, PanelPosition>>) {
  invoke("set_dock_layout", { layout: api.toJSON(), panelPositions: lastPositions }).catch((err) =>
    console.error("set_dock_layout failed:", err),
  );
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
 * Adds the "run-controls" floating panel (issue #395) if a just-restored
 * layout is missing it — i.e. it was persisted before this panel existed.
 * Same backfill pattern as `addMissingBottomPanels`, reusing
 * `resolveRevealPosition` so a restored old layout's remembered floating
 * bounds (if any survived in `panel_positions`) are honored, falling back to
 * `RUN_CONTROLS_DEFAULT_BOUNDS` otherwise. Returns whether it was added, so
 * the caller knows whether to re-persist.
 */
function addMissingRunControlsPanel(api: DockviewReadyEvent["api"], lastPositions: Partial<Record<MainPanelId, PanelPosition>>): boolean {
  if (api.getPanel("run-controls")) return false;
  const { floating } = resolveRevealPosition(api, "run-controls", lastPositions);
  api.addPanel({
    id: "run-controls",
    component: "run-controls",
    title: PANEL_TITLES["run-controls"],
    floating: floating ?? RUN_CONTROLS_DEFAULT_BOUNDS,
  });
  return true;
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
 *
 * Also seeds `lastPositionsRef` from the persisted `panel_positions` map
 * (issue #393) before `fromJSON` runs, so a panel closed in a prior session
 * still has a last-known position for the View menu to restore it to. This
 * is safe to do unconditionally: `fromJSON`'s own layout-change event fires
 * `recordPanelPositions` immediately afterward (see `onReady`), which
 * overwrites the seeded entry for every panel the restored arrangement
 * actually contains with its live position, leaving the seed intact only for
 * panels the restored arrangement doesn't (i.e. ones already closed as of
 * the last save).
 */
async function restoreLayout(api: DockviewReadyEvent["api"], lastPositionsRef: React.MutableRefObject<Partial<Record<MainPanelId, PanelPosition>>>) {
  let restored = false;
  let terminalDetached = false;
  try {
    const saved = await invoke<DockLayoutData>("get_dock_layout");
    terminalDetached = saved.terminal_detached;
    if (saved.panel_positions) {
      lastPositionsRef.current = saved.panel_positions;
    }
    if (saved.dockview) {
      api.fromJSON(saved.dockview);
      restored = true;
    }
  } catch (err) {
    console.error("Failed to restore persisted dock layout, falling back to default:", err);
  }
  if (!restored) {
    addDefaultLayout(api, terminalDetached);
    persistLayout(api, lastPositionsRef.current);
    return;
  }
  if (terminalDetached) {
    api.getPanel("terminal")?.api.close();
  }
  const addedBottomPanels = addMissingBottomPanels(api, terminalDetached);
  const addedRunControls = addMissingRunControlsPanel(api, lastPositionsRef.current);
  if (addedBottomPanels || addedRunControls) {
    persistLayout(api, lastPositionsRef.current);
  }
}

/**
 * Builds the `rightHeaderActionsComponent` dockview renders once per group,
 * closing over `positionRef` so the Detach button can remember where
 * Terminal was before closing it (see `closeTerminalPanel`) — dockview gives
 * this component no way to receive extra props of its own, only the
 * per-group `IDockviewHeaderActionsProps` it defines. Groups whose active
 * panel isn't "terminal" render nothing. Clicking Detach calls the
 * `detach_terminal` command (shows the detached window, retargets the
 * console bridge, persists the flag — see `terminal.rs`) and only then
 * closes the dock panel, deliberately after the new window/target is fully
 * in place (issue #385's `emit_to`-retarget race mitigation).
 */
function makeTerminalTabActions(positionRef: React.MutableRefObject<DockedPanelPosition | null>) {
  return function TerminalTabActions({ activePanel, containerApi }: IDockviewHeaderActionsProps) {
    if (activePanel?.id !== "terminal") return null;
    const handleDetach = () => {
      invoke("detach_terminal")
        .then(() => closeTerminalPanel(containerApi, positionRef))
        .catch((err) => console.error("detach_terminal failed:", err));
    };
    return (
      <button className="dock-tab-action" onClick={handleDetach} title="Detach Terminal to its own window">
        <i className="codicon codicon-link-external" />
      </button>
    );
  };
}

/** Hosts the main window's dockview panels (Register/Disassembly/Memory/Stack/Watchpoint/CpuBus/Trace/Log/Terminal) in a dockview grid. */
export default function DockLayout() {
  const { resolvedTheme } = useTheme();
  const layoutChangeSubscriptionRef = useRef<DockviewIDisposable | null>(null);
  const persistTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const apiRef = useRef<DockviewReadyEvent["api"] | null>(null);
  const terminalPositionRef = useRef<DockedPanelPosition | null>(null);
  const lastPanelPositionRef = useRef<Partial<Record<MainPanelId, PanelPosition>>>({});
  const TerminalTabActions = useMemo(() => makeTerminalTabActions(terminalPositionRef), []);

  useEffect(
    () => () => {
      layoutChangeSubscriptionRef.current?.dispose();
      if (persistTimerRef.current !== null) clearTimeout(persistTimerRef.current);
    },
    [],
  );

  // The View menu's per-panel items (issue #393) and Ctrl+Shift+T (Terminal
  // only — see `useAppKeyBindings.ts`) both reach a panel via this event,
  // targeted at this window specifically since it's the only one hosting a
  // dockview instance. If the panel is already present — visible or just not
  // the active tab in its group — this just brings its tab to the front. If
  // it isn't (its dock tab was closed, the bug this issue fixes), it's added
  // back via `resolveRevealPosition`: at its last recorded position
  // (`lastPanelPositionRef`, kept current by `recordPanelPositions` below) if
  // that group still exists, else its usual default position. Terminal while
  // detached never reaches this handler at all — `lib.rs`'s `on_menu_event`
  // special-cases it and focuses the detached window directly instead of
  // emitting this event, since there's no dock tab to add or activate.
  useEffect(() => {
    const unlistenPromise = listen<MainPanelId>("reveal-panel", (event) => {
      const api = apiRef.current;
      if (!api) return;
      const id = event.payload;
      const existing = api.getPanel(id);
      if (existing) {
        existing.api.setActive();
        return;
      }
      const { position, initialHeight, floating } = resolveRevealPosition(api, id, lastPanelPositionRef.current);
      if (floating) {
        api.addPanel({ id, component: id, title: PANEL_TITLES[id], floating });
      } else {
        api.addPanel({ id, component: id, title: PANEL_TITLES[id], position, initialHeight });
      }
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
      closeTerminalPanel(apiRef.current, terminalPositionRef);
    });
    return () => { unlistenPromise.then((f) => f()); };
  }, []);

  // Restores Terminal to the group/index it occupied before the detach that
  // preceded this reattach (see `positionForReattach`), falling back to the
  // default bottom-group tab position when that's no longer resolvable —
  // e.g. a fresh session with no remembered position, or a group that got
  // emptied and removed while Terminal was away.
  useEffect(() => {
    const unlistenPromise = listen("terminal-reattached", () => {
      const api = apiRef.current;
      if (!api || api.getPanel("terminal")) return;
      const position = positionForReattach(api, terminalPositionRef.current);
      api.addPanel({ id: "terminal", component: "terminal", title: PANEL_TITLES.terminal, position });
    });
    return () => { unlistenPromise.then((f) => f()); };
  }, []);

  const onReady = useCallback((event: DockviewReadyEvent) => {
    apiRef.current = event.api;
    restoreLayout(event.api, lastPanelPositionRef);
    layoutChangeSubscriptionRef.current = event.api.onDidLayoutChange(() => {
      recordPanelPositions(event.api, lastPanelPositionRef);
      if (persistTimerRef.current !== null) clearTimeout(persistTimerRef.current);
      persistTimerRef.current = setTimeout(() => {
        persistTimerRef.current = null;
        persistLayout(event.api, lastPanelPositionRef.current);
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
