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

  const detach = useCallback(() => {
    invoke("detach_stack_panel").catch((e) => console.error("[spike] detach_stack_panel failed:", e));
  }, []);

  const reattach = useCallback(() => {
    invoke("reattach_stack_panel").catch((e) => console.error("[spike] reattach_stack_panel failed:", e));
  }, []);

  return (
    <div style={{ display: "flex", flexDirection: "column", height: "100%", width: "100%" }}>
      <SpikeThemeSync onChange={setResolvedTheme} />
      <div style={{ display: "flex", gap: 8, padding: 8, flex: "0 0 auto" }}>
        <button onClick={detach}>Detach Stack (Q4/Q5)</button>
        <button onClick={reattach}>Reattach Stack</button>
        <button onClick={resetLayout}>Reset Layout</button>
        <span style={{ opacity: 0.7, fontSize: "0.8rem", alignSelf: "center" }}>
          Rearrange/resize panels, then restart the app to check layout persistence (Q6).
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
