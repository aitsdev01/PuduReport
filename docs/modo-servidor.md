# Modo servidor

PuduReport sigue siendo una app de escritorio local-first. El modo servidor
agrega tres piezas opcionales para que un sistema externo (por ejemplo PuduHub)
pueda alojar informes sin crear un segundo editor ni un segundo motor de PDF:

1. **Editor web**: el mismo frontend React compilado para navegador, con una
   capa de datos HTTP en lugar del IPC de Tauri.
2. **`pudureport-service`**: binario Rust que genera el PDF con el mismo
   `pudureport-core` y las mismas plantillas Typst.
3. **Paquete `.pudupkg`**: el proyecto completo en un ZIP verificable, para
   moverlo entre equipos o enviarlo al servicio.

La app de escritorio no cambia de comportamiento: sigue sin red salvo los
destinos de exportacion que el usuario configure.

```text
            escritorio                                 navegador
  +--------------------------+             +------------------------------+
  | React (src/)             |             | React (src/) build:web       |
  |   lib/api.ts             |             |   lib/api.ts                 |
  |   TauriBackend -- IPC ---+--> Rust     |   HttpBackend -- fetch ------+--> API de informes
  +--------------------------+  (core)     +------------------------------+     (mismo origen)
                                                                                    |
                                                         paquete .pudupkg           v
                                                  +-----------------------------------------+
                                                  | pudureport-service (core + Typst)       |
                                                  +-----------------------------------------+
```

## Capa de datos (`src/lib/backend/`)

`src/lib/api.ts` sigue siendo la unica puerta de datos de las pantallas: sus
funciones exportadas no cambiaron y ahora delegan en la capa activa.

| Archivo | Rol |
|---|---|
| `backend/types.ts` | Interfaz `Backend` (todas las operaciones de `api.ts`) y `Capabilities`. |
| `backend/index.ts` | `setBackend()` / `getBackend()`; el punto de entrada fija la capa antes de montar React. |
| `backend/tauri.ts` | `createTauriBackend()`: `invoke`, `convertFileSrc` y el titulo de la ventana. Unico modulo que importa `@tauri-apps`. |
| `backend/http.ts` | `HttpBackend`: API de informes, CSRF, bloqueo y revision. Solo importa tipos (se prueba aislado). |

Ademas de las operaciones de siempre, `api.ts` expone:

- `capabilities()`: que puede hacer la capa activa. En web estan apagados
  workspaces, tablero de proyectos, git, MCP, destinos de API, CSV, plantillas,
  rutas locales, vista previa por pagina y paquetes (portada y marca si: son
  del informe). Las
  pantallas ocultan esos controles.
- `assetUrl(projectId, ruta)` y `brandingAssetUrl(ruta)`: reemplazan el uso
  directo de `convertFileSrc`. En web la evidencia `assets/x.png` se sirve en
  `{apiBase}/{informe}/assets/assets/x.png` (la ruta completa va despues de
  `/assets/`); cualquier ruta que no sea `assets/...` con extension de imagen
  devuelve vacio.
- `setWindowTitle()`, `lastGeneratedPdf()` y `downloadGeneratedPdf()`.

### `HttpBackend`

Sigue la API de informes de PuduHub (`/api/admin/informes`, sesion BFF):

- Mismo origen, `credentials: "same-origin"`. Igual que el frontend de
  PuduHub, antes de cada peticion que no es `GET` pide `GET /api/auth/csrf`
  (la sesion rota y la cookie con ella) y envia la cookie `XSRF-TOKEN` en la
  cabecera `X-XSRF-TOKEN`. Las escrituras no se reintentan.
- Trabaja sobre un solo informe. Al abrir: `GET /{id}` y
  `POST /{id}/bloqueo` `{"forzar": false, "revision": N}`. El token del bloqueo
  llega solo al tomarlo (`LockView.token`) y se envia en `X-Lock-Token` en toda
  escritura, renovacion (`POST /{id}/bloqueo` cada 60 s con la `revision`
  vigente; vence a los 15 min) y al liberar (`DELETE /{id}/bloqueo`, con
  `keepalive` al cerrar la pagina). Un bloqueo propio vencido que nadie tomo se
  renueva con el mismo token si la revision coincide; si no, PuduHub responde
  `lock_required` y el editor pide recargar.
- Si PuduHub indica `puedeForzar: true` en `InformeDetail` (solo para un
  bloqueo propio de otra sesion o del escritorio), el editor ofrece "Tomar el
  bloqueo aqui" (`forzar: true`). Sin esa marca usa `bloqueo.propio`. En los demas
  casos muestra quien y hasta cuando y permite reintentar.
- Las escrituras se serializan en una cola: cada una envia la `revision`
  vigente y toma la nueva de la respuesta. Asi dos guardados casi simultaneos
  del mismo usuario no chocan entre si.
- `409 lock_held`, `409 lock_required`, `409 stale_revision` y `401` detienen
  las escrituras siguientes y emiten un evento de sesion. La interfaz muestra un
  aviso con "Recargar" (y "Iniciar sesion" si la sesion expiro); lo que no
  alcanzo a guardarse sigue en pantalla.
- PDF: `POST /{id}/pdf` `{}`, luego `GET /{id}/pdf/{trabajo}` hasta `listo` o
  `error`. La descarga (`/{id}/pdf/{trabajo}/archivo`) es solo para Admin: a un
  Ingeniero (`403`) se le informa que el PDF quedo generado y que lo descarga
  un administrador.

#### Peticiones y respuestas

| Peticion | Cuerpo | Respuesta que usa el editor |
|---|---|---|
| `GET /{id}` | - | `InformeDetail`: `revision`, `proyecto` (ProjectMeta), `orden`, `workspace` (WorkspaceMeta o `null` = defaults), `puedeForzar` |
| `PUT /{id}/workspace` | `{workspace: {name, branding, watermark}, revision}` | `Revision` |
| `POST /{id}/bloqueo` | `{forzar, revision}` | `LockView`: `token` (solo al tomar), `bloqueo.hasta`, `revision` |
| `DELETE /{id}/bloqueo` | - | `Revision` |
| `PUT /{id}/proyecto` | `{meta, revision}` | `Revision` |
| `GET /{id}/hallazgos?incluirCuerpo=1` | - | `[{id, meta, body}]`. Con `413 too_large` (mas de 25 MiB) o elementos sin `body`, se usa la lista sin cuerpos y `GET /{id}/hallazgos/{uid}` (`Hallazgo`), hasta 6 en paralelo |
| `POST /{id}/hallazgos` | `{titulo, revision}` | `Hallazgo` (PuduHub asigna el id `NNN-slug`) |
| `PUT /{id}/hallazgos/{uid}` | `{meta, body, revision}` | `Revision` |
| `DELETE /{id}/hallazgos/{uid}?revision=N` | - | `Revision` |
| `PUT /{id}/orden` | `{orden, revision}` | `Revision` |
| `POST /{id}/assets?revision=N&destino=assets&nombre=evidencia.png` | bytes, un solo `Content-Type: image/*` | `AssetView`: `ruta`, `revision` |
| `POST /{id}/assets?revision=N&destino=branding&nombre=marca.png` | bytes, un solo `Content-Type: image/*` | `AssetView`: `ruta: "branding/x.png"`, `referencia: "/branding/x.png"` (la que se guarda en `workspace.branding`), `revision` |
| `GET /{id}/assets/{ruta}` | - | la imagen: evidencia en `/{id}/assets/assets/x.png`, marca en `/{id}/assets/branding/x.png` |
| `POST /{id}/pdf` | `{}` | `PdfJob`: `id`, `estado`, `errorCodigo` |
| `GET /{id}/pdf/{trabajo}` | - | `PdfJob` |
| `GET /{id}/pdf/{trabajo}/archivo` | - | PDF (Admin) o `403` |
| `POST /cvss` (sin id) | `{vector}` | `{score, severity, vector, version}` |

Errores: `{"error": "<codigo>", ...detalles}`. `lock_held` trae
`bloqueo: {usuarioId, nombre, hasta, origen, propio}`; `stale_revision` trae
`revision`. PuduHub omite las listas vacias del proyecto (por ejemplo
`layout`); el editor las completa al leer y conserva las claves que no conoce
al guardar.

`POST /` de PuduHub crea cada informe con la estructura de
`POST /v1/proyecto-inicial` del servicio (secciones y layout del tipo), asi un
informe nuevo abre en la web igual que un proyecto nuevo de escritorio.

## Editor web (`npm run build:web`)

- `vite build --mode web` usa otra entrada (`src/web/main.tsx`) y escribe en
  `dist-web/`. El bundle no incluye `@tauri-apps` (la capa Tauri no se importa
  desde la entrada web).
- Base de publicacion: `PUDU_WEB_BASE` (por defecto `/editor/`).
- Prefijo de la API: `VITE_PUDU_API_BASE` al compilar (por defecto
  `/api/admin/informes`). Nunca se toma de la URL, para que un enlace no pueda
  apuntar el editor a otra API.
- Configuracion en tiempo de ejecucion: `?informe=<uuid>`; se valida que sea
  un UUID antes de usarlo en cualquier ruta.
- CSP sin `unsafe-eval`: `default-src 'self'`, imagenes `self data: blob:`,
  `connect-src 'self'`, `object-src 'none'`. El servidor que aloja el editor
  deberia enviar la misma politica como cabecera.
- `npm run build` (escritorio) no cambia.

```sh
PUDU_WEB_BASE=/editor/ VITE_PUDU_API_BASE=/api/admin/informes npm run build:web
# Servir dist-web/ en /editor/ del mismo origen que la API:
#   https://hub.example/editor/?informe=0f8fad5b-d9cb-469f-a165-70867728950e
```

El shell web (`src/web/WebApp.tsx`) muestra Hallazgos o Contenido, Reporte,
Portada y marca, y PDF, con el indicador de bloqueo. Reutiliza `FindingEditor`,
`ContentEditor`, `ReportBuilder`, `CoverEditor` y `PdfPreview`. La portada y la
marca son del informe (`workspace` en PuduHub), no de un workspace local; la
marca de agua se edita en Ajustes de escritorio y en web solo se conserva.

## Paquete `.pudupkg` (`core/src/package.rs`)

ZIP con los mismos archivos del proyecto en disco:

```text
manifest.json          formato, metadatos y SHA-256 de cada archivo
project.yaml           identico al del proyecto
findings/<id>.md       front-matter + cuerpo
assets/<archivo>       evidencias (PNG, JPEG, GIF o WebP)
workspace.yaml         opcional: nombre, marca y marca de agua (sin destinos de API)
branding/<archivo>     opcional: logo y fondo de portada referenciados
```

```json
{
  "formato": 1,
  "informeId": "0f8fad5b-d9cb-469f-a165-70867728950e",
  "version": 3,
  "baseVersion": 2,
  "generadoPor": "pudureport 0.0.25",
  "proyecto": "proyecto-de-ejemplo",
  "archivos": [{ "ruta": "project.yaml", "sha256": "<hex>", "bytes": 1234 }]
}
```

`informeId`, `version` y `baseVersion` son opcionales (los usa la
sincronizacion con PuduHub). La exportacion es determinista (orden fijo y
fecha fija) y omite adjuntos que no sean imagenes permitidas.

La importacion valida todo antes de escribir:

- Rutas relativas normalizadas: sin `..`, `.`, `/` inicial, `\`, `:` ni
  segmentos que empiecen con punto; solo `[A-Za-z0-9._-]` y las carpetas
  conocidas. Nombres duplicados (tambien solo por mayusculas) se rechazan.
- Sin enlaces simbolicos, entradas cifradas ni tipos especiales.
- Limites: paquete 200 MiB, descomprimido 200 MiB, 4096 entradas, 1000
  hallazgos, `project.yaml` 1 MiB, hallazgo 512 KiB, imagen 10 MiB. La lectura se
  corta en el limite aunque el tamano declarado mienta (anti zip-bomb).
- Imagenes: extension permitida y firma real (SVG y HTML se rechazan).
- Cada archivo debe estar en el manifiesto con su hash y tamano, y el
  manifiesto no puede listar archivos ausentes.
- YAML y front-matter con el mismo parser que la app. La marca solo puede
  apuntar a archivos de `branding/` incluidos.
- Se escribe en una carpeta temporal que se renombra al final; nunca pisa un
  proyecto existente. En escritorio no se toca la marca del workspace.

En la app: Vista previa, "Exportar paquete" (a `build/`) y Proyectos,
"Importar paquete".

## Servicio `pudureport-service`

Ver [service/README.md](../service/README.md) para la API, la configuracion y
la imagen. En resumen:

- `POST /v1/render` recibe un paquete y devuelve el PDF; `?plantilla=` es
  opcional y solo acepta plantillas incluidas. `POST /v1/cvss` y `GET /healthz`.
- `POST /v1/proyecto-inicial` `{"tipo", "nombre", "cliente"?}` devuelve el
  `project.yaml` de un proyecto nuevo de ese tipo (`application/yaml;
  charset=utf-8`), con las mismas secciones, layout y valores por defecto que
  crea la app de escritorio (`workspace::new_project_meta`, sin escribir en
  disco) y `finding_order` vacio. A diferencia del archivo que escribe la app,
  el layout por defecto del tipo va explicito (es el que la app reconstruye al
  leer). `tipo` debe ser uno de los 13 tipos conocidos
  (`models::PROJECT_TYPES`). Asi PuduHub crea informes con la misma estructura
  sin duplicar las plantillas de secciones.
- Clave compartida en `X-Pudureport-Service-Key`, comparada en tiempo
  constante; admite varias claves para rotar. Sin clave no arranca.
- Cada render importa el paquete en un temporal propio que se borra siempre,
  con limite de tamano, de tiempo (Typst se mata al vencer) y de concurrencia
  (`503 busy`).
- Registros JSON por linea con metodo, ruta, estado, duracion, tamanos y
  plantilla; nunca contenido ni detalles de error que puedan citarlo.
- Cabeceras de version: servicio, huella de plantillas y Typst.

## Seguridad

- El editor web no interpreta Markdown como HTML (mismas reglas del editor de
  escritorio) y solo carga imagenes del mismo origen.
- Todo dato de la URL se valida (UUID); el prefijo de la API es fijo al
  compilar.
- Las escrituras exigen CSRF, bloqueo y revision: un conflicto detiene el
  guardado sin descartar lo que hay en pantalla.
- El servicio no hace llamadas de red: el contenido se escapa al convertirlo a
  Typst (no puede importar paquetes) y la imagen debe correr sin salida a
  Internet. Typst solo lee dentro del temporal del trabajo (`--root`).
- El paquete se valida completo antes de escribir; ver la lista de arriba.

## Exportacion a API: `uid`

Los destinos de exportacion pueden activar el campo `uid` (id estable del
hallazgo, el nombre de archivo). No esta activado por defecto. Ver
[export-api.md](export-api.md).
