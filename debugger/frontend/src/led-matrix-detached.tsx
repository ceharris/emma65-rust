import React from "react";
import ReactDOM from "react-dom/client";
import LedMatrixPanel from "./LedMatrixPanel";
import { ThemeProvider } from "./ThemeContext";
import { useAppKeyBindings } from "./useAppKeyBindings";
import "./styles/global.scss";

/**
 * Root of the detached LED Matrix's own window/document (memory-mapped LED matrix device plan,
 * Work Unit 5) — wraps the shared `LedMatrixPanel` in its own `ThemeProvider`, since React
 * context can't cross a window boundary, mirroring `display-detached.tsx` exactly. Installs the
 * app-wide key bindings directly, since this window has no `App.tsx` root to install them for it.
 */
function LedMatrixDetachedWindow() {
  useAppKeyBindings();
  return <LedMatrixPanel />;
}

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <ThemeProvider>
      <LedMatrixDetachedWindow />
    </ThemeProvider>
  </React.StrictMode>,
);
