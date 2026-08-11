import { useCallback, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { DockviewReact, DockviewReadyEvent, DockviewTheme, IDockviewPanelProps } from "dockview-react";
import "dockview-react/dist/styles/dockview.css";
import "./styles/spike.scss";
import SpikeStackPanel from "./SpikeStackPanel";
import SpikeMountLog from "./SpikeMountLog";
import SpikeThemeSync from "./SpikeThemeSync";
import SpikeXtermPanel from "./SpikeXtermPanel";

/**
 * Phase 0 spike only (issue #379). Base theme object proving out spike
 * question 1 — see `styles/spike.scss` for the `--dv-*` mapping onto the
 * app's existing `--color-*` palette. `colorScheme` is overridden per-render
 * with the debugger's actual resolved theme (see `SpikeThemeSync`).
 */
const EMMA65_SPIKE_THEME_BASE: Omit<DockviewTheme, "colorScheme"> = {
  name: "emma65Spike",
  className: "dockview-theme-emma65",
};

const StackTab: React.FC<IDockviewPanelProps> = () => (
  <>
    <SpikeMountLog label="stack" />
    <SpikeStackPanel />
  </>
);

const OtherTab: React.FC<IDockviewPanelProps> = () => (
  <>
    <SpikeMountLog label="other" />
    <div style={{ padding: 8 }}>
      Switch to this tab and back to Stack/Terminal, then check the devtools
      console for mount/unmount logs (spike question 2).
    </div>
  </>
);

const TerminalTab: React.FC<IDockviewPanelProps> = () => (
  <>
    <SpikeMountLog label="terminal" />
    <SpikeXtermPanel />
  </>
);

const components = {
  stack: StackTab,
  other: OtherTab,
  terminal: TerminalTab,
};

const DEFAULT_LAYOUT_PANEL_IDS = ["stack", "other", "terminal"];

/**
 * Adds the default set of panels to an empty (or just-cleared) dockview.
 *
 * Finding (spike question 6): dockview's default tabs are closable via the
 * built-in "x", and closing one removes it from the layout entirely — there
 * is no built-in "reopen a closed panel" affordance, and the empty result
 * persists just like any other layout change. A real Phase 3/4 integration
 * needs its own restore mechanism (e.g. a Window-menu "Reveal <panel>"
 * action per closed panel, or disabling the close button on panels that
 * must always exist) — dockview does not provide one out of the box.
 */
function addDefaultPanels(api: DockviewReadyEvent["api"]) {
  for (const id of DEFAULT_LAYOUT_PANEL_IDS) {
    api.addPanel({ id, component: id, title: id });
  }
}

export default function DockviewSpike() {
  const apiRef = useRef<DockviewReadyEvent["api"] | null>(null);
  const [resolvedTheme, setResolvedTheme] = useState<"dark" | "light">("dark");

  const onReady = useCallback((event: DockviewReadyEvent) => {
    apiRef.current = event.api;

    // Spike question 6: round-trip the layout through the Tauri command
    // boundary, falling back to a hardcoded default on a missing/corrupt blob.
    invoke<unknown>("get_spike_layout").then((saved) => {
      if (saved) {
        try {
          event.api.fromJSON(saved as Parameters<typeof event.api.fromJSON>[0]);
          return;
        } catch (e) {
          console.error("[spike] fromJSON failed, falling back to default layout:", e);
        }
      }
      addDefaultPanels(event.api);
    });

    event.api.onDidLayoutChange(() => {
      invoke("set_spike_layout", { layout: event.api.toJSON() }).catch((e) =>
        console.error("[spike] set_spike_layout failed:", e)
      );
    });
  }, []);

  const resetLayout = useCallback(() => {
    const api = apiRef.current;
    if (!api) return;
    api.clear();
    addDefaultPanels(api);
  }, []);

  // Order matters for spike question 5 (no lost `spike-tick` events): the
  // new window must be fully built — its `SpikeTicker` listener registered
  // — before the docked panel that's been receiving ticks is torn down.
  // `detach_stack_panel`'s `await` only guarantees the window exists, not
  // that its JS has run yet; same handshake-timing risk the plan flags for
  // Phase 6's real `terminal_ready`-style concern, just not resolved here.
  const detach = useCallback((command: string) => {
    invoke(command)
      .then(() => {
        const panel = apiRef.current?.getPanel("stack");
        if (panel) apiRef.current?.removePanel(panel);
      })
      .catch((e) => console.error(`[spike] ${command} failed:`, e));
  }, []);

  // Reverse order from detach: re-add the docked panel (and its ticker
  // listener) before the backend destroys/hides the detached window and
  // flips the `emit_to` target back, for the same reason.
  const reattach = useCallback((command: string) => {
    if (!apiRef.current?.getPanel("stack")) {
      apiRef.current?.addPanel({ id: "stack", component: "stack", title: "stack" });
    }
    invoke(command).catch((e) => console.error(`[spike] ${command} failed:`, e));
  }, []);

  return (
    <div style={{ display: "flex", flexDirection: "column", height: "100%", width: "100%" }}>
      <SpikeThemeSync onChange={setResolvedTheme} />
      <div style={{ display: "flex", gap: 8, padding: 8, flex: "0 0 auto", flexWrap: "wrap" }}>
        <button onClick={() => detach("detach_stack_panel")}>Detach Stack (dynamic window)</button>
        <button onClick={() => reattach("reattach_stack_panel")}>Reattach Stack (dynamic window)</button>
        <span style={{ opacity: 0.5 }}>|</span>
        <button onClick={() => detach("detach_stack_panel_static")}>Detach Stack (static A/B)</button>
        <button onClick={() => reattach("reattach_stack_panel_static")}>Reattach Stack (static A/B)</button>
        <span style={{ opacity: 0.5 }}>|</span>
        <button onClick={resetLayout}>Reset Layout</button>
        <span style={{ opacity: 0.7, fontSize: "0.8rem", alignSelf: "center" }}>
          Rearrange/resize panels, then restart the app to check layout persistence (Q6). The
          "static A/B" pair tests whether the app-wide freeze on detach is specific to dynamic
          window creation (WebviewWindowBuilder) vs. a pre-declared hidden window shown/hidden
          like Terminal/Trace/Log.
        </span>
      </div>
      <div style={{ flex: "1 1 auto", minHeight: 0 }}>
        <DockviewReact
          components={components}
          onReady={onReady}
          theme={{ ...EMMA65_SPIKE_THEME_BASE, colorScheme: resolvedTheme }}
        />
      </div>
    </div>
  );
}
