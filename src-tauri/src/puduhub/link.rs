// SPDX-License-Identifier: GPL-3.0-only
// Copyright (c) 2026 bc0d3

//! Vinculo de un proyecto local con un informe de PuduHub.
//!
//! `<proyecto>/.puduhub.json` guarda el servidor, el id del informe, la
//! version de base (la ultima recibida de PuduHub), la fecha de la ultima
//! sincronizacion y la huella del contenido en ese momento. Nunca guarda el
//! token. No entra en el paquete (la exportacion solo lee `project.yaml`,
//! `findings/` y `assets/`) y se agrega al `.gitignore` del workspace.
//!
//! La huella resume el contenido ya interpretado (proyecto, hallazgos y
//! SHA-256 de las evidencias), no los bytes de los YAML: asi un archivo
//! reescrito sin cambios no cuenta como cambio sin enviar.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::Path;

use pudureport_core::package::sha256_hex;
use pudureport_core::workspace;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{is_timestamp, is_uuid, normalize_server_url, HubError, MAX_VERSION};

/// Nombre del archivo de vinculo dentro de la carpeta del proyecto.
pub const LINK_FILE: &str = ".puduhub.json";

/// Version del formato del archivo de vinculo.
pub const LINK_FORMAT: u32 = 1;

/// Tamano maximo aceptado del archivo de vinculo.
const MAX_LINK_BYTES: u64 = 16 * 1024;

/// Contenido de `.puduhub.json`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Link {
    pub formato: u32,
    /// URL base normalizada de PuduHub.
    pub servidor: String,
    pub informe_id: String,
    /// Version de PuduHub sobre la que se edita (base de la fusion).
    pub base_version: u64,
    /// Ultima sincronizacion (ISO 8601, UTC).
    pub sincronizado_at: String,
    /// Huella del contenido al sincronizar ("" = desconocida: hay cambios).
    pub huella: String,
}

impl Link {
    fn validate(&self) -> Result<(), HubError> {
        let valid = self.formato == LINK_FORMAT
            && normalize_server_url(&self.servidor).ok().as_deref() == Some(self.servidor.as_str())
            && is_uuid(&self.informe_id)
            && (1..=MAX_VERSION).contains(&self.base_version)
            && is_timestamp(&self.sincronizado_at)
            && (self.huella.is_empty()
                || (self.huella.len() == 64
                    && self
                        .huella
                        .chars()
                        .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))));
        if valid {
            Ok(())
        } else {
            Err(invalid_link())
        }
    }
}

fn invalid_link() -> HubError {
    HubError::new(
        "vinculo_invalido",
        format!("El archivo {LINK_FILE} del proyecto no es valido. Vuelva a abrir el informe desde PuduHub."),
    )
}

fn checked_project(root: &Path, project_id: &str) -> Result<std::path::PathBuf, HubError> {
    // Mismo criterio que el workspace: un solo segmento, sin '.' inicial.
    workspace::read_project_meta(root, project_id).map_err(HubError::local)?;
    Ok(root.join(project_id))
}

/// Lee el vinculo del proyecto; None si no esta vinculado.
pub fn read_link(root: &Path, project_id: &str) -> Result<Option<Link>, HubError> {
    let path = checked_project(root, project_id)?.join(LINK_FILE);
    let meta = match fs::symlink_metadata(&path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(HubError::local(e)),
    };
    if !meta.is_file() || meta.len() > MAX_LINK_BYTES {
        return Err(invalid_link());
    }
    let bytes = fs::read(&path).map_err(HubError::local)?;
    let link: Link = serde_json::from_slice(&bytes).map_err(|_| invalid_link())?;
    link.validate()?;
    Ok(Some(link))
}

/// Escribe el vinculo (archivo temporal + rename) y lo agrega al `.gitignore`
/// del workspace si este usa git.
pub fn write_link(root: &Path, project_id: &str, link: &Link) -> Result<(), HubError> {
    link.validate()?;
    let dir = checked_project(root, project_id)?;
    let bytes = serde_json::to_vec_pretty(link).map_err(HubError::local)?;
    let tmp = dir.join(format!(".puduhub-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> std::io::Result<()> {
        let mut file = fs::File::create(&tmp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&tmp, dir.join(LINK_FILE))
    })();
    if let Err(e) = result {
        let _ = fs::remove_file(&tmp);
        return Err(HubError::local(e));
    }
    ensure_gitignored(root).map_err(HubError::local)
}

/// Borra el vinculo (por ejemplo, de una copia de respaldo).
pub fn remove_link(project_dir: &Path) {
    let _ = fs::remove_file(project_dir.join(LINK_FILE));
}

/// Agrega `.puduhub.json` al `.gitignore` del workspace si hay git o un
/// `.gitignore` (el vinculo es de este equipo, no del proyecto).
fn ensure_gitignored(root: &Path) -> std::io::Result<()> {
    let path = root.join(".gitignore");
    let has_git = root.join(".git").exists();
    let current = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && has_git => String::new(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    if current.lines().any(|line| line.trim() == LINK_FILE) {
        return Ok(());
    }
    let mut next = current;
    if !next.is_empty() && !next.ends_with('\n') {
        next.push('\n');
    }
    next.push_str("# Vinculo local con PuduHub (sin secretos)\n");
    next.push_str(LINK_FILE);
    next.push('\n');
    fs::write(path, next)
}

/// Contenido interpretado de un proyecto, para la huella y para comparar.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    meta: Value,
    findings: BTreeMap<String, Value>,
    titles: BTreeMap<String, String>,
    assets: BTreeMap<String, String>,
}

/// Lee el proyecto tal como lo exporta el paquete: `project.yaml`, los
/// hallazgos y las evidencias que son imagenes permitidas.
pub fn snapshot(root: &Path, project_id: &str) -> Result<Snapshot, HubError> {
    let meta = workspace::read_project_meta(root, project_id).map_err(HubError::local)?;
    let mut findings = BTreeMap::new();
    let mut titles = BTreeMap::new();
    for finding in workspace::list_findings(root, project_id).map_err(HubError::local)? {
        titles.insert(finding.id.clone(), finding.meta.title.clone());
        let value = serde_json::to_value(&finding).map_err(HubError::local)?;
        findings.insert(finding.id, value);
    }
    let mut assets = BTreeMap::new();
    let dir = root.join(project_id).join("assets");
    if dir.is_dir() {
        for entry in fs::read_dir(&dir).map_err(HubError::local)? {
            let entry = entry.map_err(HubError::local)?;
            if !entry.file_type().map_err(HubError::local)?.is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            let bytes = fs::read(entry.path()).map_err(HubError::local)?;
            if name.starts_with('.') || !workspace::is_raster_image(&bytes) {
                continue;
            }
            assets.insert(name, sha256_hex(&bytes));
        }
    }
    Ok(Snapshot {
        meta: serde_json::to_value(&meta).map_err(HubError::local)?,
        findings,
        titles,
        assets,
    })
}

impl Snapshot {
    /// SHA-256 del contenido interpretado (orden estable).
    pub fn huella(&self) -> String {
        let value = serde_json::json!({
            "proyecto": self.meta,
            "hallazgos": self.findings,
            "evidencias": self.assets,
        });
        sha256_hex(value.to_string().as_bytes())
    }

    /// Titulos locales para nombrar conflictos en la interfaz.
    pub fn conflict_context(&self) -> ConflictContext {
        let secciones = self
            .meta
            .get("sections")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|s| {
                        Some((
                            s.get("key")?.as_str()?.to_string(),
                            s.get("title")?.as_str()?.to_string(),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        ConflictContext {
            hallazgos: self.titles.clone(),
            secciones,
        }
    }
}

/// Titulos locales de hallazgos (por id) y secciones (por clave).
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct ConflictContext {
    pub hallazgos: BTreeMap<String, String>,
    pub secciones: BTreeMap<String, String>,
}

/// Que cambio en el proyecto local al aplicar la version de PuduHub.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Changes {
    pub hallazgos_nuevos: Vec<String>,
    pub hallazgos_actualizados: Vec<String>,
    pub hallazgos_eliminados: Vec<String>,
    /// Titulos de secciones agregadas, cambiadas o quitadas.
    pub secciones: Vec<String>,
    /// Campos del proyecto (claves de `project.yaml`) que cambiaron.
    pub campos: Vec<String>,
    pub evidencias: usize,
    pub orden: bool,
}

fn sections_by_key(meta: &Value) -> BTreeMap<String, (String, Value)> {
    meta.get("sections")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|s| {
                    let key = s.get("key")?.as_str()?.to_string();
                    let title = s.get("title").and_then(Value::as_str).unwrap_or("");
                    Some((key, (title.to_string(), s.clone())))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Diferencias de `before` a `after`, por entidad.
pub fn diff(before: &Snapshot, after: &Snapshot) -> Changes {
    let mut changes = Changes::default();
    for (id, value) in &after.findings {
        let title = after.titles.get(id).cloned().unwrap_or_else(|| id.clone());
        match before.findings.get(id) {
            None => changes.hallazgos_nuevos.push(title),
            Some(previous) if previous != value => changes.hallazgos_actualizados.push(title),
            Some(_) => {}
        }
    }
    for id in before.findings.keys() {
        if !after.findings.contains_key(id) {
            let title = before.titles.get(id).cloned().unwrap_or_else(|| id.clone());
            changes.hallazgos_eliminados.push(title);
        }
    }
    let (old_sections, new_sections) =
        (sections_by_key(&before.meta), sections_by_key(&after.meta));
    for (key, (title, value)) in &new_sections {
        if old_sections.get(key).map(|(_, v)| v) != Some(value) {
            changes.secciones.push(title.clone());
        }
    }
    for (key, (title, _)) in &old_sections {
        if !new_sections.contains_key(key) {
            changes.secciones.push(title.clone());
        }
    }
    let empty = serde_json::Map::new();
    let old_meta = before.meta.as_object().unwrap_or(&empty);
    let new_meta = after.meta.as_object().unwrap_or(&empty);
    let mut keys: Vec<&String> = old_meta.keys().chain(new_meta.keys()).collect();
    keys.sort();
    keys.dedup();
    for key in keys {
        if key == "sections" || key == "finding_order" {
            continue;
        }
        if old_meta.get(key) != new_meta.get(key) {
            changes.campos.push(key.clone());
        }
    }
    changes.orden = old_meta.get("finding_order") != new_meta.get("finding_order")
        && changes.hallazgos_nuevos.is_empty()
        && changes.hallazgos_eliminados.is_empty();
    changes.evidencias = after
        .assets
        .iter()
        .filter(|(name, sha)| before.assets.get(*name) != Some(*sha))
        .count()
        + before
            .assets
            .keys()
            .filter(|name| !after.assets.contains_key(*name))
            .count();
    changes
}
