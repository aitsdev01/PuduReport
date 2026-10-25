// SPDX-License-Identifier: GPL-3.0-only
// Copyright (c) 2026 bc0d3

// Capa de datos del editor web: API de informes de PuduHub (mismo origen,
// sesion por cookie + CSRF). Trabaja sobre UN informe: toma el bloqueo de
// edicion al abrir, lo renueva y lo libera al salir; cada escritura envia la
// `revision` vigente y la actualiza con la respuesta.
//
// Este modulo no importa valores de otros modulos (solo tipos), para poder
// probarlo aislado con node --test.
import type { Backend, Capabilities, PackageExport } from "./types";
import type {
  ApiSendResult,
  CvssResult,
  CvssVersion,
  ExportTarget,
  Finding,
  FindingMeta,
  FindingTemplate,
  GitBranch,
  GitChange,
  GitCommit,
  GitState,
  McpStatus,
  PdfTemplate,
  ProjectAssignment,
  ProjectMeta,
  ProjectSummary,
  RecentWorkspace,
  Severity,
  Snippet,
  WorkspaceMeta,
  WorkspaceStats,
} from "../types";

const WEB: Capabilities = {
  mode: "web",
  workspaces: false,
  projectList: false,
  git: false,
  mcp: false,
  exportTargets: false,
  csvExport: false,
  templateEditing: false,
  // Portada y marca del informe: workspace.yaml propio en PuduHub.
  branding: true,
  localFiles: false,
  pagePreview: false,
  packages: false,
  puduhub: false,
};

/** Cookie y cabecera CSRF de la sesion de PuduHub (las de Angular). */
export const CSRF_COOKIE = "XSRF-TOKEN";
export const CSRF_HEADER = "X-XSRF-TOKEN";
/** Cabecera con el token del bloqueo de edicion. */
export const LOCK_HEADER = "X-Lock-Token";

export interface HttpBackendOptions {
  /** Prefijo de la API de informes, ej. "/api/admin/informes". */
  apiBase: string;
  /** Id (UUID) del informe abierto. */
  informeId: string;
  /** Ruta que emite la cookie CSRF. */
  csrfUrl?: string;
  /** Inyectables para pruebas. */
  fetch?: typeof fetch;
  readCookie?: (name: string) => string | null;
  /** Renovacion del bloqueo (ms). */
  lockRenewMs?: number;
  /** Consulta del estado de un PDF (ms) y espera maxima. */
  pdfPollMs?: number;
  pdfTimeoutMs?: number;
  /** Lecturas de hallazgos en paralelo al listar. */
  findingConcurrency?: number;
  /** Se ejecuta antes de generar un PDF (ej. guardar borradores pendientes). */
  beforePdf?: () => Promise<void>;
  /** Guarda un archivo descargado (por defecto, enlace temporal del navegador). */
  saveFile?: (blob: Blob, name: string) => void;
}

/** Quien tiene el bloqueo (`bloqueo` de PuduHub). */
export interface LockHolder {
  nombre: string;
  hasta: string | null;
  origen: string;
  /** El mismo usuario, en otra sesion o en escritorio. */
  propio: boolean;
}

/** Eventos de la sesion de edicion, para que la interfaz avise sin perder nada. */
export type SessionEvent =
  | { kind: "locked"; hasta: string | null }
  | ({ kind: "lock_held" } & LockHolder)
  | { kind: "lock_lost" }
  | { kind: "stale_revision" }
  | { kind: "auth" };

export type OpenResult =
  | { kind: "ready" }
  | ({ kind: "lock_held"; puedeForzar: boolean } & LockHolder);

const NO_HOLDER: LockHolder = { nombre: "", hasta: null, origen: "", propio: false };

/** Error de la API con un mensaje listo para mostrar. */
export class BackendError extends Error {
  readonly status: number;
  readonly code: string;
  readonly holder: LockHolder;

  constructor(status: number, code: string, message: string, holder: LockHolder = NO_HOLDER) {
    super(message);
    this.name = "BackendError";
    this.status = status;
    this.code = code;
    this.holder = holder;
  }

  // Las notificaciones muestran String(error): sin el prefijo "Error:".
  override toString(): string {
    return this.message;
  }
}

type Json = Record<string, unknown>;

function isObject(value: unknown): value is Json {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function str(value: unknown): string {
  return typeof value === "string" ? value : "";
}

/** Hora legible de un ISO-8601; si no se puede leer, el texto tal cual. */
export function formatHasta(hasta: string | null): string {
  if (!hasta) return "";
  const date = new Date(hasta);
  if (Number.isNaN(date.getTime())) return hasta;
  return date.toLocaleTimeString("es", { hour: "2-digit", minute: "2-digit" });
}

/** `{"bloqueo": {usuarioId, nombre, hasta, origen, propio}}` de un 409 lock_held. */
function holderFrom(body: unknown): LockHolder {
  const source = isObject(body) && isObject(body.bloqueo) ? body.bloqueo : null;
  if (!source) return NO_HOLDER;
  return {
    nombre: str(source.nombre),
    hasta: str(source.hasta) || null,
    origen: str(source.origen),
    propio: source.propio === true,
  };
}

/** Texto para quien tiene el bloqueo. */
export function holderText(holder: LockHolder): string {
  const hora = formatHasta(holder.hasta);
  const hasta = hora ? ` hasta las ${hora}` : "";
  if (holder.propio) {
    const donde =
      holder.origen === "escritorio" ? "en PuduReport de escritorio" : "en otra pestaña";
    return `Ya tienes este informe abierto ${donde}${hasta}.`;
  }
  return `El informe esta en edicion por ${holder.nombre || "otra persona"}${hasta}.`;
}

/** Traduce una respuesta de error de PuduHub (`{"error": codigo, ...}`) a un mensaje claro. */
export function errorFrom(status: number, body: unknown): BackendError {
  const code = isObject(body) ? str(body.error) : "";
  if (status === 409) {
    switch (code) {
      case "lock_held": {
        const holder = holderFrom(body);
        return new BackendError(status, code, holderText(holder), holder);
      }
      case "lock_required":
        return new BackendError(
          status,
          code,
          "Se perdio el bloqueo de edicion. Recarga la pagina para seguir editando.",
        );
      case "stale_revision":
        return new BackendError(
          status,
          code,
          "El informe cambio en otra sesion. Recarga la pagina para ver la version vigente.",
        );
      case "merge_conflict":
        return new BackendError(status, code, "Hay cambios en conflicto con otra sesion.");
      case "pdf_not_ready":
        return new BackendError(status, code, "El PDF todavia no esta listo.");
      default:
        return new BackendError(
          status,
          code || "conflict",
          "La operacion choco con otro cambio. Intenta de nuevo.",
        );
    }
  }
  if (status === 401) {
    return new BackendError(
      status,
      code || "unauthorized",
      "La sesion expiro. Vuelve a iniciar sesion en PuduHub.",
    );
  }
  if (status === 403) {
    if (code === "csrf") {
      return new BackendError(
        status,
        code,
        "No se pudo validar la solicitud. Recarga la pagina e intenta de nuevo.",
      );
    }
    return new BackendError(
      status,
      code || "forbidden",
      "No tienes permiso para esta accion en el informe.",
    );
  }
  if (status === 404) {
    return new BackendError(status, code || "not_found", "No se encontro el informe o el recurso.");
  }
  if (status === 413) {
    return new BackendError(
      status,
      code || "too_large",
      "El contenido supera el tamano permitido.",
    );
  }
  if (status === 415) {
    return new BackendError(status, code || "unsupported", "Tipo de archivo no permitido.");
  }
  if (status === 400 || status === 422) {
    return new BackendError(
      status,
      code || "invalid_payload",
      "PuduHub rechazo los datos enviados.",
    );
  }
  return new BackendError(
    status,
    code || "http_error",
    `PuduHub respondio con un error (${status}).`,
  );
}

function unsupported<T>(): Promise<T> {
  return Promise.reject(
    new BackendError(0, "unsupported", "Esta funcion no esta disponible en el editor web."),
  );
}

const IMAGE_TYPES: Record<string, string> = {
  png: "image/png",
  jpg: "image/jpeg",
  jpeg: "image/jpeg",
  gif: "image/gif",
  webp: "image/webp",
};

/** `assets/<segmento>[/<segmento>...].<imagen>` con segmentos ASCII seguros. */
const ASSET_PATH = /^assets(\/[A-Za-z0-9_-][A-Za-z0-9._-]{0,127})+\.(png|jpe?g|gif|webp)$/i;
/** Referencia de marca de workspace.yaml: `/branding/<archivo>` plano. */
const BRANDING_REF = /^\/branding\/[A-Za-z0-9_-][A-Za-z0-9._-]{0,127}\.(png|jpe?g|gif|webp)$/i;
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

function defaultReadCookie(name: string): string | null {
  if (typeof document === "undefined") return null;
  for (const part of document.cookie.split(";")) {
    const [key, ...rest] = part.trim().split("=");
    if (key === name) return decodeURIComponent(rest.join("="));
  }
  return null;
}

function defaultSaveFile(blob: Blob, name: string) {
  const url = URL.createObjectURL(blob);
  const link = document.createElement("a");
  link.href = url;
  link.download = name;
  document.body.appendChild(link);
  link.click();
  link.remove();
  setTimeout(() => URL.revokeObjectURL(url), 30_000);
}

function base64ToBytes(data: string): Uint8Array {
  const binary = atob(data);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
  return bytes;
}

function wait(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

/** Marca por defecto de PuduReport (PuduHub envia `workspace: null`). */
function defaultWorkspace(): WorkspaceMeta {
  return {
    name: "PuduHub",
    branding: {
      logo_path: "",
      cover_background: "",
      primary_color: "#1f6fb2",
      cover_color: "",
      cover_layout: "centered",
      cover_scrim: 0.5,
      findings_page_break: true,
      body_font: "",
      mono_font: "",
      cover_show_logo: true,
      cover_subtitle: "",
      cover_show_period: true,
      cover_show_org: true,
      cover_show_accent: true,
    },
    watermark: { enabled: false, text: "", opacity: 0.1, size: 60 },
    project_order: [],
    export_targets: [],
  };
}

/** workspace.yaml del informe con los defaults de PuduReport para lo que falte. */
function normalizeWorkspace(raw: unknown): WorkspaceMeta {
  const base = defaultWorkspace();
  if (!isObject(raw)) return base;
  return {
    ...base,
    ...raw,
    name: str(raw.name) || base.name,
    branding: { ...base.branding, ...(isObject(raw.branding) ? raw.branding : {}) },
    watermark: { ...base.watermark, ...(isObject(raw.watermark) ? raw.watermark : {}) },
    project_order: [],
    export_targets: [],
  } as WorkspaceMeta;
}

/**
 * Proyecto de PuduHub con los campos que las pantallas esperan. PuduHub omite
 * listas vacias (por ejemplo `layout`); las claves desconocidas se conservan
 * para devolverlas intactas al guardar.
 */
function normalizeProject(raw: Json, orden: unknown): ProjectMeta {
  const list = (value: unknown) => (Array.isArray(value) ? value : []);
  const text = (value: unknown) => str(value);
  return {
    ...raw,
    name: text(raw.name),
    client: text(raw.client),
    gerencia: text(raw.gerencia),
    area: text(raw.area),
    ambiente: text(raw.ambiente) as ProjectMeta["ambiente"],
    exposicion: text(raw.exposicion) as ProjectMeta["exposicion"],
    project_type: text(raw.project_type) || "pentest",
    osid: text(raw.osid),
    template_override: text(raw.template_override),
    start_date: text(raw.start_date),
    end_date: text(raw.end_date),
    scope: list(raw.scope),
    team: list(raw.team),
    project_status: (text(raw.project_status) || "done") as ProjectMeta["project_status"],
    assignment_history: list(raw.assignment_history),
    sections: list(raw.sections),
    layout: list(raw.layout),
    finding_order: Array.isArray(orden) ? orden.map(String) : list(raw.finding_order).map(String),
  };
}

function findingFrom(body: unknown): Finding {
  if (!isObject(body) || typeof body.id !== "string" || !isObject(body.meta)) {
    throw new BackendError(502, "invalid_payload", "Respuesta de PuduHub invalida.");
  }
  return {
    id: body.id,
    meta: body.meta as unknown as FindingMeta,
    body: typeof body.body === "string" ? body.body : "",
  };
}

interface RequestOptions {
  json?: unknown;
  body?: Uint8Array;
  contentType?: string;
  query?: Record<string, string>;
  /** Ruta absoluta (sin el prefijo del informe). */
  absolute?: boolean;
  /** Al salir de la pagina: no se renueva el CSRF y se usa keepalive. */
  unload?: boolean;
}

export class HttpBackend implements Backend {
  readonly capabilities = WEB;
  private readonly apiBase: string;
  private readonly informeId: string;
  private readonly csrfUrl: string;
  private readonly fetchImpl: typeof fetch;
  private readonly readCookie: (name: string) => string | null;
  private readonly lockRenewMs: number;
  private readonly pdfPollMs: number;
  private readonly pdfTimeoutMs: number;
  private readonly findingConcurrency: number;
  private readonly beforePdf?: () => Promise<void>;
  private readonly saveFile: (blob: Blob, name: string) => void;

  private revision: number | null = null;
  private lockToken: string | null = null;
  private lockHeld = false;
  private renewTimer: ReturnType<typeof setInterval> | null = null;
  private blocked: BackendError | null = null;
  private queue: Promise<unknown> = Promise.resolve();
  private inFlight = 0;
  private csrfRequest: Promise<void> | null = null;
  private readonly listeners = new Set<(event: SessionEvent) => void>();
  private lastPdf: string | null = null;
  /** `InformeDetail.puedeForzar` si PuduHub lo envia; null = no informado. */
  private canForce: boolean | null = null;
  /** workspace.yaml del informe (`InformeDetail.workspace`; null = defaults). */
  private workspace: WorkspaceMeta = defaultWorkspace();
  private unloadInstalled = false;

  constructor(options: HttpBackendOptions) {
    this.apiBase = options.apiBase.replace(/\/+$/, "");
    this.informeId = options.informeId;
    this.csrfUrl = options.csrfUrl ?? "/api/auth/csrf";
    this.fetchImpl = options.fetch ?? ((input, init) => fetch(input, init));
    this.readCookie = options.readCookie ?? defaultReadCookie;
    this.lockRenewMs = options.lockRenewMs ?? 60_000;
    this.pdfPollMs = options.pdfPollMs ?? 1_500;
    this.pdfTimeoutMs = options.pdfTimeoutMs ?? 180_000;
    this.findingConcurrency = Math.max(1, options.findingConcurrency ?? 6);
    this.beforePdf = options.beforePdf;
    this.saveFile = options.saveFile ?? defaultSaveFile;
  }

  // --- Sesion -------------------------------------------------------------

  /** Id del informe; las pantallas lo usan como id de proyecto. */
  get projectId(): string {
    return this.informeId;
  }

  /** Revision vigente conocida (null antes de abrir). */
  get currentRevision(): number | null {
    return this.revision;
  }

  onSessionEvent(listener: (event: SessionEvent) => void): () => void {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  }

  private emit(event: SessionEvent) {
    for (const listener of this.listeners) listener(event);
  }

  /**
   * Pide la cookie CSRF, igual que el frontend de PuduHub antes de cada
   * escritura (la sesion rota y la cookie con ella). Peticiones simultaneas
   * comparten la misma llamada.
   */
  private prepareCsrf(): Promise<void> {
    if (this.csrfRequest) return this.csrfRequest;
    const request = (async () => {
      const response = await this.fetchImpl(this.csrfUrl, {
        method: "GET",
        credentials: "same-origin",
        cache: "no-store",
      });
      if (!response.ok) throw errorFrom(response.status, await this.readJson(response));
    })().finally(() => {
      if (this.csrfRequest === request) this.csrfRequest = null;
    });
    this.csrfRequest = request;
    return request;
  }

  /**
   * Abre el informe: documento y bloqueo de edicion. Si el bloqueo lo tiene
   * otra sesion, lo informa sin tomar nada; `forzar` lo toma (PuduHub solo lo
   * permite sobre una sesion propia o a un Admin).
   */
  async open(forzar = false): Promise<OpenResult> {
    await this.fetchInforme();
    try {
      await this.takeLock(forzar);
    } catch (error) {
      if (error instanceof BackendError && error.code === "lock_held") {
        // Sin la marca de PuduHub, solo se ofrece forzar una sesion propia.
        const puedeForzar = this.canForce ?? error.holder.propio;
        return { kind: "lock_held", ...error.holder, puedeForzar };
      }
      throw error;
    }
    this.blocked = null;
    this.startRenewal();
    this.installUnload();
    return { kind: "ready" };
  }

  /** POST /bloqueo: toma o renueva (con X-Lock-Token). El token llega solo al tomarlo. */
  private async takeLock(forzar = false): Promise<void> {
    // La revision vista permite renovar un bloqueo propio vencido que nadie tomo;
    // si el borrador cambio, PuduHub responde lock_required (recargar).
    const json: Json = { forzar };
    if (this.revision !== null) json.revision = this.revision;
    const body = await this.request("POST", "/bloqueo", { json });
    if (isObject(body) && typeof body.token === "string" && body.token) {
      this.lockToken = body.token;
    }
    this.lockHeld = true;
    this.checkRevision(body);
    const bloqueo = isObject(body) && isObject(body.bloqueo) ? body.bloqueo : null;
    this.emit({ kind: "locked", hasta: bloqueo ? str(bloqueo.hasta) || null : null });
  }

  private startRenewal() {
    if (this.renewTimer) clearInterval(this.renewTimer);
    this.renewTimer = setInterval(() => void this.renewLock(), this.lockRenewMs);
    // En Node (pruebas) el temporizador no debe mantener vivo el proceso.
    (this.renewTimer as { unref?: () => void }).unref?.();
  }

  /** Renueva el bloqueo; si se perdio, bloquea las escrituras y avisa. */
  async renewLock(): Promise<void> {
    if (!this.lockHeld || this.blocked) return;
    try {
      await this.takeLock();
    } catch (error) {
      if (!(error instanceof BackendError)) return; // red: se reintenta luego
      if (error.status === 409 || error.status === 401) {
        this.lockHeld = false;
        this.stopRenewal();
        this.block(error);
      }
    }
  }

  private stopRenewal() {
    if (this.renewTimer) clearInterval(this.renewTimer);
    this.renewTimer = null;
  }

  private installUnload() {
    if (this.unloadInstalled || typeof window === "undefined") return;
    this.unloadInstalled = true;
    window.addEventListener("pagehide", () => void this.release(true));
  }

  /** Libera el bloqueo (al salir de la pagina usa keepalive). */
  async release(unload = false): Promise<void> {
    this.stopRenewal();
    if (!this.lockHeld) return;
    this.lockHeld = false;
    try {
      await this.request("DELETE", "/bloqueo", { unload });
    } catch {
      // El bloqueo vence solo (15 min); no hay nada que mostrar al salir.
    }
    this.lockToken = null;
  }

  private block(error: BackendError) {
    if (this.blocked) return;
    this.blocked = error;
    if (error.code === "lock_held") {
      this.emit({ kind: "lock_held", ...error.holder });
    } else if (error.code === "stale_revision") {
      this.emit({ kind: "stale_revision" });
    } else if (error.status === 401) {
      this.emit({ kind: "auth" });
    } else {
      this.emit({ kind: "lock_lost" });
    }
  }

  // --- HTTP ---------------------------------------------------------------

  private async readJson(response: Response): Promise<unknown> {
    if (response.status === 204) return null;
    const type = response.headers.get("content-type") ?? "";
    if (!type.includes("json")) return null;
    try {
      return await response.json();
    } catch {
      return null;
    }
  }

  private url(path: string, options: { absolute?: boolean; query?: Record<string, string> }) {
    const base = options.absolute
      ? `${this.apiBase}${path}`
      : `${this.apiBase}/${encodeURIComponent(this.informeId)}${path}`;
    const query = options.query ? `?${new URLSearchParams(options.query).toString()}` : "";
    return base + query;
  }

  private async send(method: string, path: string, options: RequestOptions): Promise<Response> {
    const headers: Record<string, string> = { Accept: "application/json" };
    if (method !== "GET") {
      if (!options.unload) await this.prepareCsrf();
      const token = this.readCookie(CSRF_COOKIE);
      if (!token && !options.unload) {
        throw new BackendError(403, "csrf", "No se pudo preparar la solicitud segura.");
      }
      if (token) headers[CSRF_HEADER] = token;
      if (this.lockToken) headers[LOCK_HEADER] = this.lockToken;
    }
    let body: BodyInit | undefined;
    if (options.json !== undefined) {
      headers["Content-Type"] = "application/json";
      body = JSON.stringify(options.json);
    } else if (options.body) {
      headers["Content-Type"] = options.contentType ?? "application/octet-stream";
      body = options.body as BodyInit;
    }
    return this.fetchImpl(this.url(path, options), {
      method,
      headers,
      body,
      credentials: "same-origin",
      cache: "no-store",
      keepalive: options.unload,
    });
  }

  private async request(method: string, path: string, options: RequestOptions = {}) {
    const response = await this.send(method, path, options);
    const parsed = await this.readJson(response);
    if (!response.ok) throw errorFrom(response.status, parsed);
    return parsed;
  }

  /** Escritura serializada con la revision vigente; conserva el orden. */
  private write<T>(task: (revision: number) => Promise<T>): Promise<T> {
    const run = this.queue.then(async () => {
      if (this.blocked) throw this.blocked;
      this.inFlight += 1;
      try {
        return await task(this.revision ?? 1);
      } catch (error) {
        if (
          error instanceof BackendError &&
          (error.status === 401 ||
            (error.status === 409 &&
              ["lock_held", "lock_required", "stale_revision"].includes(error.code)))
        ) {
          this.block(error);
        }
        throw error;
      } finally {
        this.inFlight -= 1;
      }
    });
    this.queue = run.catch(() => undefined);
    return run;
  }

  /** Toma la revision de una respuesta de escritura (`Revision`, `Hallazgo`, `AssetView`). */
  private takeRevision(body: unknown) {
    if (isObject(body) && typeof body.revision === "number") this.revision = body.revision;
  }

  /** Revision de una lectura: la primera fija la base; una mayor indica otra sesion. */
  private checkRevision(body: unknown) {
    if (!isObject(body) || typeof body.revision !== "number") return;
    if (this.revision === null) this.revision = body.revision;
    else if (body.revision > this.revision && this.inFlight === 0) {
      // Alguien mas escribio: no se adopta en silencio.
      this.block(errorFrom(409, { error: "stale_revision" }));
    }
  }

  private checkProject(projectId: string): Promise<void> {
    if (projectId === this.informeId) return Promise.resolve();
    return Promise.reject(
      new BackendError(0, "wrong_informe", "El editor web abre un solo informe."),
    );
  }

  /** GET /{id} (InformeDetail): proyecto, orden y revision. */
  private async fetchInforme(): Promise<ProjectMeta> {
    const body = await this.request("GET", "");
    if (!isObject(body) || !isObject(body.proyecto)) {
      throw new BackendError(502, "invalid_payload", "Respuesta de PuduHub invalida.");
    }
    this.checkRevision(body);
    if (typeof body.puedeForzar === "boolean") this.canForce = body.puedeForzar;
    if ("workspace" in body) this.workspace = normalizeWorkspace(body.workspace);
    return normalizeProject(body.proyecto, body.orden);
  }

  // --- Backend: recursos del informe --------------------------------------

  /** GET /{id}/assets/{ruta}: la ruta completa (`assets/...`) va despues de /assets/. */
  assetUrl(projectId: string, relPath: string): string {
    if (projectId !== this.informeId || !ASSET_PATH.test(relPath)) return "";
    const encoded = relPath.split("/").map(encodeURIComponent).join("/");
    return `${this.apiBase}/${encodeURIComponent(this.informeId)}/assets/${encoded}`;
  }

  /** `/branding/x.png` de workspace.yaml -> GET /{id}/assets/branding/x.png. */
  brandingAssetUrl(path: string): string {
    if (!BRANDING_REF.test(path)) return "";
    const encoded = path.slice(1).split("/").map(encodeURIComponent).join("/");
    return `${this.apiBase}/${encodeURIComponent(this.informeId)}/assets/${encoded}`;
  }

  setWindowTitle(title: string): void {
    if (typeof document !== "undefined") document.title = title;
  }

  lastGeneratedPdf(projectId: string): string | null {
    return projectId === this.informeId ? this.lastPdf : null;
  }

  async loadProject(id: string): Promise<ProjectMeta> {
    await this.checkProject(id);
    return this.fetchInforme();
  }

  async saveProject(id: string, meta: ProjectMeta): Promise<void> {
    await this.checkProject(id);
    await this.write(async (revision) => {
      this.takeRevision(await this.request("PUT", "/proyecto", { json: { meta, revision } }));
    });
  }

  async getWorkspaceMeta(): Promise<WorkspaceMeta> {
    return this.workspace;
  }

  /** PUT /{id}/workspace {workspace, revision} -> Revision. */
  async saveWorkspaceMeta(meta: WorkspaceMeta): Promise<void> {
    await this.write(async (revision) => {
      // Solo la parte portable: PuduHub descarta tablero y destinos de API.
      const workspace = { name: meta.name, branding: meta.branding, watermark: meta.watermark };
      this.takeRevision(await this.request("PUT", "/workspace", { json: { workspace, revision } }));
      this.workspace = normalizeWorkspace(workspace);
    });
  }

  async listProjects(): Promise<ProjectSummary[]> {
    const meta = await this.fetchInforme();
    return [
      {
        id: this.informeId,
        name: meta.name,
        client: meta.client,
        project_type: meta.project_type,
        start_date: meta.start_date,
        end_date: meta.end_date,
        finding_count: meta.finding_order.length,
        project_status: meta.project_status,
        assignment_history: meta.assignment_history,
      },
    ];
  }

  /**
   * GET /{id}/hallazgos?incluirCuerpo=1. Si PuduHub no devuelve el cuerpo (la
   * opcion no existe aun), se lee cada GET /{id}/hallazgos/{uid}, con
   * concurrencia acotada.
   */
  async listFindings(projectId: string): Promise<Finding[]> {
    await this.checkProject(projectId);
    let body: unknown;
    try {
      body = await this.request("GET", "/hallazgos", { query: { incluirCuerpo: "1" } });
    } catch (error) {
      // Con todos los cuerpos supera el limite de PuduHub (413): lista sin
      // cuerpos y lectura de cada hallazgo.
      if (!(error instanceof BackendError) || error.status !== 413) throw error;
      body = await this.request("GET", "/hallazgos");
    }
    const items = (Array.isArray(body) ? body.filter(isObject) : []).filter(
      (item) => typeof item.id === "string" && item.id !== "",
    );
    const out: (Finding | null)[] = items.map((item) =>
      typeof item.body === "string" && isObject(item.meta) ? findingFrom(item) : null,
    );
    const missing = out.flatMap((finding, index) => (finding ? [] : [index]));
    let next = 0;
    const worker = async () => {
      while (next < missing.length) {
        const index = missing[next++];
        out[index] = await this.fetchFinding(str(items[index].id));
      }
    };
    await Promise.all(
      Array.from({ length: Math.min(this.findingConcurrency, missing.length) }, worker),
    );
    return out.filter((finding): finding is Finding => finding !== null);
  }

  private async fetchFinding(uid: string): Promise<Finding> {
    return findingFrom(await this.request("GET", `/hallazgos/${encodeURIComponent(uid)}`));
  }

  async loadFinding(projectId: string, findingId: string): Promise<Finding> {
    await this.checkProject(projectId);
    return this.fetchFinding(findingId);
  }

  /** PUT /{id}/hallazgos/{uid} {meta, body, revision} -> Revision. */
  async saveFinding(projectId: string, finding: Finding): Promise<Finding> {
    await this.checkProject(projectId);
    return this.write(async (revision) => {
      const body = await this.request("PUT", `/hallazgos/${encodeURIComponent(finding.id)}`, {
        json: { meta: finding.meta, body: finding.body, revision },
      });
      this.takeRevision(body);
      return finding;
    });
  }

  /** POST /{id}/hallazgos {titulo, revision} -> Hallazgo (id asignado por PuduHub). */
  async createFinding(projectId: string, title: string): Promise<Finding> {
    await this.checkProject(projectId);
    return this.write(async (revision) => {
      const body = await this.request("POST", "/hallazgos", { json: { titulo: title, revision } });
      this.takeRevision(body);
      return findingFrom(body);
    });
  }

  /** DELETE /{id}/hallazgos/{uid}?revision=N -> Revision. */
  async deleteFinding(projectId: string, findingId: string): Promise<void> {
    await this.checkProject(projectId);
    await this.write(async (revision) => {
      const body = await this.request("DELETE", `/hallazgos/${encodeURIComponent(findingId)}`, {
        query: { revision: String(revision) },
      });
      this.takeRevision(body);
    });
  }

  /** PUT /{id}/orden {orden, revision} -> Revision. */
  async reorderFindings(projectId: string, order: string[]): Promise<void> {
    await this.checkProject(projectId);
    await this.write(async (revision) => {
      this.takeRevision(await this.request("PUT", "/orden", { json: { orden: order, revision } }));
    });
  }

  /** POST /{id}/assets?revision=&destino=&nombre= (cuerpo binario) -> AssetView. */
  private uploadImage(
    destino: "assets" | "branding",
    ext: string,
    dataBase64: string,
  ): Promise<Json> {
    const clean = ext.toLowerCase().replace(/[^a-z0-9]/g, "");
    const type = IMAGE_TYPES[clean];
    if (!type) {
      return Promise.reject(
        new BackendError(415, "unsupported", "Solo se permiten imagenes PNG, JPEG, GIF o WebP."),
      );
    }
    const bytes = base64ToBytes(dataBase64);
    const nombre = `${destino === "branding" ? "marca" : "evidencia"}.${clean}`;
    return this.write(async (revision) => {
      const body = await this.request("POST", "/assets", {
        body: bytes,
        contentType: type,
        query: { revision: String(revision), destino, nombre },
      });
      this.takeRevision(body);
      return isObject(body) ? body : {};
    });
  }

  /** Evidencia: devuelve la ruta `assets/...` para el markdown. */
  async saveAsset(projectId: string, ext: string, dataBase64: string): Promise<string> {
    await this.checkProject(projectId);
    const ruta = str((await this.uploadImage("assets", ext, dataBase64)).ruta);
    if (!ASSET_PATH.test(ruta)) {
      throw new BackendError(502, "invalid_payload", "PuduHub no devolvio la ruta de la imagen.");
    }
    return ruta;
  }

  /** Marca: devuelve la referencia `/branding/...` para workspace.yaml. */
  async saveBrandingAsset(ext: string, dataBase64: string): Promise<string> {
    const referencia = str((await this.uploadImage("branding", ext, dataBase64)).referencia);
    if (!BRANDING_REF.test(referencia)) {
      throw new BackendError(502, "invalid_payload", "PuduHub no devolvio la ruta de la imagen.");
    }
    return referencia;
  }

  /** POST {apiBase}/cvss {vector} -> {score, severity, vector, version}. */
  async calcCvss(_version: CvssVersion, vector: string): Promise<CvssResult> {
    const body = await this.request("POST", "/cvss", { json: { vector }, absolute: true });
    if (!isObject(body) || typeof body.score !== "number") {
      throw new BackendError(502, "invalid_payload", "Respuesta de PuduHub invalida.");
    }
    return {
      score: body.score,
      severity: str(body.severity) as Severity,
      vector: str(body.vector) || vector,
    };
  }

  /**
   * POST /{id}/pdf (PdfJob) y GET /{id}/pdf/{trabajo} hasta `listo` o
   * `error`. Devuelve la URL de descarga.
   */
  async generatePdf(projectId: string): Promise<string[]> {
    await this.checkProject(projectId);
    await this.beforePdf?.();
    await this.queue; // que las escrituras pendientes lleguen antes
    if (this.blocked) throw this.blocked;
    let job = await this.request("POST", "/pdf", { json: {} });
    const id = isObject(job) ? str(job.id) : "";
    if (!UUID.test(id)) {
      throw new BackendError(502, "invalid_payload", "PuduHub no devolvio el trabajo de PDF.");
    }
    const started = Date.now();
    for (;;) {
      const estado = isObject(job) ? str(job.estado) : "";
      if (estado === "listo") break;
      if (estado === "error") {
        const code = isObject(job) ? str(job.errorCodigo) : "";
        throw new BackendError(
          500,
          code || "pdf_error",
          `No se pudo generar el PDF en PuduHub${code ? ` (${code})` : ""}.`,
        );
      }
      if (Date.now() - started > this.pdfTimeoutMs) {
        throw new BackendError(
          504,
          "pdf_timeout",
          "La generacion del PDF esta tardando mas de lo esperado. Intenta de nuevo en unos minutos.",
        );
      }
      await wait(this.pdfPollMs);
      job = await this.request("GET", `/pdf/${id}`);
    }
    const url = `${this.apiBase}/${encodeURIComponent(this.informeId)}/pdf/${id}/archivo`;
    this.lastPdf = url;
    return [url];
  }

  /**
   * Descarga el PDF generado. PuduHub la reserva a Admin (auditada): un
   * Ingeniero recibe 403 y se le explica sin tratarlo como un fallo.
   */
  async downloadGeneratedPdf(projectId: string, url: string): Promise<void> {
    await this.checkProject(projectId);
    const prefix = `${this.apiBase}/${encodeURIComponent(this.informeId)}/pdf/`;
    if (!url.startsWith(prefix) || !url.endsWith("/archivo")) {
      throw new BackendError(0, "invalid_url", "Enlace de PDF no valido.");
    }
    const response = await this.fetchImpl(url, {
      method: "GET",
      credentials: "same-origin",
      cache: "no-store",
    });
    if (response.status === 403) {
      throw new BackendError(
        403,
        "forbidden",
        "El PDF quedo generado en PuduHub. La descarga esta reservada a administradores: pidesela a un administrador del informe.",
      );
    }
    if (!response.ok) throw errorFrom(response.status, await this.readJson(response));
    const disposition = response.headers.get("content-disposition") ?? "";
    const name = /filename="([A-Za-z0-9._-]+)"/.exec(disposition)?.[1] ?? "informe.pdf";
    this.saveFile(await response.blob(), name);
  }

  // --- Fuera del alcance del editor web -----------------------------------

  pickWorkspace = (): Promise<string | null> => Promise.resolve(null);
  getStoredWorkspace = (): Promise<string | null> => Promise.resolve(null);
  listRecentWorkspaces = (): Promise<RecentWorkspace[]> => Promise.resolve([]);
  removeRecentWorkspace = (): Promise<void> => unsupported();
  openWorkspace = (): Promise<WorkspaceMeta> => unsupported();
  createWorkspace = (): Promise<WorkspaceMeta> => unsupported();
  workspaceStats = (): Promise<WorkspaceStats> => unsupported();
  createProject = (): Promise<ProjectSummary> => unsupported();
  createExampleProject = (): Promise<ProjectSummary> => unsupported();
  deleteProject = (): Promise<void> => unsupported();
  assignProjectClosure = (): Promise<ProjectAssignment> => unsupported();
  reorderProjects = (): Promise<void> => unsupported();
  listFindingTemplates = (): Promise<FindingTemplate[]> => Promise.resolve([]);
  saveFindingTemplate = (): Promise<void> => unsupported();
  instantiateTemplate = (): Promise<Finding> => unsupported();
  listSnippets = (): Promise<Snippet[]> => Promise.resolve([]);
  saveSnippet = (): Promise<void> => unsupported();
  listPdfTemplates = (): Promise<PdfTemplate[]> => Promise.resolve([]);
  duplicateTemplate = (): Promise<string> => unsupported();
  readTemplateSource = (): Promise<string> => unsupported();
  saveTemplateSource = (): Promise<void> => unsupported();
  deleteTemplate = (): Promise<void> => unsupported();
  saveTemplateMeta = (): Promise<void> => unsupported();
  previewPdf = (): Promise<string[]> => unsupported();
  exportCsv = (): Promise<string> => unsupported();
  listExportTargets = (): Promise<ExportTarget[]> => Promise.resolve([]);
  saveExportTarget = (): Promise<void> => unsupported();
  deleteExportTarget = (): Promise<void> => unsupported();
  setExportToken = (): Promise<void> => unsupported();
  hasExportToken = (): Promise<boolean> => Promise.resolve(false);
  previewExportPayload = (): Promise<unknown> => unsupported();
  sendExportToApi = (): Promise<ApiSendResult> => unsupported();
  exportPackage = (): Promise<PackageExport> => unsupported();
  importPackage = (): Promise<ProjectSummary | null> => unsupported();
  openPath = (): Promise<void> => unsupported();
  revealPath = (): Promise<void> => unsupported();
  gitInit = (): Promise<void> => unsupported();
  gitCommit = (): Promise<void> => unsupported();
  gitStatus = (): Promise<GitState> => unsupported();
  gitLog = (): Promise<GitCommit[]> => unsupported();
  gitBranches = (): Promise<GitBranch[]> => unsupported();
  gitCommitFiles = (): Promise<GitChange[]> => unsupported();
  mcpStatus = (): Promise<McpStatus> => unsupported();
  mcpConnect = (): Promise<void> => unsupported();
  mcpDisconnect = (): Promise<void> => unsupported();
}
