// SPDX-License-Identifier: GPL-3.0-only
// Copyright (c) 2026 bc0d3

// Configuracion en tiempo de ejecucion del editor web. El informe llega por
// la URL (?informe=<uuid>) y se valida antes de usarlo en cualquier ruta. El
// prefijo de la API se fija al compilar (VITE_PUDU_API_BASE): nunca se toma de
// la URL, para que un enlace no pueda apuntar el editor a otra API.
//
// Sin imports: se prueba aislado con node --test.

export const DEFAULT_API_BASE = "/api/admin/informes";

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const API_BASE = /^\/(?!\/)[A-Za-z0-9/_-]*$/;

export interface WebConfig {
  apiBase: string;
  informeId: string;
}

export type WebConfigResult = { ok: true; config: WebConfig } | { ok: false; error: string };

export function isUuid(value: string): boolean {
  return UUID.test(value);
}

/** Prefijo de API valido: ruta absoluta del mismo origen, sin "//" ni "..". */
export function isApiBase(value: string): boolean {
  return API_BASE.test(value) && !value.includes("//");
}

export function parseWebConfig(search: string, apiBase: string | undefined): WebConfigResult {
  const base = (apiBase ?? "").trim() || DEFAULT_API_BASE;
  if (!isApiBase(base)) {
    return { ok: false, error: "El prefijo de la API del editor no es valido." };
  }
  const params = new URLSearchParams(search);
  const values = params.getAll("informe");
  if (values.length !== 1) {
    return { ok: false, error: "Falta el informe a editar (parametro informe)." };
  }
  const informeId = values[0].trim().toLowerCase();
  if (!isUuid(informeId)) {
    return { ok: false, error: "El identificador del informe no es valido." };
  }
  return { ok: true, config: { apiBase: base.replace(/\/+$/, ""), informeId } };
}
