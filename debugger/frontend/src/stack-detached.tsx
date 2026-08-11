import React from "react";
import ReactDOM from "react-dom/client";
import SpikeStackPanel from "./SpikeStackPanel";
import "./styles/global.scss";

// Phase 0 spike only (issue #379) — throwaway entry point mounting the same
// SpikeStackPanel used inside the dockview shell, proving out the detach
// mechanism (spike question 4). Removed with the rest of the spike code once
// the write-up lands.
ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <SpikeStackPanel />
  </React.StrictMode>
);
