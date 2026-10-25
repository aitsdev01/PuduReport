// SPDX-License-Identifier: GPL-3.0-only
// Copyright (c) 2026 bc0d3

// Capa de datos de escritorio: IPC con el backend Rust (Tauri). Es el unico
// modulo de la app que importa @tauri-apps; el editor web no lo incluye.
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import type { Backend, Capabilities } from "./types";
import type { WorkspaceMeta } from "../types";

const DESKTOP: Capabilities = {
  mode: "desktop",
  workspaces: true,
  projectList: true,
  git: true,
  mcp: true,
  exportTargets: true,
  csvExport: true,
  templateEditing: true,
  branding: true,
  localFiles: true,
  pagePreview: true,
  packages: true,
};

/** Convierte una ruta local a URL del protocolo asset; ante error, la deja igual. */
function fileSrc(path: string): string {
  try {
    return convertFileSrc(path);
  } catch {
    return path;
  }
}

export function createTauriBackend(): Backend {
  // Raiz del workspace abierto, para resolver evidencias y marca a URLs.
  let root: string | null = null;

  async function remember(path: string, opened: Promise<WorkspaceMeta>) {
    const meta = await opened;
    root = path;
    return meta;
  }

  return {
    capabilities: DESKTOP,

    assetUrl: (projectId, relPath) => (root ? fileSrc(`${root}/${projectId}/${relPath}`) : relPath),
    brandingAssetUrl: (path) => (root && path ? fileSrc(`${root}${path}`) : ""),
    setWindowTitle(title) {
      try {
        getCurrentWindow()
          .setTitle(title)
          .catch(() => {});
      } catch {
        // Fuera de la ventana de Tauri (preview en navegador): ignorar.
      }
    },
    lastGeneratedPdf: () => null,
    // En escritorio el PDF ya esta en disco (openPath / revealPath).
    downloadGeneratedPdf: () => Promise.reject("No disponible en escritorio."),

    pickWorkspace: () => invoke("pick_workspace"),
    getStoredWorkspace: () => invoke("get_stored_workspace"),
    listRecentWorkspaces: () => invoke("list_recent_workspaces"),
    removeRecentWorkspace: (path) => invoke("remove_recent_workspace", { path }),
    openWorkspace: (path) => remember(path, invoke("open_workspace", { path })),
    createWorkspace: (path, name) => remember(path, invoke("create_workspace", { path, name })),
    saveWorkspaceMeta: (meta) => invoke("save_workspace_meta", { meta }),
    getWorkspaceMeta: () => invoke("get_workspace_meta"),

    workspaceStats: () => invoke("workspace_stats"),
    listProjects: () => invoke("list_projects"),
    createProject: (name, client, projectType) =>
      invoke("create_project", { name, client, projectType }),
    createExampleProject: () => invoke("create_example_project"),
    loadProject: (id) => invoke("load_project", { id }),
    deleteProject: (id) => invoke("delete_project", { id }),
    saveProject: (id, meta) => invoke("save_project", { id, meta }),

    listFindings: (projectId) => invoke("list_findings", { projectId }),
    loadFinding: (projectId, findingId) => invoke("load_finding", { projectId, findingId }),
    saveFinding: (projectId, finding) => invoke("save_finding", { projectId, finding }),
    createFinding: (projectId, title) => invoke("create_finding", { projectId, title }),
    deleteFinding: (projectId, findingId) => invoke("delete_finding", { projectId, findingId }),
    reorderFindings: (projectId, order) => invoke("reorder_findings", { projectId, order }),
    assignProjectClosure: (id, name, email) =>
      invoke("assign_project_closure", { projectId: id, name, email }),
    reorderProjects: (order) => invoke("reorder_projects", { order }),
    saveAsset: (projectId, ext, dataBase64) => invoke("save_asset", { projectId, ext, dataBase64 }),
    saveBrandingAsset: (ext, dataBase64) => invoke("save_branding_asset", { ext, dataBase64 }),

    calcCvss: (version, vector) => invoke("calc_cvss", { version, vector }),

    listFindingTemplates: () => invoke("list_finding_templates"),
    saveFindingTemplate: (template) => invoke("save_finding_template", { template }),
    instantiateTemplate: (projectId, templateId, vars) =>
      invoke("instantiate_template", { projectId, templateId, vars }),
    listSnippets: () => invoke("list_snippets"),
    saveSnippet: (snippet) => invoke("save_snippet", { snippet }),
    listPdfTemplates: () => invoke("list_pdf_templates"),
    duplicateTemplate: (name) => invoke("duplicate_template", { name }),
    readTemplateSource: (name) => invoke("read_template_source", { name }),
    saveTemplateSource: (name, content) => invoke("save_template_source", { name, content }),
    deleteTemplate: (name) => invoke("delete_template", { name }),
    saveTemplateMeta: (name, title, description, tags) =>
      invoke("save_template_meta", { name, title, description, tags }),

    generatePdf: (projectId, alsoExecutive) => invoke("generate_pdf", { projectId, alsoExecutive }),
    previewPdf: (projectId) => invoke("preview_pdf", { projectId }),
    exportCsv: (projectId, columns) => invoke("export_csv", { projectId, columns }),
    listExportTargets: () => invoke("list_export_targets"),
    saveExportTarget: (target) => invoke("save_export_target", { target }),
    deleteExportTarget: (name) => invoke("delete_export_target", { name }),
    setExportToken: (name, token) => invoke("set_export_token", { name, token }),
    hasExportToken: (name) => invoke("has_export_token", { name }),
    previewExportPayload: (projectId, targetName) =>
      invoke("preview_export_payload", { projectId, targetName }),
    sendExportToApi: (projectId, targetName) =>
      invoke("send_export_to_api", { projectId, targetName }),
    exportPackage: (projectId) => invoke("export_package", { projectId }),
    importPackage: () => invoke("import_package"),
    openPath: (path) => invoke("open_path", { path }),
    revealPath: (path) => invoke("reveal_path", { path }),

    gitInit: () => invoke("git_init"),
    gitCommit: (message) => invoke("git_commit", { message }),
    gitStatus: (projectId) => invoke("git_status", { projectId }),
    gitLog: (projectId) => invoke("git_log", { projectId }),
    gitBranches: () => invoke("git_branches"),
    gitCommitFiles: (projectId, hash) => invoke("git_commit_files", { projectId, hash }),

    mcpStatus: (client) => invoke("mcp_status", { client }),
    mcpConnect: (client) => invoke("mcp_connect", { client }),
    mcpDisconnect: (client) => invoke("mcp_disconnect", { client }),
  };
}
