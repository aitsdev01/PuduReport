// SPDX-License-Identifier: GPL-3.0-only
// Copyright (c) 2026 bc0d3

/** Texto que reemplaza a una imagen que el editor web no carga. */
export const BLOCKED_IMAGE_LABEL = "Imagen externa no permitida";

/** Contexto para resolver la `src` de una imagen del markdown. */
export interface ImageSrcContext {
  mode: "desktop" | "web";
  /** Base de evidencias del proyecto (carpeta local o informe remoto). */
  base?: string | null;
  projectId?: string | null;
  /** URL cargable de un asset del proyecto ("" si la ruta no es valida). */
  assetUrl: (projectId: string, relPath: string) => string;
}

/**
 * Resuelve la `src` de una imagen del markdown a una URL cargable, o `null`
 * si no se debe cargar.
 *
 * - Web: solo se cargan evidencias del paquete (`assets/...`) a traves de la
 *   API del informe. Cualquier otra cosa (`https:`, `http:`, `data:`, incluido
 *   SVG, `blob:` o rutas fuera de `assets/`) devuelve `null`: un informe
 *   compartido no puede hacer que el navegador de quien lo edita pida recursos
 *   a terceros ni muestre imagenes que no estan en el paquete.
 * - Escritorio: sin cambios; las URL absolutas pasan tal cual y la CSP de la
 *   ventana (`img-src 'self' asset: http://asset.localhost data:`) ya bloquea
 *   las remotas.
 */
export function resolveImageSrc(src: string, ctx: ImageSrcContext): string | null {
  if (ctx.mode === "web") {
    if (!ctx.projectId || !src.startsWith("assets/")) return null;
    const url = ctx.assetUrl(ctx.projectId, src);
    return url || null;
  }
  if (!ctx.base || !ctx.projectId || !src || /^(data:|https?:|asset:|blob:)/.test(src)) return src;
  return ctx.assetUrl(ctx.projectId, src);
}
