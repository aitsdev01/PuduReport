// SPDX-License-Identifier: GPL-3.0-only
// Copyright (c) 2026 bc0d3

//! Cliente HTTP de la API de escritorio de PuduHub (`/api/editor/informes`).
//!
//! - Autenticacion: `Authorization: Bearer phed_...` (cabecera marcada como
//!   sensible). Sin cookies ni CSRF.
//! - Sin redirecciones: una respuesta 3xx es un error (el token no viaja a
//!   otro origen).
//! - Plazos estrictos y topes de tamano en cada respuesta (el paquete hasta
//!   200 MiB); la lectura se corta en el tope aunque el servidor no declare el
//!   tamano.
//! - Los errores se mapean por `(estado, codigo)`: el cuerpo del servidor nunca
//!   se muestra y el token nunca aparece en un mensaje.

use std::fmt;
use std::time::Duration;

use reqwest::{Method, RequestBuilder, Response};
use serde::Serialize;
use serde_json::Value;

use super::{
    clean_text, is_editor_token, is_lock_token, is_loopback_host, is_timestamp, is_uuid,
    normalize_server_url, HubError, EDITOR_PREFIX, MAX_PACKAGE_BYTES, MAX_VERSION,
};

/// Cabecera con el token del bloqueo de edicion.
pub const LOCK_HEADER: &str = "X-Lock-Token";

/// Tamano de pagina del listado (PuduHub acepta 10, 20, 50 o 100).
pub const PAGE_SIZE: u32 = 20;

/// Cantidad maxima de conflictos que se muestran.
const MAX_CONFLICTS: usize = 500;

/// Topes y plazos del cliente. Los valores por defecto siguen el contrato con
/// PuduHub; las pruebas usan topes menores.
#[derive(Debug, Clone)]
pub struct Limits {
    /// Respuestas JSON (listado, detalle, bloqueo, sincronizacion).
    pub json_bytes: u64,
    /// Cuerpo de una respuesta de error (solo se lee el codigo).
    pub error_bytes: u64,
    /// Paquete `.pudupkg` descargado.
    pub package_bytes: u64,
    pub connect_timeout: Duration,
    /// Plazo total de una peticion JSON.
    pub json_timeout: Duration,
    /// Plazo total de la descarga o la subida de un paquete.
    pub package_timeout: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            json_bytes: 4 * 1024 * 1024,
            error_bytes: 64 * 1024,
            package_bytes: MAX_PACKAGE_BYTES,
            connect_timeout: Duration::from_secs(10),
            json_timeout: Duration::from_secs(30),
            // PuduHub da 240 s a paquetes y sincronizacion; un margen encima.
            package_timeout: Duration::from_secs(300),
        }
    }
}

/// Quien tiene el bloqueo de edicion de un informe (validado).
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LockInfo {
    /// Nombre visible de quien lo tiene (si PuduHub lo informa).
    pub nombre: Option<String>,
    /// Vencimiento ISO 8601.
    pub hasta: String,
    /// `web` o `escritorio`.
    pub origen: String,
    /// true si es del mismo usuario (otra sesion).
    pub propio: bool,
}

/// Fila del listado de informes asignados (validada).
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct InformeItem {
    pub id: String,
    pub titulo: String,
    pub tipo: String,
    /// `borrador` o `final`.
    pub estado: String,
    /// Ultima version inmutable (0 si todavia no tiene).
    pub version_actual: u64,
    pub bloqueo: Option<LockInfo>,
    pub actualizado_at: Option<String>,
}

/// Pagina del listado.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct InformePage {
    pub items: Vec<InformeItem>,
    pub total: u64,
    pub page: u32,
    pub page_size: u32,
    /// Filas descartadas por no pasar la validacion.
    pub omitidos: usize,
}

/// Entidad en conflicto de la fusion de tres vias (validada).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Conflict {
    /// `proyecto`, `seccion`, `hallazgo`, `orden` o `asset`.
    pub entidad: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub campo: Option<String>,
}

/// Estado de un informe segun `GET /{id}` (solo lo que usa la apertura).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InformeState {
    /// Revision vigente del borrador (para `POST /{id}/versiones`).
    pub revision: u64,
    /// Ultima version inmutable (0 si todavia no tiene).
    pub version_actual: u64,
}

/// Cabeceras de version de `GET /{id}/paquete`.
pub const VERSION_HEADER: &str = "X-Informe-Version";
pub const PENDING_DRAFT_HEADER: &str = "X-Informe-Borrador-Pendiente";

/// Respuesta de `GET /{id}/paquete` con las cabeceras ya leidas y el cuerpo
/// todavia sin descargar: si el borrador tiene cambios pendientes, la apertura
/// la descarta sin leerla y pide la version nueva.
#[derive(Debug)]
pub struct PackageDownload {
    response: Response,
    cap: u64,
    /// Version que se entrega (`X-Informe-Version`), si PuduHub la informa.
    pub version: Option<u64>,
    /// `X-Informe-Borrador-Pendiente: 1`: el borrador cambio despues de esta
    /// version (cambios de la web que el paquete no incluye).
    pub borrador_pendiente: bool,
}

impl PackageDownload {
    /// Descarga el cuerpo, cortando en el tope de paquete.
    pub async fn bytes(self) -> Result<Vec<u8>, HubError> {
        read_capped(self.response, self.cap).await
    }
}

/// Resultado de `POST /{id}/sincronizar`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncResponse {
    pub revision: u64,
    pub version: u64,
}

/// Cliente de la API de escritorio de PuduHub. El token no sale de aca.
pub struct HubClient {
    http: reqwest::Client,
    servidor: String,
    api: String,
    token: String,
    limits: Limits,
}

impl fmt::Debug for HubClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HubClient")
            .field("api", &self.api)
            .field("token", &"<oculto>")
            .finish()
    }
}

impl HubClient {
    pub fn new(servidor: &str, token: &str) -> Result<Self, HubError> {
        HubClient::with_limits(servidor, token, Limits::default())
    }

    pub fn with_limits(servidor: &str, token: &str, limits: Limits) -> Result<Self, HubError> {
        let servidor = normalize_server_url(servidor)?;
        if !is_editor_token(token) {
            return Err(invalid_token());
        }
        let loopback = reqwest::Url::parse(&servidor)
            .ok()
            .and_then(|u| u.host_str().map(is_loopback_host))
            .unwrap_or(false);
        let mut builder = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(limits.connect_timeout)
            .timeout(limits.package_timeout)
            .user_agent(concat!("PuduReport/", env!("CARGO_PKG_VERSION")))
            .https_only(!loopback);
        if loopback {
            // Desarrollo local: nunca a traves de un proxy del sistema.
            builder = builder.no_proxy();
        }
        let http = builder
            .build()
            .map_err(|_| HubError::new("red", "No se pudo preparar la conexion con PuduHub."))?;
        Ok(HubClient {
            http,
            api: format!("{servidor}{EDITOR_PREFIX}"),
            servidor,
            token: token.to_string(),
            limits,
        })
    }

    /// URL base normalizada.
    pub fn servidor(&self) -> &str {
        &self.servidor
    }

    fn request(&self, method: Method, path: &str, timeout: Duration) -> RequestBuilder {
        self.http
            .request(method, format!("{}{path}", self.api))
            .bearer_auth(&self.token)
            .timeout(timeout)
    }

    async fn send(&self, request: RequestBuilder) -> Result<Response, HubError> {
        let response = request.send().await.map_err(map_transport)?;
        let status = response.status();
        if status.is_redirection() {
            return Err(redirect_error());
        }
        if !status.is_success() {
            let body = read_capped(response, self.limits.error_bytes)
                .await
                .unwrap_or_default();
            return Err(map_status(status.as_u16(), &body));
        }
        Ok(response)
    }

    async fn json(&self, request: RequestBuilder) -> Result<Value, HubError> {
        let response = self.send(request).await?;
        let body = read_capped(response, self.limits.json_bytes).await?;
        serde_json::from_slice(&body).map_err(|_| invalid_response())
    }

    /// `GET /api/editor/informes?page=&pageSize=&q=`: informes del usuario
    /// (los asignados, o todos para un Admin).
    pub async fn list(&self, page: u32, query: &str) -> Result<InformePage, HubError> {
        let page = page.clamp(1, 1_000_000);
        let query = clean_text(query, 200);
        let mut params = vec![
            ("page", page.to_string()),
            ("pageSize", PAGE_SIZE.to_string()),
        ];
        if !query.is_empty() {
            params.push(("q", query));
        }
        let value = self
            .json(
                self.request(Method::GET, "", self.limits.json_timeout)
                    .query(&params),
            )
            .await?;
        parse_page(&value).ok_or_else(invalid_response)
    }

    /// Prueba la conexion y el token con un listado minimo. Devuelve el total
    /// de informes visibles.
    pub async fn test(&self) -> Result<u64, HubError> {
        let value = self
            .json(
                self.request(Method::GET, "", self.limits.json_timeout)
                    .query(&[("page", "1"), ("pageSize", "10")]),
            )
            .await?;
        parse_page(&value)
            .map(|page| page.total)
            .ok_or_else(invalid_response)
    }

    /// `POST /api/editor/informes` `{titulo, tipo}`: crea un informe vacio.
    pub async fn create(&self, titulo: &str, tipo: &str) -> Result<InformeItem, HubError> {
        let body = serde_json::json!({ "titulo": titulo, "tipo": tipo });
        let value = self
            .json(
                self.request(Method::POST, "", self.limits.json_timeout)
                    .json(&body),
            )
            .await?;
        parse_item(&value).ok_or_else(invalid_response)
    }

    /// `GET /{id}`: revision del borrador y ultima version.
    pub async fn state(&self, id: &str) -> Result<InformeState, HubError> {
        let id = checked_id(id)?;
        let value = self
            .json(self.request(Method::GET, &format!("/{id}"), self.limits.json_timeout))
            .await?;
        let item = parse_item(&value).ok_or_else(invalid_response)?;
        let revision = positive(value.get("revision"), u64::MAX).ok_or_else(invalid_response)?;
        if !item.id.eq_ignore_ascii_case(id) {
            return Err(invalid_response());
        }
        Ok(InformeState {
            revision,
            version_actual: item.version_actual,
        })
    }

    /// `POST /{id}/versiones` `{motivo, revision}` con el token del bloqueo:
    /// guarda el borrador como version (punto de control) y devuelve su numero.
    pub async fn create_version(
        &self,
        id: &str,
        lock: &str,
        motivo: &str,
        revision: u64,
    ) -> Result<u64, HubError> {
        let id = checked_id(id)?;
        let value = self
            .json(
                self.request(
                    Method::POST,
                    &format!("/{id}/versiones"),
                    self.limits.json_timeout,
                )
                .header(LOCK_HEADER, lock)
                .json(&serde_json::json!({ "motivo": motivo, "revision": revision })),
            )
            .await?;
        positive(value.get("version"), MAX_VERSION).ok_or_else(invalid_response)
    }

    /// `GET /{id}/paquete[?version=N]`: el `.pudupkg` de una version existente
    /// (sin `version`, la ultima). Solo lectura: nunca crea versiones; sin
    /// ninguna, PuduHub responde `409 version_required`.
    pub async fn package(
        &self,
        id: &str,
        version: Option<u64>,
    ) -> Result<PackageDownload, HubError> {
        let id = checked_id(id)?;
        let mut request = self
            .request(
                Method::GET,
                &format!("/{id}/paquete"),
                self.limits.package_timeout,
            )
            .header(reqwest::header::ACCEPT, "application/zip");
        if let Some(version) = version {
            if version == 0 || version > MAX_VERSION {
                return Err(invalid_response());
            }
            request = request.query(&[("version", version.to_string())]);
        }
        let response = self.send(request).await?;
        let headers = response.headers();
        let content_type = headers
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        if content_type != "application/zip" && content_type != "application/octet-stream" {
            return Err(invalid_response());
        }
        let served = match headers.get(VERSION_HEADER) {
            None => None,
            Some(value) => Some(
                value
                    .to_str()
                    .ok()
                    .and_then(|v| v.trim().parse::<u64>().ok())
                    .filter(|v| (1..=MAX_VERSION).contains(v))
                    .ok_or_else(invalid_response)?,
            ),
        };
        if version.is_some_and(|v| served.is_some_and(|s| s != v)) {
            return Err(invalid_response());
        }
        let borrador_pendiente = match headers.get(PENDING_DRAFT_HEADER).map(|v| v.as_bytes()) {
            None | Some(b"0") => false,
            Some(b"1") => true,
            Some(_) => return Err(invalid_response()),
        };
        if response
            .content_length()
            .is_some_and(|len| len > self.limits.package_bytes)
        {
            return Err(too_large_response());
        }
        Ok(PackageDownload {
            response,
            cap: self.limits.package_bytes,
            version: served,
            borrador_pendiente,
        })
    }

    /// `POST /{id}/bloqueo` `{forzar}`: toma el bloqueo de edicion y devuelve
    /// su token (para `X-Lock-Token`).
    pub async fn lock(&self, id: &str, forzar: bool) -> Result<String, HubError> {
        let id = checked_id(id)?;
        let value = self
            .json(
                self.request(
                    Method::POST,
                    &format!("/{id}/bloqueo"),
                    self.limits.json_timeout,
                )
                .json(&serde_json::json!({ "forzar": forzar })),
            )
            .await?;
        match value.get("token").and_then(Value::as_str) {
            Some(token) if is_lock_token(token) => Ok(token.to_string()),
            // Sin token: PuduHub renovo un bloqueo existente, que no es de
            // esta operacion. No se puede escribir sin el token.
            _ => Err(HubError::new(
                "bloqueo_tomado",
                "El informe ya esta en edicion en otra sesion. Cierrela o espere a que el bloqueo venza.",
            )),
        }
    }

    /// `DELETE /{id}/bloqueo`: libera el bloqueo (siempre al terminar).
    pub async fn unlock(&self, id: &str, lock: &str) -> Result<(), HubError> {
        let id = checked_id(id)?;
        self.json(
            self.request(
                Method::DELETE,
                &format!("/{id}/bloqueo"),
                self.limits.json_timeout,
            )
            .header(LOCK_HEADER, lock),
        )
        .await
        .map(|_| ())
    }

    /// `POST /{id}/sincronizar` con el paquete (`application/zip`) y el token
    /// del bloqueo. PuduHub fusiona y crea una version nueva.
    pub async fn synchronize(
        &self,
        id: &str,
        lock: &str,
        package: Vec<u8>,
    ) -> Result<SyncResponse, HubError> {
        let id = checked_id(id)?;
        if package.len() as u64 > MAX_PACKAGE_BYTES {
            return Err(package_too_large());
        }
        let response = self
            .send(
                self.request(
                    Method::POST,
                    &format!("/{id}/sincronizar"),
                    self.limits.package_timeout,
                )
                .header(LOCK_HEADER, lock)
                .header(reqwest::header::CONTENT_TYPE, "application/zip")
                .body(package),
            )
            .await?;
        let body = read_capped(response, self.limits.json_bytes).await?;
        let value: Value = serde_json::from_slice(&body).map_err(|_| invalid_response())?;
        let revision = positive(value.get("revision"), u64::MAX).ok_or_else(invalid_response)?;
        let version = positive(
            value.get("version").and_then(|v| v.get("version")),
            MAX_VERSION,
        )
        .ok_or_else(invalid_response)?;
        Ok(SyncResponse { revision, version })
    }
}

fn checked_id(id: &str) -> Result<&str, HubError> {
    if is_uuid(id) {
        Ok(id)
    } else {
        Err(HubError::new(
            "datos_invalidos",
            "El identificador del informe no es valido.",
        ))
    }
}

/// Lee el cuerpo completo cortando en `cap` bytes, declare o no su tamano.
async fn read_capped(mut response: Response, cap: u64) -> Result<Vec<u8>, HubError> {
    if response.content_length().is_some_and(|len| len > cap) {
        return Err(too_large_response());
    }
    let mut out = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(map_transport)? {
        if out.len() as u64 + chunk.len() as u64 > cap {
            return Err(too_large_response());
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

fn positive(value: Option<&Value>, max: u64) -> Option<u64> {
    value
        .and_then(Value::as_u64)
        .filter(|n| *n >= 1 && *n <= max)
}

fn parse_page(value: &Value) -> Option<InformePage> {
    let items = value.get("items")?.as_array()?;
    let total = value.get("total")?.as_u64()?;
    let page = value.get("page").and_then(Value::as_u64).unwrap_or(1);
    let page_size = value
        .get("pageSize")
        .and_then(Value::as_u64)
        .unwrap_or(PAGE_SIZE as u64);
    let parsed: Vec<InformeItem> = items.iter().take(100).filter_map(parse_item).collect();
    Some(InformePage {
        omitidos: items.len() - parsed.len(),
        items: parsed,
        total,
        page: page.min(1_000_000) as u32,
        page_size: page_size.min(100) as u32,
    })
}

/// Valida una fila de informe. Devuelve None si un campo obligatorio no es
/// valido (la fila no se muestra).
fn parse_item(value: &Value) -> Option<InformeItem> {
    let id = value.get("id")?.as_str()?;
    if !is_uuid(id) {
        return None;
    }
    let estado = value.get("estado")?.as_str()?;
    if !matches!(estado, "borrador" | "final") {
        return None;
    }
    let version_actual = value.get("versionActual")?.as_u64()?;
    if version_actual > MAX_VERSION {
        return None;
    }
    let titulo = clean_text(value.get("titulo")?.as_str()?, 200);
    let tipo = value
        .get("tipo")
        .and_then(Value::as_str)
        .filter(|t| {
            !t.is_empty()
                && t.len() <= 40
                && t.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_'))
        })
        .unwrap_or("")
        .to_string();
    let bloqueo = match value.get("bloqueo") {
        None | Some(Value::Null) => None,
        Some(lock) => Some(parse_lock(lock)?),
    };
    let actualizado_at = value
        .get("actualizadoAt")
        .and_then(Value::as_str)
        .filter(|t| is_timestamp(t))
        .map(str::to_string);
    Some(InformeItem {
        id: id.to_ascii_lowercase(),
        titulo: if titulo.is_empty() {
            "Sin titulo".to_string()
        } else {
            titulo
        },
        tipo,
        estado: estado.to_string(),
        version_actual,
        bloqueo,
        actualizado_at,
    })
}

fn parse_lock(value: &Value) -> Option<LockInfo> {
    let hasta = value.get("hasta")?.as_str()?;
    if !is_timestamp(hasta) {
        return None;
    }
    let origen = value.get("origen").and_then(Value::as_str).unwrap_or("");
    let origen = if matches!(origen, "web" | "escritorio") {
        origen
    } else {
        ""
    };
    let nombre = value
        .get("nombre")
        .and_then(Value::as_str)
        .map(|n| clean_text(n, 120))
        .filter(|n| !n.is_empty());
    Some(LockInfo {
        nombre,
        hasta: hasta.to_string(),
        origen: origen.to_string(),
        propio: value
            .get("propio")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

fn safe_conflict_text(value: Option<&Value>, max: usize) -> Option<String> {
    value
        .and_then(Value::as_str)
        .filter(|v| {
            !v.is_empty()
                && v.len() <= max
                && v.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'))
        })
        .map(str::to_string)
}

fn parse_conflicts(value: &Value) -> Vec<Conflict> {
    value
        .get("entidades")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let entidad = item.get("entidad")?.as_str()?;
                    if !matches!(
                        entidad,
                        "proyecto" | "seccion" | "hallazgo" | "orden" | "asset"
                    ) {
                        return None;
                    }
                    Some(Conflict {
                        entidad: entidad.to_string(),
                        id: safe_conflict_text(item.get("id"), 255),
                        campo: safe_conflict_text(item.get("campo"), 64),
                    })
                })
                .take(MAX_CONFLICTS)
                .collect()
        })
        .unwrap_or_default()
}

/// Codigo de error publico de PuduHub (`{"error": "<codigo>"}`), si es valido.
fn error_code(body: &Value) -> &str {
    body.get("error")
        .and_then(Value::as_str)
        .filter(|c| c.len() <= 64 && c.chars().all(|ch| ch.is_ascii_lowercase() || ch == '_'))
        .unwrap_or("")
}

fn invalid_token() -> HubError {
    HubError::new(
        "token_invalido",
        "El token no tiene el formato de un token de editor de PuduHub (phed_ seguido de 43 caracteres).",
    )
}

pub(crate) fn invalid_response() -> HubError {
    HubError::new(
        "respuesta_invalida",
        "PuduHub respondio con datos que no se pueden usar. Revise que la URL apunte a PuduHub.",
    )
}

fn too_large_response() -> HubError {
    HubError::new(
        "respuesta_grande",
        "La respuesta de PuduHub supera el tamano permitido y se descarto.",
    )
}

pub(crate) fn package_too_large() -> HubError {
    HubError::new(
        "paquete_grande",
        "El paquete supera el limite de PuduHub (200 MiB en total, 10 MiB por imagen).",
    )
}

fn redirect_error() -> HubError {
    HubError::new(
        "redireccion",
        "PuduHub respondio con una redireccion y no se siguio (el token no se envia a otra direccion). Revise la URL base en Ajustes.",
    )
}

/// Mapea un estado HTTP de error y el codigo publico del cuerpo a un error
/// propio. Del cuerpo solo se leen el codigo y los detalles validados.
pub(crate) fn map_status(status: u16, body: &[u8]) -> HubError {
    let parsed: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    let code = error_code(&parsed);
    match (status, code) {
        (401, _) => HubError::new(
            "no_autorizado",
            "PuduHub rechazo el token: es invalido, expiro o fue revocado. Genere uno nuevo en PuduHub y guardelo en Ajustes.",
        ),
        (403, _) => HubError::new(
            "prohibido",
            "Su usuario de PuduHub no tiene permiso para esta accion o para este informe.",
        ),
        (404, _) => HubError::new(
            "no_encontrado",
            "El informe no existe en PuduHub o no esta asignado a su usuario.",
        ),
        (409, "lock_held") => {
            let lock = parsed.get("bloqueo").and_then(parse_lock);
            let propio = lock.as_ref().is_some_and(|l| l.propio);
            let puede_forzar =
                propio || parsed.get("puedeForzar").and_then(Value::as_bool) == Some(true);
            let quien = if propio {
                "usted, en otra sesion (web o escritorio)".to_string()
            } else {
                lock.as_ref()
                    .and_then(|l| l.nombre.clone())
                    .unwrap_or_else(|| "otro usuario".to_string())
            };
            let mut error = HubError::new(
                "bloqueo_tomado",
                format!("El informe esta en edicion por {quien}. Espere a que lo libere o a que el bloqueo venza."),
            );
            error.extra.bloqueo = lock;
            error.extra.puede_forzar = puede_forzar;
            error
        }
        (409, "lock_required") => HubError::new(
            "bloqueo_requerido",
            "El bloqueo de edicion vencio durante el envio. Vuelva a intentarlo.",
        ),
        (409, "stale_revision") => HubError::new(
            "revision_desactualizada",
            "El informe cambio en PuduHub mientras se enviaba. Vuelva a intentarlo.",
        ),
        (409, "merge_conflict") => {
            let mut error = HubError::new(
                "conflicto",
                "Hay cambios distintos en PuduHub y en este equipo sobre las mismas partes del informe. No se envio nada.",
            );
            error.extra.conflictos = parse_conflicts(&parsed);
            error
        }
        (409, "version_required") => HubError::new(
            "sin_version",
            "El informe todavia no tiene ninguna version en PuduHub y no se pudo crear una porque otra persona lo esta editando. Vuelva a intentarlo cuando se libere el bloqueo.",
        ),
        (409, "unknown_base_version") => HubError::new(
            "version_base_desconocida",
            "PuduHub no reconoce la version de la que partio este proyecto. Descargue la version del servidor.",
        ),
        (409, _) => HubError::new(
            "conflicto_servidor",
            "PuduHub rechazo la operacion por un conflicto. Vuelva a intentarlo.",
        ),
        (413, _) => package_too_large(),
        (415, _) => HubError::new(
            "tipo_no_soportado",
            "PuduHub no acepto el tipo de contenido enviado.",
        ),
        (422, "invalid_package") => HubError::new(
            "paquete_invalido",
            "PuduHub rechazo el paquete: no cumple el formato (por ejemplo, una imagen no permitida o una evidencia que no esta en el proyecto).",
        ),
        (400, _) | (422, _) => HubError::new(
            "datos_invalidos",
            "PuduHub rechazo los datos enviados por no ser validos.",
        ),
        (429, _) => {
            let mut error = HubError::new(
                "limite_peticiones",
                "Demasiados intentos seguidos. Espere un momento y vuelva a intentarlo.",
            );
            error.extra.reintentar_en = parsed
                .get("retryAfter")
                .and_then(Value::as_u64)
                .filter(|s| *s <= 3600)
                .map(|s| s as u32);
            error
        }
        (500..=599, _) => HubError::new(
            "servicio_no_disponible",
            "PuduHub no esta disponible en este momento. Intente mas tarde.",
        ),
        _ => HubError::new(
            "error_servidor",
            format!("PuduHub respondio con un estado inesperado ({status})."),
        ),
    }
}

/// Mapea un error de transporte (sin respuesta HTTP) sin copiar su texto.
fn map_transport(error: reqwest::Error) -> HubError {
    if error.is_timeout() {
        return HubError::new(
            "tiempo_agotado",
            "PuduHub no respondio a tiempo. Revise su conexion e intente de nuevo.",
        );
    }
    if error.is_redirect() {
        return redirect_error();
    }
    if is_tls_error(&error) {
        return HubError::new(
            "tls",
            "No se pudo establecer una conexion segura (TLS) con PuduHub: el certificado no es valido o no es de confianza. Puede deberse a un proxy corporativo que inspecciona TLS, a un certificado vencido o a una URL que no usa https.",
        );
    }
    HubError::new(
        "red",
        "No se pudo conectar con PuduHub. Revise la URL, su conexion a Internet o la VPN.",
    )
}

/// true si alguna causa del error es de TLS o de certificados. Solo se mira
/// el texto para clasificar: nunca se muestra.
fn is_tls_error(error: &reqwest::Error) -> bool {
    let mut source: Option<&dyn std::error::Error> = Some(error);
    while let Some(current) = source {
        let text = current.to_string().to_ascii_lowercase();
        if [
            "certificate",
            "tls",
            "handshake",
            "corrupt message",
            "invalidcontenttype",
            "unknownissuer",
            "peer sent",
        ]
        .iter()
        .any(|needle| text.contains(needle))
        {
            return true;
        }
        source = current.source();
    }
    false
}
