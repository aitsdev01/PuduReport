// SPDX-License-Identifier: GPL-3.0-only
// Copyright (c) 2026 bc0d3

//! Sincronizacion de escritorio con PuduHub (opt-in).
//!
//! El ingeniero abre un informe de PuduHub como proyecto local, lo edita sin
//! conexion y envia la actualizacion. PuduHub fusiona en tres vias por entidad
//! y reporta conflictos. Todo pasa por comandos de Tauri: la webview nunca ve
//! el token personal de editor (`phed_...`), que vive solo en el keychain.
//!
//! - [`client`]: cliente HTTP (rutas `/api/editor/informes`, Bearer), con
//!   plazos, topes de tamano, sin redirecciones y errores mapeados por codigo.
//! - [`link`]: archivo `.puduhub.json` del proyecto y huella del contenido.
//! - [`sync`]: abrir, enviar, descargar la version del servidor y subir como
//!   informe nuevo.

pub mod client;
pub mod link;
pub mod sync;

#[cfg(test)]
mod tests;

use std::sync::OnceLock;

use regex::Regex;
use serde::Serialize;

/// Prefijo de la API de escritorio de PuduHub (relativo a la URL base).
pub const EDITOR_PREFIX: &str = "/api/editor/informes";

/// Tamano maximo de un paquete (subida y descarga), igual que PuduHub.
pub const MAX_PACKAGE_BYTES: u64 = 200 * 1024 * 1024;

/// Version maxima aceptada (entero positivo de 31 bits, como PuduHub).
pub const MAX_VERSION: u64 = 2_147_483_647;

/// Error de la sincronizacion para la interfaz.
///
/// `codigo` es estable (la interfaz decide que ofrecer segun el codigo) y
/// `mensaje` es un texto propio en espanol. Nunca incluye el cuerpo de una
/// respuesta del servidor ni el token: los detalles que se muestran
/// (bloqueo, conflictos) se validan campo por campo antes de copiarlos.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HubError {
    pub codigo: String,
    pub mensaje: String,
    #[serde(flatten)]
    pub extra: Box<HubErrorExtra>,
}

/// Detalles opcionales y ya validados de un [`HubError`].
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HubErrorExtra {
    /// Quien tiene el bloqueo y hasta cuando (`bloqueo_tomado`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bloqueo: Option<client::LockInfo>,
    /// Entidades en conflicto (`conflicto`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub conflictos: Vec<client::Conflict>,
    /// Titulos locales para nombrar los conflictos (`conflicto`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contexto: Option<link::ConflictContext>,
    /// Segundos sugeridos antes de reintentar (`limite_peticiones`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reintentar_en: Option<u32>,
    /// Proyecto local afectado (por ejemplo, con cambios sin enviar).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proyecto_id: Option<String>,
    /// Si el usuario puede tomar el bloqueo de todos modos (otra sesion suya).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub puede_forzar: bool,
}

impl HubError {
    pub fn new(codigo: &str, mensaje: impl Into<String>) -> Self {
        HubError {
            codigo: codigo.to_string(),
            mensaje: mensaje.into(),
            extra: Box::default(),
        }
    }

    /// Error de archivos o del workspace local (mensaje propio, sin red).
    pub fn local(detalle: impl std::fmt::Display) -> Self {
        HubError::new("local", format!("Error local: {detalle}"))
    }
}

impl std::fmt::Display for HubError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.mensaje)
    }
}

impl std::error::Error for HubError {}

fn regex(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("patron valido"))
}

/// UUID textual en minusculas o mayusculas (id de informe de PuduHub).
pub fn is_uuid(value: &str) -> bool {
    static CELL: OnceLock<Regex> = OnceLock::new();
    regex(
        &CELL,
        r"^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$",
    )
    .is_match(value)
}

/// Token personal de editor de PuduHub: `phed_` + 43 caracteres Base64 URL.
pub fn is_editor_token(value: &str) -> bool {
    static CELL: OnceLock<Regex> = OnceLock::new();
    regex(&CELL, r"^phed_[A-Za-z0-9_-]{43}$").is_match(value)
}

/// Token del bloqueo de edicion que entrega PuduHub (43 caracteres Base64 URL).
pub fn is_lock_token(value: &str) -> bool {
    static CELL: OnceLock<Regex> = OnceLock::new();
    regex(&CELL, r"^[A-Za-z0-9_-]{43}$").is_match(value)
}

/// Fecha y hora ISO 8601 con zona (como las serializa PuduHub).
pub fn is_timestamp(value: &str) -> bool {
    static CELL: OnceLock<Regex> = OnceLock::new();
    regex(
        &CELL,
        r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d{1,9})?(Z|[+-]\d{2}:\d{2})$",
    )
    .is_match(value)
}

/// Texto del servidor apto para mostrar: sin caracteres de control, sin
/// espacios al borde y acotado a `max` caracteres.
pub fn clean_text(value: &str, max: usize) -> String {
    let cleaned: String = value
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    cleaned.trim().chars().take(max).collect()
}

/// Normaliza y valida la URL base de PuduHub.
///
/// Exige `https://`. Solo para desarrollo se acepta `http://` hacia
/// `localhost`, `127.0.0.1` o `[::1]`. Sin usuario ni contrasena, consulta ni
/// fragmento. Devuelve la URL sin barra final (por ejemplo
/// `https://hub.example.cl` o `https://hub.example.cl/puduhub`).
pub fn normalize_server_url(raw: &str) -> Result<String, HubError> {
    let invalid = |detalle: &str| {
        HubError::new(
            "url_invalida",
            format!("La URL de PuduHub no es valida: {detalle}."),
        )
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.len() > 512 {
        return Err(invalid("ingrese una URL como https://hub.ejemplo.cl"));
    }
    let url = reqwest::Url::parse(trimmed)
        .map_err(|_| invalid("ingrese una URL como https://hub.ejemplo.cl"))?;
    let host = url
        .host_str()
        .ok_or_else(|| invalid("falta el servidor"))?
        .to_ascii_lowercase();
    match url.scheme() {
        "https" => {}
        "http" if is_loopback_host(&host) => {}
        "http" => {
            return Err(invalid(
                "debe usar https:// (http solo se permite para localhost en desarrollo)",
            ))
        }
        _ => return Err(invalid("debe usar https://")),
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(invalid("no incluya usuario ni contrasena en la URL"));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(invalid("no incluya parametros (?) ni fragmentos (#)"));
    }
    // host_str() ya trae los corchetes de una IPv6 ("[::1]").
    let mut normalized = format!("{}://{host}", url.scheme());
    if let Some(port) = url.port() {
        normalized.push_str(&format!(":{port}"));
    }
    let path = url.path().trim_end_matches('/');
    if path.contains("//") || path.contains("..") {
        return Err(invalid("la ruta no es valida"));
    }
    normalized.push_str(path);
    Ok(normalized)
}

/// Hosts de desarrollo (sin TLS permitido y sin proxy).
pub fn is_loopback_host(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "[::1]" | "::1")
}
