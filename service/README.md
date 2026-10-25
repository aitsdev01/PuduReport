# pudureport-service

Servicio interno que genera el PDF de un informe con el mismo motor de la app
de escritorio (`pudureport-core`: `data.json` + plantillas Typst incluidas) y
calcula CVSS. Pensado para una red interna: no hace llamadas de red, exige una
clave compartida y nunca registra el contenido de los documentos.

Arquitectura y formato de paquete: [docs/modo-servidor.md](../docs/modo-servidor.md).

## API

| Ruta | Cuerpo | Respuesta |
|---|---|---|
| `POST /v1/render?plantilla=<nombre>` | paquete `.pudupkg` (`Content-Type: application/zip`) | `200 application/pdf` |
| `POST /v1/cvss` | `{"vector": "CVSS:3.1/...", "version": "3.1"}` (`version` opcional) | `{"score", "severity", "vector", "version"}` |
| `POST /v1/proyecto-inicial` | `{"tipo": "pentest", "nombre": "...", "cliente": "..."}` (`cliente` opcional) | `200 application/yaml; charset=utf-8`: `project.yaml` de un proyecto nuevo, igual al que crea la app (secciones y layout del tipo, `finding_order` vacio) |
| `GET /healthz` | - | `{"estado": "ok", "version", "plantillas", "typst"}` (sin clave) |

- Autenticacion: cabecera `X-Pudureport-Service-Key` en `/v1/*`. Se compara en
  tiempo constante con las claves configuradas.
- `tipo` de `/v1/proyecto-inicial`: `pentest`, `redteam`, `oscp`, `htb`,
  `ejecutivo`, `documento`, `cti`, `incidente`, `auditoria`, `cumplimiento`,
  `riesgos`, `hunting` o `retest`. `nombre` (1 a 200 caracteres) y `cliente`
  (hasta 200) sin caracteres de control; campos desconocidos se rechazan.
- `plantilla` es opcional y solo acepta plantillas incluidas (`pentest`,
  `ejecutivo`, `retest`, `oscp`, `htb`, `documento-libre`, `cti`,
  `incidente`); sin ella se usa la del proyecto.
- Toda respuesta trae `X-Pudureport-Version` (servicio), `X-Pudureport-Plantillas`
  (huella de las plantillas incluidas) y `X-Pudureport-Typst`; el render agrega
  `X-Pudureport-Plantilla` (la usada).
- Errores: JSON `{"error": "<codigo>", "detalle"?: "..."}`. `401 unauthorized`,
  `413 payload_too_large` / `package_too_large`, `415 unsupported_media_type`,
  `422 invalid_package` / `unknown_template` / `render_failed` /
  `invalid_vector` / `invalid_payload` / `invalid_type`, `408 upload_timeout`,
  `503 busy` (con `Retry-After`), `504 render_timeout`.
- Orden de `/v1/render`: clave, tipo de contenido, `Content-Length` (si supera
  el limite, `413` sin leer el cuerpo), plantilla y cupo de concurrencia
  (`503` sin leer el cuerpo). Solo con el cupo tomado se lee el cuerpo, con el
  limite aplicado mientras llega (tambien sin `Content-Length`) y con
  `PUDUREPORT_SERVICE_UPLOAD_TIMEOUT_SECS` como tope de tiempo.
- `render_failed` tambien cubre un Markdown que, al convertirlo, supera
  `max(8 x entrada, 1 MiB)` (tope de 64 MiB por texto).

## Configuracion (variables de entorno)

| Variable | Defecto | Uso |
|---|---|---|
| `PUDUREPORT_SERVICE_KEY_FILE` | - | Archivo con una clave por linea (minimo 32 caracteres). Varias lineas permiten rotar. Recomendado (secreto montado). |
| `PUDUREPORT_SERVICE_KEY` | - | Alternativa: la clave en una variable. |
| `PUDUREPORT_SERVICE_ADDR` | `0.0.0.0:8080` | Direccion de escucha. |
| `PUDU_TYPST_BIN` | `typst` del PATH | Binario de Typst. |
| `PUDUREPORT_SERVICE_MAX_BYTES` | `209715200` (200 MiB) | Tamano maximo del paquete. |
| `PUDUREPORT_SERVICE_TIMEOUT_SECS` | `60` | Tiempo maximo de Typst; al vencer se mata el proceso. |
| `PUDUREPORT_SERVICE_UPLOAD_TIMEOUT_SECS` | `120` | Tiempo maximo para recibir el paquete; al vencer, `408` y se libera el cupo. |
| `PUDUREPORT_SERVICE_CONCURRENCY` | `2` | Renders simultaneos; el resto recibe `503`. |
| `PUDUREPORT_SERVICE_TMPDIR` | temporal del sistema | Base de los temporales por trabajo (se borran siempre). |

Sin clave, o con una clave corta, el servicio no arranca. Si Typst no responde
a `--version`, tampoco.

Typst corre con el entorno vacio salvo `PATH`, `HOME`, `TMPDIR`/`TMP`/`TEMP`,
`LANG`/`LC_ALL`/`LC_CTYPE`, `TYPST_FONT_PATHS`, `SOURCE_DATE_EPOCH`, las
variables XDG y de fontconfig (y las del sistema en Windows). Ni
`PUDUREPORT_SERVICE_KEY` ni ninguna otra variable del servicio llegan al
proceso hijo. Cada render usa `typst compile --jobs 1`.

## Ejecutar en local

```sh
# Desde la raiz del repo, con el sidecar de Typst ya descargado.
export PUDUREPORT_SERVICE_KEY="$(openssl rand -hex 32)"
export PUDU_TYPST_BIN="$PWD/src-tauri/binaries/typst-aarch64-apple-darwin"
cargo run -p pudureport-service

curl -sf http://127.0.0.1:8080/healthz
curl -sf -X POST "http://127.0.0.1:8080/v1/render" \
  -H "X-Pudureport-Service-Key: $PUDUREPORT_SERVICE_KEY" \
  -H "Content-Type: application/zip" \
  --data-binary @informe.pudupkg -o informe.pdf
```

Un paquete se obtiene desde la app de escritorio (Vista previa, "Exportar
paquete") o con `pudureport_core::package::export_package`.

## Imagen de contenedor

```sh
docker build -f service/Dockerfile -t pudureport-service .
docker run --rm -p 8080:8080 --read-only --tmpfs /tmp \
  --cap-drop ALL --security-opt no-new-privileges \
  -e PUDUREPORT_SERVICE_KEY_FILE=/run/secrets/clave \
  -v "$PWD/clave.txt:/run/secrets/clave:ro" \
  pudureport-service
```

- Base fijada por digest; Typst `v0.13.1` descargado con verificacion SHA-256
  (amd64 y arm64).
- Usuario sin privilegios (`10001`); solo escribe en `/tmp`, asi que funciona
  con el sistema de archivos raiz de solo lectura.
- Fuentes incluidas: Liberation (OFL), DejaVu y JetBrains Mono (OFL).
- En el cluster: solo red interna, NetworkPolicy sin salida a Internet y
  limites de CPU y memoria acordes a `PUDUREPORT_SERVICE_CONCURRENCY`.

### Limites recomendados del contenedor

Con los valores por defecto (`PUDUREPORT_SERVICE_CONCURRENCY=2`, paquete de
hasta 200 MiB): memoria 1 GiB y 256 procesos (`--memory 1g --pids-limit 256`;
es lo que fija el compose de PuduHub). Cada render retiene el paquete
comprimido, su copia descomprimida en el temporal y el proceso de Typst (un
hilo por render); si se sube la concurrencia o el tamano maximo, subir la
memoria en proporcion. El limite de procesos acota a Typst si algo lo hiciera
crear procesos o hilos sin control. Con `/tmp` en `tmpfs`, su tamano tambien
cuenta como memoria.

## Pruebas

`cargo test -p pudureport-service` ejercita el router completo: render del
proyecto de ejemplo a PDF (se omite si no hay Typst), CVSS 3.1 y 4.0, clave
ausente o incorrecta, limite de tamano (declarado, sin `Content-Length` y sin
fin), subida que no termina, `503 busy` sin leer el cuerpo, tipo de contenido,
paquetes con `../` (zip-slip) y `proyecto-inicial` (todos los tipos y
validacion). `tests/typst_env.rs` usa un Typst falso que vuelca su entorno para
comprobar que la clave no llega al proceso hijo y que se pasa `--jobs 1`.
