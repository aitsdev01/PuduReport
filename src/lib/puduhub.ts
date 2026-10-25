// SPDX-License-Identifier: GPL-3.0-only
// Copyright (c) 2026 bc0d3

// Sincronizacion de escritorio con PuduHub: tipos de los comandos de Tauri y
// helpers puros (etiquetas de conflictos, resumenes). Sin imports en tiempo de
// ejecucion: se prueba aislado con node --test. El token nunca pasa por aca:
// lo guarda y lo usa solo el backend Rust.

/** Conexion configurada en el workspace (sin el token). */
export interface PuduHubConnectionView {
  servidor: string | null;
  tieneToken: boolean;
}

export interface PuduHubTestResult {
  servidor: string;
  total: number;
}

/** Quien tiene el bloqueo de edicion (validado en Rust). */
export interface PuduHubLock {
  nombre: string | null;
  /** ISO 8601. */
  hasta: string;
  origen: "web" | "escritorio" | "";
  propio: boolean;
}

export interface PuduHubLocalCopy {
  proyectoId: string;
  baseVersion: number;
  cambiosSinEnviar: boolean;
}

export interface PuduHubInforme {
  id: string;
  titulo: string;
  tipo: string;
  estado: "borrador" | "final";
  versionActual: number;
  bloqueo: PuduHubLock | null;
  actualizadoAt: string | null;
  local: PuduHubLocalCopy | null;
}

export interface PuduHubListResult {
  items: PuduHubInforme[];
  total: number;
  page: number;
  pageSize: number;
  omitidos: number;
}

export interface PuduHubOpenResult {
  proyectoId: string;
  nombre: string;
  version: number;
  actualizado: boolean;
  /** Por ejemplo, cambios de la web que no venian en la version descargada. */
  aviso: string | null;
}

export interface PuduHubLinkStatus {
  vinculado: boolean;
  servidor: string | null;
  informeId: string | null;
  baseVersion: number | null;
  sincronizadoAt: string | null;
  cambiosSinEnviar: boolean;
  mismoServidor: boolean;
  vinculoInvalido: boolean;
}

/** Lo que llego de PuduHub al aplicar la fusion. */
export interface PuduHubChanges {
  hallazgosNuevos: string[];
  hallazgosActualizados: string[];
  hallazgosEliminados: string[];
  secciones: string[];
  campos: string[];
  evidencias: number;
  orden: boolean;
}

export interface PuduHubSyncOutcome {
  informeId: string;
  proyectoId: string;
  version: number;
  aplicado: boolean;
  cambios: PuduHubChanges;
  omitidos: string[];
  aviso: string | null;
}

export interface PuduHubDownloadResult {
  proyectoId: string;
  respaldoId: string | null;
  version: number;
  aviso: string | null;
}

export type PuduHubEntity = "proyecto" | "seccion" | "hallazgo" | "orden" | "asset";

export interface PuduHubConflict {
  entidad: PuduHubEntity;
  id?: string;
  campo?: string;
}

/** Titulos locales para nombrar los conflictos. */
export interface PuduHubConflictContext {
  hallazgos: Record<string, string>;
  secciones: Record<string, string>;
}

/** Error de un comando de PuduHub (codigo estable + mensaje propio). */
export interface PuduHubError {
  codigo: string;
  mensaje: string;
  bloqueo?: PuduHubLock;
  conflictos?: PuduHubConflict[];
  contexto?: PuduHubConflictContext;
  reintentarEn?: number;
  proyectoId?: string;
  puedeForzar?: boolean;
}

/** Operaciones de sincronizacion (solo escritorio). */
export interface PuduHubApi {
  getConnection(): Promise<PuduHubConnectionView>;
  saveConnection(servidor: string, token: string): Promise<PuduHubConnectionView>;
  deleteConnection(): Promise<void>;
  testConnection(): Promise<PuduHubTestResult>;
  listInformes(page: number, query: string): Promise<PuduHubListResult>;
  openInforme(informeId: string): Promise<PuduHubOpenResult>;
  linkStatus(projectId: string): Promise<PuduHubLinkStatus>;
  sendProject(projectId: string, forzar: boolean): Promise<PuduHubSyncOutcome>;
  downloadServerVersion(projectId: string): Promise<PuduHubDownloadResult>;
  uploadNew(projectId: string): Promise<PuduHubSyncOutcome>;
}

/** Normaliza lo que rechaza un comando (objeto de Rust o texto) a un error. */
export function toPuduHubError(error: unknown): PuduHubError {
  if (typeof error === "object" && error !== null) {
    const value = error as Record<string, unknown>;
    if (typeof value.codigo === "string" && typeof value.mensaje === "string") {
      return value as unknown as PuduHubError;
    }
  }
  return {
    codigo: "local",
    mensaje: typeof error === "string" && error ? error : "Ocurrio un error inesperado.",
  };
}

/** Etiquetas de los campos de project.yaml que pueden entrar en conflicto. */
export const PROJECT_FIELD_LABELS: Record<string, string> = {
  name: "Nombre del proyecto",
  client: "Cliente",
  gerencia: "Gerencia",
  area: "Area",
  ambiente: "Ambiente",
  exposicion: "Exposicion",
  project_type: "Tipo de proyecto",
  osid: "Identificador OSID",
  template_override: "Plantilla del PDF",
  start_date: "Fecha de inicio",
  end_date: "Fecha de termino",
  scope: "Alcance (objetivos)",
  team: "Equipo",
  project_status: "Estado del proyecto",
  assignment_history: "Historial de asignacion",
  layout: "Estructura del reporte",
  causas_raiz: "Causas raiz",
  plan_remediacion: "Plan de remediacion",
  orden_secciones: "Orden de las secciones",
};

export interface ConflictLabel {
  /** Tipo de entidad legible ("Hallazgo", "Seccion", ...). */
  tipo: string;
  /** Nombre legible (titulo local, etiqueta del campo o id). */
  nombre: string;
}

/** Nombre legible de una entidad en conflicto, con los titulos locales. */
export function conflictLabel(
  conflict: PuduHubConflict,
  context?: PuduHubConflictContext,
): ConflictLabel {
  const id = conflict.id ?? "";
  switch (conflict.entidad) {
    case "hallazgo": {
      const title = context?.hallazgos[id]?.trim();
      return { tipo: "Hallazgo", nombre: title || id || "Hallazgo sin identificar" };
    }
    case "seccion": {
      const title = context?.secciones[id]?.trim();
      return { tipo: "Seccion", nombre: title || id || "Seccion sin identificar" };
    }
    case "proyecto": {
      const field = conflict.campo ?? "";
      return {
        tipo: "Proyecto",
        nombre: PROJECT_FIELD_LABELS[field] ?? (field || "Datos del proyecto"),
      };
    }
    case "orden":
      return { tipo: "Orden", nombre: "Orden de los hallazgos" };
    case "asset":
      return {
        tipo: "Evidencia",
        nombre: id.replace(/^assets\//, "") || "Evidencia sin identificar",
      };
    default:
      return { tipo: "Otro", nombre: id || "Elemento sin identificar" };
  }
}

/** Etiquetas de todos los conflictos de un error, sin repetir. */
export function conflictLabels(error: PuduHubError): ConflictLabel[] {
  const seen = new Set<string>();
  const out: ConflictLabel[] = [];
  for (const conflict of error.conflictos ?? []) {
    const label = conflictLabel(conflict, error.contexto);
    const key = `${label.tipo}\u0000${label.nombre}`;
    if (!seen.has(key)) {
      seen.add(key);
      out.push(label);
    }
  }
  return out;
}

/** Fecha y hora local corta de un ISO 8601 ("" si no es valido). */
export function formatTimestamp(iso: string | null | undefined): string {
  if (!iso) return "";
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return "";
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${pad(date.getDate())}-${pad(date.getMonth() + 1)}-${date.getFullYear()} ${pad(
    date.getHours(),
  )}:${pad(date.getMinutes())}`;
}

/** "Ana Perez (web) hasta 25-10-2026 12:15" para mostrar un bloqueo. */
export function lockSummary(lock: PuduHubLock): string {
  const who = lock.propio ? "Usted, en otra sesion" : lock.nombre || "Otro usuario";
  const where = lock.origen ? ` (${lock.origen})` : "";
  const until = formatTimestamp(lock.hasta);
  return until ? `${who}${where} hasta ${until}` : `${who}${where}`;
}

function listed(label: string, items: string[]): string {
  const shown = items.slice(0, 5).join(", ");
  const more = items.length > 5 ? ` y ${items.length - 5} mas` : "";
  return `${label}: ${shown}${more}`;
}

/** Lineas del resumen de lo que llego de PuduHub tras enviar. */
export function changesSummary(changes: PuduHubChanges): string[] {
  const lines: string[] = [];
  if (changes.hallazgosNuevos.length)
    lines.push(listed("Hallazgos nuevos", changes.hallazgosNuevos));
  if (changes.hallazgosActualizados.length)
    lines.push(listed("Hallazgos actualizados", changes.hallazgosActualizados));
  if (changes.hallazgosEliminados.length)
    lines.push(listed("Hallazgos quitados", changes.hallazgosEliminados));
  if (changes.secciones.length) lines.push(listed("Secciones", changes.secciones));
  if (changes.campos.length)
    lines.push(
      listed(
        "Datos del proyecto",
        changes.campos.map((field) => PROJECT_FIELD_LABELS[field] ?? field),
      ),
    );
  if (changes.evidencias) lines.push(`Evidencias: ${changes.evidencias}`);
  if (changes.orden) lines.push("Orden de los hallazgos");
  return lines;
}

/** Texto del indicador del proyecto vinculado. */
export function linkIndicator(status: PuduHubLinkStatus): string {
  if (status.vinculoInvalido) return "Vinculo con PuduHub no valido";
  if (!status.vinculado) return "Sin vincular a PuduHub";
  return `Vinculado a PuduHub, version ${status.baseVersion ?? "-"}; cambios sin enviar: ${
    status.cambiosSinEnviar ? "si" : "no"
  }`;
}
