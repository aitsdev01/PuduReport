// SPDX-License-Identifier: GPL-3.0-only
// Copyright (c) 2026 bc0d3

//! Paquete `.pudupkg`: un proyecto completo en un ZIP portable.
//!
//! Sirve para mover un proyecto entre equipos y para que un servicio (por
//! ejemplo `pudureport-service`) genere el PDF con el mismo motor. El formato
//! no inventa nada: son los mismos archivos del proyecto en disco.
//!
//! ```text
//! manifest.json          formato, metadatos y SHA-256 de cada archivo
//! project.yaml           identico al del proyecto
//! findings/<id>.md       front-matter + cuerpo, identicos a los del proyecto
//! assets/<archivo>       evidencias (solo imagenes PNG, JPEG, GIF o WebP)
//! workspace.yaml         opcional: solo nombre, marca y marca de agua
//! branding/<archivo>     opcional: logo y fondo de portada referenciados
//! ```
//!
//! La importacion desconfia del contenido: valida cada ruta (sin `..`, sin
//! rutas absolutas, sin enlaces simbolicos), acota tamanos y cantidad de
//! archivos (anti zip-bomb), exige extensiones y firmas de imagen permitidas,
//! verifica el SHA-256 de cada archivo contra el manifiesto y parsea el YAML
//! con el mismo parser que la app antes de escribir nada en disco.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::{Cursor, Read, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, DateTime, ZipArchive, ZipWriter};

use crate::models::{ProjectMeta, WorkspaceMeta};
use crate::workspace;

/// Version del formato de paquete que produce y acepta esta version.
pub const FORMAT_VERSION: u32 = 1;

/// Extension de archivo recomendada para los paquetes.
pub const EXTENSION: &str = "pudupkg";

/// Extensiones de imagen permitidas para evidencias y marca.
pub const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp"];

const MANIFEST: &str = "manifest.json";
const PROJECT: &str = "project.yaml";
const WORKSPACE: &str = "workspace.yaml";

#[derive(Debug, thiserror::Error)]
pub enum PackageError {
    #[error("paquete invalido: {0}")]
    Invalid(String),
    #[error("el paquete supera el limite permitido: {0}")]
    TooLarge(String),
    #[error("error de workspace: {0}")]
    Workspace(#[from] workspace::WorkspaceError),
    #[error("error de entrada/salida: {0}")]
    Io(#[from] std::io::Error),
    #[error("error al escribir el paquete: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("error de serializacion: {0}")]
    Json(#[from] serde_json::Error),
    #[error("error de YAML: {0}")]
    Yaml(#[from] serde_yaml::Error),
}

type Result<T> = std::result::Result<T, PackageError>;

fn invalid(msg: impl Into<String>) -> PackageError {
    PackageError::Invalid(msg.into())
}

/// Limites de importacion. Los valores por defecto siguen el contrato con
/// PuduHub (proyecto 1 MiB, hallazgo 512 KiB, asset 10 MiB, paquete 200 MiB,
/// hasta 1000 hallazgos).
#[derive(Debug, Clone)]
pub struct PackageLimits {
    /// Tamano maximo del ZIP recibido (comprimido).
    pub max_package_bytes: u64,
    /// Suma maxima de los tamanos descomprimidos.
    pub max_total_uncompressed: u64,
    /// Cantidad maxima de entradas del ZIP (archivos y carpetas).
    pub max_entries: usize,
    /// Cantidad maxima de hallazgos.
    pub max_findings: usize,
    pub max_manifest_bytes: u64,
    pub max_project_bytes: u64,
    pub max_workspace_bytes: u64,
    pub max_finding_bytes: u64,
    pub max_asset_bytes: u64,
}

impl Default for PackageLimits {
    fn default() -> Self {
        const MIB: u64 = 1024 * 1024;
        PackageLimits {
            max_package_bytes: 200 * MIB,
            max_total_uncompressed: 200 * MIB,
            max_entries: 4096,
            max_findings: 1000,
            max_manifest_bytes: MIB,
            max_project_bytes: MIB,
            max_workspace_bytes: MIB,
            max_finding_bytes: 512 * 1024,
            max_asset_bytes: 10 * MIB,
        }
    }
}

/// Un archivo listado en el manifiesto.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManifestFile {
    /// Ruta relativa dentro del paquete (separador `/`).
    pub ruta: String,
    /// SHA-256 del contenido, en hexadecimal minuscula.
    pub sha256: String,
    /// Tamano en bytes.
    pub bytes: u64,
}

/// `manifest.json` del paquete.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    /// Version del formato (hoy siempre 1).
    pub formato: u32,
    /// Id del informe en PuduHub, si el paquete viene de alli o va hacia alla.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub informe_id: Option<String>,
    /// Version del informe que representa el paquete.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<u64>,
    /// Version sobre la que se edito (para la fusion de tres vias).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_version: Option<u64>,
    /// Herramienta y version que genero el paquete.
    #[serde(default)]
    pub generado_por: String,
    /// Id (carpeta) del proyecto de origen. Vacio = se deriva del nombre.
    #[serde(default)]
    pub proyecto: String,
    /// Archivos del paquete con su hash (todos menos el propio manifiesto).
    #[serde(default)]
    pub archivos: Vec<ManifestFile>,
}

/// Metadatos opcionales que se escriben en el manifiesto al exportar.
#[derive(Debug, Clone, Default)]
pub struct PackageInfo {
    pub informe_id: Option<String>,
    pub version: Option<u64>,
    pub base_version: Option<u64>,
    /// Vacio = `pudureport <version del crate>`.
    pub generado_por: String,
}

/// Resultado de exportar un proyecto.
#[derive(Debug)]
pub struct ExportedPackage {
    /// Bytes del ZIP.
    pub bytes: Vec<u8>,
    pub manifest: Manifest,
    /// Archivos de `assets/` que no se incluyeron por no ser imagenes
    /// permitidas (rutas relativas al proyecto).
    pub skipped: Vec<String>,
}

/// Opciones de importacion.
#[derive(Debug, Clone, Default)]
pub struct ImportOptions {
    /// Escribe tambien `workspace.yaml` y `branding/` del paquete en la raiz.
    /// Solo para un workspace nuevo y efimero (por ejemplo, un render en el
    /// servicio): en un workspace existente la marca no se pisa.
    pub apply_workspace: bool,
    /// Id de carpeta deseado. Vacio = el del manifiesto o el nombre del
    /// proyecto. Si ya existe, se agrega un sufijo numerico.
    pub project_id: Option<String>,
    pub limits: PackageLimits,
}

/// Resultado de importar un paquete.
#[derive(Debug)]
pub struct ImportedPackage {
    /// Carpeta del proyecto creada en el workspace.
    pub project_id: String,
    pub manifest: Manifest,
    pub meta: ProjectMeta,
    pub findings: usize,
    pub assets: usize,
}

// ---------------------------------------------------------------------------
// Rutas
// ---------------------------------------------------------------------------

/// Tipo de cada entrada aceptada del paquete.
#[derive(Debug, Clone, PartialEq, Eq)]
enum EntryKind {
    Manifest,
    Project,
    Workspace,
    Finding(String),
    Asset,
    Branding,
}

/// Un segmento de ruta seguro: ASCII alfanumerico, `-`, `_` o `.`, sin
/// empezar con punto y con largo acotado.
fn safe_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment.len() <= 128
        && !segment.starts_with('.')
        && segment
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

fn image_extension(name: &str) -> bool {
    name.rsplit_once('.').is_some_and(|(stem, ext)| {
        !stem.is_empty() && IMAGE_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str())
    })
}

/// Valida y clasifica la ruta de una entrada. Rechaza todo lo que no sea una
/// ruta relativa normalizada dentro de las carpetas conocidas.
fn classify(name: &str) -> Result<EntryKind> {
    let shown: String = name.chars().take(80).collect();
    if name.is_empty() || name.len() > 255 {
        return Err(invalid(format!("ruta invalida: {shown}")));
    }
    if name.contains('\\') || name.contains('\0') || name.contains(':') {
        return Err(invalid(format!(
            "ruta con caracteres no permitidos: {shown}"
        )));
    }
    if name.starts_with('/') {
        return Err(invalid(format!("ruta absoluta no permitida: {shown}")));
    }
    let segments: Vec<&str> = name.split('/').collect();
    if segments.iter().any(|s| *s == ".." || *s == ".") {
        return Err(invalid(format!("ruta con '..' o '.': {shown}")));
    }
    if !segments.iter().all(|s| safe_segment(s)) {
        return Err(invalid(format!("ruta no permitida: {shown}")));
    }
    match segments.as_slice() {
        [MANIFEST] => Ok(EntryKind::Manifest),
        [PROJECT] => Ok(EntryKind::Project),
        [WORKSPACE] => Ok(EntryKind::Workspace),
        ["findings", file] => match file.strip_suffix(".md") {
            Some(stem) if safe_segment(stem) => Ok(EntryKind::Finding(stem.to_string())),
            _ => Err(invalid(format!("hallazgo con nombre invalido: {shown}"))),
        },
        ["assets", file] if image_extension(file) => Ok(EntryKind::Asset),
        ["branding", file] if image_extension(file) => Ok(EntryKind::Branding),
        ["assets", _] | ["branding", _] => {
            Err(invalid(format!("tipo de archivo no permitido: {shown}")))
        }
        _ => Err(invalid(format!(
            "archivo no esperado en el paquete: {shown}"
        ))),
    }
}

/// Carpetas que pueden venir como entradas de directorio (se ignoran).
fn allowed_dir(name: &str) -> bool {
    matches!(
        name.trim_end_matches('/'),
        "findings" | "assets" | "branding"
    )
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for b in digest {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

// ---------------------------------------------------------------------------
// Exportacion
// ---------------------------------------------------------------------------

/// Copia de `workspace.yaml` con solo la identidad visual: sin destinos de
/// exportacion ni orden del tablero (no son del proyecto).
fn portable_workspace(ws: &WorkspaceMeta) -> WorkspaceMeta {
    WorkspaceMeta {
        name: ws.name.clone(),
        branding: ws.branding.clone(),
        watermark: ws.watermark.clone(),
        project_order: Vec::new(),
        export_targets: Vec::new(),
    }
}

/// Nombre de archivo de una ruta de marca `/branding/<archivo>`, si es valida.
fn branding_file(path: &str) -> Option<&str> {
    let file = path.strip_prefix("/branding/")?;
    (safe_segment(file) && image_extension(file)).then_some(file)
}

/// Rutas de marca referenciadas por `workspace.yaml`, para limpiar las que no
/// existen o no son validas al exportar y verificarlas al importar.
fn branding_refs(ws: &mut WorkspaceMeta) -> Vec<&mut String> {
    let mut refs = vec![
        &mut ws.branding.logo_path,
        &mut ws.branding.cover_background,
    ];
    for el in ws.branding.cover_elements.iter_mut() {
        refs.push(&mut el.src);
    }
    refs
}

/// Exporta un proyecto del workspace a un paquete `.pudupkg` en memoria.
///
/// `include_workspace` agrega la marca del workspace (logo, colores, marca de
/// agua) para que otro equipo o el servicio rendericen igual.
pub fn export_package(
    root: &Path,
    project_id: &str,
    info: &PackageInfo,
    include_workspace: bool,
) -> Result<ExportedPackage> {
    // Valida el id y que el project.yaml se pueda leer.
    workspace::read_project_meta(root, project_id)?;
    let project_dir = root.join(project_id);

    // (ruta en el paquete, bytes). BTreeMap: orden estable y determinista.
    let mut files: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let mut skipped = Vec::new();

    files.insert(
        PROJECT.to_string(),
        fs::read(project_dir.join("project.yaml"))?,
    );

    let findings_dir = project_dir.join("findings");
    if findings_dir.is_dir() {
        for entry in fs::read_dir(&findings_dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();
            if !entry.file_type()?.is_file() || !name.ends_with(".md") || name.starts_with('.') {
                continue;
            }
            let path = format!("findings/{name}");
            classify(&path)?;
            let bytes = fs::read(entry.path())?;
            // Que el hallazgo se pueda parsear antes de empaquetarlo.
            let content = String::from_utf8(bytes.clone())
                .map_err(|_| invalid(format!("{path} no es UTF-8")))?;
            workspace::parse_finding_document(name.trim_end_matches(".md"), &content)?;
            files.insert(path, bytes);
        }
    }

    let assets_dir = project_dir.join("assets");
    if assets_dir.is_dir() {
        for entry in fs::read_dir(&assets_dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            let path = format!("assets/{name}");
            let bytes = fs::read(entry.path())?;
            if !matches!(classify(&path), Ok(EntryKind::Asset))
                || !workspace::is_raster_image(&bytes)
            {
                skipped.push(path);
                continue;
            }
            files.insert(path, bytes);
        }
    }

    if include_workspace {
        let mut ws = portable_workspace(&workspace::read_workspace_meta(root)?);
        let mut branding: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        for reference in branding_refs(&mut ws) {
            if reference.is_empty() {
                continue;
            }
            let bytes = branding_file(reference)
                .and_then(|file| {
                    fs::read(root.join("branding").join(file))
                        .ok()
                        .map(|b| (file, b))
                })
                .filter(|(_, b)| workspace::is_raster_image(b));
            match bytes {
                Some((file, b)) => {
                    branding.insert(format!("branding/{file}"), b);
                }
                // Referencia rota o fuera de branding/: no se exporta.
                None => reference.clear(),
            }
        }
        files.insert(
            WORKSPACE.to_string(),
            serde_yaml::to_string(&ws)?.into_bytes(),
        );
        files.extend(branding);
    }

    let manifest = Manifest {
        formato: FORMAT_VERSION,
        informe_id: info.informe_id.clone(),
        version: info.version,
        base_version: info.base_version,
        generado_por: if info.generado_por.is_empty() {
            format!("pudureport {}", env!("CARGO_PKG_VERSION"))
        } else {
            info.generado_por.clone()
        },
        proyecto: project_id.to_string(),
        archivos: files
            .iter()
            .map(|(ruta, bytes)| ManifestFile {
                ruta: ruta.clone(),
                sha256: sha256_hex(bytes),
                bytes: bytes.len() as u64,
            })
            .collect(),
    };

    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    let base = SimpleFileOptions::default()
        .last_modified_time(DateTime::default())
        .unix_permissions(0o644);
    let text = base.compression_method(CompressionMethod::Deflated);
    // Las imagenes ya vienen comprimidas: se guardan tal cual.
    let stored = base.compression_method(CompressionMethod::Stored);

    zip.start_file(MANIFEST, text)?;
    zip.write_all(&serde_json::to_vec_pretty(&manifest)?)?;
    for (path, bytes) in &files {
        let options = if path.starts_with("assets/") || path.starts_with("branding/") {
            stored
        } else {
            text
        };
        zip.start_file(path.as_str(), options)?;
        zip.write_all(bytes)?;
    }
    let bytes = zip.finish()?.into_inner();
    Ok(ExportedPackage {
        bytes,
        manifest,
        skipped,
    })
}

// ---------------------------------------------------------------------------
// Importacion
// ---------------------------------------------------------------------------

/// Contenido ya validado de un paquete, listo para escribir.
#[derive(Debug)]
pub struct ValidatedPackage {
    pub manifest: Manifest,
    pub meta: ProjectMeta,
    project_yaml: Vec<u8>,
    workspace: Option<WorkspaceMeta>,
    findings: Vec<(String, Vec<u8>)>,
    assets: Vec<(String, Vec<u8>)>,
    branding: Vec<(String, Vec<u8>)>,
}

impl ValidatedPackage {
    /// Cantidad de hallazgos del paquete.
    pub fn finding_count(&self) -> usize {
        self.findings.len()
    }
}

/// Lee y valida un paquete completo en memoria, sin tocar el disco.
pub fn validate_package(bytes: &[u8], limits: &PackageLimits) -> Result<ValidatedPackage> {
    if bytes.len() as u64 > limits.max_package_bytes {
        return Err(PackageError::TooLarge(format!(
            "el paquete pesa {} bytes (maximo {})",
            bytes.len(),
            limits.max_package_bytes
        )));
    }
    let mut archive =
        ZipArchive::new(Cursor::new(bytes)).map_err(|e| invalid(format!("ZIP ilegible: {e}")))?;
    if archive.len() > limits.max_entries {
        return Err(PackageError::TooLarge(format!(
            "el paquete tiene {} entradas (maximo {})",
            archive.len(),
            limits.max_entries
        )));
    }

    let mut seen: HashSet<String> = HashSet::new();
    let mut contents: BTreeMap<String, (EntryKind, Vec<u8>)> = BTreeMap::new();
    let mut total: u64 = 0;

    for index in 0..archive.len() {
        let mut file = archive
            .by_index(index)
            .map_err(|e| invalid(format!("entrada ilegible: {e}")))?;
        let name = file
            .name()
            .map_err(|e| invalid(format!("nombre de entrada ilegible: {e}")))?
            .to_string();
        // Duplicados (tambien solo por mayusculas): en sistemas de archivos
        // que no distinguen mayusculas, uno pisaria al otro.
        if !seen.insert(name.to_ascii_lowercase()) {
            return Err(invalid(format!("entrada duplicada: {name}")));
        }
        if file.encrypted() {
            return Err(invalid(format!("entrada cifrada no permitida: {name}")));
        }
        // Enlaces simbolicos y cualquier tipo especial (solo archivos y
        // carpetas regulares).
        let file_type = file.unix_mode().map(|m| m & 0o170_000);
        if file.is_symlink()
            || matches!(file_type, Some(t) if t != 0 && t != 0o100_000 && t != 0o040_000)
        {
            return Err(invalid(format!("enlace simbolico no permitido: {name}")));
        }
        if file.is_dir() {
            if allowed_dir(&name) {
                continue;
            }
            return Err(invalid(format!(
                "carpeta no esperada en el paquete: {name}"
            )));
        }

        let kind = classify(&name)?;
        let cap = match kind {
            EntryKind::Manifest => limits.max_manifest_bytes,
            EntryKind::Project => limits.max_project_bytes,
            EntryKind::Workspace => limits.max_workspace_bytes,
            EntryKind::Finding(_) => limits.max_finding_bytes,
            EntryKind::Asset | EntryKind::Branding => limits.max_asset_bytes,
        };
        // El tamano declarado puede mentir: se corta la lectura en el limite.
        if file.size() > cap {
            return Err(PackageError::TooLarge(format!(
                "{name} declara {} bytes (maximo {cap})",
                file.size()
            )));
        }
        let mut data = Vec::new();
        file.by_ref()
            .take(cap + 1)
            .read_to_end(&mut data)
            .map_err(|e| invalid(format!("no se pudo descomprimir {name}: {e}")))?;
        if data.len() as u64 > cap {
            return Err(PackageError::TooLarge(format!(
                "{name} supera {cap} bytes descomprimido"
            )));
        }
        total += data.len() as u64;
        if total > limits.max_total_uncompressed {
            return Err(PackageError::TooLarge(format!(
                "el contenido descomprimido supera {} bytes",
                limits.max_total_uncompressed
            )));
        }
        contents.insert(name, (kind, data));
    }

    let (_, manifest_bytes) = contents
        .remove(MANIFEST)
        .ok_or_else(|| invalid("falta manifest.json"))?;
    let manifest: Manifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|e| invalid(format!("manifest.json invalido: {e}")))?;
    if manifest.formato != FORMAT_VERSION {
        return Err(invalid(format!(
            "formato de paquete no soportado: {} (se espera {FORMAT_VERSION})",
            manifest.formato
        )));
    }
    if !manifest.proyecto.is_empty() && !safe_segment(&manifest.proyecto) {
        return Err(invalid("id de proyecto invalido en el manifiesto"));
    }

    // El manifiesto debe listar exactamente los archivos presentes, cada uno
    // con su hash y tamano.
    let mut listed: HashSet<&str> = HashSet::new();
    for item in &manifest.archivos {
        if !listed.insert(item.ruta.as_str()) {
            return Err(invalid(format!(
                "archivo repetido en el manifiesto: {}",
                item.ruta
            )));
        }
        let (_, data) = contents
            .get(&item.ruta)
            .ok_or_else(|| invalid(format!("falta un archivo del manifiesto: {}", item.ruta)))?;
        if data.len() as u64 != item.bytes || sha256_hex(data) != item.sha256.to_ascii_lowercase() {
            return Err(invalid(format!("hash no coincide: {}", item.ruta)));
        }
    }
    if let Some(extra) = contents.keys().find(|k| !listed.contains(k.as_str())) {
        return Err(invalid(format!("archivo fuera del manifiesto: {extra}")));
    }

    let (_, project_yaml) = contents
        .remove(PROJECT)
        .ok_or_else(|| invalid("falta project.yaml"))?;
    let project_text =
        std::str::from_utf8(&project_yaml).map_err(|_| invalid("project.yaml no es UTF-8"))?;
    let mut meta: ProjectMeta = serde_yaml::from_str(project_text)
        .map_err(|e| invalid(format!("project.yaml invalido: {e}")))?;
    meta.reconcile_layout();
    meta.normalize_classification();

    let mut workspace_meta = None;
    let mut findings = Vec::new();
    let mut assets = Vec::new();
    let mut branding = Vec::new();
    for (name, (kind, data)) in contents {
        match kind {
            EntryKind::Finding(id) => {
                let text = std::str::from_utf8(&data)
                    .map_err(|_| invalid(format!("{name} no es UTF-8")))?;
                workspace::parse_finding_document(&id, text)
                    .map_err(|e| invalid(format!("{name}: {e}")))?;
                findings.push((id, data));
            }
            EntryKind::Asset | EntryKind::Branding => {
                // La extension no basta: se exige la firma real de la imagen.
                if !workspace::is_raster_image(&data) {
                    return Err(invalid(format!("{name} no es una imagen valida")));
                }
                let file = name.rsplit('/').next().unwrap_or_default().to_string();
                if kind == EntryKind::Asset {
                    assets.push((file, data));
                } else {
                    branding.push((file, data));
                }
            }
            EntryKind::Workspace => {
                let text = std::str::from_utf8(&data)
                    .map_err(|_| invalid("workspace.yaml no es UTF-8"))?;
                let ws: WorkspaceMeta = serde_yaml::from_str(text)
                    .map_err(|e| invalid(format!("workspace.yaml invalido: {e}")))?;
                workspace_meta = Some(portable_workspace(&ws));
            }
            EntryKind::Manifest | EntryKind::Project => {}
        }
    }
    if findings.len() > limits.max_findings {
        return Err(PackageError::TooLarge(format!(
            "el paquete trae {} hallazgos (maximo {})",
            findings.len(),
            limits.max_findings
        )));
    }

    // La marca solo puede apuntar a archivos de branding/ incluidos.
    if let Some(ws) = workspace_meta.as_mut() {
        let present: HashSet<&str> = branding.iter().map(|(f, _)| f.as_str()).collect();
        for reference in branding_refs(ws) {
            if reference.is_empty() {
                continue;
            }
            match branding_file(reference) {
                Some(file) if present.contains(file) => {}
                _ => {
                    return Err(invalid(format!(
                        "workspace.yaml referencia un archivo de marca invalido: {}",
                        reference.chars().take(80).collect::<String>()
                    )))
                }
            }
        }
    }

    Ok(ValidatedPackage {
        manifest,
        meta,
        project_yaml,
        workspace: workspace_meta,
        findings,
        assets,
        branding,
    })
}

/// Importa un paquete al workspace como un proyecto nuevo (nunca pisa uno
/// existente). Valida todo antes de escribir y escribe en una carpeta
/// temporal que se renombra al final, para no dejar proyectos a medias.
pub fn import_package(
    root: &Path,
    bytes: &[u8],
    options: &ImportOptions,
) -> Result<ImportedPackage> {
    let package = validate_package(bytes, &options.limits)?;
    write_package(root, package, options)
}

/// Escribe un paquete ya validado en el workspace.
pub fn write_package(
    root: &Path,
    package: ValidatedPackage,
    options: &ImportOptions,
) -> Result<ImportedPackage> {
    let requested = options
        .project_id
        .clone()
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| {
            if package.manifest.proyecto.is_empty() {
                package.meta.name.clone()
            } else {
                package.manifest.proyecto.clone()
            }
        });
    let project_id = workspace::unique_project_id(root, &requested);

    fs::create_dir_all(root)?;
    let staging = root.join(format!(".pudupkg-{}", uuid::Uuid::new_v4()));
    let result = (|| -> Result<()> {
        fs::create_dir_all(staging.join("findings"))?;
        fs::create_dir_all(staging.join("assets"))?;
        fs::write(staging.join(PROJECT), &package.project_yaml)?;
        for (id, data) in &package.findings {
            fs::write(staging.join("findings").join(format!("{id}.md")), data)?;
        }
        for (file, data) in &package.assets {
            fs::write(staging.join("assets").join(file), data)?;
        }
        fs::rename(&staging, root.join(&project_id))?;
        Ok(())
    })();
    if let Err(e) = result {
        let _ = fs::remove_dir_all(&staging);
        return Err(e);
    }

    if options.apply_workspace {
        if let Some(ws) = &package.workspace {
            if !package.branding.is_empty() {
                let dir = root.join("branding");
                fs::create_dir_all(&dir)?;
                for (file, data) in &package.branding {
                    fs::write(dir.join(file), data)?;
                }
            }
            workspace::write_workspace_meta(root, ws)?;
        }
    }

    Ok(ImportedPackage {
        project_id,
        findings: package.findings.len(),
        assets: package.assets.len(),
        manifest: package.manifest,
        meta: package.meta,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 0];

    fn tmp(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("pudu-pkg-{label}-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn example_package() -> (std::path::PathBuf, String, ExportedPackage) {
        let root = tmp("src");
        workspace::create_workspace(&root, "WS").unwrap();
        let (pid, _) = workspace::create_example_project(&root).unwrap();
        let rel = workspace::save_asset(&root, &pid, "png", PNG).unwrap();
        // Un adjunto no permitido se omite al exportar.
        workspace::save_asset(&root, &pid, "html", b"<script>").unwrap();
        let logo = workspace::save_branding_asset(&root, "png", PNG).unwrap();
        let mut ws = workspace::read_workspace_meta(&root).unwrap();
        ws.branding.logo_path = logo;
        ws.branding.cover_background = "/branding/no-existe.png".to_string();
        workspace::write_workspace_meta(&root, &ws).unwrap();
        let mut f = workspace::list_findings(&root, &pid).unwrap().remove(0);
        f.body.push_str(&format!("\n\n![60%]({rel})\n"));
        workspace::write_finding(&root, &pid, &f).unwrap();
        let pkg = export_package(&root, &pid, &PackageInfo::default(), true).unwrap();
        (root, pid, pkg)
    }

    /// Arma un ZIP a mano: (ruta, bytes) con manifiesto correcto opcional.
    fn build_zip(entries: &[(&str, &[u8])], with_manifest: bool) -> Vec<u8> {
        let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
        let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        if with_manifest {
            let manifest = Manifest {
                formato: 1,
                informe_id: None,
                version: None,
                base_version: None,
                generado_por: "test".to_string(),
                proyecto: "p".to_string(),
                archivos: entries
                    .iter()
                    .map(|(ruta, bytes)| ManifestFile {
                        ruta: ruta.to_string(),
                        sha256: sha256_hex(bytes),
                        bytes: bytes.len() as u64,
                    })
                    .collect(),
            };
            zip.start_file(MANIFEST, opts).unwrap();
            zip.write_all(&serde_json::to_vec(&manifest).unwrap())
                .unwrap();
        }
        for (name, bytes) in entries {
            zip.start_file(*name, opts).unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    fn project_yaml() -> Vec<u8> {
        serde_yaml::to_string(&ProjectMeta {
            name: "Demo".to_string(),
            ..Default::default()
        })
        .unwrap()
        .into_bytes()
    }

    fn err_of(bytes: &[u8]) -> String {
        validate_package(bytes, &PackageLimits::default())
            .unwrap_err()
            .to_string()
    }

    #[test]
    fn roundtrip_preserves_project_findings_and_assets() {
        let (root, pid, pkg) = example_package();
        assert_eq!(pkg.manifest.formato, 1);
        assert_eq!(pkg.skipped.len(), 1, "el .html no debe exportarse");
        assert!(pkg
            .manifest
            .archivos
            .iter()
            .any(|f| f.ruta == "workspace.yaml"));

        let dest = tmp("dst");
        let imported = import_package(
            &dest,
            &pkg.bytes,
            &ImportOptions {
                apply_workspace: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(imported.project_id, pid);
        assert_eq!(imported.findings, 3);
        assert_eq!(imported.assets, 1);

        let original = workspace::list_findings(&root, &pid).unwrap();
        let copy = workspace::list_findings(&dest, &imported.project_id).unwrap();
        assert_eq!(original.len(), copy.len());
        for (a, b) in original.iter().zip(copy.iter()) {
            assert_eq!(a.id, b.id);
            assert_eq!(a.body, b.body);
            assert_eq!(a.meta.title, b.meta.title);
        }
        let ws = workspace::read_workspace_meta(&dest).unwrap();
        assert!(ws.branding.logo_path.starts_with("/branding/"));
        assert!(dest
            .join(ws.branding.logo_path.trim_start_matches('/'))
            .is_file());
        // La referencia rota se limpio al exportar.
        assert!(ws.branding.cover_background.is_empty());

        // Importar de nuevo no pisa: crea otra carpeta.
        let again = import_package(&dest, &pkg.bytes, &ImportOptions::default()).unwrap();
        assert_ne!(again.project_id, imported.project_id);

        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&dest);
    }

    #[test]
    fn export_is_deterministic() {
        let (root, pid, pkg) = example_package();
        let again = export_package(&root, &pid, &PackageInfo::default(), true).unwrap();
        assert_eq!(pkg.bytes, again.bytes);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn rejects_zip_slip() {
        let yaml = project_yaml();
        for evil in ["../evil.md", "findings/../../evil.md", "assets/../../x.png"] {
            let bytes = build_zip(&[("project.yaml", &yaml), (evil, b"x")], true);
            assert!(
                err_of(&bytes).contains("ruta"),
                "{evil}: {}",
                err_of(&bytes)
            );
        }
    }

    #[test]
    fn rejects_absolute_and_windows_paths() {
        let yaml = project_yaml();
        for evil in [
            "/etc/passwd",
            "C:/Windows/evil.png",
            "assets\\..\\evil.png",
            "findings/.oculto.md",
        ] {
            let bytes = build_zip(&[("project.yaml", &yaml), (evil, PNG)], true);
            assert!(err_of(&bytes).starts_with("paquete invalido"), "{evil}");
        }
    }

    #[test]
    fn rejects_symlinks() {
        let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
        let opts = SimpleFileOptions::default();
        zip.start_file(MANIFEST, opts).unwrap();
        zip.write_all(br#"{"formato":1,"archivos":[]}"#).unwrap();
        zip.add_symlink("assets/link.png", "/etc/passwd", opts)
            .unwrap();
        let bytes = zip.finish().unwrap().into_inner();
        assert!(err_of(&bytes).contains("enlace simbolico"));
    }

    #[test]
    fn rejects_zip_bomb_by_uncompressed_size() {
        let yaml = project_yaml();
        // 1 MiB de ceros comprime a casi nada; el limite se aplica a lo
        // descomprimido.
        let zeros = vec![0u8; 1024 * 1024];
        let mut finding = b"---\ntitle: x\n---\n".to_vec();
        finding.extend_from_slice(&zeros);
        let bytes = build_zip(
            &[("project.yaml", &yaml), ("findings/001-x.md", &finding)],
            true,
        );
        assert!(bytes.len() < 64 * 1024);
        let err = validate_package(&bytes, &PackageLimits::default()).unwrap_err();
        assert!(matches!(err, PackageError::TooLarge(_)), "{err}");

        // Limite total, aunque cada archivo cumpla el suyo.
        let small = b"---\ntitle: x\n---\nbody".to_vec();
        let bytes = build_zip(
            &[
                ("project.yaml", &yaml),
                ("findings/001-a.md", &small),
                ("findings/002-b.md", &small),
            ],
            true,
        );
        let limits = PackageLimits {
            max_total_uncompressed: 50,
            ..Default::default()
        };
        assert!(matches!(
            validate_package(&bytes, &limits),
            Err(PackageError::TooLarge(_))
        ));
    }

    #[test]
    fn rejects_too_many_entries_and_findings() {
        let yaml = project_yaml();
        let f = b"---\ntitle: x\n---\nbody".to_vec();
        let bytes = build_zip(
            &[
                ("project.yaml", &yaml),
                ("findings/001-a.md", &f),
                ("findings/002-b.md", &f),
                ("findings/003-c.md", &f),
            ],
            true,
        );
        let limits = PackageLimits {
            max_entries: 3,
            ..Default::default()
        };
        assert!(matches!(
            validate_package(&bytes, &limits),
            Err(PackageError::TooLarge(_))
        ));
        let limits = PackageLimits {
            max_findings: 2,
            ..Default::default()
        };
        assert!(matches!(
            validate_package(&bytes, &limits),
            Err(PackageError::TooLarge(_))
        ));
        assert!(validate_package(&bytes, &PackageLimits::default()).is_ok());
    }

    #[test]
    fn rejects_disallowed_extensions_and_fake_images() {
        let yaml = project_yaml();
        for (name, data) in [
            ("assets/x.svg", b"<svg onload=alert(1)>".as_slice()),
            ("assets/x.html", b"<script>".as_slice()),
            ("assets/x", PNG),
            ("notas.txt", b"x".as_slice()),
        ] {
            let bytes = build_zip(&[("project.yaml", &yaml), (name, data)], true);
            assert!(err_of(&bytes).starts_with("paquete invalido"), "{name}");
        }
        // Extension de imagen pero contenido HTML.
        let bytes = build_zip(
            &[("project.yaml", &yaml), ("assets/x.png", b"<html>")],
            true,
        );
        assert!(err_of(&bytes).contains("no es una imagen valida"));
    }

    #[test]
    fn rejects_hash_mismatch_and_unlisted_files() {
        let (_, _, pkg) = example_package();
        // Reescribe el paquete cambiando un byte de project.yaml sin tocar el
        // manifiesto.
        let mut archive = ZipArchive::new(Cursor::new(pkg.bytes.clone())).unwrap();
        let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
        for i in 0..archive.len() {
            let mut file = archive.by_index(i).unwrap();
            let name = file.name().unwrap().to_string();
            let mut data = Vec::new();
            file.read_to_end(&mut data).unwrap();
            if name == "project.yaml" {
                data.push(b'\n');
            }
            zip.start_file(name, SimpleFileOptions::default()).unwrap();
            zip.write_all(&data).unwrap();
        }
        let tampered = zip.finish().unwrap().into_inner();
        assert!(err_of(&tampered).contains("hash no coincide"));

        // Un archivo que no esta en el manifiesto.
        let yaml = project_yaml();
        let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
        let manifest = Manifest {
            formato: 1,
            informe_id: None,
            version: None,
            base_version: None,
            generado_por: String::new(),
            proyecto: String::new(),
            archivos: vec![ManifestFile {
                ruta: "project.yaml".to_string(),
                sha256: sha256_hex(&yaml),
                bytes: yaml.len() as u64,
            }],
        };
        zip.start_file(MANIFEST, SimpleFileOptions::default())
            .unwrap();
        zip.write_all(&serde_json::to_vec(&manifest).unwrap())
            .unwrap();
        zip.start_file("project.yaml", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(&yaml).unwrap();
        zip.start_file("assets/extra.png", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(PNG).unwrap();
        let bytes = zip.finish().unwrap().into_inner();
        assert!(err_of(&bytes).contains("fuera del manifiesto"));
    }

    #[test]
    fn rejects_missing_manifest_bad_format_and_duplicates() {
        let yaml = project_yaml();
        let bytes = build_zip(&[("project.yaml", &yaml)], false);
        assert!(err_of(&bytes).contains("falta manifest.json"));

        let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
        zip.start_file(MANIFEST, SimpleFileOptions::default())
            .unwrap();
        zip.write_all(br#"{"formato":2,"archivos":[]}"#).unwrap();
        let bytes = zip.finish().unwrap().into_inner();
        assert!(err_of(&bytes).contains("formato de paquete no soportado"));

        let bytes = build_zip(
            &[
                ("project.yaml", &yaml),
                ("assets/a.png", PNG),
                ("assets/A.png", PNG),
            ],
            true,
        );
        assert!(err_of(&bytes).contains("duplicada"));

        assert!(err_of(b"no es un zip").contains("ZIP ilegible"));
    }

    #[test]
    fn rejects_invalid_yaml() {
        let bytes = build_zip(&[("project.yaml", b"name: [sin cerrar")], true);
        assert!(err_of(&bytes).contains("project.yaml invalido"));

        let yaml = project_yaml();
        let bytes = build_zip(
            &[
                ("project.yaml", &yaml),
                ("findings/001-x.md", b"sin front matter"),
            ],
            true,
        );
        assert!(err_of(&bytes).contains("findings/001-x.md"));
    }

    #[test]
    fn rejects_branding_outside_folder() {
        let yaml = project_yaml();
        let mut ws = WorkspaceMeta::default();
        ws.branding.logo_path = "/etc/passwd".to_string();
        let ws_yaml = serde_yaml::to_string(&ws).unwrap().into_bytes();
        let bytes = build_zip(
            &[("project.yaml", &yaml), ("workspace.yaml", &ws_yaml)],
            true,
        );
        assert!(err_of(&bytes).contains("archivo de marca invalido"));
    }

    #[test]
    fn rejects_package_over_size() {
        let limits = PackageLimits {
            max_package_bytes: 10,
            ..Default::default()
        };
        assert!(matches!(
            validate_package(&[0u8; 11], &limits),
            Err(PackageError::TooLarge(_))
        ));
    }
}
