import React from "react";
import ReactDOM from "react-dom/client";
import TerminalPanel from "./TerminalPanel";
import { ThemeProvider } from "./ThemeContext";
import { useAppKeyBindings } from "./useAppKeyBindings";
import "./styles/global.scss";

/**
 * Root of the detached Terminal's own window/document (issue #385) — wraps
 * the shared `TerminalPanel` (from #384) in its own `ThemeProvider`, since
 * React context can't cross a window boundary, mirroring how `main.tsx`
 * wraps `App`. Installs the app-wide key bindings directly, since this
 * window has no `App.tsx` root to install them for it.
 */
function TerminalDetachedWindow() {
  useAppKeyBindings();
  return <TerminalPanel />;
}

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <ThemeProvider>
      <TerminalDetachedWindow />
    </ThemeProvider>
  </React.StrictMode>,
);
