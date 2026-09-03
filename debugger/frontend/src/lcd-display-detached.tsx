import React from "react";
import ReactDOM from "react-dom/client";
import LcdDisplayPanel from "./LcdDisplayPanel";
import { ThemeProvider } from "./ThemeContext";
import { useAppKeyBindings } from "./useAppKeyBindings";
import "./styles/global.scss";

/**
 * Root of the detached LCD Display's own window/document (memory-mapped LCD display device plan,
 * Work Unit 5) -- wraps the shared `LcdDisplayPanel` in its own `ThemeProvider`, since React
 * context can't cross a window boundary, mirroring `led-matrix-detached.tsx` exactly. Installs the
 * app-wide key bindings directly, since this window has no `App.tsx` root to install them for it.
 */
function LcdDisplayDetachedWindow() {
  useAppKeyBindings();
  return <LcdDisplayPanel />;
}

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <ThemeProvider>
      <LcdDisplayDetachedWindow />
    </ThemeProvider>
  </React.StrictMode>,
);
