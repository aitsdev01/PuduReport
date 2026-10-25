// SPDX-License-Identifier: GPL-3.0-only
// Copyright (c) 2026 bc0d3

//! Casos de uso de la sincronizacion: listar, abrir, enviar, descargar la
//! version del servidor y subir un proyecto como informe nuevo.
//!
//! Los paquetes recibidos pasan siempre por el importador estricto de
//! `pudureport_core::package` (rutas, tamanos, firmas de imagen, hashes y YAML)
//! y solo se escriben con `package::write_package`: ningun dato del servidor
//! elige una ruta en disco. El reemplazo de un proyecto existente escribe la
//! version nueva en una carpeta aparte y la intercambia al final.

use std::fs;
use std::path::Path;

use pudureport_core::models::PROJECT_TYPES;
use pudureport_core::package::{self, ImportOptions, PackageInfo, PackageLimits, ValidatedPackage};
use pudureport_core::workspace;
use serde::Serialize;

use super::client::{HubClient, InformeItem};
use super::link::{self, Changes, Link, LINK_FORMAT};
use super::{clean_text, HubError, MAX_PACKAGE_BYTES, MAX_VERSION};

/// Estado del vinculo de un proyecto, para el indicador de la barra.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LinkStatus {
    pub vinculado: bool,
    pub servidor: Option<String>,
    pub informe_id: Option<String>,
    pub base_version: Option<u64>,
    pub sincronizado_at: Option<String>,
    pub cambios_sin_enviar: bool,
    /// false si el proyecto apunta a otro servidor que el configurado.
    pub mismo_servidor: bool,
    /// El archivo de vinculo existe pero no es valido.
    pub vinculo_invalido: bool,
}

/// Copia local de un informe del listado.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LocalCopy {
    pub proyecto_id: String,
    pub base_version: u64,
    pub cambios_sin_enviar: bool,
}

/// Fila del listado con la copia local, si existe.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ListedInforme {
    #[serde(flatten)]
    pub informe: InformeItem,
    pub local: Option<LocalCopy>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ListResult {
    pub items: Vec<ListedInforme>,
    pub total: u64,
    pub page: u32,
    pub page_size: u32,
    pub omitidos: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct OpenResult {
    pub proyecto_id: String,
    pub nombre: String,
    pub version: u64,
    /// true si se actualizo una copia local existente.
    pub actualizado: bool,
    /// Por ejemplo, cambios de la web que no venian en la version descargada.
    pub aviso: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SyncOutcome {
    pub informe_id: String,
    pub proyecto_id: String,
    /// Version creada en PuduHub.
    pub version: u64,
    /// Si el resultado de la fusion se aplico al proyecto local.
    pub aplicado: bool,
    /// Lo que llego de PuduHub al aplicar la fusion (cambios de otras personas).
    pub cambios: Changes,
    /// Adjuntos de `assets/` que no viajaron por no ser imagenes permitidas.
    pub omitidos: Vec<String>,
    pub aviso: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DownloadResult {
    pub proyecto_id: String,
    /// Copia del proyecto local antes de reemplazarlo.
    pub respaldo_id: Option<String>,
    pub version: u64,
    pub aviso: Option<String>,
}

fn not_linked() -> HubError {
    HubError::new(
        "no_vinculado",
        "Este proyecto no esta vinculado a un informe de PuduHub.",
    )
}

fn other_server(link: &Link) -> HubError {
    HubError::new(
        "otro_servidor",
        format!(
            "Este proyecto esta vinculado a otro servidor ({}). Configure esa conexion en Ajustes.",
            link.servidor
        ),
    )
}

fn linked_for(client: &HubClient, root: &Path, project_id: &str) -> Result<Link, HubError> {
    let link = link::read_link(root, project_id)?.ok_or_else(not_linked)?;
    if link.servidor != client.servidor() {
        return Err(other_server(&link));
    }
    Ok(link)
}

fn has_changes(root: &Path, project_id: &str, link: &Link) -> Result<bool, HubError> {
    Ok(link.huella.is_empty() || link::snapshot(root, project_id)?.huella() != link.huella)
}

/// Estado del vinculo de un proyecto (sin red).
pub fn link_status(
    root: &Path,
    project_id: &str,
    servidor: Option<&str>,
) -> Result<LinkStatus, HubError> {
    let empty = LinkStatus {
        vinculado: false,
        servidor: None,
        informe_id: None,
        base_version: None,
        sincronizado_at: None,
        cambios_sin_enviar: false,
        mismo_servidor: false,
        vinculo_invalido: false,
    };
    let link = match link::read_link(root, project_id) {
        Ok(Some(link)) => link,
        Ok(None) => return Ok(empty),
        Err(e) if e.codigo == "vinculo_invalido" => {
            return Ok(LinkStatus {
                vinculo_invalido: true,
                ..empty
            })
        }
        Err(e) => return Err(e),
    };
    Ok(LinkStatus {
        vinculado: true,
        cambios_sin_enviar: has_changes(root, project_id, &link)?,
        mismo_servidor: servidor == Some(link.servidor.as_str()),
        servidor: Some(link.servidor),
        informe_id: Some(link.informe_id),
        base_version: Some(link.base_version),
        sincronizado_at: Some(link.sincronizado_at),
        vinculo_invalido: false,
    })
}

/// Proyectos del workspace vinculados a `servidor`, por id de informe.
fn linked_projects(root: &Path, servidor: &str) -> Vec<(String, Link)> {
    let Ok(projects) = workspace::list_projects(root) else {
        return Vec::new();
    };
    projects
        .into_iter()
        .filter_map(|p| {
            let link = link::read_link(root, &p.id).ok().flatten()?;
            (link.servidor == servidor).then_some((p.id, link))
        })
        .collect()
}

fn find_linked(root: &Path, servidor: &str, informe_id: &str) -> Option<(String, Link)> {
    linked_projects(root, servidor)
        .into_iter()
        .find(|(_, link)| link.informe_id.eq_ignore_ascii_case(informe_id))
}

/// Informes asignados, con la copia local de cada uno.
pub async fn list(
    client: &HubClient,
    root: &Path,
    page: u32,
    query: &str,
) -> Result<ListResult, HubError> {
    let remote = client.list(page, query).await?;
    let linked = linked_projects(root, client.servidor());
    let items = remote
        .items
        .into_iter()
        .map(|informe| {
            let local = linked
                .iter()
                .find(|(_, link)| link.informe_id == informe.id)
                .map(|(project_id, link)| LocalCopy {
                    proyecto_id: project_id.clone(),
                    base_version: link.base_version,
                    cambios_sin_enviar: has_changes(root, project_id, link).unwrap_or(true),
                });
            ListedInforme { informe, local }
        })
        .collect();
    Ok(ListResult {
        items,
        total: remote.total,
        page: remote.page,
        page_size: remote.page_size,
        omitidos: remote.omitidos,
    })
}

fn invalid_download() -> HubError {
    HubError::new(
        "paquete_invalido",
        "El paquete recibido de PuduHub no es valido y no se importo.",
    )
}

/// Valida un paquete recibido con el importador estricto y comprueba que sea
/// del informe pedido. Devuelve el paquete y su version.
fn validate_download(
    bytes: &[u8],
    informe_id: &str,
    expected: Option<u64>,
) -> Result<(ValidatedPackage, u64), HubError> {
    let validated = package::validate_package(bytes, &PackageLimits::default())
        .map_err(|_| invalid_download())?;
    let same_informe = validated
        .manifest
        .informe_id
        .as_deref()
        .is_some_and(|id| id.eq_ignore_ascii_case(informe_id));
    let version = validated
        .manifest
        .version
        .filter(|v| (1..=MAX_VERSION).contains(v))
        .ok_or_else(invalid_download)?;
    if !same_informe || expected.is_some_and(|e| e != version) {
        return Err(invalid_download());
    }
    Ok((validated, version))
}

/// Motivo de la version que se crea al abrir un informe con cambios sin
/// version en PuduHub.
pub const OPEN_REASON: &str = "Apertura desde PuduReport escritorio";

/// Ultima version de un informe, validada.
struct Fetched {
    validated: ValidatedPackage,
    version: u64,
    aviso: Option<String>,
}

/// Descarga y valida una version: `version` si se indica, si no la ultima.
async fn download(
    client: &HubClient,
    informe_id: &str,
    version: Option<u64>,
) -> Result<(ValidatedPackage, u64, bool), HubError> {
    let response = client.package(informe_id, version).await?;
    let pending = response.borrador_pendiente;
    let expected = version.or(response.version);
    let bytes = response.bytes().await?;
    let (validated, version) = validate_download(&bytes, informe_id, expected)?;
    Ok((validated, version, pending))
}

/// Trae la ultima version del informe incluyendo los cambios de la web.
///
/// `GET /{id}/paquete` es de solo lectura: entrega versiones existentes. Si el
/// informe no tiene ninguna, o el borrador cambio despues de la ultima
/// (`X-Informe-Borrador-Pendiente: 1`), se toma el bloqueo, se crea un punto de
/// control con `POST /{id}/versiones` y se baja esa version. El bloqueo se
/// libera siempre al terminar (la edicion sigue en el escritorio, sin
/// conexion). Si otra persona tiene el bloqueo, se baja la ultima version tal
/// cual y se avisa si quedaron cambios de la web fuera.
async fn fetch_latest(client: &HubClient, informe_id: &str) -> Result<Fetched, HubError> {
    let lock = match client.lock(informe_id, false).await {
        Ok(lock) => lock,
        Err(e) if e.codigo == "bloqueo_tomado" => {
            let (validated, version, pending) = download(client, informe_id, None).await?;
            let aviso = pending.then(|| {
                format!(
                    "Se descargo la version {version}, pero el informe tiene cambios en PuduHub que aun no son una version y no se incluyeron: otra persona lo esta editando. Actualice cuando se libere el bloqueo."
                )
            });
            return Ok(Fetched {
                validated,
                version,
                aviso,
            });
        }
        Err(e) => return Err(e),
    };
    let result = fetch_with_lock(client, informe_id, &lock).await;
    // El bloqueo se libera siempre; si falla, vence solo.
    let _ = client.unlock(informe_id, &lock).await;
    result
}

async fn fetch_with_lock(
    client: &HubClient,
    informe_id: &str,
    lock: &str,
) -> Result<Fetched, HubError> {
    // Con el bloqueo tomado nadie mas escribe: la revision no cambia.
    let state = client.state(informe_id).await?;
    if state.version_actual > 0 {
        let response = client.package(informe_id, None).await?;
        if !response.borrador_pendiente {
            let expected = response.version;
            let bytes = response.bytes().await?;
            let (validated, version) = validate_download(&bytes, informe_id, expected)?;
            return Ok(Fetched {
                validated,
                version,
                aviso: None,
            });
        }
        // Hay cambios sin version: esta respuesta se descarta sin leerla.
    }
    let version = client
        .create_version(informe_id, lock, OPEN_REASON, state.revision)
        .await?;
    let (validated, version, _) = download(client, informe_id, Some(version)).await?;
    Ok(Fetched {
        validated,
        version,
        aviso: None,
    })
}

/// Marca de tiempo compacta para nombres de respaldo (`20261025-101500`).
fn stamp() -> String {
    workspace::now_utc_iso()
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == 'T')
        .collect::<String>()
        .replace('T', "-")
}

/// Reemplaza el contenido del proyecto por el paquete, conservando el id de
/// carpeta. Con `backup`, el proyecto anterior queda como un proyecto aparte
/// (sin vinculo); si no, se descarta salvo su carpeta `build/`.
fn replace_project(
    root: &Path,
    project_id: &str,
    validated: ValidatedPackage,
    backup: bool,
) -> Result<Option<String>, HubError> {
    workspace::read_project_meta(root, project_id).map_err(HubError::local)?;
    let staged = package::write_package(
        root,
        validated,
        &ImportOptions {
            project_id: Some(format!("{project_id}-puduhub")),
            ..Default::default()
        },
    )
    .map_err(HubError::local)?
    .project_id;
    let current = root.join(project_id);
    let aside_id = if backup {
        workspace::unique_project_id(root, &format!("{project_id}-respaldo-{}", stamp()))
    } else {
        format!(".puduhub-anterior-{}", uuid::Uuid::new_v4())
    };
    let aside = root.join(&aside_id);
    if let Err(e) = fs::rename(&current, &aside) {
        let _ = fs::remove_dir_all(root.join(&staged));
        return Err(HubError::local(e));
    }
    if let Err(e) = fs::rename(root.join(&staged), &current) {
        let _ = fs::rename(&aside, &current);
        let _ = fs::remove_dir_all(root.join(&staged));
        return Err(HubError::local(e));
    }
    if backup {
        link::remove_link(&aside);
        if let Ok(mut meta) = workspace::read_project_meta(root, &aside_id) {
            meta.name = format!("{} (respaldo local)", meta.name);
            let _ = workspace::write_project_meta(root, &aside_id, &meta);
        }
        Ok(Some(aside_id))
    } else {
        // Los PDF generados no viajan en el paquete: se conservan.
        let build = aside.join("build");
        if build.is_dir() {
            let _ = fs::rename(&build, current.join("build"));
        }
        let _ = fs::remove_dir_all(&aside);
        Ok(None)
    }
}

fn new_link(servidor: &str, informe_id: &str, version: u64, huella: String) -> Link {
    Link {
        formato: LINK_FORMAT,
        servidor: servidor.to_string(),
        informe_id: informe_id.to_ascii_lowercase(),
        base_version: version,
        sincronizado_at: workspace::now_utc_iso(),
        huella,
    }
}

/// Descarga la ultima version de un informe (con los cambios de la web, ver
/// [`fetch_latest`]) y la abre como proyecto local.
/// Si ya hay una copia vinculada, la actualiza solo si no tiene cambios sin
/// enviar.
pub async fn open_informe(
    client: &HubClient,
    root: &Path,
    informe_id: &str,
) -> Result<OpenResult, HubError> {
    let existing = find_linked(root, client.servidor(), informe_id);
    if let Some((project_id, link)) = &existing {
        if has_changes(root, project_id, link)? {
            let mut error = HubError::new(
                "cambios_locales",
                "Este informe ya esta en este equipo con cambios sin enviar. Envielos a PuduHub antes de actualizar la copia local.",
            );
            error.extra.proyecto_id = Some(project_id.clone());
            return Err(error);
        }
    }
    let Fetched {
        validated,
        version,
        aviso,
    } = fetch_latest(client, informe_id).await?;
    let (project_id, actualizado) = match existing {
        Some((project_id, _)) => {
            replace_project(root, &project_id, validated, false)?;
            (project_id, true)
        }
        None => {
            let imported = package::write_package(root, validated, &ImportOptions::default())
                .map_err(HubError::local)?;
            (imported.project_id, false)
        }
    };
    let snapshot = link::snapshot(root, &project_id)?;
    link::write_link(
        root,
        &project_id,
        &new_link(client.servidor(), informe_id, version, snapshot.huella()),
    )?;
    let nombre = workspace::read_project_meta(root, &project_id)
        .map(|m| m.name)
        .unwrap_or_default();
    Ok(OpenResult {
        proyecto_id: project_id,
        nombre,
        version,
        actualizado,
        aviso,
    })
}

/// Envia el proyecto a PuduHub: exporta el paquete con `informeId` y
/// `baseVersion`, toma el bloqueo, sincroniza, libera el bloqueo y aplica el
/// resultado de la fusion.
///
/// El paquete no lleva `workspace.yaml`: la marca del workspace local no es la
/// del informe (al abrir no se aplica al workspace) y PuduHub conserva la marca
/// del informe cuando el paquete no la trae.
pub async fn send_project(
    client: &HubClient,
    root: &Path,
    project_id: &str,
    forzar: bool,
) -> Result<SyncOutcome, HubError> {
    send(client, root, project_id, forzar, false).await
}

async fn send(
    client: &HubClient,
    root: &Path,
    project_id: &str,
    forzar: bool,
    include_workspace: bool,
) -> Result<SyncOutcome, HubError> {
    let link = linked_for(client, root, project_id)?;
    let before = link::snapshot(root, project_id)?;
    let exported = package::export_package(
        root,
        project_id,
        &PackageInfo {
            informe_id: Some(link.informe_id.clone()),
            version: Some(link.base_version),
            base_version: Some(link.base_version),
            generado_por: String::new(),
        },
        include_workspace,
    )
    .map_err(HubError::local)?;
    if exported.bytes.len() as u64 > MAX_PACKAGE_BYTES {
        return Err(super::client::package_too_large());
    }
    let id = link.informe_id.clone();
    let lock = client.lock(&id, forzar).await.map_err(|mut e| {
        e.extra.proyecto_id = Some(project_id.to_string());
        e
    })?;
    let result = client.synchronize(&id, &lock, exported.bytes).await;
    // El bloqueo se libera siempre; si falla, vence solo.
    let _ = client.unlock(&id, &lock).await;
    let response = result.map_err(|mut e| {
        e.extra.proyecto_id = Some(project_id.to_string());
        if e.codigo == "conflicto" {
            e.extra.contexto = Some(before.conflict_context());
        }
        e
    })?;

    let mut outcome = SyncOutcome {
        informe_id: id.clone(),
        proyecto_id: project_id.to_string(),
        version: response.version,
        aplicado: false,
        cambios: Changes::default(),
        omitidos: exported.skipped,
        aviso: None,
    };
    // PuduHub no devuelve el documento fusionado: se baja la version creada.
    let validated = match download(client, &id, Some(response.version)).await {
        Ok((validated, _, _)) => validated,
        Err(_) => {
            // La base no avanza: el proximo envio vuelve a fusionar sin perder
            // cambios de nadie.
            outcome.aviso = Some(format!(
                "Se creo la version {} en PuduHub, pero no se pudo descargar el resultado. Use \"Actualizar desde PuduHub\" mas tarde.",
                response.version
            ));
            return Ok(outcome);
        }
    };
    if link::snapshot(root, project_id)?.huella() != before.huella() {
        outcome.aviso = Some(
            "El proyecto cambio en este equipo durante el envio: no se aplico el resultado. Vuelva a enviar."
                .to_string(),
        );
        return Ok(outcome);
    }
    replace_project(root, project_id, validated, false)?;
    let after = link::snapshot(root, project_id)?;
    outcome.cambios = link::diff(&before, &after);
    outcome.aplicado = true;
    link::write_link(
        root,
        project_id,
        &new_link(client.servidor(), &id, response.version, after.huella()),
    )?;
    Ok(outcome)
}

/// Reemplaza el proyecto local por la ultima version de PuduHub, guardando
/// antes una copia del proyecto como respaldo (para resolver conflictos).
pub async fn download_server_version(
    client: &HubClient,
    root: &Path,
    project_id: &str,
) -> Result<DownloadResult, HubError> {
    let link = linked_for(client, root, project_id)?;
    let Fetched {
        validated,
        version,
        aviso,
    } = fetch_latest(client, &link.informe_id).await?;
    let backup = replace_project(root, project_id, validated, true)?;
    let snapshot = link::snapshot(root, project_id)?;
    link::write_link(
        root,
        project_id,
        &new_link(
            client.servidor(),
            &link.informe_id,
            version,
            snapshot.huella(),
        ),
    )?;
    Ok(DownloadResult {
        proyecto_id: project_id.to_string(),
        respaldo_id: backup,
        version,
        aviso,
    })
}

/// Crea un informe nuevo en PuduHub (`POST /`), crea su primera version como
/// base y envia el proyecto local con la sincronizacion normal (con la marca
/// del workspace).
pub async fn upload_new(
    client: &HubClient,
    root: &Path,
    project_id: &str,
) -> Result<SyncOutcome, HubError> {
    if link::read_link(root, project_id)?.is_some() {
        return Err(HubError::new(
            "ya_vinculado",
            "Este proyecto ya esta vinculado a un informe de PuduHub. Use \"Enviar a PuduHub\".",
        ));
    }
    let meta = workspace::read_project_meta(root, project_id).map_err(HubError::local)?;
    let mut titulo = clean_text(&meta.name, 200);
    if titulo.is_empty() {
        titulo = clean_text(project_id, 200);
    }
    let tipo = if PROJECT_TYPES.contains(&meta.project_type.as_str()) {
        meta.project_type.as_str()
    } else {
        "pentest"
    };
    let created = client.create(&titulo, tipo).await?;
    // El informe nuevo no tiene versiones: el punto de control del borrador
    // recien creado es la base de la fusion (vacia), asi que entra todo lo
    // local.
    let Fetched { version, .. } = fetch_latest(client, &created.id).await?;
    link::write_link(
        root,
        project_id,
        &new_link(client.servidor(), &created.id, version, String::new()),
    )?;
    // Informe nuevo: viaja tambien la marca del workspace (logo, colores y
    // marca de agua) para que el PDF de PuduHub se vea igual que el local.
    send(client, root, project_id, false, true)
        .await
        .map(|mut outcome| {
            if outcome.aviso.is_none() {
                outcome.aviso = Some(format!("Informe creado en PuduHub: {}", created.titulo));
            }
            outcome
        })
        .map_err(|mut e| {
            e.mensaje = format!(
                "El informe se creo en PuduHub y el proyecto quedo vinculado, pero el envio fallo: {} Reintente con \"Enviar a PuduHub\".",
                e.mensaje
            );
            e
        })
}
