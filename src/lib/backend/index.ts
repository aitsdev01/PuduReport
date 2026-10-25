// SPDX-License-Identifier: GPL-3.0-only
// Copyright (c) 2026 bc0d3

// Registro de la capa de datos activa. El punto de entrada (main.tsx o
// web/main.tsx) la fija antes de montar la interfaz.
import type { Backend } from "./types";

export type { Backend, Capabilities, PackageExport } from "./types";

let active: Backend | null = null;

export function setBackend(backend: Backend): void {
  active = backend;
}

export function getBackend(): Backend {
  if (!active) {
    throw new Error("La capa de datos no se inicializo");
  }
  return active;
}
