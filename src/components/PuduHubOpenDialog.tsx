// SPDX-License-Identifier: GPL-3.0-only
// Copyright (c) 2026 bc0d3

// Proyectos, "Abrir desde PuduHub": informes asignados al usuario. Descarga el
// paquete de la ultima version (Rust crea antes una version con los cambios
// de la web si hace falta) y lo importa como proyecto local (o actualiza la
// copia local si no tiene cambios sin enviar). Todo dato del servidor llega
// validado desde Rust y se muestra como texto.
import { useCallback, useEffect, useState } from "react";
import * as api from "../lib/api";
import { typeLabel } from "../lib/projectTypes";
import {
  formatTimestamp,
  lockSummary,
  toPuduHubError,
  type PuduHubError,
  type PuduHubInforme,
  type PuduHubListResult,
} from "../lib/puduhub";
import { Modal } from "./Modal";
import { useToast } from "./Toast";
import "../styles/puduhub.css";

interface Props {
  onClose: () => void;
  /** Proyecto local abierto o actualizado. */
  onOpened: (projectId: string) => void;
}

export function PuduHubOpenDialog({ onClose, onOpened }: Props) {
  const { notify } = useToast();
  const [query, setQuery] = useState("");
  const [submitted, setSubmitted] = useState("");
  const [page, setPage] = useState(1);
  const [result, setResult] = useState<PuduHubListResult | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<PuduHubError | null>(null);
  const [busyId, setBusyId] = useState<string | null>(null);

  const load = useCallback(async (nextPage: number, text: string) => {
    setLoading(true);
    setError(null);
    try {
      setResult(await api.puduhub().listInformes(nextPage, text));
      setPage(nextPage);
    } catch (e) {
      setError(toPuduHubError(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load(1, "");
  }, [load]);

  async function open(informe: PuduHubInforme) {
    setBusyId(informe.id);
    setError(null);
    try {
      const opened = await api.puduhub().openInforme(informe.id);
      if (opened.aviso) notify(opened.aviso, "info");
      onOpened(opened.proyectoId);
      onClose();
    } catch (e) {
      setError(toPuduHubError(e));
    } finally {
      setBusyId(null);
    }
  }

  const pages = result ? Math.max(1, Math.ceil(result.total / Math.max(1, result.pageSize))) : 1;

  return (
    <Modal title="Abrir desde PuduHub" onClose={onClose} className="hub-dialog">
      <form
        className="row"
        style={{ gap: 6 }}
        onSubmit={(e) => {
          e.preventDefault();
          setSubmitted(query);
          void load(1, query);
        }}
      >
        <input
          className="input"
          style={{ flex: 1 }}
          placeholder="Buscar por titulo..."
          aria-label="Buscar informes"
          maxLength={200}
          value={query}
          onChange={(e) => setQuery(e.target.value)}
        />
        <button className="btn small" type="submit" disabled={loading}>
          Buscar
        </button>
      </form>

      {error && (
        <div className="hub-warning hub-error" role="alert">
          {error.mensaje}
          {error.codigo === "cambios_locales" && error.proyectoId && (
            <div style={{ marginTop: 6 }}>
              <button
                className="btn small"
                onClick={() => {
                  onOpened(error.proyectoId ?? "");
                  onClose();
                }}
              >
                Abrir la copia local
              </button>
            </div>
          )}
        </div>
      )}

      {loading && !result && <p className="faint">Cargando informes...</p>}
      {result && result.items.length === 0 && (
        <p className="faint">No hay informes asignados a su usuario.</p>
      )}
      {result && result.items.length > 0 && (
        <ul className="hub-list">
          {result.items.map((informe) => {
            const local = informe.local;
            return (
              <li key={informe.id} className="hub-row">
                <div className="hub-row-main">
                  <div className="hub-row-title">{informe.titulo}</div>
                  <div className="hub-row-meta">
                    <span className="mini-tag">
                      {informe.tipo ? typeLabel(informe.tipo) : "Sin tipo"}
                    </span>
                    <span className="mini-tag">
                      {informe.estado === "final" ? "Final" : "Borrador"}
                    </span>
                    <span>
                      {informe.versionActual > 0
                        ? `Version ${informe.versionActual}`
                        : "Sin versiones"}
                    </span>
                    {informe.actualizadoAt && (
                      <span>Actualizado {formatTimestamp(informe.actualizadoAt)}</span>
                    )}
                  </div>
                  <div className="hub-row-meta">
                    <span>
                      <i className="ti ti-lock" aria-hidden="true" />{" "}
                      {informe.bloqueo ? `En edicion: ${lockSummary(informe.bloqueo)}` : "Libre"}
                    </span>
                  </div>
                  {local && (
                    <div className="hub-row-meta">
                      <span>
                        En este equipo (base version {local.baseVersion})
                        {local.cambiosSinEnviar ? ", con cambios sin enviar" : ", sin cambios"}
                      </span>
                    </div>
                  )}
                </div>
                <div className="hub-row-actions">
                  {local ? (
                    <>
                      <button
                        className="btn small"
                        onClick={() => {
                          onOpened(local.proyectoId);
                          onClose();
                        }}
                      >
                        Abrir copia local
                      </button>
                      <button
                        className="btn small"
                        disabled={!!busyId || local.cambiosSinEnviar}
                        title={
                          local.cambiosSinEnviar
                            ? "Envie primero los cambios locales a PuduHub"
                            : "Reemplaza la copia local por la ultima version de PuduHub"
                        }
                        onClick={() => void open(informe)}
                      >
                        {busyId === informe.id ? "Descargando..." : "Actualizar copia local"}
                      </button>
                    </>
                  ) : (
                    <button
                      className="btn small primary"
                      disabled={!!busyId}
                      onClick={() => void open(informe)}
                    >
                      {busyId === informe.id ? "Descargando..." : "Descargar y abrir"}
                    </button>
                  )}
                </div>
              </li>
            );
          })}
        </ul>
      )}

      {result && pages > 1 && (
        <div className="row" style={{ gap: 6, justifyContent: "flex-end" }}>
          <button
            className="btn small"
            disabled={loading || page <= 1}
            onClick={() => void load(page - 1, submitted)}
          >
            Anterior
          </button>
          <span className="faint" style={{ fontSize: 12 }}>
            Pagina {page} de {pages}
          </span>
          <button
            className="btn small"
            disabled={loading || page >= pages}
            onClick={() => void load(page + 1, submitted)}
          >
            Siguiente
          </button>
        </div>
      )}
      {result && result.omitidos > 0 && (
        <p className="faint" style={{ fontSize: 11 }}>
          {result.omitidos} informe(s) no se muestran porque PuduHub envio datos no validos.
        </p>
      )}
    </Modal>
  );
}
