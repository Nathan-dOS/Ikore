import React from "react";
import ReactDOM from "react-dom/client";
import { App } from "./App";
import { startApplication } from "./app/applicationController";
import "./styles.css";

const stopApplication = startApplication();
window.addEventListener("beforeunload", stopApplication, { once: true });

if (import.meta.hot) {
  import.meta.hot.dispose(stopApplication);
}

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>
);
