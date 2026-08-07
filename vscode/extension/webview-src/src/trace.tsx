import React from "react";
import ReactDOM from "react-dom/client";
import TracePanel from "./TracePanel";
import "@vscode/codicons/dist/codicon.css";

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <TracePanel />
  </React.StrictMode>
);
