// SPDX-License-Identifier: GPL-3.0-only
// Copyright (c) 2026 bc0d3
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import ts from "typescript";

async function load(path) {
  const source = readFileSync(new URL(path, import.meta.url), "utf8");
  const { outputText } = ts.transpileModule(source, {
    compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2022 },
  });
  return import(`data:text/javascript;base64,${Buffer.from(outputText).toString("base64")}`);
}

const { HttpBackend, errorFrom } = await load("../src/lib/backend/http.ts");
const { parseWebConfig } = await load("../src/web/config.ts");

const ID = "0f8fad5b-d9cb-469f-a165-70867728950e";
const JOB = "7c9e6679-7425-40de-944b-e07fc1f90ae7";
const BASE = "/api/admin/informes";
const HASTA = "2026-10-24T12:15:00+00:00";

function json(status, body) {
  return new Response(body === undefined ? null : JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

/** fetch falso: responde segun "METODO ruta" y registra cada llamada. */
function fakeServer(routes) {
  const calls = [];
  const fetch = async (url, init = {}) => {
    const method = init.method ?? "GET";
    const [path, search = ""] = String(url).split("?");
    const call = {
      method,
      url: String(url),
      path,
      query: new URLSearchParams(search),
      headers: init.headers ?? {},
      body: typeof init.body === "string" ? JSON.parse(init.body) : init.body,
      credentials: init.credentials,
      keepalive: init.keepalive,
    };
    calls.push(call);
    const handler = routes[`${method} ${path}`];
    if (!handler) return json(404, { error: "not_found" });
    return handler(call, calls);
  };
  return { fetch, calls };
}

/** InformeDetail de PuduHub (campos usados por el editor). */
function informe(revision = 7) {
  return {
    id: ID,
    titulo: "Informe",
    revision,
    bloqueo: null,
    proyecto: { name: "Informe", client: "ACME", project_type: "pentest", sections: [] },
    orden: ["001-a"],
  };
}

function lockView(token = "lock-1") {
  return {
    token,
    bloqueo: { usuarioId: "u1", nombre: null, hasta: HASTA, origen: "web", propio: true },
    revision: 7,
  };
}

function backendWith(routes, saved = []) {
  const server = fakeServer({
    "GET /api/auth/csrf": () => new Response(null, { status: 204 }),
    ...routes,
  });
  const backend = new HttpBackend({
    apiBase: BASE,
    informeId: ID,
    fetch: server.fetch,
    readCookie: (name) => (name === "XSRF-TOKEN" ? "tok-1" : null),
    pdfPollMs: 1,
    saveFile: (blob, name) => saved.push({ blob, name }),
  });
  return { backend, calls: server.calls };
}

const baseRoutes = {
  [`GET ${BASE}/${ID}`]: () => json(200, informe()),
  [`POST ${BASE}/${ID}/bloqueo`]: (call) =>
    json(200, lockView(call.headers["X-Lock-Token"] ? null : "lock-1")),
  [`DELETE ${BASE}/${ID}/bloqueo`]: () => json(200, { revision: 7 }),
};

test("abre el informe: documento, bloqueo y escrituras con revision, CSRF y X-Lock-Token", async () => {
  let revision = 7;
  const { backend, calls } = backendWith({
    ...baseRoutes,
    [`PUT ${BASE}/${ID}/hallazgos/001-a`]: (call) => {
      assert.equal(call.body.revision, revision);
      assert.deepEqual(Object.keys(call.body).sort(), ["body", "meta", "revision"]);
      revision += 1;
      return json(200, { revision });
    },
  });
  assert.deepEqual(await backend.open(), { kind: "ready" });
  assert.equal(backend.currentRevision, 7);
  const lock = calls.find((c) => c.path.endsWith("/bloqueo"));
  assert.deepEqual(lock.body, { forzar: false });

  const project = await backend.loadProject(ID);
  assert.deepEqual(project.finding_order, ["001-a"]);
  assert.deepEqual(project.layout, [], "las listas omitidas se completan");

  // Dos guardados simultaneos salen en orden, cada uno con la revision vigente.
  const finding = { id: "001-a", meta: { title: "A" }, body: "x" };
  await Promise.all([backend.saveFinding(ID, finding), backend.saveFinding(ID, finding)]);
  assert.equal(backend.currentRevision, 9);

  const put = calls.find((c) => c.method === "PUT");
  assert.equal(put.headers["X-XSRF-TOKEN"], "tok-1");
  assert.equal(put.headers["X-Lock-Token"], "lock-1");
  assert.equal(put.credentials, "same-origin");
  // Como el frontend de PuduHub: CSRF renovado antes de cada escritura.
  const index = calls.indexOf(put);
  assert.equal(calls[index - 1].url, "/api/auth/csrf");
  const get = calls.find((c) => c.method === "GET" && c.path.endsWith(ID));
  assert.equal(get.headers["X-XSRF-TOKEN"], undefined);

  // Renovar no pierde el token (PuduHub solo lo devuelve al tomarlo).
  await backend.renewLock();
  assert.equal(calls.at(-1).headers["X-Lock-Token"], "lock-1");

  await backend.release(true);
  const release = calls.at(-1);
  assert.equal(release.method, "DELETE");
  assert.equal(release.keepalive, true);
  assert.equal(release.headers["X-Lock-Token"], "lock-1");
});

test("bloqueo de otra persona al abrir: informa quien y no escribe", async () => {
  const { backend, calls } = backendWith({
    ...baseRoutes,
    [`POST ${BASE}/${ID}/bloqueo`]: () =>
      json(409, {
        error: "lock_held",
        bloqueo: { usuarioId: "u2", nombre: "Ana", hasta: HASTA, origen: "web", propio: false },
      }),
  });
  const opened = await backend.open();
  assert.equal(opened.kind, "lock_held");
  assert.equal(opened.nombre, "Ana");
  assert.equal(opened.propio, false);
  assert.ok(!calls.some((c) => c.method === "PUT"));
});

test("bloqueo propio en otra sesion: se puede tomar con forzar", async () => {
  const { backend, calls } = backendWith({
    ...baseRoutes,
    [`POST ${BASE}/${ID}/bloqueo`]: (call) =>
      call.body.forzar
        ? json(200, lockView("lock-2"))
        : json(409, {
            error: "lock_held",
            bloqueo: {
              usuarioId: "u1",
              nombre: null,
              hasta: HASTA,
              origen: "escritorio",
              propio: true,
            },
          }),
  });
  const first = await backend.open();
  assert.equal(first.kind, "lock_held");
  assert.equal(first.propio, true);
  assert.equal(first.puedeForzar, true, "sin marca de PuduHub: sesion propia");
  assert.deepEqual(await backend.open(true), { kind: "ready" });
  assert.ok(calls.some((c) => c.body && c.body.forzar === true));
  await backend.release();
});

test("revision vencida: avisa y detiene las escrituras siguientes", async () => {
  const events = [];
  const { backend, calls } = backendWith({
    ...baseRoutes,
    [`PUT ${BASE}/${ID}/proyecto`]: () => json(409, { error: "stale_revision", revision: 9 }),
  });
  backend.onSessionEvent((e) => events.push(e.kind));
  await backend.open();
  await assert.rejects(backend.saveProject(ID, { name: "x" }), /cambio en otra sesion/);
  assert.ok(events.includes("stale_revision"));
  const before = calls.length;
  await assert.rejects(backend.saveProject(ID, { name: "y" }), /cambio en otra sesion/);
  assert.equal(calls.length, before, "no debe volver a escribir");
  await backend.release();
});

test("bloqueo perdido al renovar: avisa lock_lost", async () => {
  const events = [];
  let renewals = 0;
  const { backend } = backendWith({
    ...baseRoutes,
    [`POST ${BASE}/${ID}/bloqueo`]: () =>
      renewals++ === 0 ? json(200, lockView()) : json(409, { error: "lock_required" }),
  });
  backend.onSessionEvent((e) => events.push(e.kind));
  await backend.open();
  await backend.renewLock();
  assert.ok(events.includes("lock_lost"));
  await assert.rejects(backend.reorderFindings(ID, []), /bloqueo/);
  await backend.release();
});

test("hallazgos: lista con id+meta y cuerpo de cada uno; crear, ordenar y borrar", async () => {
  const { backend, calls } = backendWith({
    ...baseRoutes,
    [`GET ${BASE}/${ID}/hallazgos`]: () =>
      json(200, [
        { id: "001-a", meta: { title: "A" } },
        { id: "002-b", meta: { title: "B" } },
      ]),
    [`GET ${BASE}/${ID}/hallazgos/001-a`]: () =>
      json(200, { id: "001-a", meta: { title: "A" }, body: "cuerpo A", revision: 7 }),
    [`GET ${BASE}/${ID}/hallazgos/002-b`]: () =>
      json(200, { id: "002-b", meta: { title: "B" }, body: "cuerpo B", revision: 7 }),
    [`POST ${BASE}/${ID}/hallazgos`]: (call) => {
      assert.deepEqual(call.body, { titulo: "Nuevo", revision: 7 });
      return json(201, { id: "003-nuevo", meta: { title: "Nuevo" }, body: "", revision: 8 });
    },
    [`PUT ${BASE}/${ID}/orden`]: (call) => {
      assert.deepEqual(call.body, { orden: ["003-nuevo", "001-a"], revision: 8 });
      return json(200, { revision: 9 });
    },
    [`DELETE ${BASE}/${ID}/hallazgos/002-b`]: (call) => {
      assert.equal(call.query.get("revision"), "9");
      return json(200, { revision: 10 });
    },
  });
  await backend.open();
  const list = await backend.listFindings(ID);
  assert.deepEqual(
    list.map((f) => [f.id, f.body]),
    [
      ["001-a", "cuerpo A"],
      ["002-b", "cuerpo B"],
    ],
  );
  const created = await backend.createFinding(ID, "Nuevo");
  assert.equal(created.id, "003-nuevo");
  assert.equal("revision" in created, false);
  await backend.reorderFindings(ID, ["003-nuevo", "001-a"]);
  await backend.deleteFinding(ID, "002-b");
  assert.equal(backend.currentRevision, 10);
  assert.ok(calls.length > 0);
  await backend.release();
});

test("evidencias: subida con revision, destino y nombre; URL con la ruta completa", async () => {
  const { backend, calls } = backendWith({
    ...baseRoutes,
    [`POST ${BASE}/${ID}/assets`]: () =>
      json(201, {
        ruta: "assets/evidencia-0123456789ab.png",
        sha256: "0".repeat(64),
        bytes: 2,
        contentType: "image/png",
        revision: 8,
      }),
  });
  await backend.open();
  const ruta = await backend.saveAsset(ID, "png", Buffer.from([0x89, 0x50]).toString("base64"));
  assert.equal(ruta, "assets/evidencia-0123456789ab.png");
  assert.equal(backend.currentRevision, 8);
  const post = calls.find((c) => c.path.endsWith("/assets"));
  assert.equal(post.headers["Content-Type"], "image/png");
  assert.equal(post.query.get("revision"), "7");
  assert.equal(post.query.get("destino"), "assets");
  assert.equal(post.query.get("nombre"), "evidencia.png");
  assert.ok(post.body instanceof Uint8Array);
  await assert.rejects(backend.saveAsset(ID, "svg", "PHN2Zz4="), /Solo se permiten imagenes/);

  assert.equal(backend.assetUrl(ID, ruta), `${BASE}/${ID}/assets/${ruta}`);
  assert.equal(
    backend.assetUrl(ID, "assets/branding/logo.png"),
    `${BASE}/${ID}/assets/assets/branding/logo.png`,
  );
  for (const bad of [
    "../x.png",
    "assets/../x.png",
    "/etc/passwd",
    "assets/.oculto.png",
    "assets/x.svg",
    "https://x/y.png",
  ]) {
    assert.equal(backend.assetUrl(ID, bad), "", bad);
  }
  assert.equal(backend.assetUrl("otro", ruta), "");
  await backend.release();
});

test("PDF en PuduHub: trabajo, consulta hasta listo y descarga (403 para Ingeniero)", async () => {
  let polls = 0;
  let role = "Ingeniero";
  const saved = [];
  const job = (estado) => ({ id: JOB, version: 3, estado, errorCodigo: null });
  const { backend, calls } = backendWith(
    {
      ...baseRoutes,
      [`POST ${BASE}/${ID}/pdf`]: () => json(202, job("pendiente")),
      [`GET ${BASE}/${ID}/pdf/${JOB}`]: () => json(200, job(polls++ < 2 ? "procesando" : "listo")),
      [`GET ${BASE}/${ID}/pdf/${JOB}/archivo`]: () =>
        role === "Admin"
          ? new Response("%PDF-1.7", {
              status: 200,
              headers: {
                "content-type": "application/pdf",
                "content-disposition": 'attachment; filename="informe-v3.pdf"',
              },
            })
          : json(403, { error: "forbidden" }),
    },
    saved,
  );
  await backend.open();
  const [url] = await backend.generatePdf(ID, false);
  assert.equal(url, `${BASE}/${ID}/pdf/${JOB}/archivo`);
  assert.equal(backend.lastGeneratedPdf(ID), url);
  assert.deepEqual(calls.find((c) => c.path.endsWith("/pdf")).body, {});

  await assert.rejects(backend.downloadGeneratedPdf(ID, url), (error) => {
    assert.equal(error.status, 403);
    assert.match(String(error), /reservada a administradores/);
    return true;
  });
  role = "Admin";
  await backend.downloadGeneratedPdf(ID, url);
  assert.equal(saved[0].name, "informe-v3.pdf");
  await assert.rejects(backend.downloadGeneratedPdf(ID, "https://otro/x.pdf"), /no valido/);
  await backend.release();
});

test("PDF con error informa el codigo", async () => {
  const { backend } = backendWith({
    ...baseRoutes,
    [`POST ${BASE}/${ID}/pdf`]: () =>
      json(202, { id: JOB, version: 1, estado: "error", errorCodigo: "render_failed" }),
  });
  await backend.open();
  await assert.rejects(backend.generatePdf(ID, false), /render_failed/);
  await backend.release();
});

test("CVSS por PuduHub", async () => {
  const { backend } = backendWith({
    [`POST ${BASE}/cvss`]: (call) => {
      assert.deepEqual(call.body, { vector: "CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:H" });
      return json(200, {
        score: 9.8,
        severity: "critical",
        vector: call.body.vector,
        version: "3.1",
      });
    },
  });
  const result = await backend.calcCvss("3.1", "CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:H");
  assert.equal(result.score, 9.8);
  assert.equal(result.severity, "critical");
});

test("hallazgos con incluirCuerpo: sin lecturas por hallazgo si vienen los cuerpos", async () => {
  const { backend, calls } = backendWith({
    ...baseRoutes,
    [`GET ${BASE}/${ID}/hallazgos`]: (call) =>
      json(
        200,
        call.query.get("incluirCuerpo") === "1"
          ? [
              { id: "001-a", meta: { title: "A" }, body: "cuerpo A" },
              { id: "002-b", meta: { title: "B" } },
            ]
          : [],
      ),
    [`GET ${BASE}/${ID}/hallazgos/002-b`]: () =>
      json(200, { id: "002-b", meta: { title: "B" }, body: "cuerpo B", revision: 7 }),
  });
  await backend.open();
  const list = await backend.listFindings(ID);
  assert.deepEqual(
    list.map((f) => [f.id, f.body]),
    [
      ["001-a", "cuerpo A"],
      ["002-b", "cuerpo B"],
    ],
  );
  // Solo el que vino sin cuerpo se lee aparte.
  assert.equal(calls.filter((c) => c.path.includes("/hallazgos/")).length, 1);
  await backend.release();
});

test("puedeForzar de PuduHub manda; sin la marca, solo la sesion propia", async () => {
  const held = (propio) =>
    json(409, {
      error: "lock_held",
      bloqueo: { usuarioId: "u2", nombre: "Ana", hasta: HASTA, origen: "web", propio },
    });
  const withFlag = backendWith({
    ...baseRoutes,
    [`GET ${BASE}/${ID}`]: () => json(200, { ...informe(), puedeForzar: true }),
    [`POST ${BASE}/${ID}/bloqueo`]: () => held(false),
  });
  const admin = await withFlag.backend.open();
  assert.equal(admin.kind, "lock_held");
  assert.equal(admin.puedeForzar, true);

  const noFlag = backendWith({ ...baseRoutes, [`POST ${BASE}/${ID}/bloqueo`]: () => held(false) });
  assert.equal((await noFlag.backend.open()).puedeForzar, false);

  const denied = backendWith({
    ...baseRoutes,
    [`GET ${BASE}/${ID}`]: () => json(200, { ...informe(), puedeForzar: false }),
    [`POST ${BASE}/${ID}/bloqueo`]: () => held(true),
  });
  assert.equal((await denied.backend.open()).puedeForzar, false);
});

test("mensajes de error claros", () => {
  assert.match(
    String(errorFrom(409, { error: "lock_held", bloqueo: { nombre: "Ana", propio: false } })),
    /Ana/,
  );
  assert.match(
    String(errorFrom(409, { error: "lock_held", bloqueo: { origen: "escritorio", propio: true } })),
    /escritorio/,
  );
  assert.match(String(errorFrom(413, { error: "too_large" })), /tamano permitido/);
  assert.doesNotMatch(String(errorFrom(500, null)), /^Error:/);
});

test("configuracion web: solo UUID y prefijo de API del mismo origen", () => {
  assert.deepEqual(parseWebConfig(`?informe=${ID.toUpperCase()}`, undefined), {
    ok: true,
    config: { apiBase: BASE, informeId: ID },
  });
  for (const search of ["", "?informe=1", "?informe=../x", `?informe=${ID}&informe=${ID}`]) {
    assert.equal(parseWebConfig(search, BASE).ok, false, search);
  }
  for (const base of ["//evil.example/api", "https://evil.example", "api", "/api/../x"]) {
    assert.equal(parseWebConfig(`?informe=${ID}`, base).ok, false, base);
  }
  assert.equal(parseWebConfig(`?informe=${ID}`, "/api/editor/informes/").ok, true);
});
