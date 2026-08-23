import React from "react";
import ReactDOM from "react-dom/client";
import DisplayPanel from "./DisplayPanel";
import { ThemeProvider } from "./ThemeContext";
import { useAppKeyBindings } from "./useAppKeyBindings";
import "./styles/global.scss";

/**
 * Root of the detached Display's own window/document (memory-mapped display device plan, Work
 * Unit 5) — wraps the shared `DisplayPanel` in its own `ThemeProvider`, since React context can't
 * cross a window boundary, mirroring `terminal-detached.tsx` exactly. Installs the app-wide key
 * bindings directly, since this window has no `App.tsx` root to install them for it.
 */
function DisplayDetachedWindow() {
  useAppKeyBindings();
  return <DisplayPanel />;
}

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <ThemeProvider>
      <DisplayDetachedWindow />
    </ThemeProvider>
  </React.StrictMode>,
);
