import React from "react";
import ReactDOM from "react-dom/client";
import LogPanel from "./LogPanel";
import "./styles/global.scss";
import "@vscode/codicons/dist/codicon.css";

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <LogPanel />
  </React.StrictMode>
);
