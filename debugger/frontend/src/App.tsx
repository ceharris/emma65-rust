import { lazy, Suspense, useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import ClearRecentDialog from "./ClearRecentDialog";
import ExitConfirmDialog from "./ExitConfirmDialog";
import { ExecutionProvider } from "./ExecutionContext";
import NewProfileDialog from "./NewProfileDialog";
import RestoreLayoutDialog from "./RestoreLayoutDialog";
import { RunControlsProvider } from "./RunControlsContext";
import StatusBar from "./StatusBar";
import ThemeSelector from "./ThemeSelector";
import { useAppKeyBindings } from "./useAppKeyBindings";

// Loaded as a dynamic import (rather than a static one) so the splash screen
// below doesn't have to wait on DockLayout's module graph — dockview-react,
// xterm, and every dock panel — to be fetched and evaluated before it can
// paint (issue #405). `loadDockLayout` is called eagerly on mount (not just
// from `lazy()`'s own trigger) so the chunk starts loading in parallel with
// the backend's own session startup instead of only starting once the
// session is already ready.
const loadDockLayout = () => import("./layout/DockLayout");
const DockLayout = lazy(loadDockLayout);

interface SessionStatus {
  message: string;
  ok: boolean;
}

export default function App() {
  const [status, setStatus] = useState<SessionStatus | null>(null);

  useAppKeyBindings();

  // Ctrl+O/Ctrl+Q: open an existing profile / quit the app. Handled here (not
  // in the shared APP_KEY_BINDINGS array) since App.tsx only mounts in the
  // main window and both actions — like New Profile's Ctrl+N — are
  // main-window-only (issue #351: Ctrl+Q previously fired from every window,
  // including as a surprise exit while typing in the Terminal window, where
  // Ctrl+Q is conventionally XON).
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (!(e.ctrlKey || e.metaKey)) return;
      if (e.key === "o") {
        e.preventDefault();
        invoke("open_profile").catch((err) => console.error("open_profile failed:", err));
      } else if (e.key === "q") {
        e.preventDefault();
        invoke("quit").catch((err) => console.error("quit failed:", err));
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, []);

  useEffect(() => {
    const unlistenPromise = listen<SessionStatus>("session-status", (event) => {
      setStatus(event.payload);
    });

    invoke<SessionStatus | null>("get_session_status").then((current) => {
      if (current !== null) {
        setStatus(current);
      }
    });

    return () => { unlistenPromise.then((f) => f()); };
  }, []);

  // Start fetching DockLayout's chunk as soon as the splash screen is up,
  // in parallel with the backend loading the emulator session, so it's
  // already in the module cache by the time `status.ok` flips true and
  // `lazy()` needs it — see the `loadDockLayout` comment above.
  useEffect(() => {
    loadDockLayout();
  }, []);

  if (status === null || !status.ok) {
    return (
      <>
        <NewProfileDialog />
        <ClearRecentDialog />
        <RestoreLayoutDialog />
        <ExitConfirmDialog />
        <div className="app-splash">
          {status === null ? (
            <span className="status-pending">Initializing…</span>
          ) : (
            <span className="status-error">{status.message}</span>
          )}
        </div>
      </>
    );
  }

  return (
    <>
      <NewProfileDialog />
      <ClearRecentDialog />
      <RestoreLayoutDialog />
      <ExitConfirmDialog />
      <div className="app-shell">
        <header className="app-toolbar">
          <ThemeSelector />
        </header>
        <ExecutionProvider>
          <RunControlsProvider>
            <Suspense fallback={<div className="app-splash"><span className="status-pending">Initializing…</span></div>}>
              <DockLayout />
            </Suspense>
            <StatusBar />
          </RunControlsProvider>
        </ExecutionProvider>
      </div>
    </>
  );
}
