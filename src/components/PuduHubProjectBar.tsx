// SPDX-License-Identifier: GPL-3.0-only
// Copyright (c) 2026 bc0d3

// Barra del proyecto: indicador "Vinculado a PuduHub, version N; cambios sin
// enviar: si/no" y las acciones Enviar, Actualizar y Subir como nuevo
// informe. Los conflictos, el bloqueo tomado y los demas errores se muestran
// por codigo; nunca se pisa nada local sin dejar un respaldo.
import { useCallback, useEffect, useState } from "react";
import * as api from "../lib/api";
import {
  changesSummary,
  conflictLabels,
  linkIndicator,
  lockSummary,
  toPuduHubError,
  type PuduHubError,
  type PuduHubLinkStatus,
  type PuduHubSyncOutcome,
} from "../lib/puduhub";
import { Modal } from "./Modal";
import { useToast } from "./Toast";
import "../styles/puduhub.css";

interface Props {
  projectId: string;
  /** Guarda lo pendiente antes de exportar; false si no se pudo. */
  onBeforeSync: () => Promise<boolean>;
  /** El proyecto local cambio en disco (recargar editores y listados). */
  onReplaced: () => void;
}

type Dialog =
  | { kind: "result"; outcome: PuduHubSyncOutcome; nuevo: boolean }
  | { kind: "error"; error: PuduHubError }
  | { kind: "confirmUpload" }
  | { kind: "confirmDownload" };

type Busy = "" | "enviar" | "subir" | "descargar";

const REFRESH_MS = 20_000;

export function PuduHubProjectBar({ projectId, onBeforeSync, onReplaced }: Props) {
  const { notify } = useToast();
  const [connected, setConnected] = useState(false);
  const [status, setStatus] = useState<PuduHubLinkStatus | null>(null);
  const [busy, setBusy] = useState<Busy>("");
  const [dialog, setDialog] = useState<Dialog | null>(null);

  useEffect(() => {
    let cancelled = false;
    api
      .puduhub()
      .getConnection()
      .then((c) => {
        if (!cancelled) setConnected(!!c.servidor && c.tieneToken);
      })
      .catch(() => {
        if (!cancelled) setConnected(false);
      });
    return () => {
      cancelled = true;
    };
  }, [projectId]);

  const refresh = useCallback(async () => {
    try {
      setStatus(await api.puduhub().linkStatus(projectId));
    } catch {
      setStatus(null);
    }
  }, [projectId]);

  useEffect(() => {
    setStatus(null);
    void refresh();
    const timer = window.setInterval(() => void refresh(), REFRESH_MS);
    const onFocus = () => void refresh();
    window.addEventListener("focus", onFocus);
    return () => {
      window.clearInterval(timer);
      window.removeEventListener("focus", onFocus);
    };
  }, [refresh]);

  async function run<T>(kind: Busy, action: () => Promise<T>): Promise<T | undefined> {
    setBusy(kind);
    try {
      return await action();
    } catch (e) {
      setDialog({ kind: "error", error: toPuduHubError(e) });
      return undefined;
    } finally {
      setBusy("");
      void refresh();
    }
  }

  async function send(forzar: boolean) {
    setDialog(null);
    if (!(await onBeforeSync())) return;
    const outcome = await run("enviar", () => api.puduhub().sendProject(projectId, forzar));
    if (!outcome) return;
    if (outcome.aplicado) onReplaced();
    setDialog({ kind: "result", outcome, nuevo: false });
  }

  async function upload() {
    setDialog(null);
    if (!(await onBeforeSync())) return;
    const outcome = await run("subir", () => api.puduhub().uploadNew(projectId));
    if (!outcome) return;
    if (outcome.aplicado) onReplaced();
    setDialog({ kind: "result", outcome, nuevo: true });
  }

  // Sin cambios locales: reemplaza por la ultima version, sin respaldo.
  async function update() {
    if (!status?.informeId) return;
    if (status.cambiosSinEnviar) {
      setDialog({ kind: "confirmDownload" });
      return;
    }
    if (!(await onBeforeSync())) return;
    const informeId = status.informeId;
    const opened = await run("descargar", () => api.puduhub().openInforme(informeId));
    if (!opened) return;
    onReplaced();
    notify(`Proyecto actualizado a la version ${opened.version} de PuduHub`, "ok");
    if (opened.aviso) notify(opened.aviso, "info");
  }

  // Con respaldo: el proyecto actual queda como un proyecto aparte.
  async function downloadServerVersion() {
    setDialog(null);
    if (!(await onBeforeSync())) return;
    const result = await run("descargar", () => api.puduhub().downloadServerVersion(projectId));
    if (!result) return;
    onReplaced();
    notify(
      result.respaldoId
        ? `Version ${result.version} descargada. Su copia anterior quedo como proyecto "${result.respaldoId}".`
        : `Version ${result.version} descargada.`,
      "ok",
    );
    if (result.aviso) notify(result.aviso, "info");
  }

  if (!status || (!status.vinculado && !status.vinculoInvalido && !connected)) return null;

  const indicator = linkIndicator(status);

  return (
    <div className="hub-bar">
      {status.vinculado || status.vinculoInvalido ? (
        <>
          <span className="hub-status" title={indicator} aria-label={indicator} role="status">
            <span
              className={`hub-dot ${
                status.vinculoInvalido ? "invalid" : status.cambiosSinEnviar ? "pending" : ""
              }`}
              aria-hidden="true"
            />
            {status.vinculoInvalido
              ? "PuduHub: vinculo no valido"
              : `PuduHub v${status.baseVersion ?? "-"} · ${
                  status.cambiosSinEnviar ? "cambios sin enviar" : "al dia"
                }`}
          </span>
          {status.vinculado && (
            <>
              <button
                className="btn small"
                disabled={!!busy || !status.mismoServidor}
                title={
                  status.mismoServidor
                    ? "Descargar la ultima version de PuduHub"
                    : `Vinculado a otro servidor (${status.servidor ?? ""})`
                }
                onClick={() => void update()}
              >
                <i className="ti ti-cloud-download" aria-hidden="true" />
                {busy === "descargar" ? "Descargando..." : "Actualizar"}
              </button>
              <button
                className="btn small primary"
                disabled={!!busy || !status.mismoServidor}
                title={
                  status.mismoServidor
                    ? "Enviar los cambios a PuduHub"
                    : `Vinculado a otro servidor (${status.servidor ?? ""})`
                }
                onClick={() => void send(false)}
              >
                <i className="ti ti-cloud-upload" aria-hidden="true" />
                {busy === "enviar" ? "Enviando..." : "Enviar a PuduHub"}
              </button>
            </>
          )}
        </>
      ) : (
        <button
          className="btn small"
          disabled={!!busy}
          title="Crear un informe nuevo en PuduHub con este proyecto"
          onClick={() => setDialog({ kind: "confirmUpload" })}
        >
          <i className="ti ti-cloud-upload" aria-hidden="true" />
          {busy === "subir" ? "Subiendo..." : "Subir a PuduHub"}
        </button>
      )}

      {dialog?.kind === "confirmUpload" && (
        <Modal
          title="Subir como nuevo informe"
          onClose={() => setDialog(null)}
          footer={
            <>
              <button className="btn" onClick={() => setDialog(null)}>
                Cancelar
              </button>
              <button className="btn primary" onClick={() => void upload()}>
                Crear informe y enviar
              </button>
            </>
          }
        >
          <p className="muted" style={{ margin: 0, lineHeight: 1.5 }}>
            Se creara un informe nuevo en PuduHub con el nombre y el tipo de este proyecto, y se
            enviara su contenido completo (hallazgos, secciones y evidencias). Desde ahi el proyecto
            queda vinculado al informe.
          </p>
        </Modal>
      )}

      {dialog?.kind === "confirmDownload" && (
        <Modal
          title="Descargar version del servidor"
          onClose={() => setDialog(null)}
          footer={
            <>
              <button className="btn" onClick={() => setDialog(null)}>
                Cancelar
              </button>
              <button className="btn primary" onClick={() => void downloadServerVersion()}>
                Descargar y guardar respaldo
              </button>
            </>
          }
        >
          <p className="muted" style={{ margin: 0, lineHeight: 1.5 }}>
            Este proyecto tiene cambios sin enviar. Se guardara una copia completa como proyecto
            aparte (respaldo local) y luego se reemplazara por la ultima version de PuduHub.
          </p>
        </Modal>
      )}

      {dialog?.kind === "result" && (
        <Modal
          title={dialog.nuevo ? "Informe creado en PuduHub" : "Cambios enviados a PuduHub"}
          onClose={() => setDialog(null)}
          footer={
            <button className="btn primary" onClick={() => setDialog(null)}>
              Listo
            </button>
          }
        >
          <SyncSummary outcome={dialog.outcome} />
        </Modal>
      )}

      {dialog?.kind === "error" && (
        <SyncErrorDialog
          error={dialog.error}
          onClose={() => setDialog(null)}
          onForce={() => void send(true)}
          onDownload={() => void downloadServerVersion()}
        />
      )}
    </div>
  );
}

function SyncSummary({ outcome }: { outcome: PuduHubSyncOutcome }) {
  const lines = changesSummary(outcome.cambios);
  return (
    <div style={{ fontSize: 13, lineHeight: 1.5 }}>
      <p style={{ marginTop: 0 }}>
        PuduHub creo la <strong>version {outcome.version}</strong> del informe.
      </p>
      {outcome.aplicado &&
        (lines.length ? (
          <>
            <p className="muted" style={{ marginBottom: 4 }}>
              Se incorporaron cambios que estaban en PuduHub:
            </p>
            <ul className="hub-conflicts">
              {lines.map((line) => (
                <li key={line}>{line}</li>
              ))}
            </ul>
          </>
        ) : (
          <p className="muted">
            No habia cambios de otras personas: el proyecto local queda igual.
          </p>
        ))}
      {outcome.aviso && <div className="hub-warning">{outcome.aviso}</div>}
      {outcome.omitidos.length > 0 && (
        <div className="hub-warning">
          No se enviaron estos adjuntos porque no son imagenes permitidas (PNG, JPEG, GIF o WebP):{" "}
          {outcome.omitidos.join(", ")}
        </div>
      )}
    </div>
  );
}

interface ErrorProps {
  error: PuduHubError;
  onClose: () => void;
  onForce: () => void;
  onDownload: () => void;
}

function SyncErrorDialog({ error, onClose, onForce, onDownload }: ErrorProps) {
  const conflicts = conflictLabels(error);
  const isConflict = error.codigo === "conflicto";
  const canDownload = isConflict || error.codigo === "version_base_desconocida";
  const settingsHint = [
    "no_autorizado",
    "sin_token",
    "sin_conexion",
    "tls",
    "url_invalida",
  ].includes(error.codigo);
  return (
    <Modal
      title={isConflict ? "Conflictos con PuduHub" : "No se pudo completar"}
      onClose={onClose}
      className="hub-dialog"
      footer={
        <>
          <button className="btn" onClick={onClose}>
            {canDownload ? "Cancelar" : "Cerrar"}
          </button>
          {error.codigo === "bloqueo_tomado" && error.puedeForzar && (
            <button className="btn primary" onClick={onForce}>
              Tomar el bloqueo y enviar
            </button>
          )}
          {canDownload && (
            <button className="btn primary" onClick={onDownload}>
              Descargar version del servidor
            </button>
          )}
        </>
      }
    >
      <p style={{ marginTop: 0, fontSize: 13, lineHeight: 1.5 }}>{error.mensaje}</p>
      {error.bloqueo && <div className="hub-warning">{lockSummary(error.bloqueo)}</div>}
      {conflicts.length > 0 && (
        <>
          <p className="muted" style={{ marginBottom: 4, fontSize: 12 }}>
            Partes cambiadas en PuduHub y en este equipo:
          </p>
          <ul className="hub-conflicts">
            {conflicts.map((label) => (
              <li key={`${label.tipo}-${label.nombre}`}>
                <strong>{label.tipo}:</strong> {label.nombre}
              </li>
            ))}
          </ul>
        </>
      )}
      {canDownload && (
        <div className="hub-warning">
          "Descargar version del servidor" guarda primero una copia completa de este proyecto como
          proyecto aparte (respaldo local) y luego lo reemplaza por la version de PuduHub. Puede
          copiar sus cambios desde el respaldo y volver a enviar.
        </div>
      )}
      {error.reintentarEn !== undefined && (
        <p className="faint" style={{ fontSize: 12 }}>
          Reintente en {error.reintentarEn} segundos.
        </p>
      )}
      {settingsHint && (
        <p className="faint" style={{ fontSize: 12 }}>
          Revise la conexion en Ajustes, "Conexion con PuduHub".
        </p>
      )}
    </Modal>
  );
}
