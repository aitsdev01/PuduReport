// SPDX-License-Identifier: GPL-3.0-only
// Copyright (c) 2026 bc0d3

//! `pudureport-service`: genera el PDF de un paquete `.pudupkg` con el mismo
//! pipeline de `pudureport-core` (data.json + plantillas Typst incluidas) y
//! calcula CVSS. Pensado para una red interna: no hace llamadas de red, exige
//! una clave compartida en cada peticion y nunca registra el contenido de los
//! documentos.
//!
//! Rutas:
//! - `POST /v1/render` cuerpo: paquete `.pudupkg`; `?plantilla=<nombre>`
//!   opcional. Responde `application/pdf`.
//! - `POST /v1/cvss` `{"vector": "...", "version": "3.1" | "4.0"}`.
//! - `POST /v1/proyecto-inicial` `{"tipo", "nombre", "cliente"?}`: el
//!   `project.yaml` de un proyecto nuevo de ese tipo, como lo crea la app.
//! - `GET /healthz` sin autenticacion (sondas del orquestador).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::{Body, Bytes};
use axum::extract::rejection::{BytesRejection, JsonRejection, QueryRejection};
use axum::extract::{DefaultBodyLimit, Query, Request, State};
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use pudureport_core::models::{CvssVersion, PROJECT_TYPES};
use pudureport_core::package::{self, ImportOptions, PackageError, PackageLimits};
use pudureport_core::pdf::{self, PdfError, RenderOptions};
use pudureport_core::{cvss, workspace};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;

/// Cabecera con la clave compartida.
pub const KEY_HEADER: &str = "x-pudureport-service-key";
/// Version del servicio (y del core) que genero la respuesta.
pub const VERSION_HEADER: &str = "x-pudureport-version";
/// Huella de las plantillas incluidas (SHA-256 abreviado).
pub const TEMPLATES_HEADER: &str = "x-pudureport-plantillas";
/// Plantilla usada en un render.
pub const TEMPLATE_HEADER: &str = "x-pudureport-plantilla";
/// Version del motor Typst.
pub const TYPST_HEADER: &str = "x-pudureport-typst";

/// Largo minimo de una clave compartida.
pub const MIN_KEY_LEN: usize = 32;

const MIB: usize = 1024 * 1024;

/// Configuracion del servicio.
#[derive(Clone)]
pub struct Config {
    /// SHA-256 de las claves aceptadas (mas de una durante una rotacion).
    key_digests: Vec<[u8; 32]>,
    pub typst_bin: PathBuf,
    pub typst_version: String,
    /// Tamano maximo del cuerpo de `/v1/render` (paquete comprimido).
    pub max_body_bytes: usize,
    /// Tiempo maximo de Typst por render.
    pub render_timeout: Duration,
    /// Renders simultaneos; el resto recibe 503.
    pub concurrency: usize,
    /// Directorio base de los temporales por trabajo.
    pub temp_dir: PathBuf,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Nunca imprime las claves (ni sus hashes).
        f.debug_struct("Config")
            .field("claves", &self.key_digests.len())
            .field("typst_bin", &self.typst_bin)
            .field("typst_version", &self.typst_version)
            .field("max_body_bytes", &self.max_body_bytes)
            .field("render_timeout", &self.render_timeout)
            .field("concurrency", &self.concurrency)
            .field("temp_dir", &self.temp_dir)
            .finish()
    }
}

fn digest(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

/// Lee las claves de un texto: una por linea; se ignoran lineas vacias y las
/// que empiezan con `#`. Varias claves permiten rotar sin cortar el servicio.
pub fn parse_keys(text: &str) -> Result<Vec<String>, String> {
    let keys: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_string)
        .collect();
    if keys.is_empty() {
        return Err("no hay ninguna clave configurada".to_string());
    }
    if keys.iter().any(|k| k.len() < MIN_KEY_LEN) {
        return Err(format!(
            "cada clave debe tener al menos {MIN_KEY_LEN} caracteres"
        ));
    }
    Ok(keys)
}

impl Config {
    /// Configuracion con valores por defecto y las claves indicadas.
    pub fn new(keys: &[String], typst_bin: PathBuf, typst_version: String) -> Self {
        Config {
            key_digests: keys.iter().map(|k| digest(k.as_bytes())).collect(),
            typst_bin,
            typst_version,
            max_body_bytes: 200 * MIB,
            render_timeout: Duration::from_secs(60),
            concurrency: 2,
            temp_dir: std::env::temp_dir(),
        }
    }

    /// Lee la configuracion del entorno. Falla (y el servicio no arranca) si
    /// no hay clave o si es demasiado corta.
    ///
    /// - `PUDUREPORT_SERVICE_KEY_FILE`: archivo con una clave por linea
    ///   (recomendado, por ejemplo un secreto montado).
    /// - `PUDUREPORT_SERVICE_KEY`: la clave en una variable (alternativa).
    /// - `PUDU_TYPST_BIN`: ruta del binario de Typst.
    /// - `PUDUREPORT_SERVICE_MAX_BYTES`, `PUDUREPORT_SERVICE_TIMEOUT_SECS`,
    ///   `PUDUREPORT_SERVICE_CONCURRENCY`, `PUDUREPORT_SERVICE_TMPDIR`.
    pub fn from_env() -> Result<Self, String> {
        let text = match std::env::var("PUDUREPORT_SERVICE_KEY_FILE") {
            Ok(path) if !path.is_empty() => std::fs::read_to_string(&path)
                .map_err(|e| format!("no se pudo leer PUDUREPORT_SERVICE_KEY_FILE: {e}"))?,
            _ => std::env::var("PUDUREPORT_SERVICE_KEY").unwrap_or_default(),
        };
        let keys = parse_keys(&text)?;
        let typst_bin = pdf::resolve_typst().map_err(|e| e.to_string())?;
        let typst_version = typst_version(&typst_bin)?;
        let mut cfg = Config::new(&keys, typst_bin, typst_version);
        if let Some(v) = env_number("PUDUREPORT_SERVICE_MAX_BYTES")? {
            cfg.max_body_bytes = v as usize;
        }
        if let Some(v) = env_number("PUDUREPORT_SERVICE_TIMEOUT_SECS")? {
            cfg.render_timeout = Duration::from_secs(v);
        }
        if let Some(v) = env_number("PUDUREPORT_SERVICE_CONCURRENCY")? {
            cfg.concurrency = (v as usize).max(1);
        }
        if let Ok(dir) = std::env::var("PUDUREPORT_SERVICE_TMPDIR") {
            if !dir.is_empty() {
                cfg.temp_dir = PathBuf::from(dir);
            }
        }
        Ok(cfg)
    }

    /// Compara la clave recibida con las configuradas en tiempo constante
    /// (sobre los SHA-256, asi el largo no filtra informacion y se recorren
    /// todas las claves sin salir antes).
    pub fn accepts(&self, provided: &[u8]) -> bool {
        let candidate = digest(provided);
        let mut ok = 0u8;
        for key in &self.key_digests {
            let mut diff = 0u8;
            for (a, b) in key.iter().zip(candidate.iter()) {
                diff |= a ^ b;
            }
            ok |= u8::from(diff == 0);
        }
        ok == 1
    }
}

fn env_number(name: &str) -> Result<Option<u64>, String> {
    match std::env::var(name) {
        Ok(v) if !v.trim().is_empty() => v
            .trim()
            .parse::<u64>()
            .map(Some)
            .map_err(|_| format!("{name} debe ser un numero entero")),
        _ => Ok(None),
    }
}

/// Ejecuta `typst --version`; ademas confirma que el binario funciona.
pub fn typst_version(bin: &Path) -> Result<String, String> {
    let output = std::process::Command::new(bin)
        .arg("--version")
        .output()
        .map_err(|e| format!("no se pudo ejecutar Typst ({}): {e}", bin.display()))?;
    if !output.status.success() {
        return Err("Typst no respondio a --version".to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Huella de las plantillas incluidas: cambia si cambia cualquier plantilla.
pub fn templates_fingerprint() -> String {
    let mut hasher = Sha256::new();
    for (name, source) in pdf::BUNDLED_TEMPLATES {
        hasher.update(name.as_bytes());
        hasher.update([0]);
        hasher.update(source.as_bytes());
        hasher.update([0]);
    }
    hasher
        .finalize()
        .iter()
        .take(6)
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Registro estructurado (una linea JSON por evento). Nunca incluye el
/// contenido de los documentos: solo metadatos operativos.
pub fn log_event(evento: &str, fields: Value) {
    let mut line = json!({ "ts": workspace::now_utc_iso(), "evento": evento });
    if let (Some(obj), Value::Object(extra)) = (line.as_object_mut(), fields) {
        obj.extend(extra);
    }
    println!("{line}");
}

/// Estado compartido por los manejadores.
#[derive(Clone)]
pub struct AppState {
    cfg: Arc<Config>,
    permits: Arc<Semaphore>,
    templates: Arc<str>,
}

impl AppState {
    pub fn new(cfg: Config) -> Self {
        let permits = Arc::new(Semaphore::new(cfg.concurrency.max(1)));
        AppState {
            cfg: Arc::new(cfg),
            permits,
            templates: templates_fingerprint().into(),
        }
    }
}

/// Router completo del servicio.
pub fn router(state: AppState) -> Router {
    let render = post(render).layer(DefaultBodyLimit::max(state.cfg.max_body_bytes));
    let cvss = post(cvss_handler).layer(DefaultBodyLimit::max(16 * 1024));
    let initial = post(initial_project).layer(DefaultBodyLimit::max(16 * 1024));
    let protected = Router::new()
        .route("/v1/render", render)
        .route("/v1/cvss", cvss)
        .route("/v1/proyecto-inicial", initial)
        // La clave se verifica antes de leer el cuerpo.
        .route_layer(middleware::from_fn_with_state(state.clone(), require_key));
    Router::new()
        .route("/healthz", get(healthz))
        .merge(protected)
        .fallback(|| async { error(StatusCode::NOT_FOUND, "not_found", None) })
        .layer(middleware::from_fn_with_state(
            state.clone(),
            common_headers,
        ))
        .with_state(state)
}

fn error(status: StatusCode, code: &str, detalle: Option<String>) -> Response {
    let mut body = json!({ "error": code });
    if let Some(d) = detalle {
        body["detalle"] = Value::String(d);
    }
    (status, Json(body)).into_response()
}

/// Valor de cabecera; nunca vacio (PuduHub exige las cuatro de version).
fn header_value(value: &str) -> HeaderValue {
    let value = value.trim();
    if value.is_empty() {
        return HeaderValue::from_static("desconocida");
    }
    HeaderValue::from_str(value).unwrap_or_else(|_| HeaderValue::from_static("desconocida"))
}

/// Cabeceras comunes y registro de acceso (metodo, ruta, estado, duracion).
async fn common_headers(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let started = Instant::now();
    let method = req.method().to_string();
    let path = req.uri().path().to_string();
    let request_id = uuid::Uuid::new_v4().to_string();
    let mut response = next.run(req).await;
    let headers = response.headers_mut();
    headers.insert(
        HeaderName::from_static(VERSION_HEADER),
        HeaderValue::from_static(env!("CARGO_PKG_VERSION")),
    );
    headers.insert(
        HeaderName::from_static(TEMPLATES_HEADER),
        header_value(&state.templates),
    );
    headers.insert(
        HeaderName::from_static(TYPST_HEADER),
        header_value(&state.cfg.typst_version),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        HeaderName::from_static("x-request-id"),
        header_value(&request_id),
    );
    log_event(
        "http",
        json!({
            "request_id": request_id,
            "metodo": method,
            "ruta": path,
            "estado": response.status().as_u16(),
            "ms": started.elapsed().as_millis() as u64,
        }),
    );
    response
}

async fn require_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    req: Request,
    next: Next,
) -> Response {
    let provided = headers
        .get(KEY_HEADER)
        .map(|v| v.as_bytes())
        .unwrap_or_default();
    if provided.is_empty() || !state.cfg.accepts(provided) {
        return error(StatusCode::UNAUTHORIZED, "unauthorized", None);
    }
    next.run(req).await
}

async fn healthz(State(state): State<AppState>) -> Response {
    Json(json!({
        "estado": "ok",
        "version": env!("CARGO_PKG_VERSION"),
        "plantillas": &*state.templates,
        "typst": state.cfg.typst_version,
    }))
    .into_response()
}

#[derive(Deserialize)]
struct CvssRequest {
    vector: String,
    #[serde(default)]
    version: Option<String>,
}

async fn cvss_handler(payload: Result<Json<CvssRequest>, JsonRejection>) -> Response {
    let Ok(Json(req)) = payload else {
        return error(StatusCode::UNPROCESSABLE_ENTITY, "invalid_payload", None);
    };
    let vector = req.vector.trim();
    if vector.is_empty() || vector.len() > 512 {
        return error(StatusCode::UNPROCESSABLE_ENTITY, "invalid_vector", None);
    }
    let version = match req.version.as_deref() {
        Some("3.1") => CvssVersion::V31,
        Some("4.0") => CvssVersion::V40,
        Some(_) => return error(StatusCode::UNPROCESSABLE_ENTITY, "invalid_version", None),
        None if vector.starts_with("CVSS:4.0") => CvssVersion::V40,
        None => CvssVersion::V31,
    };
    match cvss::calc(version, vector) {
        Ok(result) => Json(json!({
            "score": result.score,
            "severity": result.severity,
            "vector": result.vector,
            "version": match version {
                CvssVersion::V31 => "3.1",
                CvssVersion::V40 => "4.0",
            },
        }))
        .into_response(),
        Err(e) => error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_vector",
            Some(e.to_string()),
        ),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InitialProjectRequest {
    tipo: String,
    nombre: String,
    #[serde(default)]
    cliente: Option<String>,
}

/// Texto corto valido: sin caracteres de control y con largo acotado.
fn short_text(value: &str, max: usize) -> Option<String> {
    let value = value.trim();
    (value.chars().count() <= max && !value.chars().any(char::is_control))
        .then(|| value.to_string())
}

/// `project.yaml` de un proyecto nuevo del tipo pedido, con las mismas
/// secciones, layout y valores por defecto que crea la app de escritorio.
async fn initial_project(payload: Result<Json<InitialProjectRequest>, JsonRejection>) -> Response {
    let Ok(Json(req)) = payload else {
        return error(StatusCode::UNPROCESSABLE_ENTITY, "invalid_payload", None);
    };
    let tipo = req.tipo.trim();
    if !PROJECT_TYPES.contains(&tipo) {
        return error(StatusCode::UNPROCESSABLE_ENTITY, "invalid_type", None);
    }
    let nombre = match short_text(&req.nombre, 200) {
        Some(n) if !n.is_empty() => n,
        _ => return error(StatusCode::UNPROCESSABLE_ENTITY, "invalid_payload", None),
    };
    let Some(cliente) = short_text(req.cliente.as_deref().unwrap_or_default(), 200) else {
        return error(StatusCode::UNPROCESSABLE_ENTITY, "invalid_payload", None);
    };
    match workspace::initial_project_yaml(&nombre, &cliente, tipo) {
        Ok(yaml) => {
            log_event(
                "proyecto_inicial",
                json!({ "tipo": tipo, "bytes_salida": yaml.len() }),
            );
            let mut response = Response::new(Body::from(yaml));
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/yaml; charset=utf-8"),
            );
            response
        }
        Err(_) => error(StatusCode::INTERNAL_SERVER_ERROR, "internal_error", None),
    }
}

#[derive(Deserialize)]
struct RenderQuery {
    #[serde(default)]
    plantilla: Option<String>,
}

/// Directorio temporal de un trabajo; se borra al salir de alcance, tambien
/// si el render falla o vence.
struct JobDir(PathBuf);

impl JobDir {
    fn create(base: &Path) -> std::io::Result<Self> {
        let dir = base.join(format!("pudureport-job-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir)?;
        Ok(JobDir(dir))
    }
}

impl Drop for JobDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Error de un render, ya clasificado para la respuesta HTTP.
struct RenderFailure {
    status: StatusCode,
    code: &'static str,
    detalle: Option<String>,
}

impl From<PackageError> for RenderFailure {
    fn from(e: PackageError) -> Self {
        match e {
            PackageError::Invalid(_) => RenderFailure {
                status: StatusCode::UNPROCESSABLE_ENTITY,
                code: "invalid_package",
                detalle: Some(e.to_string()),
            },
            PackageError::TooLarge(_) => RenderFailure {
                status: StatusCode::PAYLOAD_TOO_LARGE,
                code: "package_too_large",
                detalle: Some(e.to_string()),
            },
            _ => internal(),
        }
    }
}

impl From<PdfError> for RenderFailure {
    fn from(e: PdfError) -> Self {
        match e {
            PdfError::TemplateNotFound(_) => RenderFailure {
                status: StatusCode::UNPROCESSABLE_ENTITY,
                code: "unknown_template",
                detalle: Some(e.to_string()),
            },
            PdfError::Compile(_) => RenderFailure {
                status: StatusCode::UNPROCESSABLE_ENTITY,
                code: "render_failed",
                detalle: Some(e.to_string()),
            },
            PdfError::Timeout(_) => RenderFailure {
                status: StatusCode::GATEWAY_TIMEOUT,
                code: "render_timeout",
                detalle: None,
            },
            _ => internal(),
        }
    }
}

fn internal() -> RenderFailure {
    RenderFailure {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        code: "internal_error",
        detalle: None,
    }
}

struct Rendered {
    pdf: Vec<u8>,
    template: String,
    filename: String,
}

/// Importa el paquete en un workspace temporal y genera el PDF. Bloqueante.
fn render_package(
    cfg: &Config,
    bytes: &[u8],
    template: Option<String>,
) -> Result<Rendered, RenderFailure> {
    let job = JobDir::create(&cfg.temp_dir).map_err(|_| internal())?;
    let root = job.0.join("ws");
    let limits = PackageLimits {
        max_package_bytes: cfg.max_body_bytes as u64,
        ..Default::default()
    };
    let imported = package::import_package(
        &root,
        bytes,
        &ImportOptions {
            apply_workspace: true,
            project_id: Some("informe".to_string()),
            limits,
        },
    )?;
    let rendered = pdf::render_bundled(
        &root,
        &imported.project_id,
        &cfg.typst_bin,
        &RenderOptions {
            template,
            timeout: Some(cfg.render_timeout),
        },
    )?;
    let pdf = std::fs::read(&rendered.path).map_err(|_| internal())?;
    let filename = rendered
        .path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "reporte.pdf".to_string())
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .collect::<String>();
    Ok(Rendered {
        pdf,
        template: rendered.template,
        filename,
    })
}

fn accepted_content_type(headers: &HeaderMap) -> bool {
    let value = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    let mime = value.split(';').next().unwrap_or_default().trim();
    matches!(
        mime.to_ascii_lowercase().as_str(),
        "application/zip" | "application/octet-stream"
    )
}

async fn render(
    State(state): State<AppState>,
    query: Result<Query<RenderQuery>, QueryRejection>,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    let started = Instant::now();
    let Ok(Query(query)) = query else {
        return error(StatusCode::UNPROCESSABLE_ENTITY, "invalid_query", None);
    };
    if !accepted_content_type(&headers) {
        return error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_media_type",
            None,
        );
    }
    let body = match body {
        Ok(body) => body,
        Err(rejection) if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE => {
            return error(StatusCode::PAYLOAD_TOO_LARGE, "payload_too_large", None)
        }
        Err(_) => return error(StatusCode::BAD_REQUEST, "invalid_body", None),
    };
    let template = query.plantilla.filter(|t| !t.is_empty());
    if let Some(name) = &template {
        if !pdf::bundled_template_names().contains(&name.as_str()) {
            return error(StatusCode::UNPROCESSABLE_ENTITY, "unknown_template", None);
        }
    }
    let Ok(permit) = state.permits.clone().try_acquire_owned() else {
        let mut response = error(StatusCode::SERVICE_UNAVAILABLE, "busy", None);
        response
            .headers_mut()
            .insert(header::RETRY_AFTER, HeaderValue::from_static("2"));
        return response;
    };

    let cfg = state.cfg.clone();
    let bytes_in = body.len();
    let task = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        render_package(&cfg, &body, template)
    });
    // Respaldo del limite de Typst: el trabajo bloqueante igual termina (y
    // limpia su temporal) cuando el proceso de Typst se corta.
    let deadline = state.cfg.render_timeout + Duration::from_secs(15);
    let result = match tokio::time::timeout(deadline, task).await {
        Ok(Ok(result)) => result,
        Ok(Err(_)) => Err(internal()),
        Err(_) => Err(RenderFailure {
            status: StatusCode::GATEWAY_TIMEOUT,
            code: "render_timeout",
            detalle: None,
        }),
    };

    match result {
        Ok(rendered) => {
            log_event(
                "render",
                json!({
                    "resultado": "ok",
                    "plantilla": rendered.template,
                    "bytes_entrada": bytes_in,
                    "bytes_salida": rendered.pdf.len(),
                    "ms": started.elapsed().as_millis() as u64,
                }),
            );
            let mut response = Response::new(Body::from(rendered.pdf));
            let headers = response.headers_mut();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/pdf"),
            );
            headers.insert(
                header::CONTENT_DISPOSITION,
                header_value(&format!("attachment; filename=\"{}\"", rendered.filename)),
            );
            headers.insert(
                HeaderName::from_static(TEMPLATE_HEADER),
                header_value(&rendered.template),
            );
            response
        }
        Err(failure) => {
            // Solo el codigo: el detalle puede citar contenido del documento.
            log_event(
                "render",
                json!({
                    "resultado": failure.code,
                    "bytes_entrada": bytes_in,
                    "ms": started.elapsed().as_millis() as u64,
                }),
            );
            error(failure.status, failure.code, failure.detalle)
        }
    }
}
