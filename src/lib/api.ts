// SPDX-License-Identifier: GPL-3.0-only
// Copyright (c) 2026 bc0d3

// Puerta unica de datos de la interfaz. Delega en la capa de datos activa
// (lib/backend): TauriBackend en escritorio, HttpBackend en el editor web.
// Regla de README.dev.md: ningun componente llama a invoke() ni a fetch()
// directamente.
import { getBackend } from "./backend";
import type { Capabilities, PackageExport } from "./backend";
import type { PuduHubApi } from "./puduhub";
import type {
  ApiSendResult,
  CvssResult,
  CvssVersion,
  ExportTarget,
  Finding,
  FindingTemplate,
  GitBranch,
  GitChange,
  GitCommit,
  GitState,
  McpClient,
  McpStatus,
  PdfTemplate,
  ProjectAssignment,
  ProjectMeta,
  ProjectSummary,
  WorkspaceStats,
  RecentWorkspace,
  Snippet,
  WorkspaceMeta,
} from "./types";

// --- Workspace ---

/** Abre el file picker para elegir/crear la carpeta del workspace. */
export function pickWorkspace(): Promise<string | null> {
  return getBackend().pickWorkspace();
}

/** Ruta del ultimo workspace abierto (persistida en tauri-plugin-store). */
export function getStoredWorkspace(): Promise<string | null> {
  return getBackend().getStoredWorkspace();
}

/** Workspaces recientes para la pantalla de bienvenida (mas reciente primero). */
export function listRecentWorkspaces(): Promise<RecentWorkspace[]> {
  return getBackend().listRecentWorkspaces();
}

/** Quita un workspace de recientes (no borra del disco). */
export function removeRecentWorkspace(path: string): Promise<void> {
  return getBackend().removeRecentWorkspace(path);
}

/** Abre un workspace existente en la ruta dada. */
export function openWorkspace(path: string): Promise<WorkspaceMeta> {
  return getBackend().openWorkspace(path);
}

/** Crea un workspace nuevo (estructura inicial) en la ruta dada. */
export function createWorkspace(path: string, name: string): Promise<WorkspaceMeta> {
  return getBackend().createWorkspace(path, name);
}

export function saveWorkspaceMeta(meta: WorkspaceMeta): Promise<void> {
  return getBackend().saveWorkspaceMeta(meta);
}

/** Relee workspace.yaml sin efectos secundarios (ej. tras reordenar el tablero Kanban). */
export function getWorkspaceMeta(): Promise<WorkspaceMeta> {
  return getBackend().getWorkspaceMeta();
}

// --- Proyectos ---

/** Resumen del workspace para el dashboard de Inicio (totales + severidades). */
export function workspaceStats(): Promise<WorkspaceStats> {
  return getBackend().workspaceStats();
}

export function listProjects(): Promise<ProjectSummary[]> {
  return getBackend().listProjects();
}

export function createProject(
  name: string,
  client: string,
  projectType: string,
): Promise<ProjectSummary> {
  return getBackend().createProject(name, client, projectType);
}

/** Crea un proyecto de ejemplo completo (secciones + hallazgos demo). */
export function createExampleProject(): Promise<ProjectSummary> {
  return getBackend().createExampleProject();
}

export function loadProject(id: string): Promise<ProjectMeta> {
  return getBackend().loadProject(id);
}

/** Borra un proyecto completo (carpeta y contenido). No se puede deshacer. */
export function deleteProject(id: string): Promise<void> {
  return getBackend().deleteProject(id);
}

export function saveProject(id: string, meta: ProjectMeta): Promise<void> {
  return getBackend().saveProject(id, meta);
}

// --- Hallazgos ---

export function listFindings(projectId: string): Promise<Finding[]> {
  return getBackend().listFindings(projectId);
}

export function loadFinding(projectId: string, findingId: string): Promise<Finding> {
  return getBackend().loadFinding(projectId, findingId);
}

/** Guarda un hallazgo. Devuelve el hallazgo (el id puede cambiar si el slug cambio). */
export function saveFinding(projectId: string, finding: Finding): Promise<Finding> {
  return getBackend().saveFinding(projectId, finding);
}

export function createFinding(projectId: string, title: string): Promise<Finding> {
  return getBackend().createFinding(projectId, title);
}

export function deleteFinding(projectId: string, findingId: string): Promise<void> {
  return getBackend().deleteFinding(projectId, findingId);
}

export function reorderFindings(projectId: string, order: string[]): Promise<void> {
  return getBackend().reorderFindings(projectId, order);
}

/** Asigna el cierre de un proyecto (nombre + correo) y lo mueve a esa columna del tablero. */
export function assignProjectClosure(
  id: string,
  name: string,
  email: string,
): Promise<ProjectAssignment> {
  return getBackend().assignProjectClosure(id, name, email);
}

/** Reordena las tarjetas del tablero Kanban de Proyectos. */
export function reorderProjects(order: string[]): Promise<void> {
  return getBackend().reorderProjects(order);
}

/** Guarda un asset (evidencia) y devuelve su ruta relativa (assets/<uuid>.<ext>). */
export function saveAsset(projectId: string, ext: string, dataBase64: string): Promise<string> {
  return getBackend().saveAsset(projectId, ext, dataBase64);
}

/** Guarda un asset de marca (logo / fondo de portada). Devuelve ruta root-relative. */
export function saveBrandingAsset(ext: string, dataBase64: string): Promise<string> {
  return getBackend().saveBrandingAsset(ext, dataBase64);
}

// --- CVSS ---

/** Calcula puntaje y severidad a partir de un vector CVSS 3.1 o 4.0. */
export function calcCvss(version: CvssVersion, vector: string): Promise<CvssResult> {
  return getBackend().calcCvss(version, vector);
}

// --- Libreria de plantillas ---

export function listFindingTemplates(): Promise<FindingTemplate[]> {
  return getBackend().listFindingTemplates();
}

export function saveFindingTemplate(template: FindingTemplate): Promise<void> {
  return getBackend().saveFindingTemplate(template);
}

/** Clona una plantilla de hallazgo a un proyecto, reemplazando variables. */
export function instantiateTemplate(
  projectId: string,
  templateId: string,
  vars: Record<string, string>,
): Promise<Finding> {
  return getBackend().instantiateTemplate(projectId, templateId, vars);
}

export function listSnippets(): Promise<Snippet[]> {
  return getBackend().listSnippets();
}

export function saveSnippet(snippet: Snippet): Promise<void> {
  return getBackend().saveSnippet(snippet);
}

export function listPdfTemplates(): Promise<PdfTemplate[]> {
  return getBackend().listPdfTemplates();
}

/** Duplica una plantilla a la libreria del usuario. Devuelve el nuevo nombre. */
export function duplicateTemplate(name: string): Promise<string> {
  return getBackend().duplicateTemplate(name);
}

/** Lee el codigo fuente .typ de una plantilla. */
export function readTemplateSource(name: string): Promise<string> {
  return getBackend().readTemplateSource(name);
}

/** Guarda el codigo fuente .typ en la libreria del usuario. */
export function saveTemplateSource(name: string, content: string): Promise<void> {
  return getBackend().saveTemplateSource(name, content);
}

/** Elimina una plantilla de la libreria del usuario (no las incluidas). */
export function deleteTemplate(name: string): Promise<void> {
  return getBackend().deleteTemplate(name);
}

/** Guarda la metadata (.meta.yaml) de una plantilla: titulo, descripcion y tags. */
export function saveTemplateMeta(
  name: string,
  title: string,
  description: string,
  tags: string[],
): Promise<void> {
  return getBackend().saveTemplateMeta(name, title, description, tags);
}

// --- PDF ---

/**
 * Compila el PDF del proyecto. Si alsoExecutive es true y la plantilla no es ya
 * la ejecutiva, genera ademas un segundo PDF ejecutivo. Devuelve las rutas
 * producidas (principal primero).
 */
export function generatePdf(projectId: string, alsoExecutive = false): Promise<string[]> {
  return getBackend().generatePdf(projectId, alsoExecutive);
}

/** Exporta un resumen de hallazgos a CSV con las columnas elegidas. Devuelve la ruta. */
export function exportCsv(projectId: string, columns: string[]): Promise<string> {
  return getBackend().exportCsv(projectId, columns);
}

// --- Exportacion a API externa (opt-in) ---

/** Lista los destinos de exportacion configurados en el workspace. */
export function listExportTargets(): Promise<ExportTarget[]> {
  return getBackend().listExportTargets();
}

/** Crea o actualiza (por nombre) un destino de exportacion. */
export function saveExportTarget(target: ExportTarget): Promise<void> {
  return getBackend().saveExportTarget(target);
}

/** Borra un destino y su token del keychain. */
export function deleteExportTarget(name: string): Promise<void> {
  return getBackend().deleteExportTarget(name);
}

/** Guarda (o borra, si es vacio) el token del destino en el keychain del SO. */
export function setExportToken(name: string, token: string): Promise<void> {
  return getBackend().setExportToken(name, token);
}

/** Indica si el destino ya tiene un token guardado (sin exponerlo). */
export function hasExportToken(name: string): Promise<boolean> {
  return getBackend().hasExportToken(name);
}

/** Devuelve el JSON exacto que se enviaria (para la vista previa/consentimiento). */
export function previewExportPayload(projectId: string, targetName: string): Promise<unknown> {
  return getBackend().previewExportPayload(projectId, targetName);
}

/** Envia el proyecto al destino (POST JSON con Bearer). Requiere token guardado. */
export function sendExportToApi(projectId: string, targetName: string): Promise<ApiSendResult> {
  return getBackend().sendExportToApi(projectId, targetName);
}

/** Renderiza el PDF a imagenes PNG (data URLs) para la vista previa embebida. */
export function previewPdf(projectId: string): Promise<string[]> {
  return getBackend().previewPdf(projectId);
}

/** Abre un archivo con la app por defecto del sistema. */
export function openPath(path: string): Promise<void> {
  return getBackend().openPath(path);
}

/** Abre el explorador mostrando el archivo (revela la carpeta). */
export function revealPath(path: string): Promise<void> {
  return getBackend().revealPath(path);
}

// --- Git ---

export function gitInit(): Promise<void> {
  return getBackend().gitInit();
}

export function gitCommit(message: string): Promise<void> {
  return getBackend().gitCommit(message);
}

/** Cambios sin commitear de un proyecto. */
export function gitStatus(projectId: string): Promise<GitState> {
  return getBackend().gitStatus(projectId);
}

/** Historial de commits que tocan un proyecto. */
export function gitLog(projectId: string): Promise<GitCommit[]> {
  return getBackend().gitLog(projectId);
}

/** Ramas locales del workspace (la actual marcada). */
export function gitBranches(): Promise<GitBranch[]> {
  return getBackend().gitBranches();
}

/** Archivos de un proyecto que cambia un commit (para el panel de detalle). */
export function gitCommitFiles(projectId: string, hash: string): Promise<GitChange[]> {
  return getBackend().gitCommitFiles(projectId, hash);
}

// --- Integracion MCP (Claude Desktop / Claude Code) ---

/** Estado de la integracion con un cliente MCP para el workspace abierto. */
export function mcpStatus(client: McpClient): Promise<McpStatus> {
  return getBackend().mcpStatus(client);
}

/** Conecta el workspace abierto al cliente MCP indicado. */
export function mcpConnect(client: McpClient): Promise<void> {
  return getBackend().mcpConnect(client);
}

/** Desconecta: quita la entrada de PuduReport del cliente MCP indicado. */
export function mcpDisconnect(client: McpClient): Promise<void> {
  return getBackend().mcpDisconnect(client);
}

// --- Paquete .pudupkg (escritorio) ---

/** Exporta el proyecto a build/*.pudupkg (con la marca del workspace). */
export function exportPackage(projectId: string): Promise<PackageExport> {
  return getBackend().exportPackage(projectId);
}

/** Pide un .pudupkg y lo importa como proyecto nuevo. null si se cancela. */
export function importPackage(): Promise<ProjectSummary | null> {
  return getBackend().importPackage();
}

// --- Capa de datos activa ---

/** Que funciones ofrece la capa de datos activa (escritorio o web). */
export function capabilities(): Capabilities {
  return getBackend().capabilities;
}

/** URL cargable de una evidencia (assets/...) del proyecto. */
export function assetUrl(projectId: string, relPath: string): string {
  return getBackend().assetUrl(projectId, relPath);
}

/** URL cargable de un asset de marca (/branding/...). */
export function brandingAssetUrl(path: string): string {
  return getBackend().brandingAssetUrl(path);
}

/** Titulo de la ventana (escritorio) o de la pestaña (web). */
export function setWindowTitle(title: string): void {
  getBackend().setWindowTitle(title);
}

/** Ultimo PDF generado en esta sesion (solo web: URL de descarga). */
export function lastGeneratedPdf(projectId: string): string | null {
  return getBackend().lastGeneratedPdf(projectId);
}

/** Descarga un PDF generado en el servidor (solo web). */
export function downloadGeneratedPdf(projectId: string, url: string): Promise<void> {
  return getBackend().downloadGeneratedPdf(projectId, url);
}

/**
 * Sincronizacion con PuduHub (solo escritorio). Las pantallas la usan solo si
 * `capabilities().puduhub`; el editor web no la tiene.
 */
export function puduhub(): PuduHubApi {
  const sync = getBackend().puduhub;
  if (!sync) {
    throw new Error("La sincronizacion con PuduHub no esta disponible.");
  }
  return sync;
}
