// SPDX-License-Identifier: GPL-3.0-only
// Copyright (c) 2026 bc0d3

// Punto de entrada del editor web (npm run build:web). No importa nada de
// Tauri: la capa de datos es la API de informes de PuduHub.
import React from "react";
import ReactDOM from "react-dom/client";
import { setBackend } from "../lib/backend";
import { HttpBackend } from "../lib/backend/http";
import { flushWrites } from "../lib/pendingWrites";
import { parseWebConfig } from "./config";
import { WebApp } from "./WebApp";

// Fuentes e iconos empaquetados (sin CDN), igual que en escritorio.
import "@fontsource/inter/400.css";
import "@fontsource/inter/500.css";
import "@fontsource/inter/600.css";
import "@fontsource/jetbrains-mono/400.css";
import "@tabler/icons-webfont/dist/tabler-icons.min.css";

import "../styles.css";
import "../styles/workbench.css";
import "./web.css";

const root = document.getElementById("root");
if (!root) {
  throw new Error("no se encontro el elemento #root");
}

const parsed = parseWebConfig(window.location.search, import.meta.env.VITE_PUDU_API_BASE);

if (!parsed.ok) {
  ReactDOM.createRoot(root).render(
    <div className="shell-state" role="alert">
      <i className="ti ti-alert-circle" aria-hidden="true" />
      <h2>No se pudo abrir el editor</h2>
      <p>{parsed.error}</p>
    </div>,
  );
} else {
  const backend = new HttpBackend({
    apiBase: parsed.config.apiBase,
    informeId: parsed.config.informeId,
    // Antes de pedir un PDF se guardan los borradores pendientes.
    beforePdf: flushWrites,
  });
  setBackend(backend);
  ReactDOM.createRoot(root).render(
    <React.StrictMode>
      <WebApp backend={backend} />
    </React.StrictMode>,
  );
}
