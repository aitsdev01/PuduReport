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
use crate::yaml_guard::YamlBudget;

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

// Limites de seguridad compartidos con el importador de PuduHub
// (`informes/paquete.py`, `limits.py` y `documento.py`): ambos lados aceptan
// el mismo conjunto de paquetes.

/// Sobre 64 KiB, una entrada puede expandirse a lo mas 200 veces.
const RATIO_THRESHOLD_BYTES: u64 = 64 * 1024;
const MAX_RATIO: u64 = 200;
/// Sobre 1 MiB de contenido, el paquete completo a lo mas 50 veces su tamano.
const ARCHIVE_RATIO_THRESHOLD_BYTES: u64 = 1024 * 1024;
const MAX_ARCHIVE_RATIO: u64 = 50;
/// Front-matter de un hallazgo (bytes UTF-8), revisado antes de parsear el YAML.
const MAX_FRONT_MATTER_BYTES: usize = 128 * 1024;
/// Lado y area maximos que puede declarar una imagen.
const MAX_IMAGE_SIDE: u64 = 20_000;
const MAX_IMAGE_PIXELS: u64 = 50_000_000;

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
    /// Suma maxima de las entradas de texto (manifiesto, `project.yaml`,
    /// `workspace.yaml` y `findings/*.md`): lo que cabe en un documento
    /// guardado de PuduHub.
    pub max_text_bytes: u64,
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
            max_text_bytes: 48 * MIB,
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

// ---------------------------------------------------------------------------
// Dimensiones declaradas de las imagenes
// ---------------------------------------------------------------------------

/// Pasos maximos al recorrer la estructura de una imagen (chunks, segmentos,
/// sub-bloques). Una imagen legitima de 10 MiB usa unas decenas de miles.
const MAX_IMAGE_STEPS: u32 = 200_000;

/// Marcadores SOF0..SOF15 de JPEG salvo DHT (C4), JPG (C8) y DAC (CC).
fn jpeg_sof(marker: u8) -> bool {
    (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC)
}

fn be16(data: &[u8], at: usize) -> u64 {
    u64::from(u16::from_be_bytes([data[at], data[at + 1]]))
}

fn le16(data: &[u8], at: usize) -> u64 {
    u64::from(u16::from_le_bytes([data[at], data[at + 1]]))
}

fn le24(data: &[u8], at: usize) -> u64 {
    u64::from(data[at]) | u64::from(data[at + 1]) << 8 | u64::from(data[at + 2]) << 16
}

fn le32(data: &[u8], at: usize) -> u64 {
    u64::from(u32::from_le_bytes([
        data[at],
        data[at + 1],
        data[at + 2],
        data[at + 3],
    ]))
}

/// Contador de pasos: `None` cuando la estructura es demasiado compleja.
fn step(count: &mut u32) -> Option<()> {
    if *count >= MAX_IMAGE_STEPS {
        return None;
    }
    *count += 1;
    Some(())
}

fn png_sizes(data: &[u8]) -> Vec<(u64, u64)> {
    if data.len() < 24 || &data[12..16] != b"IHDR" {
        return Vec::new();
    }
    let width = u64::from(u32::from_be_bytes([data[16], data[17], data[18], data[19]]));
    let height = u64::from(u32::from_be_bytes([data[20], data[21], data[22], data[23]]));
    vec![(width, height)]
}

fn gif_sizes(data: &[u8]) -> Option<Vec<(u64, u64)>> {
    if data.len() < 10 {
        return Some(Vec::new());
    }
    let mut sizes = vec![(le16(data, 6), le16(data, 8))];
    let Some(&flags) = data.get(10) else {
        return Some(sizes);
    };
    let color_table = |flags: u8| {
        if flags & 0x80 != 0 {
            3usize << ((flags & 0x07) + 1)
        } else {
            0
        }
    };
    let mut offset = 13 + color_table(flags);
    let mut steps = 0;
    while offset < data.len() {
        step(&mut steps)?;
        match data[offset] {
            0x3B => break,
            0x21 => offset += 2,
            0x2C => {
                if offset + 10 > data.len() {
                    break;
                }
                sizes.push((le16(data, offset + 5), le16(data, offset + 7)));
                offset += 10 + color_table(data[offset + 9]) + 1;
            }
            _ => break,
        }
        while offset < data.len() && data[offset] != 0 {
            step(&mut steps)?;
            offset += usize::from(data[offset]) + 1;
        }
        offset += 1;
    }
    Some(sizes)
}

fn jpeg_sizes(data: &[u8]) -> Option<Vec<(u64, u64)>> {
    let mut sizes = Vec::new();
    let mut offset = 2;
    let mut steps = 0;
    while let Some(found) = data
        .get(offset..)
        .and_then(|rest| rest.iter().position(|&b| b == 0xFF))
    {
        offset += found;
        if offset + 1 >= data.len() {
            break;
        }
        step(&mut steps)?;
        let marker = data[offset + 1];
        // Relleno, 0xFF00 y marcadores sin largo (TEM, RST0-7): como libjpeg,
        // se salta lo que haya entre segmentos.
        if matches!(marker, 0x00 | 0x01 | 0xFF) || (0xD0..=0xD7).contains(&marker) {
            offset += if marker == 0xFF { 1 } else { 2 };
            continue;
        }
        if marker == 0xD9 || offset + 4 > data.len() {
            break;
        }
        let length = be16(data, offset + 2) as usize;
        if length < 2 {
            break;
        }
        if jpeg_sof(marker) && offset + 9 <= data.len() {
            sizes.push((be16(data, offset + 7), be16(data, offset + 5)));
        }
        offset += 2 + length;
    }
    Some(sizes)
}

fn webp_sizes(data: &[u8]) -> Option<Vec<(u64, u64)>> {
    let mut sizes = Vec::new();
    // (inicio, fin, dentro de un cuadro ANMF): los cuadros se recorren una vez.
    let mut pending = vec![(12usize, data.len(), false)];
    let mut steps = 0;
    while let Some((mut offset, end, nested)) = pending.pop() {
        while offset + 8 <= end {
            step(&mut steps)?;
            let kind = &data[offset..offset + 4];
            let size = le32(data, offset + 4) as usize;
            let start = offset + 8;
            let payload = &data[start..start.saturating_add(size).min(end)];
            match kind {
                b"VP8X" if payload.len() >= 10 => {
                    sizes.push((le24(payload, 4) + 1, le24(payload, 7) + 1));
                }
                b"VP8 " if payload.len() >= 10 && payload[3..6] == [0x9D, 0x01, 0x2A] => {
                    sizes.push((le16(payload, 6) & 0x3FFF, le16(payload, 8) & 0x3FFF));
                }
                b"VP8L" if payload.len() >= 5 && payload[0] == 0x2F => {
                    let bits = le32(payload, 1);
                    sizes.push(((bits & 0x3FFF) + 1, ((bits >> 14) & 0x3FFF) + 1));
                }
                b"ANMF" if !nested && payload.len() >= 16 => {
                    sizes.push((le24(payload, 6) + 1, le24(payload, 9) + 1));
                    pending.push((start + 16, start + payload.len(), true));
                }
                _ => {}
            }
            offset = start.saturating_add(size).saturating_add(size & 1);
        }
    }
    Some(sizes)
}

/// Lienzo y cuadros que declara una imagen antes de sus pixeles (PNG IHDR,
/// pantalla y cuadros GIF, SOFn de JPEG, VP8/VP8L/VP8X/ANMF de WebP). `None`
/// si la estructura es demasiado compleja para recorrerla.
fn declared_image_sizes(data: &[u8]) -> Option<Vec<(u64, u64)>> {
    if data.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        Some(png_sizes(data))
    } else if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        jpeg_sizes(data)
    } else if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        gif_sizes(data)
    } else if data.len() >= 12 && &data[0..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        webp_sizes(data)
    } else {
        Some(Vec::new())
    }
}

/// Los decodificadores reservan ancho x alto antes de leer pixeles: unos pocos
/// bytes no pueden declarar un lienzo enorme (Typst del servicio es el blanco).
fn check_image_dimensions(name: &str, data: &[u8]) -> Result<()> {
    let sizes = declared_image_sizes(data)
        .ok_or_else(|| invalid(format!("{name} tiene una estructura demasiado compleja")))?;
    for (width, height) in sizes {
        if width > MAX_IMAGE_SIDE || height > MAX_IMAGE_SIDE || width * height > MAX_IMAGE_PIXELS {
            return Err(invalid(format!(
                "{name} declara {width}x{height} pixeles (maximo {MAX_IMAGE_SIDE} por lado y \
                 {MAX_IMAGE_PIXELS} en total)"
            )));
        }
    }
    Ok(())
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
                || check_image_dimensions(&path, &bytes).is_err()
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
                .filter(|(file, b)| {
                    workspace::is_raster_image(b) && check_image_dimensions(file, b).is_ok()
                });
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

    // Primera pasada, solo encabezados (sin descomprimir): rutas, tipos,
    // limites por entrada, razon de compresion y totales.
    let archive_bytes = bytes.len() as u64;
    let mut seen: HashSet<String> = HashSet::new();
    let mut entries: Vec<Option<(String, EntryKind, u64)>> = Vec::with_capacity(archive.len());
    let mut total: u64 = 0;
    let mut text: u64 = 0;
    for index in 0..archive.len() {
        let file = archive
            .by_index_raw(index)
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
                entries.push(None);
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
        let size = file.size();
        if size > cap {
            return Err(PackageError::TooLarge(format!(
                "{name} declara {size} bytes (maximo {cap})"
            )));
        }
        // El tamano comprimido declarado no puede superar el paquete (una
        // mentira que esconderia la razon).
        let compressed = file.compressed_size().min(archive_bytes).max(1);
        if size > RATIO_THRESHOLD_BYTES && size > MAX_RATIO * compressed {
            return Err(PackageError::TooLarge(format!(
                "{name} se expande mas de {MAX_RATIO} veces al descomprimir"
            )));
        }
        total += size;
        if total > limits.max_total_uncompressed {
            return Err(PackageError::TooLarge(format!(
                "el contenido descomprimido supera {} bytes",
                limits.max_total_uncompressed
            )));
        }
        if !matches!(kind, EntryKind::Asset | EntryKind::Branding) {
            text += size;
            if text > limits.max_text_bytes {
                return Err(PackageError::TooLarge(format!(
                    "los archivos de texto superan {} bytes",
                    limits.max_text_bytes
                )));
            }
        }
        entries.push(Some((name, kind, size)));
    }
    // Muchas entradas justo bajo la razon por entrada siguen siendo una bomba.
    if total > ARCHIVE_RATIO_THRESHOLD_BYTES && total > MAX_ARCHIVE_RATIO * archive_bytes {
        return Err(PackageError::TooLarge(format!(
            "el paquete se expande mas de {MAX_ARCHIVE_RATIO} veces al descomprimir"
        )));
    }

    // Segunda pasada: lectura acotada al tamano declarado (ya validado) mas un
    // byte; si los bytes reales no coinciden, el encabezado mentia.
    let mut contents: BTreeMap<String, (EntryKind, Vec<u8>)> = BTreeMap::new();
    for (index, entry) in entries.into_iter().enumerate() {
        let Some((name, kind, size)) = entry else {
            continue;
        };
        let file = archive
            .by_index(index)
            .map_err(|e| invalid(format!("entrada ilegible: {e}")))?;
        let mut data = Vec::with_capacity(size as usize);
        file.take(size + 1)
            .read_to_end(&mut data)
            .map_err(|e| invalid(format!("no se pudo descomprimir {name}: {e}")))?;
        if data.len() as u64 != size {
            return Err(invalid(format!(
                "{name}: el tamano descomprimido no coincide con el declarado"
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
    // Un solo presupuesto de YAML para project.yaml y todos los front-matter.
    let mut yaml_budget = YamlBudget::default();
    yaml_budget
        .check(project_text)
        .map_err(|e| invalid(format!("project.yaml invalido: {e}")))?;
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
                let (yaml, _) = workspace::split_front_matter(text, &id)
                    .map_err(|e| invalid(format!("{name}: {e}")))?;
                if yaml.len() > MAX_FRONT_MATTER_BYTES {
                    return Err(PackageError::TooLarge(format!(
                        "{name}: el front-matter supera {MAX_FRONT_MATTER_BYTES} bytes"
                    )));
                }
                yaml_budget
                    .check(&yaml)
                    .map_err(|e| invalid(format!("{name}: {e}")))?;
                workspace::parse_finding_document(&id, text)
                    .map_err(|e| invalid(format!("{name}: {e}")))?;
                findings.push((id, data));
            }
            EntryKind::Asset | EntryKind::Branding => {
                // La extension no basta: se exige la firma real de la imagen.
                if !workspace::is_raster_image(&data) {
                    return Err(invalid(format!("{name} no es una imagen valida")));
                }
                check_image_dimensions(&name, &data)?;
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
                YamlBudget::default()
                    .check(text)
                    .map_err(|e| invalid(format!("workspace.yaml invalido: {e}")))?;
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
        build_zip_with(entries, with_manifest, CompressionMethod::Deflated)
    }

    fn build_zip_with(
        entries: &[(&str, &[u8])],
        with_manifest: bool,
        method: CompressionMethod,
    ) -> Vec<u8> {
        let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
        let opts = SimpleFileOptions::default().compression_method(method);
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

    /// Texto que casi no se comprime (hexadecimal pseudoaleatorio).
    fn noise(len: usize) -> String {
        let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
        (0..len)
            .map(|_| {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                char::from(b"0123456789abcdef"[(state >> 60) as usize])
            })
            .collect()
    }

    #[test]
    fn rejects_entry_compression_ratio() {
        let yaml = project_yaml();
        // 300 KiB de ceros: bajo el limite del hallazgo, pero se expande miles
        // de veces.
        let mut finding = b"---\ntitle: x\n---\n".to_vec();
        finding.extend_from_slice(&vec![b'a'; 300 * 1024]);
        let bytes = build_zip(
            &[("project.yaml", &yaml), ("findings/001-x.md", &finding)],
            true,
        );
        let err = validate_package(&bytes, &PackageLimits::default()).unwrap_err();
        assert!(matches!(err, PackageError::TooLarge(_)), "{err}");
        assert!(err.to_string().contains("200 veces"), "{err}");
    }

    #[test]
    fn rejects_archive_compression_ratio() {
        let yaml = project_yaml();
        // Cada hallazgo queda bajo 64 KiB (sin razon por entrada), pero el
        // paquete completo se expande mas de 50 veces.
        let mut finding = b"---\ntitle: x\n---\n".to_vec();
        finding.extend_from_slice(&vec![b'a'; 60 * 1024]);
        let names: Vec<String> = (0..40).map(|i| format!("findings/{i:03}-x.md")).collect();
        let mut entries: Vec<(&str, &[u8])> = vec![("project.yaml", &yaml)];
        entries.extend(names.iter().map(|n| (n.as_str(), finding.as_slice())));
        let bytes = build_zip(&entries, true);
        let err = validate_package(&bytes, &PackageLimits::default()).unwrap_err();
        assert!(matches!(err, PackageError::TooLarge(_)), "{err}");
        assert!(err.to_string().contains("50 veces"), "{err}");
    }

    /// Cambia el tamano descomprimido declarado de `name` en sus encabezados
    /// local y central.
    fn patch_declared_size(zip: &mut [u8], name: &str, size: u32) {
        let mut patched = 0;
        for at in 0..zip.len().saturating_sub(46) {
            // (inicio del nombre, tamano descomprimido, largo del nombre).
            let (name_at, size_at, len_at) = match zip[at..at + 4] {
                [0x50, 0x4B, 0x03, 0x04] => (30, 22, 26),
                [0x50, 0x4B, 0x01, 0x02] => (46, 24, 28),
                _ => continue,
            };
            let len = u16::from_le_bytes([zip[at + len_at], zip[at + len_at + 1]]) as usize;
            if zip.get(at + name_at..at + name_at + len) == Some(name.as_bytes()) {
                zip[at + size_at..at + size_at + 4].copy_from_slice(&size.to_le_bytes());
                patched += 1;
            }
        }
        assert_eq!(patched, 2);
    }

    #[test]
    fn lying_size_header_cannot_over_read() {
        let yaml = project_yaml();
        let stored = build_zip_with(&[("project.yaml", &yaml)], true, CompressionMethod::Stored);
        // Declara menos de lo que trae: la lectura se corta en lo declarado + 1.
        let mut bytes = stored.clone();
        patch_declared_size(&mut bytes, "project.yaml", 4);
        let err = err_of(&bytes);
        assert!(
            err.starts_with("paquete invalido") && err.contains("project.yaml"),
            "{err}"
        );
        // Declara mas de lo que trae.
        let mut bytes = stored;
        patch_declared_size(&mut bytes, "project.yaml", yaml.len() as u32 + 10);
        let err = err_of(&bytes);
        assert!(err.contains("no coincide con el declarado"), "{err}");
    }

    #[test]
    fn rejects_text_over_combined_limit() {
        let yaml = project_yaml();
        let f = b"---\ntitle: x\n---\nbody".to_vec();
        let bytes = build_zip(&[("project.yaml", &yaml), ("findings/001-a.md", &f)], true);
        assert_eq!(PackageLimits::default().max_text_bytes, 48 * 1024 * 1024);
        let limits = PackageLimits {
            max_text_bytes: 100,
            ..Default::default()
        };
        let err = validate_package(&bytes, &limits).unwrap_err();
        assert!(err.to_string().contains("archivos de texto"), "{err}");
        assert!(validate_package(&bytes, &PackageLimits::default()).is_ok());
    }

    #[test]
    fn rejects_front_matter_over_limit() {
        let yaml = project_yaml();
        let big = format!(
            "---\ntitle: x\ndescription: {}\n---\nbody",
            noise(130 * 1024)
        );
        let bytes = build_zip(
            &[
                ("project.yaml", &yaml),
                ("findings/001-x.md", big.as_bytes()),
            ],
            true,
        );
        let err = err_of(&bytes);
        assert!(err.contains("front-matter supera"), "{err}");

        let ok = format!(
            "---\ntitle: x\ndescription: {}\n---\nbody",
            noise(100 * 1024)
        );
        let bytes = build_zip(
            &[
                ("project.yaml", &yaml),
                ("findings/001-x.md", ok.as_bytes()),
            ],
            true,
        );
        validate_package(&bytes, &PackageLimits::default()).unwrap();
    }

    #[test]
    fn rejects_yaml_anchors_aliases_and_tags() {
        let yaml = b"name: &nombre Demo\nclient: *nombre\n".to_vec();
        let bytes = build_zip(&[("project.yaml", &yaml)], true);
        let err = err_of(&bytes);
        assert!(
            err.contains("project.yaml invalido") && err.contains("anclas"),
            "{err}"
        );

        let project = project_yaml();
        let finding = b"---\ntitle: !!str x\n---\nbody".to_vec();
        let bytes = build_zip(
            &[("project.yaml", &project), ("findings/001-x.md", &finding)],
            true,
        );
        let err = err_of(&bytes);
        assert!(
            err.contains("findings/001-x.md") && err.contains("anclas"),
            "{err}"
        );
    }

    #[test]
    fn rejects_yaml_too_deep() {
        let mut yaml = String::from("name: Demo\nextra:\n");
        for level in 1..=33 {
            yaml.push_str(&" ".repeat(level * 2));
            yaml.push_str("k:\n");
        }
        let bytes = build_zip(&[("project.yaml", yaml.as_bytes())], true);
        let err = err_of(&bytes);
        assert!(err.contains("anidamiento demasiado profundo"), "{err}");
    }

    #[test]
    fn rejects_yaml_with_too_many_nodes() {
        let mut yaml = String::from("name: Demo\nextra:\n");
        yaml.push_str(&"- x\n".repeat(50_000));
        let bytes = build_zip_with(
            &[("project.yaml", yaml.as_bytes())],
            true,
            CompressionMethod::Stored,
        );
        let err = err_of(&bytes);
        assert!(err.contains("demasiados nodos"), "{err}");
    }

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13];
        out.extend_from_slice(b"IHDR");
        out.extend_from_slice(&width.to_be_bytes());
        out.extend_from_slice(&height.to_be_bytes());
        out.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
        out
    }

    fn gif(width: u16, height: u16) -> Vec<u8> {
        let mut out = b"GIF89a".to_vec();
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&[0, 0, 0]);
        // Descriptor de imagen: el cuadro declara su propio tamano.
        out.push(0x2C);
        out.extend_from_slice(&[0, 0, 0, 0]);
        out.extend_from_slice(&width.to_le_bytes());
        out.extend_from_slice(&height.to_le_bytes());
        out.extend_from_slice(&[0, 2, 0, 0x3B]);
        out
    }

    fn jpeg(width: u16, height: u16) -> Vec<u8> {
        let mut out = vec![0xFF, 0xD8, 0xFF, 0xE0, 0, 4, 0, 0, 0xFF, 0xC0, 0, 11, 8];
        out.extend_from_slice(&height.to_be_bytes());
        out.extend_from_slice(&width.to_be_bytes());
        out.extend_from_slice(&[1, 1, 0x11, 0, 0xFF, 0xD9]);
        out
    }

    fn webp(width: u32, height: u32) -> Vec<u8> {
        let mut chunk = b"VP8X".to_vec();
        chunk.extend_from_slice(&10u32.to_le_bytes());
        chunk.extend_from_slice(&[0, 0, 0, 0]);
        chunk.extend_from_slice(&(width - 1).to_le_bytes()[..3]);
        chunk.extend_from_slice(&(height - 1).to_le_bytes()[..3]);
        let mut out = b"RIFF".to_vec();
        out.extend_from_slice(&(4 + chunk.len() as u32).to_le_bytes());
        out.extend_from_slice(b"WEBP");
        out.extend_from_slice(&chunk);
        out
    }

    #[test]
    fn rejects_images_with_huge_declared_dimensions() {
        let yaml = project_yaml();
        let huge: [(&str, Vec<u8>); 6] = [
            ("assets/a.png", png(20_001, 10)),
            ("assets/b.png", png(10_000, 10_000)),
            ("assets/c.gif", gif(30_000, 1)),
            ("assets/d.jpg", jpeg(1, 20_001)),
            ("assets/e.webp", webp(8_000, 8_000)),
            ("branding/f.png", png(u32::MAX, u32::MAX)),
        ];
        for (name, data) in &huge {
            let bytes = build_zip(&[("project.yaml", &yaml), (name, data)], true);
            let err = err_of(&bytes);
            assert!(err.contains("pixeles"), "{name}: {err}");
        }
        let fine: [(&str, Vec<u8>); 4] = [
            ("assets/a.png", png(20_000, 2_500)),
            ("assets/c.gif", gif(640, 480)),
            ("assets/d.jpg", jpeg(4_000, 3_000)),
            ("assets/e.webp", webp(7_000, 7_000)),
        ];
        for (name, data) in &fine {
            let bytes = build_zip(&[("project.yaml", &yaml), (name, data)], true);
            validate_package(&bytes, &PackageLimits::default())
                .unwrap_or_else(|e| panic!("{name}: {e}"));
        }
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
