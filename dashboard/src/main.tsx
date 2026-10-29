import React from "react";
import { createRoot } from "react-dom/client";
import Authentication from "./Auth";
import "./styles.css";
import "./Platform.css";
createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <Authentication />
  </React.StrictMode>,
);
