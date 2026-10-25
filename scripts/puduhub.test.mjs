// SPDX-License-Identifier: GPL-3.0-only
// Copyright (c) 2026 bc0d3
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import ts from "typescript";

const source = readFileSync(new URL("../src/lib/puduhub.ts", import.meta.url), "utf8");
const { outputText } = ts.transpileModule(source, {
  compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2022 },
});
const {
  PROJECT_FIELD_LABELS,
  changesSummary,
  conflictLabel,
  conflictLabels,
  formatTimestamp,
  linkIndicator,
  lockSummary,
  toPuduHubError,
} = await import(`data:text/javascript;base64,${Buffer.from(outputText).toString("base64")}`);

const context = {
  hallazgos: { "001-sqli": "Inyeccion SQL en login" },
  secciones: { resumen: "Resumen ejecutivo" },
};

test("conflictos con titulos locales", () => {
  assert.deepEqual(conflictLabel({ entidad: "hallazgo", id: "001-sqli" }, context), {
    tipo: "Hallazgo",
    nombre: "Inyeccion SQL en login",
  });
  assert.deepEqual(conflictLabel({ entidad: "seccion", id: "resumen" }, context), {
    tipo: "Seccion",
    nombre: "Resumen ejecutivo",
  });
  assert.deepEqual(conflictLabel({ entidad: "proyecto", campo: "client" }, context), {
    tipo: "Proyecto",
    nombre: "Cliente",
  });
  assert.deepEqual(conflictLabel({ entidad: "proyecto", campo: "orden_secciones" }), {
    tipo: "Proyecto",
    nombre: "Orden de las secciones",
  });
  assert.deepEqual(conflictLabel({ entidad: "orden" }), {
    tipo: "Orden",
    nombre: "Orden de los hallazgos",
  });
  assert.deepEqual(conflictLabel({ entidad: "asset", id: "assets/captura-01.png" }), {
    tipo: "Evidencia",
    nombre: "captura-01.png",
  });
});

test("sin titulo local se usa el id o la clave del campo", () => {
  assert.equal(
    conflictLabel({ entidad: "hallazgo", id: "009-nuevo" }, context).nombre,
    "009-nuevo",
  );
  assert.equal(conflictLabel({ entidad: "seccion", id: "anexo" }).nombre, "anexo");
  assert.equal(
    conflictLabel({ entidad: "proyecto", campo: "campo_futuro" }).nombre,
    "campo_futuro",
  );
  assert.equal(conflictLabel({ entidad: "proyecto" }).nombre, "Datos del proyecto");
  assert.equal(conflictLabel({ entidad: "hallazgo" }).nombre, "Hallazgo sin identificar");
  assert.equal(
    conflictLabel(
      { entidad: "hallazgo", id: "001-sqli" },
      { hallazgos: { "001-sqli": "  " }, secciones: {} },
    ).nombre,
    "001-sqli",
  );
});

test("etiquetas de un error sin repetidos", () => {
  const labels = conflictLabels({
    codigo: "conflicto",
    mensaje: "x",
    conflictos: [
      { entidad: "hallazgo", id: "001-sqli" },
      { entidad: "hallazgo", id: "001-sqli" },
      { entidad: "proyecto", campo: "team" },
    ],
    contexto: context,
  });
  assert.deepEqual(
    labels.map((l) => `${l.tipo}: ${l.nombre}`),
    ["Hallazgo: Inyeccion SQL en login", "Proyecto: Equipo"],
  );
  assert.deepEqual(conflictLabels({ codigo: "red", mensaje: "x" }), []);
});

test("todos los campos de project.yaml tienen etiqueta", () => {
  for (const key of ["name", "client", "start_date", "end_date", "layout", "causas_raiz"]) {
    assert.ok(PROJECT_FIELD_LABELS[key], key);
  }
});

test("errores de comandos normalizados", () => {
  const error = { codigo: "bloqueo_tomado", mensaje: "En edicion", puedeForzar: true };
  assert.equal(toPuduHubError(error), error);
  assert.deepEqual(toPuduHubError("fallo local"), { codigo: "local", mensaje: "fallo local" });
  assert.equal(toPuduHubError(undefined).codigo, "local");
  assert.equal(toPuduHubError({ codigo: 1 }).codigo, "local");
});

test("indicador del proyecto vinculado", () => {
  const base = {
    vinculado: true,
    servidor: "https://hub.example.cl",
    informeId: "x",
    baseVersion: 4,
    sincronizadoAt: "2026-10-25T10:00:00Z",
    cambiosSinEnviar: false,
    mismoServidor: true,
    vinculoInvalido: false,
  };
  assert.equal(linkIndicator(base), "Vinculado a PuduHub, version 4; cambios sin enviar: no");
  assert.equal(
    linkIndicator({ ...base, cambiosSinEnviar: true }),
    "Vinculado a PuduHub, version 4; cambios sin enviar: si",
  );
  assert.equal(linkIndicator({ ...base, vinculado: false }), "Sin vincular a PuduHub");
  assert.equal(
    linkIndicator({ ...base, vinculado: false, vinculoInvalido: true }),
    "Vinculo con PuduHub no valido",
  );
});

test("bloqueo y fechas", () => {
  const lock = { nombre: "Ana Perez", hasta: "2026-10-25T12:15:00Z", origen: "web", propio: false };
  assert.match(lockSummary(lock), /^Ana Perez \(web\) hasta \d{2}-10-2026 \d{2}:15$/);
  assert.match(lockSummary({ ...lock, propio: true }), /^Usted, en otra sesion/);
  assert.match(lockSummary({ ...lock, nombre: null, origen: "" }), /^Otro usuario hasta/);
  assert.equal(lockSummary({ ...lock, hasta: "no-es-fecha" }), "Ana Perez (web)");
  assert.equal(formatTimestamp(""), "");
  assert.equal(formatTimestamp(null), "");
});

test("resumen de lo que llego de PuduHub", () => {
  const empty = {
    hallazgosNuevos: [],
    hallazgosActualizados: [],
    hallazgosEliminados: [],
    secciones: [],
    campos: [],
    evidencias: 0,
    orden: false,
  };
  assert.deepEqual(changesSummary(empty), []);
  const lines = changesSummary({
    ...empty,
    hallazgosNuevos: ["A", "B", "C", "D", "E", "F", "G"],
    campos: ["client", "otro"],
    evidencias: 2,
    orden: true,
  });
  assert.deepEqual(lines, [
    "Hallazgos nuevos: A, B, C, D, E y 2 mas",
    "Datos del proyecto: Cliente, otro",
    "Evidencias: 2",
    "Orden de los hallazgos",
  ]);
});
