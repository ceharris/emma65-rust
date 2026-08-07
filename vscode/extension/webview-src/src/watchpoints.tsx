import React from "react";
import ReactDOM from "react-dom/client";
import WatchpointPanel from "./WatchpointPanel";
import "./styles/global.scss";
import "@vscode/codicons/dist/codicon.css";

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <WatchpointPanel />
  </React.StrictMode>
);
