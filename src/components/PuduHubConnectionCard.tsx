// SPDX-License-Identifier: GPL-3.0-only
// Copyright (c) 2026 bc0d3

// Ajustes, "Conexion con PuduHub": URL base y token personal de editor. El
// token se envia una sola vez al backend Rust, que lo guarda en el keychain;
// el campo se vacia al guardar y nunca se vuelve a leer.
import { useEffect, useState } from "react";
import * as api from "../lib/api";
import { toPuduHubError, type PuduHubConnectionView } from "../lib/puduhub";
import { ConfirmDialog } from "./ConfirmDialog";
import { useToast } from "./Toast";
import "../styles/puduhub.css";

type Busy = "" | "guardar" | "probar" | "quitar";

export function PuduHubConnectionCard() {
  const { notify } = useToast();
  const [view, setView] = useState<PuduHubConnectionView | null>(null);
  const [servidor, setServidor] = useState("");
  const [token, setToken] = useState("");
  const [busy, setBusy] = useState<Busy>("");
  const [result, setResult] = useState<{ ok: boolean; text: string } | null>(null);
  const [confirmRemove, setConfirmRemove] = useState(false);

  useEffect(() => {
    let cancelled = false;
    api
      .puduhub()
      .getConnection()
      .then((current) => {
        if (cancelled) return;
        setView(current);
        setServidor(current.servidor ?? "");
      })
      .catch((error: unknown) => {
        if (!cancelled) setResult({ ok: false, text: toPuduHubError(error).mensaje });
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const dirty = servidor.trim() !== (view?.servidor ?? "") || token.trim() !== "";

  async function save(): Promise<boolean> {
    try {
      const saved = await api.puduhub().saveConnection(servidor, token);
      setView(saved);
      setServidor(saved.servidor ?? "");
      setToken("");
      return true;
    } catch (error) {
      setResult({ ok: false, text: toPuduHubError(error).mensaje });
      return false;
    }
  }

  async function handleSave() {
    setBusy("guardar");
    setResult(null);
    if (await save()) notify("Conexion con PuduHub guardada", "ok");
    setBusy("");
  }

  async function handleTest() {
    setBusy("probar");
    setResult(null);
    try {
      if (dirty && !(await save())) return;
      const tested = await api.puduhub().testConnection();
      setResult({
        ok: true,
        text: `Conexion correcta con ${tested.servidor}: ${tested.total} informe(s) disponibles para su usuario.`,
      });
    } catch (error) {
      setResult({ ok: false, text: toPuduHubError(error).mensaje });
    } finally {
      setBusy("");
    }
  }

  async function handleRemove() {
    setBusy("quitar");
    setResult(null);
    try {
      await api.puduhub().deleteConnection();
      setView({ servidor: null, tieneToken: false });
      setServidor("");
      setToken("");
      notify("Conexion con PuduHub quitada", "ok");
    } catch (error) {
      setResult({ ok: false, text: toPuduHubError(error).mensaje });
    } finally {
      setBusy("");
    }
  }

  return (
    <div className="card">
      <div className="row" style={{ gap: 10, alignItems: "flex-start" }}>
        <i className="ti ti-cloud-up" style={{ color: "var(--accent)" }} aria-hidden="true" />
        <div style={{ flex: 1 }}>
          <strong>Conexion con PuduHub</strong>
          <div className="faint" style={{ fontSize: 12, lineHeight: 1.5 }}>
            Abra informes de PuduHub, editelos sin conexion y envie los cambios. El token se guarda
            solo en el llavero del sistema, nunca en archivos del workspace.
          </div>
        </div>
      </div>

      <div className="field" style={{ marginTop: 12 }}>
        <label className="field-label-top" htmlFor="puduhub-url">
          URL de PuduHub
        </label>
        <input
          id="puduhub-url"
          className="input mono"
          placeholder="https://hub.ejemplo.cl"
          value={servidor}
          autoComplete="off"
          spellCheck={false}
          onChange={(e) => setServidor(e.target.value)}
        />
        <div className="faint" style={{ fontSize: 11, marginTop: 4 }}>
          Solo https. Para desarrollo se acepta http://localhost o http://127.0.0.1.
        </div>
      </div>

      <div className="field">
        <label className="field-label-top" htmlFor="puduhub-token">
          Token de editor
        </label>
        <input
          id="puduhub-token"
          className="input mono"
          type="password"
          autoComplete="off"
          spellCheck={false}
          placeholder={
            view?.tieneToken
              ? "Guardado en el llavero (escriba uno nuevo para reemplazarlo)"
              : "phed_..."
          }
          value={token}
          onChange={(e) => setToken(e.target.value)}
        />
        <div className="faint" style={{ fontSize: 11, marginTop: 4 }}>
          Generelo en PuduHub, en Tokens de editor. Vence segun lo que elija al crearlo (hasta 90
          dias).
        </div>
      </div>

      {result && (
        <div className={`hub-warning ${result.ok ? "" : "hub-error"}`} role="status">
          {result.text}
        </div>
      )}

      <div className="row" style={{ gap: 6, justifyContent: "flex-end" }}>
        {view?.servidor && (
          <button className="btn small" disabled={!!busy} onClick={() => setConfirmRemove(true)}>
            Quitar conexion
          </button>
        )}
        <button className="btn small" disabled={!!busy || !servidor.trim()} onClick={handleTest}>
          {busy === "probar" ? "Probando..." : "Probar conexion"}
        </button>
        <button
          className="btn small primary"
          disabled={!!busy || !servidor.trim() || !dirty}
          onClick={handleSave}
        >
          {busy === "guardar" ? "Guardando..." : "Guardar"}
        </button>
      </div>

      {confirmRemove && (
        <ConfirmDialog
          title="Quitar conexion con PuduHub"
          message="Se borra la URL de este workspace y el token del llavero. Los proyectos vinculados conservan sus archivos."
          confirmLabel="Quitar"
          onConfirm={() => void handleRemove()}
          onClose={() => setConfirmRemove(false)}
        />
      )}
    </div>
  );
}
