import React from "react";
import ReactDOM from "react-dom/client";
import DockviewSpike from "./DockviewSpike";
import "./styles/global.scss";

// Phase 0 spike only (issue #379) — throwaway entry point, removed with the
// rest of the spike code once the write-up lands.
ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <DockviewSpike />
  </React.StrictMode>
);
