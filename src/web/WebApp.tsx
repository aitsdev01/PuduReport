// SPDX-License-Identifier: GPL-3.0-only
// Copyright (c) 2026 bc0d3

// Shell del editor web (modo servidor): un solo informe de PuduHub, con las
// mismas vistas de edicion que la app de escritorio. Sin workspaces, tablero,
// git, MCP ni plantillas: eso queda en escritorio.
import { useCallback, useEffect, useRef, useState } from "react";
import * as api from "../lib/api";
import type { ProjectMeta, WorkspaceMeta } from "../lib/types";
import { familyForProject, usesFreeMarkdown } from "../lib/projectTypes";
import { flushWrites, hasPendingWrites } from "../lib/pendingWrites";
import {
  formatHasta,
  holderText,
  type HttpBackend,
  type LockHolder,
  type SessionEvent,
} from "../lib/backend/http";
import { ToastProvider, useToast } from "../components/Toast";
import { FindingEditor } from "../views/FindingEditor";
import { ContentEditor } from "../views/ContentEditor";
import { ReportBuilder } from "../views/ReportBuilder";
import { PdfPreview } from "../screens/PdfPreview";

type WebView = "editor" | "reporte" | "preview";

type OpenState =
  | { kind: "opening" }
  | { kind: "ready" }
  | ({ kind: "lock_held"; puedeForzar: boolean } & LockHolder)
  | { kind: "error"; message: string };

interface Props {
  backend: HttpBackend;
}

export function WebApp({ backend }: Props) {
  return (
    <ToastProvider>
      <WebInner backend={backend} />
    </ToastProvider>
  );
}

/** Mensaje de un evento que detiene el guardado, sin descartar lo que hay en pantalla. */
function noticeText(event: SessionEvent): string {
  const keep =
    "Lo que no alcanzo a guardarse sigue en pantalla: copialo antes de recargar si lo necesitas.";
  switch (event.kind) {
    case "stale_revision":
      return `El informe cambio en otra sesion y ya no se guardan tus cambios. ${keep}`;
    case "lock_held":
      return `Otra sesion tomo el bloqueo de edicion. ${holderText(event)} ${keep}`;
    case "auth":
      return `La sesion de PuduHub expiro. Inicia sesion de nuevo y recarga. ${keep}`;
    default:
      return `Se perdio el bloqueo de edicion. ${keep}`;
  }
}

function WebInner({ backend }: Props) {
  const { guard } = useToast();
  const projectId = backend.projectId;
  const [state, setState] = useState<OpenState>({ kind: "opening" });
  const [project, setProject] = useState<ProjectMeta | null>(null);
  const [workspace, setWorkspace] = useState<WorkspaceMeta | null>(null);
  const [lockUntil, setLockUntil] = useState<string | null>(null);
  const [notice, setNotice] = useState<SessionEvent | null>(null);
  const [view, setView] = useState<WebView>("editor");
  const navigating = useRef(false);
  const [dark, setDark] = useState<boolean>(() => {
    try {
      const saved = window.localStorage.getItem("pudu-theme");
      if (saved) return saved === "dark";
    } catch {
      // Almacenamiento bloqueado: se usa la preferencia del sistema.
    }
    return window.matchMedia?.("(prefers-color-scheme: dark)").matches ?? true;
  });

  useEffect(() => {
    document.documentElement.classList.toggle("dark", dark);
    try {
      window.localStorage.setItem("pudu-theme", dark ? "dark" : "light");
    } catch {
      // Sin almacenamiento: el tema vale solo para esta pestaña.
    }
  }, [dark]);

  useEffect(
    () =>
      backend.onSessionEvent((event) => {
        if (event.kind === "locked") setLockUntil(event.hasta);
        else setNotice(event);
      }),
    [backend],
  );

  const open = useCallback(
    async (forzar = false) => {
      setState({ kind: "opening" });
      try {
        const result = await backend.open(forzar);
        if (result.kind === "lock_held") {
          setState(result);
          return;
        }
        const [meta, ws] = await Promise.all([api.loadProject(projectId), api.getWorkspaceMeta()]);
        setProject(meta);
        setWorkspace(ws);
        setState({ kind: "ready" });
      } catch (error) {
        setState({ kind: "error", message: String(error) });
      }
    },
    [backend, projectId],
  );

  useEffect(() => {
    void open();
  }, [open]);

  useEffect(() => {
    api.setWindowTitle(project ? `${project.name} — PuduReport` : "PuduReport");
  }, [project]);

  // Al cerrar la pestaña: intenta guardar y avisa si quedan cambios en vuelo.
  useEffect(() => {
    function beforeUnload(event: BeforeUnloadEvent) {
      void flushWrites().catch(() => {});
      if (hasPendingWrites()) {
        event.preventDefault();
        event.returnValue = "";
      }
    }
    window.addEventListener("beforeunload", beforeUnload);
    return () => window.removeEventListener("beforeunload", beforeUnload);
  }, []);

  async function navigate(next: WebView) {
    if (navigating.current || next === view) return;
    navigating.current = true;
    try {
      const saved = await guard(flushWrites().then(() => true));
      // Si el guardado esta detenido (aviso visible), igual se puede mirar
      // otra vista: lo pendiente queda en memoria.
      if (saved || notice) setView(next);
    } finally {
      navigating.current = false;
    }
  }

  if (state.kind === "opening") {
    return (
      <div className="shell-state" role="status">
        <i className="ti ti-hourglass" aria-hidden="true" />
        <h2>Abriendo informe</h2>
        <p>Cargando el informe y tomando el bloqueo de edicion...</p>
      </div>
    );
  }

  if (state.kind === "lock_held" || state.kind === "error") {
    const held = state.kind === "lock_held";
    return (
      <div className="shell-state" role="alert">
        <i className={`ti ${held ? "ti-lock" : "ti-alert-circle"}`} aria-hidden="true" />
        <h2>{held ? "Informe en edicion" : "No se pudo abrir el informe"}</h2>
        <p>
          {held
            ? `${holderText(state)} ${state.puedeForzar ? "Puedes tomar el bloqueo aqui; la otra sesion dejara de poder guardar." : "Puedes reintentar cuando libere el bloqueo."}`
            : state.message}
        </p>
        <div className="row">
          <button className="btn primary" onClick={() => void open()}>
            Reintentar
          </button>
          {held && state.puedeForzar && (
            <button className="btn" onClick={() => void open(true)}>
              Tomar el bloqueo aqui
            </button>
          )}
        </div>
      </div>
    );
  }

  const freeMarkdown = usesFreeMarkdown(project);
  const family = familyForProject(project, []);
  const tabs: { view: WebView; label: string; icon: string }[] = [
    {
      view: "editor",
      label: freeMarkdown ? "Contenido" : "Hallazgos",
      icon: freeMarkdown ? "ti-markdown" : "ti-bug",
    },
    { view: "reporte", label: "Reporte", icon: "ti-file-text" },
    { view: "preview", label: "PDF", icon: "ti-file-type-pdf" },
  ];
  const lockHora = formatHasta(lockUntil);

  return (
    <div className="shell web-shell">
      <a className="skip-link" href="#workspace-content">
        Ir al contenido
      </a>
      <div className="workspace-shell">
        <header className="workspace-bar">
          <span className="workspace-name">PuduReport</span>
          <span className="breadcrumb-divider" aria-hidden="true">
            /
          </span>
          <span className="workspace-location" title={project?.client}>
            {project?.name ?? "Informe"}
          </span>
          <span className="web-lock" role="status">
            <i className={`ti ${notice ? "ti-lock-open" : "ti-lock"}`} aria-hidden="true" />
            {notice
              ? "Guardado detenido"
              : lockHora
                ? `Editando · bloqueo hasta las ${lockHora}`
                : "Editando"}
          </span>
          <button
            className="icon-btn"
            aria-label={dark ? "Usar tema claro" : "Usar tema oscuro"}
            title={dark ? "Tema claro" : "Tema oscuro"}
            onClick={() => setDark((d) => !d)}
          >
            <i className={`ti ${dark ? "ti-sun" : "ti-moon"}`} aria-hidden="true" />
          </button>
        </header>
        <nav className="project-tabs" aria-label="Vistas del informe">
          {tabs.map((tab) => (
            <div className={`project-tab ${view === tab.view ? "selected" : ""}`} key={tab.view}>
              <button
                className="project-tab-select"
                aria-current={view === tab.view ? "page" : undefined}
                onClick={() => void navigate(tab.view)}
              >
                <i className={`ti ${tab.icon}`} aria-hidden="true" />
                <span>{tab.label}</span>
              </button>
            </div>
          ))}
        </nav>
        {notice && (
          <div className="web-notice" role="alert">
            <i className="ti ti-alert-triangle" aria-hidden="true" />
            <span>{noticeText(notice)}</span>
            {notice.kind === "auth" ? (
              <a className="btn" href="/auth/login?expired=1" target="_blank" rel="noopener">
                Iniciar sesion
              </a>
            ) : null}
            <button className="btn" onClick={() => window.location.reload()}>
              Recargar
            </button>
          </div>
        )}
        <main className="content" id="workspace-content" tabIndex={-1}>
          {view === "editor" &&
            (freeMarkdown ? (
              <ContentEditor
                projectId={projectId}
                assetBase={projectId}
                onGoToPreview={() => void navigate("preview")}
                onPickProject={() => undefined}
              />
            ) : (
              <FindingEditor
                projectId={projectId}
                assetBase={projectId}
                projectType={project?.project_type}
                family={family}
                onGoToPreview={() => void navigate("preview")}
                onPickProject={() => undefined}
              />
            ))}
          {view === "reporte" && workspace && (
            <ReportBuilder
              workspace={workspace}
              projectId={projectId}
              assetBase={projectId}
              onWorkspaceSaved={setWorkspace}
              onProjectMetaChange={setProject}
              onGoToPreview={() => void navigate("preview")}
              onPickProject={() => undefined}
            />
          )}
          {view === "preview" && (
            <PdfPreview projectId={projectId} onPickProject={() => undefined} />
          )}
        </main>
      </div>
    </div>
  );
}
