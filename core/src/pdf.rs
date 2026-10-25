// SPDX-License-Identifier: GPL-3.0-only
// Copyright (c) 2026 bc0d3

//! Pipeline de generacion de PDF.
//!
//! Separacion critica (README.dev.md): datos (`data.json`, generado) vs
//! presentacion (`.typ`, editable). El backend serializa el proyecto a
//! `build/data.json`, copia la plantilla activa a `build/report.typ` y compila
//! con Typst. Cualquier plantilla consume el mismo `data.json`.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::markdown;
use crate::models::{
    categoria_riesgo_label, prioridad_label, prioridad_rank, Branding, ProjectMeta, ReportBlock,
    Severity, TeamMember, Watermark, CATEGORIAS_RIESGO,
};
use crate::sections;
use crate::workspace;

use crate::naming;

#[derive(Debug, thiserror::Error)]
pub enum PdfError {
    #[error("error de workspace: {0}")]
    Workspace(#[from] workspace::WorkspaceError),
    #[error("error de entrada/salida: {0}")]
    Io(#[from] std::io::Error),
    #[error("error de serializacion JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("no se encontro la plantilla: {0}")]
    TemplateNotFound(String),
    #[error(
        "No se pudo generar el PDF. Revisa el formato del contenido (negrita/cursiva o corchetes sin cerrar) en los hallazgos o secciones.\n\n{0}"
    )]
    Compile(String),
    #[error("no se encontro el binario de Typst")]
    TypstNotFound,
    #[error("la generacion del PDF supero el tiempo limite de {0} s")]
    Timeout(u64),
    #[error("{0}")]
    Markdown(#[from] markdown::MarkdownError),
}

type Result<T> = std::result::Result<T, PdfError>;

// --- Documento que consume la plantilla Typst (build/data.json) ---

#[derive(Serialize)]
struct DataDoc {
    workspace: WorkspaceData,
    project: ProjectData,
    findings: Vec<FindingData>,
    severity_counts: SeverityCounts,
    /// Riesgos para el negocio: hallazgos agrupados por `categoria_riesgo`, en
    /// el orden de `CATEGORIAS_RIESGO`. Solo grupos con hallazgos.
    riesgos_negocio: Vec<RiesgoNegocioData>,
}

#[derive(Serialize)]
struct WorkspaceData {
    name: String,
    branding: Branding,
    watermark: Watermark,
}

#[derive(Serialize)]
struct ProjectData {
    name: String,
    client: String,
    /// Gerencia del cliente (opcional). La portada la muestra si no esta vacia.
    gerencia: String,
    /// Area del cliente (opcional). La portada la muestra si no esta vacia.
    area: String,
    /// Tipo de proyecto. Lo puede consumir la plantilla.
    project_type: String,
    /// OSID del candidato (tipos de examen). Va en la portada.
    osid: String,
    start_date: String,
    end_date: String,
    scope: Vec<String>,
    team: Vec<TeamMember>,
    sections: Vec<SectionData>,
    /// Orden/estructura del cuerpo: la plantilla recorre estos bloques y
    /// renderiza cada uno segun su kind. Se reconcilia (o se usa el default del
    /// tipo) en build_data.
    layout: Vec<ReportBlock>,
    /// Causas raiz con titulo, cada una con los numeros de los hallazgos
    /// visibles que apuntan a ella.
    causas_raiz: Vec<CausaRaizData>,
    /// Acciones con texto, ordenadas por prioridad (estable: dentro de una
    /// misma prioridad se conserva el orden del usuario).
    plan_remediacion: Vec<AccionData>,
}

#[derive(Serialize)]
struct CausaRaizData {
    clave: String,
    titulo: String,
    /// Texto plano (no markup): la plantilla lo inserta como string.
    descripcion: String,
    /// Numeros de hallazgo del reporte (1-based), en orden.
    hallazgos: Vec<usize>,
}

#[derive(Serialize)]
struct AccionData {
    /// Slug de `PRIORIDADES_REMEDIACION` ("" = sin especificar).
    prioridad: String,
    /// Etiqueta legible de la prioridad ("" si no tiene).
    prioridad_label: String,
    /// Texto plano.
    accion: String,
    /// Numeros de los hallazgos visibles que cierra, ascendentes.
    hallazgos: Vec<usize>,
    responsable: String,
}

#[derive(Serialize)]
struct RiesgoNegocioData {
    /// Slug de la categoria.
    categoria: String,
    /// Etiqueta legible de la categoria.
    etiqueta: String,
    hallazgos: Vec<RiesgoHallazgoData>,
}

#[derive(Serialize)]
struct RiesgoHallazgoData {
    number: usize,
    title: String,
    severity: String,
    impacto_negocio: String,
}

#[derive(Serialize)]
struct SectionData {
    key: String,
    title: String,
    /// Cuerpo ya convertido a markup de Typst.
    body: String,
}

#[derive(Serialize)]
struct FindingData {
    id: String,
    /// Numero del hallazgo en el reporte (1-based, sin contar los ocultos): el
    /// mismo que muestran el indice y el detalle.
    number: usize,
    title: String,
    severity: String,
    cvss: String,
    cvss_version: String,
    cvss_vector: String,
    cwe: String,
    status: String,
    affected: Vec<String>,
    /// Hallazgo nuevo detectado en el retest (la plantilla retest lo separa).
    new_in_retest: bool,
    /// Modulo o componente afectado (texto plano, puede venir vacio).
    modulo: String,
    /// Impacto para el negocio (texto plano, puede venir vacio).
    impacto_negocio: String,
    /// Slug de la categoria de riesgo ("" = sin especificar).
    categoria_riesgo: String,
    /// Clave de la causa raiz ("" = sin causa raiz).
    causa_raiz: String,
    /// Cuerpo ya convertido a markup de Typst.
    body: String,
}

#[derive(Serialize, Default)]
struct SeverityCounts {
    critical: u32,
    high: u32,
    medium: u32,
    low: u32,
    info: u32,
}

impl SeverityCounts {
    fn add(&mut self, severity: Severity) {
        match severity {
            Severity::Critical => self.critical += 1,
            Severity::High => self.high += 1,
            Severity::Medium => self.medium += 1,
            Severity::Low => self.low += 1,
            Severity::Info => self.info += 1,
        }
    }
}

fn severity_str(s: Severity) -> String {
    match s {
        Severity::Critical => "critical",
        Severity::High => "high",
        Severity::Medium => "medium",
        Severity::Low => "low",
        Severity::Info => "info",
    }
    .to_string()
}

fn version_str(v: crate::models::CvssVersion) -> String {
    match v {
        crate::models::CvssVersion::V31 => "3.1",
        crate::models::CvssVersion::V40 => "4.0",
    }
    .to_string()
}

fn status_str(s: crate::models::FindingStatus) -> String {
    serde_json::to_string(&s)
        .unwrap_or_default()
        .trim_matches('"')
        .to_string()
}

/// Prepara el layout para data.json: convierte el cuerpo de los bloques de
/// texto libre (markdown) a markup de Typst, igual que las secciones. El resto
/// de los bloques pasa tal cual.
fn layout_for_data(layout: Vec<ReportBlock>) -> Result<Vec<ReportBlock>> {
    layout
        .into_iter()
        .map(|mut b| {
            if b.kind == "text" {
                let body = b
                    .config
                    .get("body")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                if let Some(body) = body {
                    let typ = markdown::to_typst(&body)?;
                    b.config
                        .insert("body".to_string(), serde_json::Value::String(typ));
                }
            }
            Ok(b)
        })
        .collect()
}

/// Construye el documento de datos del proyecto a partir de los archivos.
fn build_data(root: &Path, project_id: &str) -> Result<DataDoc> {
    let ws = workspace::read_workspace_meta(root)?;
    let mut project = workspace::read_project_meta(root, project_id)?;
    // Layout efectivo del cuerpo: el guardado, o el default del tipo si esta
    // vacio. Deja la lista de bloques consistente con las secciones.
    project.reconcile_layout();
    // Los hallazgos ocultos no entran al PDF ni cuentan en los resumenes.
    let findings: Vec<_> = workspace::list_findings(root, project_id)?
        .into_iter()
        .filter(|f| !f.meta.hidden)
        .collect();

    let mut counts = SeverityCounts::default();
    let findings_data: Vec<FindingData> = findings
        .into_iter()
        .enumerate()
        .map(|(i, f)| -> Result<FindingData> {
            counts.add(f.meta.severity);
            // Campos ocultados puntualmente (sin ocultar el hallazgo entero):
            // "affected" se vacia (las plantillas ya omiten la linea si el
            // vector esta vacio) y las secciones del body se filtran antes
            // de convertir a markup de Typst.
            let hide = |key: &str| f.meta.hidden_fields.iter().any(|h| h == key);
            let affected = if hide("affected") {
                Vec::new()
            } else {
                f.meta.affected
            };
            let body = sections::strip_hidden_sections(&f.body, &f.meta.hidden_fields);
            Ok(FindingData {
                id: f.id,
                number: i + 1,
                title: f.meta.title,
                severity: severity_str(f.meta.severity),
                cvss: f.meta.cvss,
                cvss_version: version_str(f.meta.cvss_version),
                cvss_vector: f.meta.cvss_vector,
                // La plantilla muestra los CWE en un chip; se unen por coma.
                cwe: f.meta.cwe.join(", "),
                status: status_str(f.meta.status),
                affected,
                new_in_retest: f.meta.new_in_retest,
                modulo: f.meta.modulo.trim().to_string(),
                impacto_negocio: f.meta.impacto_negocio.trim().to_string(),
                categoria_riesgo: f.meta.categoria_riesgo,
                causa_raiz: f.meta.causa_raiz,
                body: markdown::to_typst(&body)?,
            })
        })
        .collect::<Result<_>>()?;

    let causas_raiz = root_causes_data(&project, &findings_data);
    let plan_remediacion = remediation_plan_data(&project, &findings_data);
    let riesgos_negocio = business_risks_data(&findings_data);

    // Solo se incluyen las secciones activadas.
    let sections = project
        .sections
        .into_iter()
        .filter(|s| s.enabled)
        .map(|s| {
            Ok(SectionData {
                body: markdown::to_typst(&s.body)?,
                key: s.key,
                title: s.title,
            })
        })
        .collect::<Result<_>>()?;

    Ok(DataDoc {
        workspace: WorkspaceData {
            name: ws.name,
            branding: ws.branding,
            watermark: ws.watermark,
        },
        project: ProjectData {
            name: project.name,
            client: project.client,
            gerencia: project.gerencia,
            area: project.area,
            project_type: project.project_type,
            osid: project.osid,
            start_date: project.start_date,
            end_date: project.end_date,
            scope: project
                .scope
                .into_iter()
                .map(|s| markdown::to_typst(&s).map_err(PdfError::from))
                .collect::<Result<_>>()?,
            team: project.team,
            sections,
            layout: layout_for_data(project.layout)?,
            causas_raiz,
            plan_remediacion,
        },
        findings: findings_data,
        severity_counts: counts,
        riesgos_negocio,
    })
}

/// Causas raiz para el PDF: solo las que tienen titulo, con los numeros de los
/// hallazgos visibles que las referencian.
fn root_causes_data(project: &ProjectMeta, findings: &[FindingData]) -> Vec<CausaRaizData> {
    project
        .causas_raiz
        .iter()
        .filter(|c| !c.titulo.trim().is_empty())
        .map(|c| CausaRaizData {
            clave: c.clave.clone(),
            titulo: c.titulo.trim().to_string(),
            descripcion: c.descripcion.trim().to_string(),
            hallazgos: findings
                .iter()
                .filter(|f| f.causa_raiz == c.clave)
                .map(|f| f.number)
                .collect(),
        })
        .collect()
}

/// Plan de remediacion para el PDF: solo acciones con texto, ordenadas por
/// prioridad (orden estable), con los ids traducidos a los numeros del
/// reporte. Los hallazgos ocultos no tienen numero y no se listan.
fn remediation_plan_data(project: &ProjectMeta, findings: &[FindingData]) -> Vec<AccionData> {
    let mut plan: Vec<AccionData> = project
        .plan_remediacion
        .iter()
        .filter(|a| !a.accion.trim().is_empty())
        .map(|a| {
            let mut hallazgos: Vec<usize> = a
                .hallazgos
                .iter()
                .filter_map(|id| findings.iter().find(|f| &f.id == id).map(|f| f.number))
                .collect();
            hallazgos.sort_unstable();
            hallazgos.dedup();
            AccionData {
                prioridad: a.prioridad.clone(),
                prioridad_label: prioridad_label(&a.prioridad).to_string(),
                accion: a.accion.trim().to_string(),
                hallazgos,
                responsable: a.responsable.trim().to_string(),
            }
        })
        .collect();
    plan.sort_by_key(|a| prioridad_rank(&a.prioridad));
    plan
}

/// Riesgos para el negocio: hallazgos visibles con categoria, agrupados en el
/// orden de `CATEGORIAS_RIESGO` y, dentro de cada grupo, en el del reporte.
fn business_risks_data(findings: &[FindingData]) -> Vec<RiesgoNegocioData> {
    CATEGORIAS_RIESGO
        .iter()
        .filter(|c| !c.is_empty())
        .filter_map(|categoria| {
            let hallazgos: Vec<RiesgoHallazgoData> = findings
                .iter()
                .filter(|f| f.categoria_riesgo == *categoria)
                .map(|f| RiesgoHallazgoData {
                    number: f.number,
                    title: f.title.clone(),
                    severity: f.severity.clone(),
                    impacto_negocio: f.impacto_negocio.clone(),
                })
                .collect();
            (!hallazgos.is_empty()).then(|| RiesgoNegocioData {
                categoria: categoria.to_string(),
                etiqueta: categoria_riesgo_label(categoria).to_string(),
                hallazgos,
            })
        })
        .collect()
}

/// Resuelve la plantilla .typ activa: primero la libreria del usuario, luego
/// las plantillas base empaquetadas con la app.
fn resolve_template(root: &Path, templates_dir: &Path, name: &str) -> Result<PathBuf> {
    // Defensa contra traversal: la plantilla activa viene de workspace.yaml,
    // que podria ser de un tercero. Solo se aceptan nombres simples.
    if name.is_empty() || name.contains('/') || name.contains('\\') || name.contains("..") {
        return Err(PdfError::TemplateNotFound(name.to_string()));
    }
    let user = root.join("library/templates").join(format!("{name}.typ"));
    if user.exists() {
        return Ok(user);
    }
    let builtin = templates_dir.join(format!("{name}.typ"));
    if builtin.exists() {
        return Ok(builtin);
    }
    Err(PdfError::TemplateNotFound(name.to_string()))
}

/// Resuelve el binario de Typst: variable de entorno, sidecar junto al
/// ejecutable, o `typst` en el PATH.
pub fn resolve_typst() -> Result<PathBuf> {
    if let Ok(path) = std::env::var("PUDU_TYPST_BIN") {
        let p = PathBuf::from(path);
        if p.exists() {
            return Ok(p);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            // En desarrollo los sidecars conservan el sufijo del target triple.
            if let Some(suffix) = exe
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| name.strip_prefix("pudureport-mcp-"))
            {
                let sibling = dir.join(format!("typst-{suffix}"));
                if sibling.is_file() {
                    return Ok(sibling);
                }
            }
            for candidate in ["typst", "typst.exe"] {
                let p = dir.join(candidate);
                if p.exists() {
                    return Ok(p);
                }
            }
        }
    }
    // Fallback al PATH (resuelto por el SO al ejecutar).
    Ok(PathBuf::from("typst"))
}

/// Serializa data.json y copia la plantilla activa al directorio build.
/// Devuelve (build_dir, report.typ) listos para compilar.
fn prepare_build(
    root: &Path,
    project_id: &str,
    templates_dir: &Path,
    template_name: &str,
) -> Result<(PathBuf, PathBuf)> {
    let data = build_data(root, project_id)?;
    let build_dir = root.join(project_id).join("build");
    std::fs::create_dir_all(&build_dir)?;

    let json = serde_json::to_string_pretty(&data)?;
    std::fs::write(build_dir.join("data.json"), json)?;

    let template = resolve_template(root, templates_dir, template_name)?;
    let report_typ = build_dir.join("report.typ");
    std::fs::copy(&template, &report_typ)?;

    // Copiar el tema de color para bloques de codigo (lo usan las plantillas via
    // `set raw(theme: "code-dark.tmTheme")`); debe quedar junto a report.typ.
    let theme = templates_dir.join("code-dark.tmTheme");
    if theme.exists() {
        std::fs::copy(&theme, build_dir.join("code-dark.tmTheme"))?;
    }

    // Copiar la libreria de diseno compartida (sev-color, badges, chips...) que
    // las plantillas importan con `#import "theme.typ": *`; debe quedar junto
    // a report.typ igual que code-dark.tmTheme, sin importar si la plantilla
    // activa es builtin o un override en library/templates.
    let design_theme = templates_dir.join("theme.typ");
    if design_theme.exists() {
        std::fs::copy(&design_theme, build_dir.join("theme.typ"))?;
    }

    // Copiar los assets del proyecto junto a report.typ para que las imagenes
    // referenciadas como "assets/<uuid>.png" en markdown resuelvan en Typst.
    let src_assets = root.join(project_id).join("assets");
    if src_assets.is_dir() {
        let dst_assets = build_dir.join("assets");
        std::fs::create_dir_all(&dst_assets)?;
        for entry in std::fs::read_dir(&src_assets)? {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                std::fs::copy(entry.path(), dst_assets.join(entry.file_name()))?;
            }
        }
    }

    Ok((build_dir, report_typ))
}

/// Variables de entorno que hereda Typst. Todo lo demas se descarta (por
/// ejemplo `PUDUREPORT_SERVICE_KEY` del servicio): Typst solo necesita
/// encontrar fuentes y un temporal. En Windows se conservan las variables del
/// sistema sin las que un proceso no arranca bien ni encuentra las fuentes.
const TYPST_ENV_ALLOWLIST: &[&str] = &[
    "PATH",
    "HOME",
    "TMPDIR",
    "TMP",
    "TEMP",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TYPST_FONT_PATHS",
    "SOURCE_DATE_EPOCH",
    "XDG_DATA_HOME",
    "XDG_DATA_DIRS",
    "XDG_CONFIG_HOME",
    "XDG_CACHE_HOME",
    "FONTCONFIG_FILE",
    "FONTCONFIG_PATH",
    "SYSTEMROOT",
    "WINDIR",
    "LOCALAPPDATA",
    "APPDATA",
    "USERPROFILE",
];

/// Filtra un entorno a las variables de `TYPST_ENV_ALLOWLIST`.
fn typst_env(
    vars: impl IntoIterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    vars.into_iter()
        .filter(|(name, _)| {
            let Some(name) = name.to_str() else {
                return false;
            };
            TYPST_ENV_ALLOWLIST.iter().any(|allowed| {
                // En Windows los nombres de variables no distinguen mayusculas.
                if cfg!(windows) {
                    allowed.eq_ignore_ascii_case(name)
                } else {
                    *allowed == name
                }
            })
        })
        .collect()
}

/// Comando para ejecutar Typst con el entorno vacio salvo
/// `TYPST_ENV_ALLOWLIST`, para que ningun secreto del proceso padre (como la
/// clave del servicio) llegue al proceso hijo.
pub fn typst_command(typst_bin: &Path) -> Command {
    let mut cmd = Command::new(typst_bin);
    cmd.env_clear().envs(typst_env(std::env::vars_os()));
    cmd
}

/// Ejecuta `typst compile` mapeando el error de binario ausente. Con
/// `timeout`, mata el proceso si no termina a tiempo, y con `jobs` limita los
/// hilos de Typst (los usa el servicio; la app de escritorio no pone limite).
fn run_typst(
    typst_bin: &Path,
    root: &Path,
    report_typ: &Path,
    out: &Path,
    ppi: Option<u32>,
    timeout: Option<Duration>,
    jobs: Option<u32>,
) -> Result<()> {
    let mut cmd = typst_command(typst_bin);
    // En Windows, spawnear el sidecar abre una consola que parpadea en cada
    // compilacion; CREATE_NO_WINDOW la suprime. No-op en otras plataformas.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    cmd.arg("compile").arg("--root").arg(root);
    if let Some(jobs) = jobs {
        cmd.arg("--jobs").arg(jobs.max(1).to_string());
    }
    if let Some(ppi) = ppi {
        cmd.arg("--format")
            .arg("png")
            .arg("--ppi")
            .arg(ppi.to_string());
    }
    let mut child = cmd
        .arg(report_typ)
        .arg(out)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                PdfError::TypstNotFound
            } else {
                PdfError::Io(e)
            }
        })?;
    // stderr se lee en otro hilo para que el proceso no se bloquee si llena
    // el pipe; acotado a 1 MiB.
    let stderr = child.stderr.take();
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        if let Some(pipe) = stderr {
            let _ = pipe.take(1024 * 1024).read_to_string(&mut text);
        }
        text
    });
    let status = match timeout {
        None => child.wait()?,
        Some(limit) => {
            let deadline = Instant::now() + limit;
            loop {
                if let Some(status) = child.try_wait()? {
                    break status;
                }
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    // El hilo lector no se espera: termina solo cuando se
                    // cierra el pipe (ya sin proceso que lo bloquee).
                    drop(reader);
                    return Err(PdfError::Timeout(limit.as_secs()));
                }
                std::thread::sleep(Duration::from_millis(25));
            }
        }
    };
    let stderr = reader.join().unwrap_or_default();
    if !status.success() {
        return Err(PdfError::Compile(compile_error_summary(&stderr)));
    }
    Ok(())
}

/// Se queda solo con el diagnostico real de Typst (a partir de la primera
/// linea "error:"), descartando los warnings previos (tipicamente de fuentes
/// no instaladas) que no aportan al diagnostico del usuario.
fn compile_error_summary(stderr: &str) -> String {
    let lines: Vec<&str> = stderr.lines().collect();
    match lines
        .iter()
        .position(|l| l.trim_start().starts_with("error:"))
    {
        Some(pos) => lines[pos..].join("\n"),
        None => stderr.to_string(),
    }
}

/// Deja solo caracteres seguros para un nombre de archivo; cae a "XXXXX" vacio.
fn sanitize_osid(osid: &str) -> String {
    let s: String = osid
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    if s.is_empty() {
        "XXXXX".to_string()
    } else {
        s
    }
}

/// Plantilla .typ efectiva del proyecto: el override si existe, si no la del tipo.
fn effective_template(project: &ProjectMeta) -> String {
    if project.template_override.is_empty() {
        crate::models::template_for_type(&project.project_type).to_string()
    } else {
        project.template_override.clone()
    }
}

/// Nombre del PDF generado. Los tipos de examen siguen la convencion de
/// submission (OSCP-OS-<OSID>-Exam-Report.pdf); el resto usa el nombre
/// estandar `{Cliente}-{Tipo}-{fecha}` (ver `naming.rs`).
fn report_filename(project: &ProjectMeta) -> String {
    match project.project_type.as_str() {
        "oscp" => format!("OSCP-OS-{}-Exam-Report.pdf", sanitize_osid(&project.osid)),
        "htb" => format!("HTB-{}-Exam-Report.pdf", sanitize_osid(&project.osid)),
        _ => format!("{}.pdf", naming::standard_basename(project)),
    }
}

/// Genera el PDF del proyecto. Si `also_executive` es true y la plantilla no es
/// ya la ejecutiva, genera ademas un segundo PDF con la plantilla ejecutiva a
/// partir de los mismos datos. Devuelve las rutas producidas (principal primero).
pub fn generate_pdf(
    root: &Path,
    project_id: &str,
    templates_dir: &Path,
    typst_bin: &Path,
    also_executive: bool,
) -> Result<Vec<PathBuf>> {
    let project = workspace::read_project_meta(root, project_id)?;
    let template = effective_template(&project);
    let (build_dir, report_typ) = prepare_build(root, project_id, templates_dir, &template)?;
    let primary = build_dir.join(report_filename(&project));
    run_typst(typst_bin, root, &report_typ, &primary, None, None, None)?;
    let mut out = vec![primary];

    if also_executive && template != "ejecutivo" {
        // Reutiliza el data.json ya escrito; solo cambia la plantilla.
        let exec_src = resolve_template(root, templates_dir, "ejecutivo")?;
        let exec_typ = build_dir.join("report-ejecutivo.typ");
        std::fs::copy(&exec_src, &exec_typ)?;
        let exec_pdf = build_dir.join(format!(
            "{}-ejecutivo.pdf",
            naming::standard_basename(&project)
        ));
        run_typst(typst_bin, root, &exec_typ, &exec_pdf, None, None, None)?;
        out.push(exec_pdf);
    }
    Ok(out)
}

/// Plantillas base incluidas en el binario: (nombre de archivo, fuente).
/// Son las mismas de `templates/` que empaqueta la app de escritorio.
pub const BUNDLED_TEMPLATES: &[(&str, &str)] = &[
    ("pentest.typ", include_str!("../../templates/pentest.typ")),
    ("oscp.typ", include_str!("../../templates/oscp.typ")),
    ("htb.typ", include_str!("../../templates/htb.typ")),
    (
        "ejecutivo.typ",
        include_str!("../../templates/ejecutivo.typ"),
    ),
    (
        "documento-libre.typ",
        include_str!("../../templates/documento-libre.typ"),
    ),
    ("retest.typ", include_str!("../../templates/retest.typ")),
    ("cti.typ", include_str!("../../templates/cti.typ")),
    (
        "incidente.typ",
        include_str!("../../templates/incidente.typ"),
    ),
    ("theme.typ", include_str!("../../templates/theme.typ")),
    (
        "code-dark.tmTheme",
        include_str!("../../templates/code-dark.tmTheme"),
    ),
];

/// Nombres (sin extension) de las plantillas de reporte incluidas; excluye
/// la libreria de diseno compartida `theme`.
pub fn bundled_template_names() -> Vec<&'static str> {
    BUNDLED_TEMPLATES
        .iter()
        .filter_map(|(file, _)| file.strip_suffix(".typ"))
        .filter(|name| *name != "theme")
        .collect()
}

/// Escribe las plantillas incluidas en un directorio temporal unico, ejecuta
/// `f` con esa ruta y lo borra al terminar.
fn with_bundled_templates<T>(f: impl FnOnce(&Path) -> Result<T>) -> Result<T> {
    let templates = std::env::temp_dir().join(format!("pudu-templates-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&templates)?;
    let result = (|| {
        for (name, source) in BUNDLED_TEMPLATES {
            std::fs::write(templates.join(name), source)?;
        }
        f(&templates)
    })();
    // Solo borra el directorio temporal unico creado por esta llamada.
    let _ = std::fs::remove_dir_all(&templates);
    result
}

/// Compila con las mismas plantillas base incluidas, sin depender de Tauri.
/// Los overrides del workspace conservan prioridad. Solo devuelve rutas locales.
pub fn generate_pdf_bundled(
    root: &Path,
    project_id: &str,
    typst_bin: &Path,
) -> Result<Vec<PathBuf>> {
    workspace::read_project_meta(root, project_id)?;
    with_bundled_templates(|templates| generate_pdf(root, project_id, templates, typst_bin, false))
}

/// Opciones de un render con las plantillas incluidas (modo servicio).
#[derive(Debug, Clone, Default)]
pub struct RenderOptions {
    /// Plantilla a usar en lugar de la efectiva del proyecto. Solo se aceptan
    /// nombres de `bundled_template_names()`.
    pub template: Option<String>,
    /// Tiempo maximo de Typst; al vencer se mata el proceso.
    pub timeout: Option<Duration>,
    /// Hilos de Typst (`--jobs`). `None` usa el defecto de Typst (un hilo
    /// por CPU).
    pub jobs: Option<u32>,
}

/// PDF generado por `render_bundled`.
#[derive(Debug, Clone)]
pub struct RenderedPdf {
    /// Ruta del PDF dentro de `<proyecto>/build/`.
    pub path: PathBuf,
    /// Plantilla usada (sin extension).
    pub template: String,
}

/// Genera el PDF principal con las plantillas incluidas, con plantilla
/// opcional y limite de tiempo. Lo usa `pudureport-service`.
pub fn render_bundled(
    root: &Path,
    project_id: &str,
    typst_bin: &Path,
    options: &RenderOptions,
) -> Result<RenderedPdf> {
    let project = workspace::read_project_meta(root, project_id)?;
    let template = match options.template.as_deref().filter(|t| !t.is_empty()) {
        Some(name) if bundled_template_names().contains(&name) => name.to_string(),
        Some(name) => return Err(PdfError::TemplateNotFound(name.to_string())),
        None => effective_template(&project),
    };
    with_bundled_templates(|templates| {
        let (build_dir, report_typ) = prepare_build(root, project_id, templates, &template)?;
        let path = build_dir.join(report_filename(&project));
        run_typst(
            typst_bin,
            root,
            &report_typ,
            &path,
            None,
            options.timeout,
            options.jobs,
        )?;
        Ok(RenderedPdf {
            path,
            template: template.clone(),
        })
    })
}

/// Renderiza el PDF a PNG por pagina y los devuelve como data URLs base64,
/// para mostrar la vista previa dentro de la app (sin escribir a disco final).
pub fn preview_pdf(
    root: &Path,
    project_id: &str,
    templates_dir: &Path,
    typst_bin: &Path,
) -> Result<Vec<String>> {
    use base64::Engine;

    let project = workspace::read_project_meta(root, project_id)?;
    let template = effective_template(&project);
    let (build_dir, report_typ) = prepare_build(root, project_id, templates_dir, &template)?;
    let preview_dir = build_dir.join("preview");
    std::fs::create_dir_all(&preview_dir)?;
    // Limpiar PNGs previos para no mezclar paginas viejas.
    if let Ok(entries) = std::fs::read_dir(&preview_dir) {
        for entry in entries.flatten() {
            if entry.path().extension().is_some_and(|e| e == "png") {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }

    // Typst sustituye {p} por el numero de pagina (1-based).
    let pattern = preview_dir.join("page-{0p}.png");
    run_typst(
        typst_bin,
        root,
        &report_typ,
        &pattern,
        Some(120),
        None,
        None,
    )?;

    // Recolectar las paginas en orden.
    let mut pages: Vec<PathBuf> = std::fs::read_dir(&preview_dir)?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "png"))
        .collect();
    pages.sort();

    let engine = base64::engine::general_purpose::STANDARD;
    let mut out = Vec::new();
    for page in pages {
        let bytes = std::fs::read(&page)?;
        out.push(format!("data:image/png;base64,{}", engine.encode(&bytes)));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_fields_strip_affected_and_poc() {
        let tmp = std::env::temp_dir().join(format!("pudu-pdf-hidden-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        workspace::create_workspace(&tmp, "WS").unwrap();
        let (pid, _) = workspace::create_project(&tmp, "Web", "ACME", "pentest").unwrap();
        let mut finding = workspace::create_finding(&tmp, &pid, "SQLi").unwrap();
        finding.meta.affected = vec!["https://app.example/login".to_string()];
        finding.meta.hidden_fields = vec!["affected".to_string(), "poc".to_string()];
        finding.body = "## Descripcion\n\nTexto.\n\n## Prueba de concepto\n\nPayload secreto.\n\n## Remediacion\n\nArreglar.".to_string();
        workspace::write_finding(&tmp, &pid, &finding).unwrap();

        let data = build_data(&tmp, &pid).unwrap();
        let f = &data.findings[0];
        assert!(f.affected.is_empty());
        assert!(!f.body.contains("Payload secreto"));
        assert!(f.body.contains("Texto"));
        assert!(f.body.contains("Arreglar"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn build_data_serializes() {
        let tmp = std::env::temp_dir().join(format!("pudu-pdf-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        workspace::create_workspace(&tmp, "WS").unwrap();
        let (pid, _) = workspace::create_project(&tmp, "Web", "ACME", "pentest").unwrap();
        workspace::create_finding(&tmp, &pid, "SQLi").unwrap();

        let data = build_data(&tmp, &pid).unwrap();
        assert_eq!(data.project.client, "ACME");
        assert_eq!(data.findings.len(), 1);
        assert!(serde_json::to_string(&data).is_ok());
        // El cuerpo se serializa como layout de bloques, arrancando por la portada.
        assert_eq!(
            data.project.layout.first().map(|b| b.kind.as_str()),
            Some("cover")
        );
        assert!(data.project.layout.iter().any(|b| b.kind == "findings"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn build_data_incluye_la_parte_ejecutiva_con_la_numeracion_del_reporte() {
        use crate::models::{AccionRemediacion, CausaRaiz};

        let tmp = std::env::temp_dir().join(format!("pudu-pdf-exec-{}", uuid::Uuid::new_v4()));
        workspace::create_workspace(&tmp, "WS").unwrap();
        let (pid, mut meta) = workspace::create_project(&tmp, "Web", "ACME", "pentest").unwrap();
        meta.causas_raiz = vec![
            CausaRaiz {
                clave: "validacion".into(),
                titulo: "Validacion de entradas".into(),
                descripcion: "Solo en el cliente.".into(),
            },
            // Sin titulo: no sale en el PDF.
            CausaRaiz {
                clave: "vacia".into(),
                ..Default::default()
            },
        ];
        workspace::write_project_meta(&tmp, &pid, &meta).unwrap();

        // Tres hallazgos; el segundo oculto (no tiene numero en el reporte).
        let mut ids = Vec::new();
        for (title, hidden, categoria, causa) in [
            ("SQLi", false, "fuga_datos", "validacion"),
            ("Oculto", true, "fuga_datos", "validacion"),
            ("XSS", false, "reputacional", "validacion"),
        ] {
            let mut f = workspace::create_finding(&tmp, &pid, title).unwrap();
            f.meta.hidden = hidden;
            f.meta.categoria_riesgo = categoria.into();
            f.meta.causa_raiz = causa.into();
            f.meta.modulo = " Login ".into();
            f.meta.impacto_negocio = format!("Impacto de {title}");
            workspace::write_finding(&tmp, &pid, &f).unwrap();
            ids.push(f.id);
        }

        let mut meta = workspace::read_project_meta(&tmp, &pid).unwrap();
        meta.plan_remediacion = vec![
            AccionRemediacion {
                prioridad: "mediano".into(),
                accion: "Agregar cabeceras".into(),
                hallazgos: vec![ids[2].clone()],
                responsable: "Infra".into(),
            },
            // Sin accion: no sale en el PDF.
            AccionRemediacion {
                prioridad: "inmediata".into(),
                ..Default::default()
            },
            AccionRemediacion {
                prioridad: "inmediata".into(),
                accion: "Parametrizar consultas".into(),
                hallazgos: vec![ids[2].clone(), ids[1].clone(), ids[0].clone()],
                responsable: String::new(),
            },
        ];
        workspace::write_project_meta(&tmp, &pid, &meta).unwrap();

        let data = build_data(&tmp, &pid).unwrap();
        // Numeracion del reporte: sin contar los ocultos.
        let numbers: Vec<(usize, &str)> = data
            .findings
            .iter()
            .map(|f| (f.number, f.title.as_str()))
            .collect();
        assert_eq!(numbers, vec![(1, "SQLi"), (2, "XSS")]);
        assert_eq!(data.findings[0].modulo, "Login");
        assert_eq!(data.findings[1].categoria_riesgo, "reputacional");

        // Causas raiz: solo con titulo y con los numeros visibles.
        assert_eq!(data.project.causas_raiz.len(), 1);
        assert_eq!(data.project.causas_raiz[0].hallazgos, vec![1, 2]);

        // Plan: sin acciones vacias, ordenado por prioridad y con numeros.
        let plan: Vec<(&str, &[usize])> = data
            .project
            .plan_remediacion
            .iter()
            .map(|a| (a.prioridad.as_str(), a.hallazgos.as_slice()))
            .collect();
        assert_eq!(
            plan,
            vec![("inmediata", &[1, 2][..]), ("mediano", &[2][..])]
        );
        assert_eq!(
            data.project.plan_remediacion[0].prioridad_label,
            "Inmediata"
        );

        // Riesgos agrupados por categoria, en el orden de la lista cerrada.
        let groups: Vec<(&str, Vec<usize>)> = data
            .riesgos_negocio
            .iter()
            .map(|g| {
                (
                    g.etiqueta.as_str(),
                    g.hallazgos.iter().map(|h| h.number).collect(),
                )
            })
            .collect();
        assert_eq!(
            groups,
            vec![
                ("Fuga de datos personales", vec![1]),
                ("Reputacional", vec![2])
            ]
        );

        // El JSON expone los nombres que usan las plantillas.
        let json = serde_json::to_value(&data).unwrap();
        assert_eq!(json["findings"][0]["number"], 1);
        assert_eq!(json["findings"][0]["impacto_negocio"], "Impacto de SQLi");
        assert_eq!(json["project"]["plan_remediacion"][0]["hallazgos"][1], 2);
        assert_eq!(json["riesgos_negocio"][1]["categoria"], "reputacional");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn build_data_sin_datos_ejecutivos_deja_las_listas_vacias() {
        let tmp = std::env::temp_dir().join(format!("pudu-pdf-noexec-{}", uuid::Uuid::new_v4()));
        workspace::create_workspace(&tmp, "WS").unwrap();
        let (pid, _) = workspace::create_project(&tmp, "Web", "ACME", "pentest").unwrap();
        workspace::create_finding(&tmp, &pid, "SQLi").unwrap();
        let data = build_data(&tmp, &pid).unwrap();
        assert!(data.project.causas_raiz.is_empty());
        assert!(data.project.plan_remediacion.is_empty());
        assert!(data.riesgos_negocio.is_empty());
        assert_eq!(data.findings[0].modulo, "");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn compile_error_summary_drops_font_warnings() {
        let stderr = "warning: unknown font family: helvetica neue\n  ┌─ report.typ:107:19\n  │\n107 │ #set text(font: body-font)\n  │                   ^^^^^^^^^\n\nerror: unclosed delimiter\n  ┌─ report.typ:422:7\n  │\n422 │       eval(f.body, mode: \"markup\")\n  │       ^^^^\n";
        let summary = compile_error_summary(stderr);
        assert!(!summary.contains("warning:"));
        assert!(summary.starts_with("error: unclosed delimiter"));
    }

    #[test]
    fn compile_error_summary_keeps_stderr_without_error_line() {
        let stderr = "warning: unknown font family: helvetica neue\n";
        assert_eq!(compile_error_summary(stderr), stderr);
    }

    #[cfg(unix)]
    #[test]
    fn run_typst_kills_process_after_timeout() {
        use std::os::unix::fs::PermissionsExt;

        // Un "typst" falso que nunca termina a tiempo.
        let dir = std::env::temp_dir().join(format!("pudu-typst-lento-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let fake = dir.join("typst");
        std::fs::write(&fake, "#!/bin/sh\nexec sleep 30\n").unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

        let started = Instant::now();
        let err = run_typst(
            &fake,
            &dir,
            &dir.join("report.typ"),
            &dir.join("out.pdf"),
            None,
            Some(Duration::from_millis(300)),
            None,
        )
        .unwrap_err();
        assert!(matches!(err, PdfError::Timeout(_)), "{err}");
        assert!(started.elapsed() < Duration::from_secs(10));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn typst_env_keeps_only_the_allowlist() {
        let vars = [
            ("PATH", "/usr/bin"),
            ("HOME", "/home/pudu"),
            ("TYPST_FONT_PATHS", "/fuentes"),
            ("PUDUREPORT_SERVICE_KEY", "secreto"),
            ("PUDUREPORT_SERVICE_KEY_FILE", "/run/secrets/clave"),
            ("AWS_SECRET_ACCESS_KEY", "otro"),
        ]
        .map(|(k, v)| (k.into(), v.into()));
        let mut kept: Vec<String> = typst_env(vars)
            .into_iter()
            .map(|(k, _)| k.to_string_lossy().to_string())
            .collect();
        kept.sort();
        assert_eq!(kept, ["HOME", "PATH", "TYPST_FONT_PATHS"]);
    }

    #[cfg(unix)]
    #[test]
    fn run_typst_clears_env_and_passes_jobs() {
        use std::os::unix::fs::PermissionsExt;

        // Un "typst" falso que vuelca su entorno y sus argumentos.
        let dir = std::env::temp_dir().join(format!("pudu-typst-env-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let fake = dir.join("typst");
        let script = format!(
            "#!/bin/sh\n/usr/bin/env > '{0}/env.txt'\necho \"$@\" > '{0}/args.txt'\n",
            dir.display()
        );
        std::fs::write(&fake, script).unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

        run_typst(
            &fake,
            &dir,
            &dir.join("report.typ"),
            &dir.join("out.pdf"),
            None,
            Some(Duration::from_secs(20)),
            Some(1),
        )
        .unwrap();
        let env = std::fs::read_to_string(dir.join("env.txt")).unwrap();
        // Variables que agrega el propio shell, no el proceso padre.
        let shell = ["PWD", "OLDPWD", "SHLVL", "_"];
        for line in env.lines() {
            let name = line.split('=').next().unwrap_or_default();
            assert!(
                TYPST_ENV_ALLOWLIST.contains(&name) || shell.contains(&name),
                "variable heredada fuera de la lista: {name}"
            );
        }
        let args = std::fs::read_to_string(dir.join("args.txt")).unwrap();
        assert!(args.contains("--jobs 1"), "{args}");

        // Sin `jobs` (app de escritorio) no se agrega el argumento.
        run_typst(
            &fake,
            &dir,
            &dir.join("report.typ"),
            &dir.join("out.pdf"),
            None,
            None,
            None,
        )
        .unwrap();
        let args = std::fs::read_to_string(dir.join("args.txt")).unwrap();
        assert!(!args.contains("--jobs"), "{args}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn build_data_rejects_amplified_markdown() {
        let tmp = std::env::temp_dir().join(format!("pudu-pdf-md-{}", uuid::Uuid::new_v4()));
        workspace::create_workspace(&tmp, "WS").unwrap();
        let (pid, _) = workspace::create_project(&tmp, "Web", "ACME", "pentest").unwrap();
        let mut finding = workspace::create_finding(&tmp, &pid, "Tabla").unwrap();
        finding.body = format!(
            "{}|\n{}|\n{}",
            "|a".repeat(5_000),
            "|-".repeat(5_000),
            "x\n".repeat(5_000)
        );
        workspace::write_finding(&tmp, &pid, &finding).unwrap();
        let err = build_data(&tmp, &pid).err().expect("debia fallar");
        assert!(matches!(err, PdfError::Markdown(_)), "{err}");
        assert!(err.to_string().contains("demasiado markup"), "{err}");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn render_bundled_rejects_unknown_template() {
        let tmp = std::env::temp_dir().join(format!("pudu-pdf-tpl-{}", uuid::Uuid::new_v4()));
        workspace::create_workspace(&tmp, "WS").unwrap();
        let (pid, _) = workspace::create_project(&tmp, "Web", "ACME", "pentest").unwrap();
        for bad in ["../pentest", "theme", "no-existe"] {
            let options = RenderOptions {
                template: Some(bad.to_string()),
                ..Default::default()
            };
            let err = render_bundled(&tmp, &pid, Path::new("typst"), &options).unwrap_err();
            assert!(matches!(err, PdfError::TemplateNotFound(_)), "{bad}: {err}");
        }
        assert!(bundled_template_names().contains(&"pentest"));
        assert!(!bundled_template_names().contains(&"theme"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Devuelve true si hay un binario de Typst ejecutable disponible.
    fn typst_available(bin: &Path) -> bool {
        Command::new(bin)
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    #[test]
    fn generate_pdf_end_to_end() {
        // Integracion del pipeline completo: archivos -> data.json -> Typst -> PDF.
        let templates = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("templates");
        let typst_bin = resolve_typst().unwrap();
        if !typst_available(&typst_bin) {
            eprintln!("Typst no disponible; se omite el test de integracion de PDF");
            return;
        }

        let tmp = std::env::temp_dir().join(format!("pudu-pdfgen-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        workspace::create_workspace(&tmp, "WS").unwrap();
        let (pid, _) = workspace::create_project(&tmp, "Pentest", "ACME", "pentest").unwrap();

        let f = workspace::create_finding(&tmp, &pid, "SQL Injection en login").unwrap();
        let mut finding = f;
        finding.meta.severity = Severity::Critical;
        finding.meta.cvss = "9.8".into();
        finding.body =
            "## Descripcion\n\nEl parametro **user** es vulnerable.\n\n```sql\nSELECT 1\n```"
                .into();
        finding
            .body
            .push_str("\n\n| IP | Host | Servicio |\n|:---|:---:|---:|\n");
        for row in 1..=100 {
            finding
                .body
                .push_str(&format!("| 192.0.2.{row} | **host-{row}** | `https` |\n"));
        }
        finding.body.push_str("\n\n## Prueba de concepto\n\nEvidencia reproducible en `192.0.2.10` con resultados documentados.\n\n```text\n");
        for row in 1..=90 {
            finding.body.push_str(&format!(
                "EVIDENCIA-{row:03} respuesta registrada sin cambios en el sistema\n"
            ));
        }
        finding
            .body
            .push_str("```\n\n## Remediacion\n\nAplicar controles y verificar el resultado.\n");
        finding.meta.affected = vec!["192.0.2.10:443".into(), "Servicio de ejemplo con descripcion extensa para verificar legibilidad sin convertir toda la frase en codigo".into()];
        workspace::write_finding(&tmp, &pid, &finding).unwrap();

        // Gerencia y area pobladas: ejercita la linea opcional de la portada
        // en todas las plantillas (la rama que las muestra).
        let mut p = workspace::read_project_meta(&tmp, &pid).unwrap();
        p.gerencia = "Gerencia de Tecnologia".into();
        p.area = "Seguridad de la Informacion".into();
        p.sections[0].body = finding.body.clone();
        workspace::write_project_meta(&tmp, &pid, &p).unwrap();

        // Cada tipo de proyecto debe compilar con su plantilla derivada.
        for project_type in [
            "pentest",
            "redteam",
            "ejecutivo",
            "documento",
            "cti",
            "incidente",
            "retest",
            "oscp",
            "htb",
        ] {
            let mut project = workspace::read_project_meta(&tmp, &pid).unwrap();
            project.project_type = project_type.to_string();
            project.layout = Vec::new();
            workspace::write_project_meta(&tmp, &pid, &project).unwrap();

            let pdfs = generate_pdf(&tmp, &pid, &templates, &typst_bin, false)
                .unwrap_or_else(|e| panic!("fallo {project_type}: {e}"));
            // Artefactos opcionales para revision visual local, nunca datos de clientes.
            if let Some(dir) = std::env::var_os("PUDU_PDF_REVIEW_DIR") {
                let dir = PathBuf::from(dir).join(project_type);
                std::fs::create_dir_all(&dir).unwrap();
                std::fs::copy(&pdfs[0], dir.join("report.pdf")).unwrap();
            }
            let bytes = std::fs::metadata(&pdfs[0]).unwrap().len();
            assert!(
                bytes > 1000,
                "PDF de {project_type} sospechosamente pequeno"
            );
        }

        // Fuentes configuradas pero ausentes deben caer al respaldo tambien.
        let mut wsm = workspace::read_workspace_meta(&tmp).unwrap();
        wsm.branding.body_font = "Pudu Missing Body Font".into();
        wsm.branding.mono_font = "Pudu Missing Mono Font".into();
        workspace::write_workspace_meta(&tmp, &wsm).unwrap();

        // Layout custom por tipo: un bloque de texto libre (ejercita la conversion
        // markdown -> typst), un bloque desactivado y un reordenamiento. Debe
        // compilar en las 8 plantillas (oscp ignora el layout, pero igual compila).
        for project_type in [
            "pentest",
            "redteam",
            "ejecutivo",
            "documento",
            "cti",
            "incidente",
            "retest",
            "oscp",
            "htb",
        ] {
            let mut project = workspace::read_project_meta(&tmp, &pid).unwrap();
            project.project_type = project_type.to_string();
            let mut layout = crate::models::default_layout(project_type, &project.sections);

            let mut cfg = serde_json::Map::new();
            cfg.insert(
                "title".to_string(),
                serde_json::Value::String("Nota agregada".to_string()),
            );
            cfg.insert(
                "body".to_string(),
                serde_json::Value::String("Texto con *enfasis* y `codigo`.".to_string()),
            );
            let text_block = ReportBlock {
                kind: "text".to_string(),
                enabled: true,
                config: cfg,
            };

            // Insertar el texto despues de la portada y desactivar el bloque
            // siguiente; luego mover el ultimo bloque al frente (reordenamiento).
            let pos = usize::from(!layout.is_empty());
            layout.insert(pos, text_block);
            if layout.len() > pos + 1 {
                layout[pos + 1].enabled = false;
            }
            if layout.len() > 1 {
                let last = layout.remove(layout.len() - 1);
                layout.insert(0, last);
            }
            project.layout = layout;
            workspace::write_project_meta(&tmp, &pid, &project).unwrap();

            let pdfs = generate_pdf(&tmp, &pid, &templates, &typst_bin, false)
                .unwrap_or_else(|e| panic!("layout custom fallo {project_type}: {e}"));
            let bytes = std::fs::metadata(&pdfs[0]).unwrap().len();
            assert!(
                bytes > 1000,
                "PDF custom de {project_type} sospechosamente pequeno"
            );
        }

        // Restaurar el layout por defecto para las aserciones siguientes.
        let mut project = workspace::read_project_meta(&tmp, &pid).unwrap();
        project.layout = Vec::new();
        workspace::write_project_meta(&tmp, &pid, &project).unwrap();

        // Portada-lienzo (cover_layout = "canvas"): compila con elementos
        // posicionados en absoluto (place) en las plantillas con cover().
        let ce = |kind: &str, x: f64, y: f64, content: &str| crate::models::CoverElement {
            kind: kind.to_string(),
            x,
            y,
            w: 0.6,
            font_size: 16.0,
            align: "left".to_string(),
            color: String::new(),
            weight: "normal".to_string(),
            content: content.to_string(),
            src: String::new(),
        };
        let mut wsm = workspace::read_workspace_meta(&tmp).unwrap();
        wsm.branding.cover_layout = "canvas".to_string();
        wsm.branding.cover_elements = vec![
            ce("title", 0.1, 0.3, ""),
            ce("client", 0.1, 0.42, ""),
            ce("period", 0.1, 0.8, ""),
            ce("text", 0.1, 0.88, "Confidencial"),
        ];
        workspace::write_workspace_meta(&tmp, &wsm).unwrap();
        for project_type in [
            "pentest",
            "ejecutivo",
            "documento",
            "cti",
            "incidente",
            "retest",
            "htb",
        ] {
            let mut project = workspace::read_project_meta(&tmp, &pid).unwrap();
            project.project_type = project_type.to_string();
            project.layout = Vec::new();
            workspace::write_project_meta(&tmp, &pid, &project).unwrap();
            let pdfs = generate_pdf(&tmp, &pid, &templates, &typst_bin, false)
                .unwrap_or_else(|e| panic!("portada canvas fallo {project_type}: {e}"));
            assert!(
                std::fs::metadata(&pdfs[0]).unwrap().len() > 1000,
                "PDF canvas de {project_type} sospechosamente pequeno"
            );
        }
        // Restaurar la portada clasica.
        let mut wsm = workspace::read_workspace_meta(&tmp).unwrap();
        wsm.branding.cover_layout = "centered".to_string();
        wsm.branding.cover_elements = Vec::new();
        workspace::write_workspace_meta(&tmp, &wsm).unwrap();

        // Parte ejecutiva con datos (proyecto de ejemplo): compila en las
        // plantillas que la renderizan y en las que la ignoran.
        let (example, _) = workspace::create_example_project(&tmp).unwrap();
        for project_type in ["pentest", "redteam", "ejecutivo", "retest", "htb"] {
            let mut project = workspace::read_project_meta(&tmp, &example).unwrap();
            project.project_type = project_type.to_string();
            project.layout = Vec::new();
            workspace::write_project_meta(&tmp, &example, &project).unwrap();
            let pdfs = generate_pdf(&tmp, &example, &templates, &typst_bin, false)
                .unwrap_or_else(|e| panic!("parte ejecutiva fallo {project_type}: {e}"));
            if let Some(dir) = std::env::var_os("PUDU_PDF_REVIEW_DIR") {
                let dir = PathBuf::from(dir).join(format!("ejecutiva-{project_type}"));
                std::fs::create_dir_all(&dir).unwrap();
                std::fs::copy(&pdfs[0], dir.join("report.pdf")).unwrap();
            }
            assert!(std::fs::metadata(&pdfs[0]).unwrap().len() > 1000);
        }

        // Salida ejecutiva secundaria desde un proyecto de pentest.
        let mut project = workspace::read_project_meta(&tmp, &pid).unwrap();
        project.project_type = "pentest".to_string();
        workspace::write_project_meta(&tmp, &pid, &project).unwrap();
        let pdfs = generate_pdf(&tmp, &pid, &templates, &typst_bin, true).unwrap();
        assert_eq!(pdfs.len(), 2, "deberia generar principal + ejecutivo");

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
