import { defineConfig, type Plugin } from "vite";
import react from "@vitejs/plugin-react";

// Config de Vite afinada para Tauri.
// El puerto fijo y el HMR explicito permiten que la webview de Tauri se conecte.
const host = process.env.TAURI_DEV_HOST;

// Editor web (modo servidor, `vite build --mode web`): otra entrada sin Tauri,
// salida en dist-web/ y base configurable con PUDU_WEB_BASE (por defecto
// /editor/). CSP sin eval: imagenes, fuentes y API solo del mismo origen.
const WEB_CSP =
  "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; " +
  "img-src 'self' data: blob:; font-src 'self'; connect-src 'self'; " +
  "object-src 'none'; base-uri 'self'; form-action 'self'";

function webBase(): string {
  const raw = (process.env.PUDU_WEB_BASE ?? "/editor/").trim();
  if (!/^\/[A-Za-z0-9/_-]*$/.test(raw) || raw.includes("//")) {
    throw new Error(`PUDU_WEB_BASE invalido: ${raw}`);
  }
  return raw.endsWith("/") ? raw : `${raw}/`;
}

function webEntry(): Plugin {
  return {
    name: "pudureport-web-entry",
    transformIndexHtml: {
      order: "pre",
      handler(html) {
        return html
          .replace("/src/main.tsx", "/src/web/main.tsx")
          .replace(/(http-equiv="Content-Security-Policy" content=")[^"]*"/, `$1${WEB_CSP}"`);
      },
    },
  };
}

export default defineConfig(({ mode }) => {
  const web = mode === "web";
  return {
    base: web ? webBase() : "/",
    plugins: web ? [react(), webEntry()] : [react()],
    // Evita que Vite oculte errores de Rust en consola.
    clearScreen: false,
    server: {
      port: 1420,
      strictPort: true,
      host: host || false,
      hmr: host
        ? {
            protocol: "ws",
            host,
            port: 1421,
          }
        : undefined,
      watch: {
        // No vigilar el backend Rust desde el dev server del frontend.
        ignored: ["**/src-tauri/**"],
      },
    },
    // Tauri usa Chromium en Linux/Windows y WebKit en macOS.
    build: web
      ? {
          outDir: "dist-web",
          emptyOutDir: true,
          target: "es2020",
          sourcemap: false,
        }
      : {
          target: process.env.TAURI_ENV_PLATFORM === "windows" ? "chrome105" : "safari13",
          minify: !process.env.TAURI_ENV_DEBUG ? "esbuild" : false,
          sourcemap: !!process.env.TAURI_ENV_DEBUG,
        },
  };
});
