// SPDX-License-Identifier: GPL-3.0-only
// Copyright (c) 2026 bc0d3

/// <reference types="vite/client" />

interface ImportMetaEnv {
  /** Prefijo de la API de informes del editor web (por defecto /api/admin/informes). */
  readonly VITE_PUDU_API_BASE?: string;
}

interface ImportMeta {
  readonly env: ImportMetaEnv;
}
