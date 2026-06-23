import React from "react";
import ReactDOM from "react-dom/client";
import TerminalWindow from "./TerminalWindow";
import "./styles/global.scss";

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <TerminalWindow />
  </React.StrictMode>
);
