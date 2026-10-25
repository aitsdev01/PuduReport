// SPDX-License-Identifier: GPL-3.0-only
// Copyright (c) 2026 bc0d3
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import ts from "typescript";

const source = readFileSync(new URL("../src/lib/imageSrc.ts", import.meta.url), "utf8");
const { outputText } = ts.transpileModule(source, {
  compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2022 },
});
const { resolveImageSrc } = await import(
  `data:text/javascript;base64,${Buffer.from(outputText).toString("base64")}`
);

const ASSET = /^assets(\/[A-Za-z0-9_-][A-Za-z0-9._-]{0,127})+\.(png|jpe?g|gif|webp)$/i;
const web = {
  mode: "web",
  base: "informe",
  projectId: "informe",
  assetUrl: (id, rel) => (ASSET.test(rel) ? `/api/admin/informes/${id}/${rel}` : ""),
};
const desktop = {
  mode: "desktop",
  base: "/ws/proyecto",
  projectId: "proyecto",
  assetUrl: (id, rel) => `asset://localhost/ws/${id}/${rel}`,
};

test("web carga solo evidencias del paquete via la API", () => {
  assert.equal(resolveImageSrc("assets/a.png", web), "/api/admin/informes/informe/assets/a.png");
  assert.equal(resolveImageSrc("assets/../x.png", web), null);
  assert.equal(resolveImageSrc("assets/a.svg", web), null);
});

test("web bloquea imagenes externas y data:", () => {
  for (const src of [
    "https://tracker.example/pixel.png",
    "http://tracker.example/pixel.png",
    "//tracker.example/pixel.png",
    "data:image/svg+xml;base64,PHN2Zy8+",
    "data:image/png;base64,iVBORw0KGgo=",
    "blob:https://hub.example/1234",
    "asset://localhost/etc/passwd",
    "/etc/passwd",
    "",
  ]) {
    assert.equal(resolveImageSrc(src, web), null, src);
  }
  assert.equal(resolveImageSrc("assets/a.png", { ...web, projectId: null }), null);
});

test("escritorio no cambia", () => {
  assert.equal(resolveImageSrc("assets/a.png", desktop), "asset://localhost/ws/proyecto/assets/a.png");
  assert.equal(resolveImageSrc("https://x.example/a.png", desktop), "https://x.example/a.png");
  assert.equal(resolveImageSrc("data:image/png;base64,AA==", desktop), "data:image/png;base64,AA==");
  assert.equal(resolveImageSrc("assets/a.png", { ...desktop, base: null }), "assets/a.png");
});
