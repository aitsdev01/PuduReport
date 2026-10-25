// SPDX-License-Identifier: GPL-3.0-only
// Copyright (c) 2026 bc0d3

// Interfaz de la capa de datos. La app de escritorio usa TauriBackend (IPC con
// el backend Rust); el editor web usa HttpBackend (API de PuduHub). Las
// pantallas no la usan directo: pasan por las funciones de lib/api.ts.
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
  RecentWorkspace,
  Snippet,
  WorkspaceMeta,
  WorkspaceStats,
} from "../types";
import type { PuduHubApi } from "../puduhub";

/**
 * Que puede hacer la capa de datos activa. Las pantallas ocultan o desactivan
 * lo que no esta disponible (el editor web trabaja sobre un solo informe).
 */
export interface Capabilities {
  mode: "desktop" | "web";
  /** Abrir, crear y cambiar de workspace; recientes. */
  workspaces: boolean;
  /** Tablero de proyectos, crear, borrar y cambiar de proyecto. */
  projectList: boolean;
  /** Historial y commits con git. */
  git: boolean;
  /** Integracion con clientes MCP. */
  mcp: boolean;
  /** Destinos de exportacion a API externa. */
  exportTargets: boolean;
  /** Exportar CSV de resumen. */
  csvExport: boolean;
  /** Biblioteca de plantillas PDF y de hallazgos (editar y aplicar). */
  templateEditing: boolean;
  /** Portada y marca del workspace. */
  branding: boolean;
  /** Abrir o revelar rutas locales y PDF guardado en disco. */
  localFiles: boolean;
  /** Vista previa por pagina (PNG renderizado localmente). */
  pagePreview: boolean;
  /** Exportar e importar paquetes .pudupkg. */
  packages: boolean;
  /** Sincronizacion de escritorio con PuduHub (token en el keychain). */
  puduhub: boolean;
}

/** Resultado de exportar un paquete .pudupkg. */
export interface PackageExport {
  path: string;
  /** Archivos de assets/ omitidos por no ser imagenes permitidas. */
  skipped: string[];
}

export interface Backend {
  readonly capabilities: Capabilities;
  /** Sincronizacion con PuduHub; solo existe si `capabilities.puduhub`. */
  readonly puduhub?: PuduHubApi;

  /** URL cargable de una evidencia del proyecto (ruta relativa assets/...). */
  assetUrl(projectId: string, relPath: string): string;
  /** URL cargable de un asset de marca (ruta root-relative /branding/...). */
  brandingAssetUrl(path: string): string;
  /** Titulo de la ventana (escritorio) o de la pestaña (web). */
  setWindowTitle(title: string): void;
  /** Ultimo PDF generado en esta sesion (solo web: URL de descarga). */
  lastGeneratedPdf(projectId: string): string | null;
  /** Descarga un PDF generado en el servidor (solo web). */
  downloadGeneratedPdf(projectId: string, url: string): Promise<void>;

  // Workspace
  pickWorkspace(): Promise<string | null>;
  getStoredWorkspace(): Promise<string | null>;
  listRecentWorkspaces(): Promise<RecentWorkspace[]>;
  removeRecentWorkspace(path: string): Promise<void>;
  openWorkspace(path: string): Promise<WorkspaceMeta>;
  createWorkspace(path: string, name: string): Promise<WorkspaceMeta>;
  saveWorkspaceMeta(meta: WorkspaceMeta): Promise<void>;
  getWorkspaceMeta(): Promise<WorkspaceMeta>;

  // Proyectos
  workspaceStats(): Promise<WorkspaceStats>;
  listProjects(): Promise<ProjectSummary[]>;
  createProject(name: string, client: string, projectType: string): Promise<ProjectSummary>;
  createExampleProject(): Promise<ProjectSummary>;
  loadProject(id: string): Promise<ProjectMeta>;
  deleteProject(id: string): Promise<void>;
  saveProject(id: string, meta: ProjectMeta): Promise<void>;

  // Hallazgos
  listFindings(projectId: string): Promise<Finding[]>;
  loadFinding(projectId: string, findingId: string): Promise<Finding>;
  saveFinding(projectId: string, finding: Finding): Promise<Finding>;
  createFinding(projectId: string, title: string): Promise<Finding>;
  deleteFinding(projectId: string, findingId: string): Promise<void>;
  reorderFindings(projectId: string, order: string[]): Promise<void>;
  assignProjectClosure(id: string, name: string, email: string): Promise<ProjectAssignment>;
  reorderProjects(order: string[]): Promise<void>;
  saveAsset(projectId: string, ext: string, dataBase64: string): Promise<string>;
  saveBrandingAsset(ext: string, dataBase64: string): Promise<string>;

  // CVSS
  calcCvss(version: CvssVersion, vector: string): Promise<CvssResult>;

  // Libreria de plantillas
  listFindingTemplates(): Promise<FindingTemplate[]>;
  saveFindingTemplate(template: FindingTemplate): Promise<void>;
  instantiateTemplate(
    projectId: string,
    templateId: string,
    vars: Record<string, string>,
  ): Promise<Finding>;
  listSnippets(): Promise<Snippet[]>;
  saveSnippet(snippet: Snippet): Promise<void>;
  listPdfTemplates(): Promise<PdfTemplate[]>;
  duplicateTemplate(name: string): Promise<string>;
  readTemplateSource(name: string): Promise<string>;
  saveTemplateSource(name: string, content: string): Promise<void>;
  deleteTemplate(name: string): Promise<void>;
  saveTemplateMeta(name: string, title: string, description: string, tags: string[]): Promise<void>;

  // PDF y exportaciones
  generatePdf(projectId: string, alsoExecutive: boolean): Promise<string[]>;
  previewPdf(projectId: string): Promise<string[]>;
  exportCsv(projectId: string, columns: string[]): Promise<string>;
  listExportTargets(): Promise<ExportTarget[]>;
  saveExportTarget(target: ExportTarget): Promise<void>;
  deleteExportTarget(name: string): Promise<void>;
  setExportToken(name: string, token: string): Promise<void>;
  hasExportToken(name: string): Promise<boolean>;
  previewExportPayload(projectId: string, targetName: string): Promise<unknown>;
  sendExportToApi(projectId: string, targetName: string): Promise<ApiSendResult>;
  exportPackage(projectId: string): Promise<PackageExport>;
  importPackage(): Promise<ProjectSummary | null>;
  openPath(path: string): Promise<void>;
  revealPath(path: string): Promise<void>;

  // Git
  gitInit(): Promise<void>;
  gitCommit(message: string): Promise<void>;
  gitStatus(projectId: string): Promise<GitState>;
  gitLog(projectId: string): Promise<GitCommit[]>;
  gitBranches(): Promise<GitBranch[]>;
  gitCommitFiles(projectId: string, hash: string): Promise<GitChange[]>;

  // MCP
  mcpStatus(client: McpClient): Promise<McpStatus>;
  mcpConnect(client: McpClient): Promise<void>;
  mcpDisconnect(client: McpClient): Promise<void>;
}
