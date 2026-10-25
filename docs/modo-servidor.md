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
destinos de exportacion y la conexion con PuduHub que el usuario configure
(ver [Sincronizacion desde escritorio](#sincronizacion-desde-escritorio)).

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
- Imagenes del Markdown: en la vista visual solo se cargan evidencias del
  paquete (`assets/...`, via `GET /{id}/assets/...`). Cualquier otra `src`
  (`https:`, `http:`, `data:`, incluido SVG, `blob:` o rutas fuera de
  `assets/`) se muestra como "Imagen externa no permitida" y el Markdown
  conserva la ruta tal cual. Un informe compartido no puede hacer que el
  navegador de quien lo edita pida recursos a terceros. Es defensa en
  profundidad: la CSP de `/editor/` en PuduHub ya restringe `img-src` a
  `'self' data: blob:`. En escritorio no cambia (la CSP de la ventana ya
  bloquea las imagenes remotas).
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
  hallazgos, `project.yaml` 1 MiB, hallazgo 512 KiB, imagen 10 MiB y 48 MiB
  entre todos los archivos de texto (manifiesto, `project.yaml`,
  `workspace.yaml` y `findings/*.md`).
- Anti zip-bomb: los encabezados se revisan antes de descomprimir. Una entrada
  de mas de 64 KiB no puede expandirse mas de 200 veces, y con mas de 1 MiB de
  contenido el paquete completo no puede superar 50 veces su tamano. Cada
  entrada se lee hasta su tamano declarado mas un byte y debe medir
  exactamente lo declarado, asi un encabezado que miente no provoca lecturas
  de mas.
- Imagenes: extension permitida y firma real (SVG y HTML se rechazan). Las
  dimensiones declaradas (IHDR de PNG, pantalla y cuadros de GIF, SOFn de
  JPEG, VP8/VP8L/VP8X/ANMF de WebP) no pueden superar 20000 pixeles por lado
  ni 50 MP: un archivo pequeno no puede pedirle a Typst un lienzo enorme. La
  exportacion omite esas imagenes.
- Cada archivo debe estar en el manifiesto con su hash y tamano, y el
  manifiesto no puede listar archivos ausentes.
- YAML y front-matter con el mismo parser que la app, despues de una revision
  previa con las reglas del lector de PuduHub: front-matter de hasta 128 KiB,
  hasta 32 colecciones anidadas (bloque y flujo juntas), hasta 50000 nodos por
  archivo y 500000 nodos y 8 MiB de YAML entre `project.yaml` y todos los
  front-matter; sin anclas, alias, etiquetas, directivas ni varios
  documentos. La marca solo puede apuntar a archivos de `branding/`
  incluidos.
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
- El cupo de concurrencia se toma antes de leer el cuerpo: un
  `Content-Length` mayor al limite se rechaza (`413`) sin leerlo, un cuerpo
  sin `Content-Length` (`chunked`) se corta apenas supera el limite y una
  subida que no termina a tiempo recibe `408 upload_timeout`. Asi N subidas
  simultaneas no retienen N veces el tamano maximo en memoria.
- La conversion de Markdown a Typst acota el anidamiento (16 niveles de
  listas, citas y negrita/cursiva) y el tamano de la salida
  (`max(8 x entrada, 1 MiB)`, nunca mas de 64 MiB por texto); si se supera,
  el render falla con `422 render_failed` antes de llegar a Typst.
- Typst corre con el entorno vacio salvo `PATH`, `HOME`, `TMPDIR`, el idioma
  y las variables de fuentes (`TYPST_FONT_PATHS`, XDG, fontconfig): la clave
  del servicio y cualquier otra variable no llegan al proceso hijo. En el
  servicio corre con `--jobs 1`.
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
  Internet. Typst solo lee dentro del temporal del trabajo (`--root`) y no
  hereda el entorno del servicio.
- Contenedor: limitar memoria y procesos (el compose de PuduHub usa 1 GiB y
  256 pids con `PUDUREPORT_SERVICE_CONCURRENCY=2`). Ver service/README.md.
- El paquete se valida completo antes de escribir; ver la lista de arriba.

## Sincronizacion desde escritorio

El ingeniero abre un informe de PuduHub en la app de escritorio, lo edita sin
conexion con todas las herramientas locales y envia la actualizacion.
PuduHub fusiona en tres vias por entidad (cada campo del proyecto, cada
seccion, cada hallazgo, el orden y cada evidencia) y solo reporta conflicto si
la misma entidad cambio distinto en los dos lados. Es opcional y esta apagada
hasta configurar la conexion.

### Conexion (Ajustes, "Conexion con PuduHub")

- URL base de PuduHub. Solo `https://`; para desarrollo se acepta `http://`
  hacia `localhost`, `127.0.0.1` o `[::1]` (sin proxy). Sin usuario, consulta
  ni fragmento. Se guarda en `workspace.yaml` (`puduhub.servidor`), una por
  workspace; guardar el workspace desde otras pantallas no la cambia.
- Token personal de editor (`phed_` + 43 caracteres), generado en PuduHub. Se
  envia una sola vez al backend Rust, que lo guarda en el keychain del SO
  (servicio `PuduReport-puduhub`, cuenta `<workspace>::<servidor>`). Nunca va a
  archivos, registros ni al estado de la interfaz: el campo se vacia al
  guardar y la interfaz solo sabe si hay uno guardado.
- "Probar conexion": `GET /api/editor/informes?page=1&pageSize=10`. Errores
  explicados: token invalido, vencido o revocado (`401`), sin permiso (`403`),
  sin red, tiempo agotado y TLS (certificado no valido o no confiable, por
  ejemplo un proxy corporativo que inspecciona TLS).

### Cliente HTTP (Rust, `src-tauri/src/puduhub/`)

- La webview nunca ve el token: todas las llamadas son comandos de Tauri
  (`puduhub_*`). Mismo cliente que los destinos de exportacion (`reqwest` con
  rustls), sin dependencias nuevas.
- Los comandos rechazan con un error estructurado
  `{codigo, mensaje, bloqueo?, conflictos?, contexto?, reintentarEn?, proyectoId?, puedeForzar?}`;
  `mensaje` es un texto propio y `codigo` decide que ofrece la interfaz.
- `Authorization: Bearer phed_...` (cabecera marcada como sensible), sin
  cookies ni CSRF. Rutas `/api/editor/informes`.
- Sin redirecciones: una respuesta 3xx es un error, asi el token nunca viaja a
  otro origen. Solo `https` salvo localhost.
- Plazos: conexion 10 s, JSON 30 s, paquetes 300 s. Topes de respuesta: JSON 4
  MiB, errores 64 KiB, paquete 200 MiB (la lectura se corta en el tope aunque
  el servidor no declare el tamano). El paquete a enviar tambien se limita a
  200 MiB.
- Errores por `(estado, codigo)`: el cuerpo de una respuesta de error nunca se
  muestra; solo se copian, validados, el bloqueo (`nombre`, `hasta`, `origen`,
  `propio`), los conflictos (`entidad`, `id`, `campo`) y `retryAfter`. Ningun
  mensaje incluye el token (hay pruebas para eso).
- Cada fila del listado se valida antes de mostrarla (id UUID, estado,
  version, fecha ISO, texto sin caracteres de control y acotado); las que no
  pasan se omiten y se informa cuantas.

| Peticion | Uso |
|---|---|
| `GET /api/editor/informes?page=N&pageSize=20[&q=]` | Listar los informes asignados; tambien "Probar conexion" (`pageSize=10`) |
| `POST /api/editor/informes` `{titulo, tipo}` | Subir como nuevo informe |
| `GET /{id}` | Revision del borrador y ultima version (`revision`, `versionActual`) al abrir |
| `GET /{id}/paquete[?version=N]` | Bajar el `.pudupkg` de una version existente (sin `version`: la ultima). Solo lectura; cabeceras `X-Informe-Version` y `X-Informe-Borrador-Pendiente: 0\|1`; `409 version_required` si el informe no tiene versiones |
| `POST /{id}/versiones` `{motivo, revision}` (`X-Lock-Token`) | Punto de control al abrir: guarda el borrador como version |
| `POST /{id}/bloqueo` `{forzar}` | Tomar el bloqueo antes de abrir o sincronizar (devuelve el token del bloqueo) |
| `POST /{id}/sincronizar` (paquete `application/zip`, `X-Lock-Token`) | Enviar; respuesta `{revision, version: {version, ...}}` |
| `DELETE /{id}/bloqueo` (`X-Lock-Token`) | Liberar el bloqueo, siempre, tambien si la sincronizacion falla |

### Flujo

1. **Abrir desde PuduHub** (Proyectos): lista titulo, tipo, estado, ultima
   version y quien tiene el bloqueo. "Descargar y abrir" baja el paquete de la
   ultima version y lo importa con el importador estricto de `package.rs`.
   `GET /{id}/paquete` solo entrega versiones existentes, asi que los cambios
   hechos en la web que aun no son version se incluyen con un punto de
   control:
   1. `POST /{id}/bloqueo` `{"forzar": false}` toma el bloqueo.
   2. `GET /{id}` da la `revision` vigente y `versionActual`. Si
      `versionActual` es 0, o `GET /{id}/paquete` responde
      `X-Informe-Borrador-Pendiente: 1` (esa respuesta se descarta sin leer el
      cuerpo), se crea una version con `POST /{id}/versiones`
      `{"motivo": "Apertura desde PuduReport escritorio", "revision": N}` y
      `X-Lock-Token`.
   3. Se baja `GET /{id}/paquete?version=N`, con N de la version creada o de
      `X-Informe-Version`; el manifiesto debe traer esa misma version.
   4. El bloqueo se libera siempre al terminar: la edicion sigue sin conexion
      y cada envio toma su propio bloqueo.

   Si otra persona tiene el bloqueo (`lock_held`), no se crea version: se
   baja la ultima tal cual y, si la cabecera indica cambios pendientes, se
   avisa que los cambios de la web no se incluyeron. Si ademas el informe no
   tiene ninguna version (`409 version_required`), se explica que hay que
   esperar a que se libere el bloqueo.
   "Descargar version del servidor" (conflictos) usa el mismo camino.
   Si el informe ya esta en este equipo, "Actualizar copia local" solo se
   ofrece si no tiene cambios sin enviar; si los tiene, se avisa y no se toca.
2. **Vinculo**: `<proyecto>/.puduhub.json` con `servidor`, `informeId`,
   `baseVersion`, `sincronizadoAt` y la huella del contenido. Sin token. No
   entra en el paquete y se agrega al `.gitignore` del workspace si usa git.
   La huella es el SHA-256 del contenido ya interpretado (proyecto, hallazgos y
   evidencias), asi que reescribir un archivo sin cambios no cuenta como
   cambio.
3. **Indicador** en la barra del proyecto: "Vinculado a PuduHub, version N;
   cambios sin enviar: si/no", con "Actualizar" y "Enviar a PuduHub".
4. **Enviar a PuduHub**: guarda lo pendiente, exporta el paquete con
   `informeId`, `version` y `baseVersion` = la base del vinculo, toma el
   bloqueo, sincroniza y lo libera. PuduHub no devuelve el documento fusionado,
   asi que la app baja `GET /{id}/paquete?version=N` de la version creada, lo
   valida y reemplaza el proyecto local (los PDF de `build/` se conservan).
   Muestra la version nueva y lo que llego de otras personas (hallazgos,
   secciones, datos del proyecto, evidencias, orden). Si el proyecto cambio
   durante el envio, o la descarga falla, la base no avanza y se avisa: el
   proximo envio vuelve a fusionar sin perder cambios de nadie.
5. **Conflicto** (`409 merge_conflict`): lista cada entidad con su nombre
   local (titulo del hallazgo, titulo de la seccion, etiqueta del campo del
   proyecto). No se envia ni se pisa nada. "Descargar version del servidor"
   guarda primero una copia completa del proyecto como proyecto aparte
   (`<id>-respaldo-<fecha>`, nombre con "(respaldo local)" y sin vinculo) y
   luego reemplaza el proyecto por la version de PuduHub. "Cancelar" no hace
   nada.
6. **Otros errores**: `lock_held` (quien, desde donde y hasta cuando; si es
   otra sesion del mismo usuario se ofrece "Tomar el bloqueo y enviar" con
   `forzar: true`), `lock_required`, `stale_revision`, `unknown_base_version`
   (ofrece descargar la version del servidor), `version_required`, `401`,
   `403`, `404`, `413`,
   `415`, `422` (`invalid_package` / `invalid_payload`), `429` y `5xx`.
7. **Subir a PuduHub** (proyecto sin vincular): `POST /` con el nombre y el
   tipo del proyecto. El rol Ingeniero puede crear informes y queda asignado
   al que crea. Luego, como al abrir, se toma el bloqueo, se crea la version 1
   (vacia) con `POST /{id}/versiones` y se baja; se escribe el vinculo con base
   1 y se envia con la
   sincronizacion normal: como la base y el borrador del servidor son
   iguales, entra todo lo local sin conflictos. Si el envio falla, el informe
   ya existe y el proyecto queda vinculado para reintentar.

### Marca

Al abrir un informe no se aplica su `workspace.yaml` (la marca del workspace
local es compartida por todos sus proyectos), y el envio normal no incluye
`workspace.yaml`: PuduHub conserva la marca del informe. "Subir a PuduHub" si
incluye la marca del workspace, para que el informe nuevo tenga el mismo
logo, colores y marca de agua.

### Seguridad

- El importador del paquete es el mismo y estricto (rutas, tamanos, firmas de
  imagen, hashes y YAML) y ademas exige que el manifiesto sea del informe
  pedido y traiga una version valida. Solo `package.rs` decide rutas en
  disco: ningun dato del servidor elige donde se escribe.
- El reemplazo de un proyecto escribe la version nueva en una carpeta aparte y
  la intercambia al final; si algo falla, el proyecto anterior vuelve a su
  lugar.
- Un proyecto vinculado a otro servidor no se envia al configurado.
- Una sola operacion con PuduHub a la vez.

## Exportacion a API: `uid`

Los destinos de exportacion pueden activar el campo `uid` (id estable del
hallazgo, el nombre de archivo). No esta activado por defecto. Ver
[export-api.md](export-api.md).
