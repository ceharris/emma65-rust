import {useCallback, useEffect, useMemo, useRef} from "react";
import {invoke} from "@tauri-apps/api/core";
import {listen} from "@tauri-apps/api/event";
import {
  AddPanelPositionOptions,
  AnchoredBox,
  AnchorPosition,
  DockviewIDisposable,
  DockviewReact,
  DockviewReadyEvent,
  DockviewTheme,
  FloatingGroupOptions,
  IDockviewHeaderActionsProps,
  SerializedDockview,
} from "dockview-react";
import "dockview-react/dist/styles/dockview.css";
import "../styles/dock-layout.scss";
import {useTheme} from "../ThemeContext";
import {MainPanelId, PANEL_TITLES, panelComponents} from "./panelRegistry";
import {RUN_CONTROLS_DOCKED_HEIGHT, RUN_CONTROLS_MIN_WIDTH} from "../RunControlsPanel";
import {PanelHeaderActionProvider, usePanelHeaderActions} from "./panelHeaderActions";
import {dispatchAssemblerMenuAction} from "./assemblerMenuActions";

// Debounces persisting the layout while the user is actively dragging/resizing
// panels — onDidLayoutChange fires on every intermediate frame of a drag.
const LAYOUT_PERSIST_DEBOUNCE_MS = 500;

/**
 * `layout::get_dock_layout`'s return shape (see `layout.rs`): dockview's own
 * serialized arrangement, the "Terminal is detached to its own window" flag
 * (issue #385), and the per-panel last-known-position map (issue #393), all
 * persisted alongside each other — snake_case to match the Rust struct's
 * field names directly, same convention `StatusBar.tsx` follows for
 * `effective_speed`.
 */
interface DockLayoutData {
  dockview: SerializedDockview | null;
  terminal_detached: boolean;
  display_detached: boolean;
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
 * Default *docked* position (issue #402): the bottom of Disassembly's own
 * column, rather than a `DEFAULT_PANEL_POSITION` entry, because that map has
 * no per-entry way to also carry `RUN_CONTROLS_DOCKED_HEIGHT` as an
 * `initialHeight` — without it a "below" split would default to a 50/50
 * split, giving this single-row toolbar half of Disassembly's column.
 */
const RUN_CONTROLS_DEFAULT_POSITION: AddPanelPositionOptions = { referencePanel: "disassembly", direction: "below" };

/**
 * Bounds used by the Run Controls tab's explicit "Float" action (issue
 * #404) — the same size manually confirmed via UAT when this panel
 * defaulted to floating (issue #395). Taller than `RUN_CONTROLS_DOCKED_HEIGHT`:
 * that describes the docked *content* height, whereas a floating group
 * additionally needs room for dockview's own floating-titlebar/tab-bar
 * chrome. Unlike docked (issue #424), the floating window stays resizable —
 * dockview attaches its floating-window resize handles unconditionally, with
 * no supported way to disable them, so there's no clean way to lock this
 * one down too. An explicit action rather than a drag gesture because this
 * app's dockview grid fills the entire window with no empty margin to drop
 * into — there's nowhere drag-and-drop could resolve to "no valid dock
 * target" and float instead.
 */
const RUN_CONTROLS_FLOAT_BOUNDS: FloatingGroupOptions = { x: 460, y: 40, width: RUN_CONTROLS_MIN_WIDTH, height: 100 };

/**
 * Records `id`'s current group/index into `positionRef` before closing it, so
 * a later reattach (`positionForReattach` below) can restore it there. Shared
 * by a dock tab's own Detach button and the Window-menu/native-close-driven
 * detach path (`terminal-detach-requested`/`display-detach-requested`), since
 * both ultimately just close the same panel. Generic over `id` — Terminal
 * (issue #385) and Display (memory-mapped display device plan, Work Unit 5)
 * are the only two detachable panels, so this one function backs both.
 */
function closeDockedPanel(
  api: DockviewReadyEvent["api"] | null,
  id: MainPanelId,
  positionRef: React.MutableRefObject<DockedPanelPosition | null>,
) {
  const panel = api?.getPanel(id);
  if (!panel) return;
  positionRef.current = { group_id: panel.group.id, index: panel.group.panels.indexOf(panel) };
  panel.api.close();
}

/**
 * Resolves where to re-add a just-reattached panel: the remembered pre-detach
 * group/index if that group still exists (it won't if the panel was the last
 * one in it — dockview removes emptied groups), otherwise `fallback` (each
 * caller's own default docked position).
 */
function positionForReattach(
  api: DockviewReadyEvent["api"],
  remembered: DockedPanelPosition | null,
  fallback: AddPanelPositionOptions,
): AddPanelPositionOptions {
  const groupStillExists = remembered !== null && api.getGroup(remembered.group_id) !== undefined;
  return groupStillExists ? { referenceGroup: remembered.group_id, index: remembered.index } : fallback;
}

/**
 * Default position to re-add a *docked* panel that's missing and has no
 * remembered position (see `recordPanelPositions`/`resolveRevealPosition`
 * below) — mirrors `addDefaultLayout`'s placements so a dismissed-then-
 * restored panel lands roughly where a fresh profile would put it. Memory
 * and Trace are absent: they're the two structural roots `addDefaultLayout`
 * adds first (Memory with no position at all, Trace via an
 * `AbsolutePosition` split), so `resolveRevealPosition` special-cases both
 * instead. Run Controls (issue #395/#402) is also absent — its default needs
 * a fixed `initialHeight` alongside its position, which this map has no
 * per-entry way to carry, so `resolveRevealPosition` special-cases it too,
 * via `RUN_CONTROLS_DEFAULT_POSITION`.
 */
const DEFAULT_PANEL_POSITION: Partial<Record<MainPanelId, { referencePanel: MainPanelId; direction?: "right" | "below" }>> = {
  disassembly: { referencePanel: "memory", direction: "right" },
  registers: { referencePanel: "disassembly", direction: "right" },
  display: { referencePanel: "memory" },
  watchpoints: { referencePanel: "memory", direction: "below" },
  symbols: { referencePanel: "trace" },
  stack: { referencePanel: "registers", direction: "below" },
  breakpoints: { referencePanel: "stack", direction: "below" },
  log: { referencePanel: "trace" },
  terminal: { referencePanel: "memory" },
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
 * "trace" itself), the bottom-of-Disassembly's-column default (for
 * "run-controls"), or the corresponding `DEFAULT_PANEL_POSITION` entry (for
 * anything else, provided its reference panel is actually present —
 * otherwise dockview has no cell to split relative to, so the panel is
 * added as a new group instead, which is what an absent `position`
 * produces).
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
    return { position: RUN_CONTROLS_DEFAULT_POSITION, initialHeight: RUN_CONTROLS_DOCKED_HEIGHT };
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
// panels it can't shrink and scroll internally. This height (and the other
// *_DEFAULT_HEIGHT/WIDTH constants below) comes from a manually tailored
// arrangement at the default 1600x900 window size (issue #421) rather than
// computed pixel math — confirmed against the live app to show the full
// 16-row page with no clipping. Memory tabs with Terminal in this group, so
// this height also governs Terminal's default size.
const MEMORY_PANEL_DEFAULT_HEIGHT = 366;

// RegisterPanel's two-column layout (header + 3 rows on each side).
const REGISTERS_PANEL_DEFAULT_HEIGHT = 140;

// StackPanel always renders a fixed 8-row page (VISIBLE_PAIRS in
// StackPanel.tsx), so like Memory it can't shrink and scroll internally.
const STACK_PANEL_DEFAULT_HEIGHT = 222;

// BreakpointPanel is meant to stay small (issue #413) — a handful of rows at
// most before it scrolls internally, so it shouldn't compete with Stack or
// Registers for vertical space in their shared column.
const BREAKPOINTS_PANEL_DEFAULT_HEIGHT = 134;

// Trace/Log form a VS Code-style Output/Problems bottom dock, tabbed against
// each other. Given as a plain height (not width): both scroll internally,
// so this just trades off default vertical space against the panels above.
const BOTTOM_GROUP_DEFAULT_HEIGHT = 146;

/**
 * Hardcoded default arrangement (issue #421) mirroring a manually tailored
 * 3-column layout, captured via the existing `layout.json` persistence at
 * the default 1600x900 window size: Memory tabbed with Terminal and Display
 * (memory-mapped display device plan, Work Unit 5) (Watchpoints below),
 * Disassembly (Run Controls below), and Registers/Stack/Breakpoints stacked,
 * with Trace/Log/Symbols (issue #489) tabbed together spanning the bottom.
 * Used on first run and as the fallback whenever a persisted layout (issue
 * #382) is missing or fails to restore.
 *
 * `position: {referencePanel, direction}` splits relative to the *group*
 * containing that panel, not the whole row/column it happens to sit in —
 * so the top-level left/center/right row must be established first (memory,
 * disassembly, registers as horizontal siblings of the root). Only then can
 * watchpoints/stack be added "below" their column's top panel, which nests a
 * new column *inside* that row cell rather than splitting the grid's root.
 * Adding a "below" split before the row exists instead nests the next
 * "right" split inside that same cell, collapsing all three columns' heights
 * down to just the top row. Tabbing has no such ordering constraint, so
 * Memory/Terminal/Display's group is established with Terminal added first
 * (anchor), Display tabbed onto it next, and Memory tabbed onto it last —
 * `addPanel` makes a newly added panel active by default, so this order
 * leaves Memory as the foreground tab, ahead of both Terminal and Display in
 * the tab bar.
 *
 * `terminalDetached`/`displayDetached` skip adding the "terminal"/"display"
 * panel respectively — reachable even on a brand-new profile with no saved
 * dockview arrangement yet, since both detached flags (like the rest of
 * `layout.json`, per #342) aren't profile-scoped: either panel can already be
 * detached from a previous profile when this profile builds its very first
 * default layout. Display (memory-mapped display device plan, Work Unit 5)
 * tabs into the same group as Memory/Terminal, added before Memory in every
 * branch below so Memory — added last — stays the foreground tab, same
 * "addPanel makes a newly added panel active" reasoning as Memory/Terminal's
 * own ordering.
 */
function addDefaultLayout(api: DockviewReadyEvent["api"], terminalDetached: boolean, displayDetached: boolean) {
  const add = (
    id: MainPanelId,
    rest: { position?: AddPanelPositionOptions; initialWidth?: number; initialHeight?: number },
  ) => api.addPanel({ id, component: id, title: PANEL_TITLES[id], ...rest });

  if (!terminalDetached) {
    // 660, not 592: when Display shares this group it needs >=640px (2x its native 320px
    // width) to render at a legible integer scale by default — see DisplayPanel.tsx.
    add("terminal", { initialWidth: displayDetached ? 592 : 660 });
    if (!displayDetached) {
      add("display", { position: { referencePanel: "terminal" } });
    }
    add("memory", { position: { referencePanel: "terminal" } });
  } else if (!displayDetached) {
    add("display", { initialWidth: 660 });
    add("memory", { position: { referencePanel: "display" } });
  } else {
    add("memory", { initialWidth: 592 });
  }
  add("disassembly", { position: { referencePanel: "memory", direction: "right" } });
  add("registers", { position: { referencePanel: "disassembly", direction: "right" }, initialWidth: 202 });
  add("watchpoints", { position: { referencePanel: "memory", direction: "below" } });
  add("stack", { position: { referencePanel: "registers", direction: "below" } });
  add("breakpoints", { position: { referencePanel: "stack", direction: "below" } });

  // Bottom of Disassembly's column (issue #402) — see RUN_CONTROLS_DEFAULT_POSITION.
  // minimumHeight/maximumHeight are equal (issue #424): docked, this panel's
  // height is fixed, not just floored.
  api.addPanel({
    id: "run-controls",
    component: "run-controls",
    title: PANEL_TITLES["run-controls"],
    position: RUN_CONTROLS_DEFAULT_POSITION,
    initialHeight: RUN_CONTROLS_DOCKED_HEIGHT,
    minimumHeight: RUN_CONTROLS_DOCKED_HEIGHT,
    maximumHeight: RUN_CONTROLS_DOCKED_HEIGHT,
    minimumWidth: RUN_CONTROLS_MIN_WIDTH,
  });

  // No referencePanel: an AbsolutePosition split (dockview-core's
  // `orthogonalize`) applies to the grid's root rather than to one panel's
  // own cell, so this spans the full width below the three-column row above
  // — unlike the "below" splits just above, which nest inside their
  // column's own cell precisely because they *do* reference a panel there.
  add("trace", { position: { direction: "below" }, initialHeight: BOTTOM_GROUP_DEFAULT_HEIGHT });
  add("log", { position: { referencePanel: "trace" } });
  add("symbols", { position: { referencePanel: "trace" } });

  // Reserve Memory's full page height directly rather than sizing
  // Watchpoints (dockview gives the sibling whichever space is left over).
  api.getPanel("memory")?.api.setSize({ height: MEMORY_PANEL_DEFAULT_HEIGHT });

  // Registers/Stack/Breakpoints form one flat 3-way vertical split (see the
  // ordering note above). dockview's resizeView sets the target's size
  // exactly, then redistributes the delta across the *other* views in that
  // split — so a setSize call perturbs whatever was set by an earlier call,
  // but leaves nothing after it untouched. Stack must come last: it can't
  // shrink and scroll (fixed 8-row page, like Memory), so its size has to
  // land exactly on target, whereas Registers and Breakpoints both degrade
  // gracefully via their own overflow-y: auto — Stack's setSize redistributes
  // its delta across the other views left in this split, which ends up
  // taking whatever's left over in the column, same role CpuBus used to play
  // here.
  api.getPanel("registers")?.api.setSize({ height: REGISTERS_PANEL_DEFAULT_HEIGHT });
  api.getPanel("breakpoints")?.api.setSize({ height: BREAKPOINTS_PANEL_DEFAULT_HEIGHT });
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
 * Adds Trace, Log, Terminal, and Display if a just-restored layout is missing
 * any of them — i.e. it was persisted before #383/#384 introduced Trace/Log,
 * before #421 moved Terminal's default home to Memory's tab group, or before
 * the memory-mapped display device plan's Work Unit 5 introduced Display.
 * Returns whether anything was added, so the caller knows whether to
 * re-persist.
 *
 * `api.fromJSON` doesn't error just because the saved JSON has fewer panels
 * than `panelComponents` now registers — it happily restores a valid subset
 * — so restoring an old layout otherwise leaves a since-added panel
 * permanently missing rather than falling back to `addDefaultLayout`, which
 * is the only other place that adds them. Any future addition needs the
 * same kind of reconciliation here.
 *
 * `terminalDetached`/`displayDetached` skip adding "terminal"/"display" even
 * when absent — a missing panel is only a stale-layout bug when it isn't
 * currently detached; when it is, `restoreLayout` is the one place that
 * knows to leave it out.
 */
function addMissingBottomPanels(api: DockviewReadyEvent["api"], terminalDetached: boolean, displayDetached: boolean): boolean {
  const hasTrace = api.getPanel("trace") !== undefined;
  const hasLog = api.getPanel("log") !== undefined;
  const hasTerminal = api.getPanel("terminal") !== undefined;
  const hasDisplay = api.getPanel("display") !== undefined;
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
      position: { referencePanel: "memory" },
    });
  }
  if (!hasDisplay && !displayDetached) {
    api.addPanel({
      id: "display",
      component: "display",
      title: PANEL_TITLES.display,
      position: DEFAULT_PANEL_POSITION.display,
    });
  }
  return !hasTrace || !hasLog || (!hasTerminal && !terminalDetached) || (!hasDisplay && !displayDetached);
}

/**
 * Adds the "run-controls" panel (issue #395/#402) if a just-restored layout
 * is missing it — i.e. it was persisted before this panel existed. Same
 * backfill pattern as `addMissingBottomPanels`, reusing
 * `resolveRevealPosition` so a restored old layout's remembered position
 * (docked or floating, if it survived in `panel_positions`) is honored,
 * falling back to `RUN_CONTROLS_DEFAULT_POSITION` otherwise. Returns whether
 * it was added, so the caller knows whether to re-persist.
 */
function addMissingRunControlsPanel(api: DockviewReadyEvent["api"], lastPositions: Partial<Record<MainPanelId, PanelPosition>>): boolean {
  if (api.getPanel("run-controls")) return false;
  const { position, initialHeight, floating } = resolveRevealPosition(api, "run-controls", lastPositions);
  const base = {
    id: "run-controls" as const,
    component: "run-controls",
    title: PANEL_TITLES["run-controls"],
    minimumHeight: RUN_CONTROLS_DOCKED_HEIGHT,
    minimumWidth: RUN_CONTROLS_MIN_WIDTH,
  };
  if (floating) {
    api.addPanel({ ...base, floating });
  } else {
    // maximumHeight only while docked (issue #424) — see RUN_CONTROLS_DOCKED_HEIGHT.
    api.addPanel({ ...base, position, initialHeight, maximumHeight: RUN_CONTROLS_DOCKED_HEIGHT });
  }
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
 * The terminal-detached/display-detached flags returned alongside the
 * dockview arrangement are authoritative over whatever the arrangement
 * itself happens to contain — `detach_terminal`/`reattach_terminal`
 * (`terminal.rs`) and `detach_display`/`reattach_display` (`display.rs`)
 * persist their flag and the arrangement as two separate writes, so a crash
 * between them can leave a restored arrangement with a stale panel despite
 * the flag saying detached (or vice versa isn't possible:
 * `addMissingBottomPanels` already treats "flag false, panel missing" as a
 * reconciliation case). Any such mismatch is corrected here before the panel
 * actions render.
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
  let displayDetached = false;
  try {
    const saved = await invoke<DockLayoutData>("get_dock_layout");
    terminalDetached = saved.terminal_detached;
    displayDetached = saved.display_detached;
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
    addDefaultLayout(api, terminalDetached, displayDetached);
    persistLayout(api, lastPositionsRef.current);
    return;
  }
  if (terminalDetached) {
    api.getPanel("terminal")?.api.close();
  }
  if (displayDetached) {
    api.getPanel("display")?.api.close();
  }
  const addedBottomPanels = addMissingBottomPanels(api, terminalDetached, displayDetached);
  const addedRunControls = addMissingRunControlsPanel(api, lastPositionsRef.current);
  if (addedBottomPanels || addedRunControls) {
    persistLayout(api, lastPositionsRef.current);
  }
}

/**
 * Builds the `rightHeaderActionsComponent` dockview renders once per group
 * — dockview supports only one such component for the whole component, so
 * every panel needing a header action shares this one, branching on
 * `activePanel.id`; groups whose active panel needs no action render
 * nothing.
 *
 * Terminal's Detach button closes over `terminalPositionRef` so it can
 * remember where Terminal was before closing it (see `closeDockedPanel`).
 * Clicking it calls the `detach_terminal` command (shows the detached
 * window, retargets the console bridge, persists the flag — see
 * `terminal.rs`) and only then closes the dock panel, deliberately after the
 * new window/target is fully in place (issue #385's `emit_to`-retarget race
 * mitigation). Terminal also gets a size-preset hamburger icon (issue #462
 * Work Unit 4) alongside Detach — `TerminalPanel.tsx` registers it via
 * `usePanelHeaderAction`/`headerActions` (the generic fallback mechanism
 * below), same as Breakpoints/Watchpoints' single-action panels, but
 * rendered inline here instead of through that fallback branch since
 * Terminal already needs its own early-return for the hardcoded Detach
 * button.
 *
 * Display's Detach button (memory-mapped display device plan, Work Unit 5)
 * mirrors Terminal's exactly, closing over `displayPositionRef` and calling
 * `detach_display` — no size-preset icon, since Display has no equivalent
 * menu yet (design §11 defers dock-cell resize behavior to CSS scaling
 * rather than a user-chosen grid size).
 *
 * Run Controls' Float button (issue #404) is a plain dockview-only
 * operation — `containerApi.addFloatingGroup` moves the panel into a new
 * floating group in place, no Tauri command involved. It's needed as an
 * explicit action because this app's dockview grid fills the entire window
 * with no empty margin to drop a dragged tab into — there's nowhere for
 * dockview's own drag-to-float gesture (which floats on a drop with no
 * valid dock target) to actually resolve to "float" in this layout. Hidden
 * once the panel is already floating, since floating an already-floating
 * panel does nothing useful.
 *
 * Run Controls' Dock button is the reverse, shown only while floating.
 * dockview's public `DockviewApi` has no "move floating panel back into the
 * grid" primitive (`moveGroupOrPanel` exists only on the internal
 * `DockviewComponent`, not on the `api` this component gets), so it closes
 * the floating panel and re-adds it docked at `RUN_CONTROLS_DEFAULT_POSITION`
 * with the same fixed-height/min-width constraints `addDefaultLayout` uses —
 * the same close-then-`addPanel` pattern Terminal's detach/reattach already
 * uses to cross between docked and undocked. There's no remembered
 * pre-float position to restore (unlike Terminal's `terminalPositionRef`):
 * Run Controls always floats to the same fixed `RUN_CONTROLS_FLOAT_BOUNDS`,
 * so docking back to a single fixed position is the symmetric choice. Reuses
 * `codicon-multiple-windows`, the same icon Float and Terminal's Detach
 * button use — Terminal's own reverse operation (reattach) has no dedicated
 * button or icon of its own (native window-close, the "Attach Terminal" menu
 * item, and Ctrl+Shift+T all reach it instead), so there's no separate
 * "dock/attach" icon already established in this codebase to match instead.
 *
 * Any other panel gets a generic fallback: whatever single action it
 * registered via `usePanelHeaderAction` (see `panelHeaderActions.tsx`) is
 * rendered as a button with that action's own `icon` (a "+" via
 * `codicon-add` by default), so panels like Breakpoints, Watchpoints, and
 * Assembler get a tab-header action without hardcoding their ids here.
 * `usePanelHeaderActions()` must run on every render of this component (not
 * just the fallback branch) to satisfy the rules of hooks, since the
 * terminal/run-controls branches above return early.
 */
function makeDockTabActions(
  terminalPositionRef: React.MutableRefObject<DockedPanelPosition | null>,
  displayPositionRef: React.MutableRefObject<DockedPanelPosition | null>,
) {
  return function DockTabActions({ activePanel, containerApi }: IDockviewHeaderActionsProps) {
    const headerActions = usePanelHeaderActions();
    if (activePanel?.id === "terminal") {
      const handleDetach = () => {
        invoke("detach_terminal")
          .then(() => closeDockedPanel(containerApi, "terminal", terminalPositionRef))
          .catch((err) => console.error("detach_terminal failed:", err));
      };
      const sizeAction = headerActions.terminal;
      return (
        <>
          <button className="dock-tab-action" onClick={handleDetach} title="Detach Terminal to its own window">
            <i className="codicon codicon-multiple-windows" />
          </button>
          {sizeAction && (
            <button
              className="dock-tab-action"
              onClick={sizeAction.onClick}
              disabled={sizeAction.disabled}
              title={sizeAction.disabled ? (sizeAction.disabledTitle ?? sizeAction.title) : sizeAction.title}
            >
              <i className="codicon codicon-menu" />
            </button>
          )}
        </>
      );
    }
    if (activePanel?.id === "display") {
      const handleDetach = () => {
        invoke("detach_display")
          .then(() => closeDockedPanel(containerApi, "display", displayPositionRef))
          .catch((err) => console.error("detach_display failed:", err));
      };
      return (
        <button className="dock-tab-action" onClick={handleDetach} title="Detach Display to its own window">
          <i className="codicon codicon-multiple-windows" />
        </button>
      );
    }
    if (activePanel?.id === "run-controls" && activePanel.group.api.location.type !== "floating") {
      const handleFloat = () => containerApi.addFloatingGroup(activePanel, RUN_CONTROLS_FLOAT_BOUNDS);
      return (
        <button className="dock-tab-action" onClick={handleFloat} title="Float Run Controls">
          <i className="codicon codicon-multiple-windows" />
        </button>
      );
    }
    if (activePanel?.id === "run-controls" && activePanel.group.api.location.type === "floating") {
      const handleDock = () => {
        activePanel.api.close();
        const panel = containerApi.addPanel({
          id: "run-controls",
          component: "run-controls",
          title: PANEL_TITLES["run-controls"],
          position: RUN_CONTROLS_DEFAULT_POSITION,
          initialHeight: RUN_CONTROLS_DOCKED_HEIGHT,
          minimumHeight: RUN_CONTROLS_DOCKED_HEIGHT,
          maximumHeight: RUN_CONTROLS_DOCKED_HEIGHT,
          minimumWidth: RUN_CONTROLS_MIN_WIDTH,
        });
        // initialHeight alone leaves the panel at its prior floating size
        // until the user drags a sash — same as Registers/Breakpoints/Stack
        // below in addDefaultLayout, an explicit setSize is what actually
        // forces dockview's layout engine to apply the height now.
        panel.api.setSize({ height: RUN_CONTROLS_DOCKED_HEIGHT });
      };
      return (
        <button className="dock-tab-action" onClick={handleDock} title="Dock Run Controls">
          <i className="codicon codicon-multiple-windows" />
        </button>
      );
    }
    const action = activePanel ? headerActions[activePanel.id as MainPanelId] : undefined;
    if (action) {
      return (
        <button
          className="dock-tab-action"
          onClick={action.onClick}
          disabled={action.disabled}
          title={action.disabled ? (action.disabledTitle ?? action.title) : action.title}
        >
          <i className={`codicon codicon-${action.icon ?? "add"}`} />
        </button>
      );
    }
    return null;
  };
}

/** Hosts the main window's dockview panels (Register/Disassembly/Memory/Stack/Watchpoint/Trace/Log/Terminal) in a dockview grid. */
export default function DockLayout() {
  const { resolvedTheme } = useTheme();
  const layoutChangeSubscriptionRef = useRef<DockviewIDisposable | null>(null);
  const persistTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const apiRef = useRef<DockviewReadyEvent["api"] | null>(null);
  const terminalPositionRef = useRef<DockedPanelPosition | null>(null);
  const displayPositionRef = useRef<DockedPanelPosition | null>(null);
  const lastPanelPositionRef = useRef<Partial<Record<MainPanelId, PanelPosition>>>({});
  const DockTabActions = useMemo(() => makeDockTabActions(terminalPositionRef, displayPositionRef), []);

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
      // Run Controls needs its own size constraints regardless of which
      // branch below re-adds it — fixed height while docked (issue #424),
      // see RUN_CONTROLS_DOCKED_HEIGHT/RUN_CONTROLS_MIN_WIDTH.
      if (floating) {
        const constraints = id === "run-controls"
          ? { minimumHeight: RUN_CONTROLS_DOCKED_HEIGHT, minimumWidth: RUN_CONTROLS_MIN_WIDTH }
          : undefined;
        api.addPanel({ id, component: id, title: PANEL_TITLES[id], floating, ...constraints });
      } else {
        const constraints = id === "run-controls"
          ? { minimumHeight: RUN_CONTROLS_DOCKED_HEIGHT, maximumHeight: RUN_CONTROLS_DOCKED_HEIGHT, minimumWidth: RUN_CONTROLS_MIN_WIDTH }
          : undefined;
        api.addPanel({ id, component: id, title: PANEL_TITLES[id], position, initialHeight, ...constraints });
      }
    });
    return () => { unlistenPromise.then((f) => f()); };
  }, []);

  // Bridges `assembler-menu-action` to `AssemblerPanel.tsx` via
  // `assemblerMenuActions.ts` rather than letting the panel `listen()` for
  // it directly — see that module's doc comment for the full race it closes.
  // In short: Assembler is the only panel with its own action-dispatching
  // menu that isn't part of `addDefaultLayout` below, so its dock tab (and
  // thus its own `listen()` call) doesn't exist yet the *first* time a user
  // clicks one of its menu items — racing against the `assembler-menu-action`
  // event that exact same click already triggered. This effect, like the
  // `reveal-panel` one above, is mounted once here, well before any menu
  // click is possible, so it can never lose that race; it just forwards
  // (or queues) the action for whenever the panel is actually ready.
  useEffect(() => {
    const unlistenPromise = listen<string>("assembler-menu-action", (event) => {
      dispatchAssemblerMenuAction(event.payload);
    });
    return () => { unlistenPromise.then((f) => f()); };
  }, []);

  // Window > Restore Layout… (issue #398), confirmed via `RestoreLayoutDialog`
  // and actually triggered by the `restore_dock_layout` command: discards
  // every panel's current position/size and rebuilds the same default
  // arrangement `restoreLayout` falls back to on first run, then re-persists
  // it — the actual `layout.json` overwrite that makes the reset stick.
  // `terminalDetached`/`displayDetached` are always false here —
  // `restore_dock_layout` reattaches Terminal and/or Display first (emitting
  // their own `terminal-reattached`/`display-reattached`) if either was
  // detached, since the default layout always docks both.
  useEffect(() => {
    const unlistenPromise = listen("dock-layout-reset", () => {
      const api = apiRef.current;
      if (!api) return;
      api.clear();
      lastPanelPositionRef.current = {};
      terminalPositionRef.current = null;
      displayPositionRef.current = null;
      addDefaultLayout(api, false, false);
      persistLayout(api, lastPanelPositionRef.current);
    });
    return () => { unlistenPromise.then((f) => f()); };
  }, []);

  // Rust-driven detach/reattach (the Window > Terminal/Display menu items,
  // and each detached window's native close button) has no JS handler of its
  // own already in place to add/remove the dock panel — the dock tab's own
  // Detach button (`DockTabActions` above) does that inline since it's
  // already running in this component, but the menu/close paths instead emit
  // these events for the same effect.
  useEffect(() => {
    const unlistenPromise = listen("terminal-detach-requested", () => {
      closeDockedPanel(apiRef.current, "terminal", terminalPositionRef);
    });
    return () => { unlistenPromise.then((f) => f()); };
  }, []);

  useEffect(() => {
    const unlistenPromise = listen("display-detach-requested", () => {
      closeDockedPanel(apiRef.current, "display", displayPositionRef);
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
      const position = positionForReattach(api, terminalPositionRef.current, { referencePanel: "memory" });
      api.addPanel({ id: "terminal", component: "terminal", title: PANEL_TITLES.terminal, position });
    });
    return () => { unlistenPromise.then((f) => f()); };
  }, []);

  // Same as the Terminal reattach effect above, for Display — falls back to
  // its default position tabbed with Memory/Terminal
  // (`DEFAULT_PANEL_POSITION.display`) when there's no resolvable remembered
  // position.
  useEffect(() => {
    const unlistenPromise = listen("display-reattached", () => {
      const api = apiRef.current;
      if (!api || api.getPanel("display")) return;
      const position = positionForReattach(api, displayPositionRef.current, DEFAULT_PANEL_POSITION.display!);
      api.addPanel({ id: "display", component: "display", title: PANEL_TITLES.display, position });
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
    <PanelHeaderActionProvider>
      <div className="dock-layout">
        <DockviewReact
          components={panelComponents}
          rightHeaderActionsComponent={DockTabActions}
          onReady={onReady}
          theme={{ ...EMMA65_DOCK_THEME_BASE, colorScheme: resolvedTheme }}
        />
      </div>
    </PanelHeaderActionProvider>
  );
}
