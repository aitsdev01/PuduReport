// SPDX-License-Identifier: GPL-3.0-only
// Copyright (c) 2026 bc0d3

//! Pruebas de la sincronizacion contra un PuduHub simulado: un servidor HTTP
//! minimo sobre `TcpListener` (sin dependencias nuevas) que responde como la
//! API de escritorio y fusiona con el mismo paquete `.pudupkg`.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pudureport_core::package::{self, ImportOptions, PackageInfo, PackageLimits};
use pudureport_core::workspace;
use serde_json::{json, Value};

use super::client::{HubClient, Limits};
use super::link::{self, Link, LINK_FILE};
use super::{normalize_server_url, sync, HubError};

const TOKEN: &str = "phed_AbCdEfGhIjKlMnOpQrStUvWxYz0123456789-_abcde";
const LOCK: &str = "LockTokenAbCdEfGhIjKlMnOpQrStUvWxYz01234567";
const ID: &str = "6f1c2a9e-4b7d-4e2a-9c3b-1d2e3f4a5b6c";
const NEW_ID: &str = "0f8fad5b-d9cb-469f-a165-70867728950e";
const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 0];

// ---------------------------------------------------------------------------
// Servidor HTTP simulado
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Request {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    fn path(&self) -> &str {
        self.target.split('?').next().unwrap_or("")
    }
    fn query(&self) -> &str {
        self.target.split_once('?').map(|(_, q)| q).unwrap_or("")
    }
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    chunked: bool,
}

impl Reply {
    fn json(status: u16, value: Value) -> Reply {
        Reply {
            status,
            headers: vec![("Content-Type".into(), "application/json".into())],
            body: value.to_string().into_bytes(),
            chunked: false,
        }
    }
    fn zip(body: Vec<u8>) -> Reply {
        Reply {
            status: 200,
            headers: vec![("Content-Type".into(), "application/zip".into())],
            body,
            chunked: false,
        }
    }
}

type Handler = Arc<dyn Fn(&Request) -> Reply + Send + Sync>;

struct Mock {
    url: String,
    requests: Arc<Mutex<Vec<Request>>>,
}

impl Mock {
    fn requests(&self) -> Vec<Request> {
        self.requests.lock().unwrap().clone()
    }
    fn calls(&self) -> Vec<String> {
        self.requests()
            .iter()
            .map(|r| format!("{} {}", r.method, r.target))
            .collect()
    }
}

fn read_request(stream: &mut TcpStream) -> Option<Request> {
    let mut first = [0u8; 1];
    stream.peek(&mut first).ok()?;
    if first[0] == 0x16 {
        // Un ClientHello de TLS contra un servidor sin TLS.
        let _ = stream.write_all(b"HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\n");
        return None;
    }
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_string();
    let target = parts.next()?.to_string();
    let mut headers = Vec::new();
    loop {
        let mut header = String::new();
        reader.read_line(&mut header).ok()?;
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        let (k, v) = header.split_once(':')?;
        headers.push((k.trim().to_string(), v.trim().to_string()));
    }
    let length: usize = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).ok()?;
    Some(Request {
        method,
        target,
        headers,
        body,
    })
}

fn write_reply(stream: &mut TcpStream, reply: Reply) {
    let mut head = format!("HTTP/1.1 {} X\r\nConnection: close\r\n", reply.status);
    for (k, v) in &reply.headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    if reply.chunked {
        head.push_str("Transfer-Encoding: chunked\r\n\r\n");
        let _ = stream.write_all(head.as_bytes());
        for chunk in reply.body.chunks(512) {
            let _ = stream.write_all(format!("{:x}\r\n", chunk.len()).as_bytes());
            let _ = stream.write_all(chunk);
            let _ = stream.write_all(b"\r\n");
        }
        let _ = stream.write_all(b"0\r\n\r\n");
    } else {
        head.push_str(&format!("Content-Length: {}\r\n\r\n", reply.body.len()));
        let _ = stream.write_all(head.as_bytes());
        let _ = stream.write_all(&reply.body);
    }
    let _ = stream.flush();
}

fn serve(handler: Handler) -> Mock {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&requests);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
            if let Some(request) = read_request(&mut stream) {
                recorded.lock().unwrap().push(request.clone());
                let reply = handler(&request);
                write_reply(&mut stream, reply);
            }
        }
    });
    Mock { url, requests }
}

fn limits() -> Limits {
    Limits {
        connect_timeout: Duration::from_secs(2),
        json_timeout: Duration::from_secs(10),
        package_timeout: Duration::from_secs(20),
        ..Limits::default()
    }
}

fn client_for(mock: &Mock) -> HubClient {
    HubClient::with_limits(&mock.url, TOKEN, limits()).unwrap()
}

fn run<T>(future: impl std::future::Future<Output = T>) -> T {
    tauri::async_runtime::block_on(future)
}

// ---------------------------------------------------------------------------
// PuduHub simulado: paquetes por version y fusion trivial
// ---------------------------------------------------------------------------

fn tmp(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("pudu-hub-{label}-{}", uuid::Uuid::new_v4()));
    workspace::create_workspace(&dir, "WS").unwrap();
    dir
}

/// Paquete de una version del informe `id`, como lo entrega PuduHub.
fn package_of(root: &Path, project_id: &str, id: &str, version: u64) -> Vec<u8> {
    package::export_package(
        root,
        project_id,
        &PackageInfo {
            informe_id: Some(id.to_string()),
            version: Some(version),
            base_version: None,
            generado_por: "puduhub-test".to_string(),
        },
        false,
    )
    .unwrap()
    .bytes
}

/// Version 1 del informe: el proyecto de ejemplo con una evidencia.
fn initial_package(id: &str) -> Vec<u8> {
    let root = tmp("server");
    let (pid, _) = workspace::create_example_project(&root).unwrap();
    workspace::save_asset(&root, &pid, "png", PNG).unwrap();
    package_of(&root, &pid, id, 1)
}

/// Un proyecto vacio (lo que entrega PuduHub para un informe recien creado).
fn empty_package(id: &str) -> Vec<u8> {
    let root = tmp("server-empty");
    let (pid, _) = workspace::create_project(&root, "Nuevo", "", "pentest").unwrap();
    package_of(&root, &pid, id, 1)
}

/// "Fusion" del simulador: toma el paquete subido, aplica un cambio de otra
/// persona (si se pide) y lo guarda como la version siguiente.
fn merged_package(uploaded: &[u8], id: &str, version: u64, server_client: Option<&str>) -> Vec<u8> {
    let root = tmp("merge");
    let validated = package::validate_package(uploaded, &PackageLimits::default()).unwrap();
    let pid = package::write_package(&root, validated, &ImportOptions::default())
        .unwrap()
        .project_id;
    if let Some(client) = server_client {
        let mut meta = workspace::read_project_meta(&root, &pid).unwrap();
        meta.client = client.to_string();
        workspace::write_project_meta(&root, &pid, &meta).unwrap();
    }
    package_of(&root, &pid, id, version)
}

#[derive(Default)]
struct HubState {
    packages: BTreeMap<u64, Vec<u8>>,
    /// Respuesta fija para POST /sincronizar (por ejemplo, un conflicto).
    sync_reply: Option<(u16, Value)>,
    /// Respuesta fija para POST /bloqueo.
    lock_reply: Option<(u16, Value)>,
    /// Cambio del servidor que se aplica al fusionar.
    server_client: Option<String>,
    uploaded: Vec<Vec<u8>>,
    /// Borrador con cambios de la web que aun no son version (paquete con el
    /// contenido; la version la pone POST /versiones).
    draft: Option<Vec<u8>>,
    /// `X-Informe-Borrador-Pendiente` de la ultima version.
    draft_pending: bool,
    /// Cuerpos recibidos en POST /versiones.
    checkpoints: Vec<Value>,
}

/// Revision del borrador en el simulador.
const REVISION: u64 = 3;

fn row(id: &str, version: u64) -> Value {
    json!({
        "id": id, "titulo": "Pentest ACME", "tipo": "pentest", "estado": "borrador",
        "revision": 3, "versionActual": version, "hallazgos": 2, "bloqueo": null,
        "pdfDisponible": false, "publicado": false, "publicacion": null,
        "editadoPor": null, "editadoAt": null,
        "creadoAt": "2026-10-20T10:00:00Z", "actualizadoAt": "2026-10-24T10:00:00.123456+00:00",
    })
}

fn fake_hub(id: &'static str, state: Arc<Mutex<HubState>>) -> Mock {
    serve(Arc::new(move |req: &Request| {
        let mut st = state.lock().unwrap();
        let prefix = "/api/editor/informes";
        let rest = req.path().strip_prefix(prefix).unwrap_or("?");
        let latest = st.packages.keys().last().copied().unwrap_or(0);
        match (req.method.as_str(), rest) {
            ("GET", "") => Reply::json(
                200,
                json!({"items": [row(id, latest)], "total": 1, "page": 1, "pageSize": 20}),
            ),
            ("GET", r) if r == format!("/{id}") => Reply::json(200, {
                let mut detail = row(id, latest);
                detail["proyecto"] = json!({});
                detail["orden"] = json!([]);
                detail["workspace"] = Value::Null;
                detail["puedeForzar"] = json!(false);
                detail
            }),
            ("POST", "") => {
                // Informe recien creado: borrador vacio y ninguna version.
                st.draft = Some(empty_package(id));
                st.draft_pending = true;
                Reply::json(201, {
                    let mut detail = row(id, 0);
                    detail["proyecto"] = json!({});
                    detail["orden"] = json!([]);
                    detail
                })
            }
            ("GET", r) if r == format!("/{id}/paquete") => {
                let requested = req
                    .query()
                    .strip_prefix("version=")
                    .and_then(|v| v.parse().ok());
                if requested.is_none() && latest == 0 {
                    return Reply::json(409, json!({"error": "version_required"}));
                }
                let version = requested.unwrap_or(latest);
                match st.packages.get(&version) {
                    Some(bytes) => {
                        let pending = version != latest || st.draft_pending;
                        let mut reply = Reply::zip(bytes.clone());
                        reply
                            .headers
                            .push(("X-Informe-Version".into(), version.to_string()));
                        reply.headers.push((
                            "X-Informe-Borrador-Pendiente".into(),
                            if pending { "1" } else { "0" }.into(),
                        ));
                        reply
                    }
                    None => Reply::json(404, json!({"error": "not_found"})),
                }
            }
            ("POST", r) if r == format!("/{id}/versiones") => {
                if req.header("x-lock-token") != Some(LOCK) {
                    return Reply::json(409, json!({"error": "lock_required"}));
                }
                let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
                st.checkpoints.push(body.clone());
                if body["revision"].as_u64() != Some(REVISION) {
                    return Reply::json(
                        409,
                        json!({"error": "stale_revision", "revision": REVISION}),
                    );
                }
                let source = match st.draft.take() {
                    Some(draft) => draft,
                    None => st.packages[&latest].clone(),
                };
                let next = latest + 1;
                st.packages
                    .insert(next, merged_package(&source, id, next, None));
                st.draft_pending = false;
                Reply::json(
                    201,
                    json!({"version": next, "origen": "escritorio", "motivo": body["motivo"],
                           "revision": REVISION}),
                )
            }
            ("POST", r) if r == format!("/{id}/bloqueo") => match &st.lock_reply {
                Some((status, body)) => Reply::json(*status, body.clone()),
                None => Reply::json(
                    200,
                    json!({"token": LOCK, "revision": 3, "bloqueo": {
                        "usuarioId": "u1", "nombre": null, "hasta": "2026-10-25T12:00:00Z",
                        "origen": "escritorio", "propio": true}}),
                ),
            },
            ("DELETE", r) if r == format!("/{id}/bloqueo") => {
                Reply::json(200, json!({"revision": 3}))
            }
            ("POST", r) if r == format!("/{id}/sincronizar") => {
                st.uploaded.push(req.body.clone());
                if let Some((status, body)) = &st.sync_reply {
                    return Reply::json(*status, body.clone());
                }
                if req.header("x-lock-token") != Some(LOCK) {
                    return Reply::json(409, json!({"error": "lock_required"}));
                }
                let next = latest + 1;
                let merged = merged_package(&req.body, id, next, st.server_client.as_deref());
                st.packages.insert(next, merged);
                st.draft = None;
                st.draft_pending = false;
                Reply::json(
                    200,
                    json!({"revision": 4, "version": {"version": next, "origen": "escritorio"}}),
                )
            }
            _ => Reply::json(404, json!({"error": "not_found"})),
        }
    }))
}

fn hub_with_v1() -> (Mock, Arc<Mutex<HubState>>) {
    let state = Arc::new(Mutex::new(HubState::default()));
    state
        .lock()
        .unwrap()
        .packages
        .insert(1, initial_package(ID));
    (fake_hub(ID, Arc::clone(&state)), state)
}

fn manifest_of(bytes: &[u8]) -> package::Manifest {
    package::validate_package(bytes, &PackageLimits::default())
        .unwrap()
        .manifest
}

fn first_finding(root: &Path, pid: &str) -> pudureport_core::models::Finding {
    workspace::list_findings(root, pid).unwrap().remove(0)
}

fn assert_no_token(error: &HubError) {
    let serialized = serde_json::to_string(error).unwrap();
    assert!(!serialized.contains(TOKEN), "token en {serialized}");
    assert!(!serialized.contains("phed_"), "token en {serialized}");
    assert!(!format!("{error:?}").contains(TOKEN));
    assert!(!error.to_string().contains(TOKEN));
}

// ---------------------------------------------------------------------------
// Validaciones puras
// ---------------------------------------------------------------------------

#[test]
fn url_base_exige_https_salvo_localhost() {
    assert_eq!(
        normalize_server_url(" https://Hub.Example.cl/ ").unwrap(),
        "https://hub.example.cl"
    );
    assert_eq!(
        normalize_server_url("https://hub.example.cl/puduhub/").unwrap(),
        "https://hub.example.cl/puduhub"
    );
    assert_eq!(
        normalize_server_url("http://localhost:8000").unwrap(),
        "http://localhost:8000"
    );
    assert_eq!(
        normalize_server_url("http://127.0.0.1:9").unwrap(),
        "http://127.0.0.1:9"
    );
    for bad in [
        "http://hub.example.cl",
        "ftp://hub.example.cl",
        "hub.example.cl",
        "https://user:pw@hub.example.cl",
        "https://hub.example.cl/?x=1",
        "https://hub.example.cl/#a",
        "",
    ] {
        assert_eq!(
            normalize_server_url(bad).unwrap_err().codigo,
            "url_invalida",
            "{bad}"
        );
    }
}

#[test]
fn token_de_editor_con_formato_estricto() {
    assert!(HubClient::new("https://hub.example.cl", TOKEN).is_ok());
    for bad in [
        "",
        "phimp_x",
        "phed_corto",
        &format!("{TOKEN}x"),
        "Bearer phed_x",
    ] {
        let error = HubClient::new("https://hub.example.cl", bad).unwrap_err();
        assert_eq!(error.codigo, "token_invalido");
    }
    let client = HubClient::new("https://hub.example.cl", TOKEN).unwrap();
    assert!(!format!("{client:?}").contains(TOKEN));
}

#[test]
fn vinculo_ida_y_vuelta_sin_token_y_validado() {
    let root = tmp("link");
    let (pid, _) = workspace::create_example_project(&root).unwrap();
    assert_eq!(link::read_link(&root, &pid).unwrap(), None);
    let value = Link {
        formato: 1,
        servidor: "https://hub.example.cl".to_string(),
        informe_id: ID.to_string(),
        base_version: 7,
        sincronizado_at: "2026-10-25T10:00:00Z".to_string(),
        huella: link::snapshot(&root, &pid).unwrap().huella(),
    };
    link::write_link(&root, &pid, &value).unwrap();
    assert_eq!(link::read_link(&root, &pid).unwrap(), Some(value.clone()));
    let text = std::fs::read_to_string(root.join(&pid).join(LINK_FILE)).unwrap();
    assert!(!text.contains("phed_") && !text.to_lowercase().contains("token"));
    // El vinculo es de este equipo: queda fuera de git.
    let gitignore = std::fs::read_to_string(root.join(".gitignore")).unwrap();
    assert_eq!(gitignore.lines().filter(|l| *l == LINK_FILE).count(), 1);
    link::write_link(&root, &pid, &value).unwrap();
    let gitignore = std::fs::read_to_string(root.join(".gitignore")).unwrap();
    assert_eq!(gitignore.lines().filter(|l| *l == LINK_FILE).count(), 1);
    // Ni el paquete lo incluye.
    let exported = package::export_package(&root, &pid, &PackageInfo::default(), false).unwrap();
    assert!(exported
        .manifest
        .archivos
        .iter()
        .all(|f| !f.ruta.contains("puduhub")));

    let path = root.join(&pid).join(LINK_FILE);
    for tampered in [
        text.replace(ID, "../../etc"),
        text.replace("https://hub.example.cl", "http://hub.example.cl"),
        text.replace("\"formato\": 1", "\"formato\": 1, \"token\": \"x\""),
        text.replace("\"baseVersion\": 7", "\"baseVersion\": 0"),
        "{".to_string(),
        "x".repeat(20_000),
    ] {
        std::fs::write(&path, tampered).unwrap();
        assert_eq!(
            link::read_link(&root, &pid).unwrap_err().codigo,
            "vinculo_invalido"
        );
        let status = sync::link_status(&root, &pid, Some("https://hub.example.cl")).unwrap();
        assert!(status.vinculo_invalido && !status.vinculado);
    }
}

#[test]
fn la_huella_ignora_reescrituras_sin_cambios() {
    let root = tmp("huella");
    let (pid, _) = workspace::create_example_project(&root).unwrap();
    let before = link::snapshot(&root, &pid).unwrap().huella();
    let meta = workspace::read_project_meta(&root, &pid).unwrap();
    workspace::write_project_meta(&root, &pid, &meta).unwrap();
    assert_eq!(link::snapshot(&root, &pid).unwrap().huella(), before);
    let mut finding = first_finding(&root, &pid);
    finding.meta.title.push_str(" (editado)");
    workspace::write_finding(&root, &pid, &finding).unwrap();
    assert_ne!(link::snapshot(&root, &pid).unwrap().huella(), before);
}

// ---------------------------------------------------------------------------
// Contra el PuduHub simulado
// ---------------------------------------------------------------------------

#[test]
fn listado_valida_cada_fila() {
    let mock = serve(Arc::new(|_req: &Request| {
        let mut bad_id = row(ID, 1);
        bad_id["id"] = json!("../x");
        let mut bad_state = row(ID, 1);
        bad_state["estado"] = json!("<script>");
        let mut noisy = row(NEW_ID, 2);
        noisy["titulo"] = json!(format!("Informe\u{0007}\n con control {}", "x".repeat(400)));
        noisy["tipo"] = json!("<b>pentest</b>");
        noisy["bloqueo"] = json!({"usuarioId": "u", "nombre": "Ana\u{0000}Perez",
            "hasta": "2026-10-25T12:00:00Z", "origen": "web", "propio": false});
        let mut bad_lock = row(ID, 3);
        bad_lock["bloqueo"] = json!({"hasta": "manana", "origen": "web", "propio": false});
        Reply::json(
            200,
            json!({"items": [row(ID, 1), bad_id, bad_state, noisy, bad_lock],
                   "total": 5, "page": 1, "pageSize": 20}),
        )
    }));
    let root = tmp("list");
    let result = run(sync::list(&client_for(&mock), &root, 1, "acme")).unwrap();
    assert_eq!(result.items.len(), 2);
    assert_eq!(result.omitidos, 3);
    let noisy = &result.items[1].informe;
    assert!(noisy.titulo.chars().all(|c| !c.is_control()));
    assert!(noisy.titulo.chars().count() <= 200);
    assert_eq!(noisy.tipo, "");
    let lock = noisy.bloqueo.as_ref().unwrap();
    assert_eq!(lock.nombre.as_deref(), Some("Ana Perez"));
    assert_eq!(lock.origen, "web");
    let request = &mock.requests()[0];
    assert_eq!(
        request.header("authorization"),
        Some(&*format!("Bearer {TOKEN}"))
    );
    assert!(request.header("cookie").is_none());
    assert_eq!(request.query(), "page=1&pageSize=20&q=acme");
}

#[test]
fn probar_conexion_usa_un_listado_minimo() {
    let (mock, _) = hub_with_v1();
    assert_eq!(run(client_for(&mock).test()).unwrap(), 1);
    assert_eq!(
        mock.calls(),
        ["GET /api/editor/informes?page=1&pageSize=10"]
    );
}

#[test]
fn abrir_enviar_y_aplicar_la_fusion() {
    let (mock, state) = hub_with_v1();
    let client = client_for(&mock);
    let root = tmp("local");
    // Un git del workspace: el vinculo debe quedar ignorado.
    std::fs::create_dir_all(root.join(".git")).unwrap();

    let opened = run(sync::open_informe(&client, &root, ID)).unwrap();
    assert_eq!(opened.version, 1);
    assert!(!opened.actualizado);
    let pid = opened.proyecto_id.clone();
    assert_eq!(
        std::fs::read_dir(root.join(&pid).join("assets"))
            .unwrap()
            .count(),
        1
    );
    let status = sync::link_status(&root, &pid, Some(client.servidor())).unwrap();
    assert!(status.vinculado && status.mismo_servidor && !status.cambios_sin_enviar);
    assert_eq!(status.base_version, Some(1));

    // Edicion local sin conexion.
    let mut finding = first_finding(&root, &pid);
    finding.meta.title = "Titulo editado en el escritorio".to_string();
    workspace::write_finding(&root, &pid, &finding).unwrap();
    assert!(
        sync::link_status(&root, &pid, Some(client.servidor()))
            .unwrap()
            .cambios_sin_enviar
    );

    // Otra persona cambio el cliente en PuduHub; la fusion lo trae.
    state.lock().unwrap().server_client = Some("Cliente desde PuduHub".to_string());
    mock.requests.lock().unwrap().clear();
    let outcome = run(sync::send_project(&client, &root, &pid, false)).unwrap();
    assert_eq!(outcome.version, 2);
    assert!(outcome.aplicado, "{:?}", outcome.aviso);
    assert_eq!(outcome.cambios.campos, ["client"]);
    assert!(outcome.cambios.hallazgos_actualizados.is_empty());
    assert_eq!(
        mock.calls(),
        [
            format!("POST /api/editor/informes/{ID}/bloqueo"),
            format!("POST /api/editor/informes/{ID}/sincronizar"),
            format!("DELETE /api/editor/informes/{ID}/bloqueo"),
            format!("GET /api/editor/informes/{ID}/paquete?version=2"),
        ]
    );
    let requests = mock.requests();
    for request in &requests {
        assert_eq!(
            request.header("authorization"),
            Some(&*format!("Bearer {TOKEN}"))
        );
    }
    assert_eq!(requests[0].body, br#"{"forzar":false}"#);
    assert_eq!(requests[1].header("x-lock-token"), Some(LOCK));
    assert_eq!(requests[1].header("content-type"), Some("application/zip"));
    assert_eq!(requests[2].header("x-lock-token"), Some(LOCK));
    let sent = manifest_of(&state.lock().unwrap().uploaded[0]);
    assert_eq!(sent.informe_id.as_deref(), Some(ID));
    assert_eq!(sent.base_version, Some(1));
    assert_eq!(sent.version, Some(1));
    // La marca del workspace local no viaja: PuduHub conserva la del informe.
    assert!(sent.archivos.iter().all(|f| f.ruta != "workspace.yaml"));

    // Local: conserva la edicion propia y recibe la del servidor.
    let meta = workspace::read_project_meta(&root, &pid).unwrap();
    assert_eq!(meta.client, "Cliente desde PuduHub");
    assert_eq!(
        workspace::load_finding(&root, &pid, &finding.id)
            .unwrap()
            .meta
            .title,
        "Titulo editado en el escritorio"
    );
    let status = sync::link_status(&root, &pid, Some(client.servidor())).unwrap();
    assert_eq!(status.base_version, Some(2));
    assert!(!status.cambios_sin_enviar);
    assert_eq!(
        workspace::list_projects(&root).unwrap().len(),
        1,
        "sin carpetas temporales visibles"
    );

    // Volver a abrir sin cambios locales actualiza la misma copia.
    let again = run(sync::open_informe(&client, &root, ID)).unwrap();
    assert_eq!(
        (again.proyecto_id.as_str(), again.actualizado, again.version),
        (pid.as_str(), true, 2)
    );
}

#[test]
fn abrir_no_pisa_cambios_sin_enviar() {
    let (mock, _) = hub_with_v1();
    let client = client_for(&mock);
    let root = tmp("local-changes");
    let pid = run(sync::open_informe(&client, &root, ID))
        .unwrap()
        .proyecto_id;
    let mut finding = first_finding(&root, &pid);
    finding.body.push_str("\n\nNota local.");
    workspace::write_finding(&root, &pid, &finding).unwrap();
    mock.requests.lock().unwrap().clear();
    let error = run(sync::open_informe(&client, &root, ID)).unwrap_err();
    assert_eq!(error.codigo, "cambios_locales");
    assert_eq!(error.extra.proyecto_id.as_deref(), Some(pid.as_str()));
    assert!(mock.calls().is_empty(), "no descarga nada");
    assert!(workspace::load_finding(&root, &pid, &finding.id)
        .unwrap()
        .body
        .contains("Nota local."));
}

#[test]
fn conflicto_lista_entidades_con_titulos_y_no_toca_nada() {
    let (mock, state) = hub_with_v1();
    let client = client_for(&mock);
    let root = tmp("conflict");
    let pid = run(sync::open_informe(&client, &root, ID))
        .unwrap()
        .proyecto_id;
    let finding = first_finding(&root, &pid);
    let section = workspace::read_project_meta(&root, &pid).unwrap().sections[0].clone();
    let mut edited = finding.clone();
    edited.meta.title = "Cambio local".to_string();
    workspace::write_finding(&root, &pid, &edited).unwrap();
    state.lock().unwrap().sync_reply = Some((
        409,
        json!({"error": "merge_conflict", "entidades": [
            {"entidad": "hallazgo", "id": finding.id},
            {"entidad": "seccion", "id": section.key},
            {"entidad": "proyecto", "campo": "client"},
            {"entidad": "orden"},
            {"entidad": "<img src=x>", "id": "x"},
            {"entidad": "hallazgo", "id": "../../etc/passwd\u{0000}"}
        ], "detalle": "texto interno que no se muestra"}),
    ));
    mock.requests.lock().unwrap().clear();
    let error = run(sync::send_project(&client, &root, &pid, false)).unwrap_err();
    assert_eq!(error.codigo, "conflicto");
    assert_no_token(&error);
    assert!(!serde_json::to_string(&error)
        .unwrap()
        .contains("texto interno"));
    assert_eq!(error.extra.conflictos.len(), 5);
    assert_eq!(
        error.extra.conflictos[0].id.as_deref(),
        Some(finding.id.as_str())
    );
    assert_eq!(error.extra.conflictos[2].campo.as_deref(), Some("client"));
    assert_eq!(
        error.extra.conflictos[4].id, None,
        "id con caracteres no permitidos"
    );
    let context = error.extra.contexto.as_ref().unwrap();
    assert_eq!(context.hallazgos[&finding.id], "Cambio local");
    assert_eq!(context.secciones[&section.key], section.title);
    // El bloqueo se libera igual y no se descarga ni aplica nada.
    assert_eq!(
        mock.calls(),
        [
            format!("POST /api/editor/informes/{ID}/bloqueo"),
            format!("POST /api/editor/informes/{ID}/sincronizar"),
            format!("DELETE /api/editor/informes/{ID}/bloqueo"),
        ]
    );
    assert_eq!(
        workspace::load_finding(&root, &pid, &finding.id)
            .unwrap()
            .meta
            .title,
        "Cambio local"
    );
    let status = sync::link_status(&root, &pid, Some(client.servidor())).unwrap();
    assert_eq!(status.base_version, Some(1));
    assert!(status.cambios_sin_enviar);

    // "Descargar version del servidor": respaldo local y copia del servidor.
    let downloaded = run(sync::download_server_version(&client, &root, &pid)).unwrap();
    let backup = downloaded.respaldo_id.unwrap();
    assert_eq!(downloaded.version, 1);
    assert_eq!(
        workspace::load_finding(&root, &backup, &finding.id)
            .unwrap()
            .meta
            .title,
        "Cambio local"
    );
    assert!(workspace::read_project_meta(&root, &backup)
        .unwrap()
        .name
        .ends_with("(respaldo local)"));
    assert_eq!(link::read_link(&root, &backup).unwrap(), None);
    assert_eq!(
        workspace::load_finding(&root, &pid, &finding.id)
            .unwrap()
            .meta
            .title,
        finding.meta.title
    );
    assert!(
        !sync::link_status(&root, &pid, Some(client.servidor()))
            .unwrap()
            .cambios_sin_enviar
    );
}

#[test]
fn bloqueo_tomado_informa_quien_y_hasta_cuando() {
    let (mock, state) = hub_with_v1();
    let client = client_for(&mock);
    let root = tmp("lock");
    let pid = run(sync::open_informe(&client, &root, ID))
        .unwrap()
        .proyecto_id;
    state.lock().unwrap().lock_reply = Some((
        409,
        json!({"error": "lock_held", "bloqueo": {"usuarioId": "u2", "nombre": "Ana Perez",
            "hasta": "2026-10-25T12:15:00+00:00", "origen": "web", "propio": false}}),
    ));
    mock.requests.lock().unwrap().clear();
    let error = run(sync::send_project(&client, &root, &pid, false)).unwrap_err();
    assert_eq!(error.codigo, "bloqueo_tomado");
    assert!(error.mensaje.contains("Ana Perez"));
    assert!(!error.extra.puede_forzar);
    let lock = error.extra.bloqueo.as_ref().unwrap();
    assert_eq!(lock.hasta, "2026-10-25T12:15:00+00:00");
    assert_eq!(lock.origen, "web");
    assert_no_token(&error);
    assert_eq!(
        mock.calls(),
        [format!("POST /api/editor/informes/{ID}/bloqueo")]
    );

    // Otra sesion del mismo usuario: se puede tomar con forzar.
    state.lock().unwrap().lock_reply = Some((
        409,
        json!({"error": "lock_held", "bloqueo": {"usuarioId": "u1", "nombre": null,
            "hasta": "2026-10-25T12:15:00Z", "origen": "web", "propio": true}}),
    ));
    let error = run(sync::send_project(&client, &root, &pid, false)).unwrap_err();
    assert!(error.extra.puede_forzar);
    state.lock().unwrap().lock_reply = None;
    mock.requests.lock().unwrap().clear();
    run(sync::send_project(&client, &root, &pid, true)).unwrap();
    assert_eq!(mock.requests()[0].body, br#"{"forzar":true}"#);
}

/// Borrador del servidor: la version 1 con un cambio hecho en la web.
fn web_draft(id: &str, client: &str) -> Vec<u8> {
    merged_package(&initial_package(id), id, 1, Some(client))
}

#[test]
fn abrir_crea_punto_de_control_con_cambios_de_la_web() {
    let (mock, state) = hub_with_v1();
    {
        let mut st = state.lock().unwrap();
        st.draft = Some(web_draft(ID, "Cambio en la web"));
        st.draft_pending = true;
    }
    let client = client_for(&mock);
    let root = tmp("open-checkpoint");
    let opened = run(sync::open_informe(&client, &root, ID)).unwrap();
    assert_eq!(opened.version, 2);
    assert_eq!(opened.aviso, None);
    assert_eq!(
        mock.calls(),
        [
            format!("POST /api/editor/informes/{ID}/bloqueo"),
            format!("GET /api/editor/informes/{ID}"),
            format!("GET /api/editor/informes/{ID}/paquete"),
            format!("POST /api/editor/informes/{ID}/versiones"),
            format!("GET /api/editor/informes/{ID}/paquete?version=2"),
            format!("DELETE /api/editor/informes/{ID}/bloqueo"),
        ]
    );
    let requests = mock.requests();
    assert_eq!(requests[0].body, br#"{"forzar":false}"#);
    assert_eq!(requests[3].header("x-lock-token"), Some(LOCK));
    assert_eq!(requests[5].header("x-lock-token"), Some(LOCK));
    assert_eq!(
        state.lock().unwrap().checkpoints,
        [json!({"motivo": "Apertura desde PuduReport escritorio", "revision": REVISION})]
    );
    // El cambio de la web llego y es la base del vinculo.
    let meta = workspace::read_project_meta(&root, &opened.proyecto_id).unwrap();
    assert_eq!(meta.client, "Cambio en la web");
    let status = sync::link_status(&root, &opened.proyecto_id, Some(client.servidor())).unwrap();
    assert_eq!(status.base_version, Some(2));
    assert!(!status.cambios_sin_enviar);

    // Sin cambios pendientes no se crea otra version.
    mock.requests.lock().unwrap().clear();
    let again = run(sync::open_informe(&client, &root, ID)).unwrap();
    assert_eq!((again.version, again.actualizado), (2, true));
    assert_eq!(
        mock.calls(),
        [
            format!("POST /api/editor/informes/{ID}/bloqueo"),
            format!("GET /api/editor/informes/{ID}"),
            format!("GET /api/editor/informes/{ID}/paquete"),
            format!("DELETE /api/editor/informes/{ID}/bloqueo"),
        ]
    );
    assert_eq!(state.lock().unwrap().checkpoints.len(), 1);
}

fn lock_held_by_other() -> (u16, Value) {
    (
        409,
        json!({"error": "lock_held", "bloqueo": {"usuarioId": "u2", "nombre": "Ana Perez",
            "hasta": "2026-10-25T12:15:00+00:00", "origen": "web", "propio": false}}),
    )
}

#[test]
fn abrir_con_bloqueo_ajeno_baja_la_ultima_version_y_avisa() {
    let (mock, state) = hub_with_v1();
    {
        let mut st = state.lock().unwrap();
        st.draft = Some(web_draft(ID, "Cambio en la web"));
        st.draft_pending = true;
        st.lock_reply = Some(lock_held_by_other());
    }
    let client = client_for(&mock);
    let root = tmp("open-locked");
    let opened = run(sync::open_informe(&client, &root, ID)).unwrap();
    assert_eq!(opened.version, 1);
    let aviso = opened.aviso.expect("aviso de cambios pendientes");
    assert!(aviso.contains("no se incluyeron"), "{aviso}");
    assert_eq!(
        mock.calls(),
        [
            format!("POST /api/editor/informes/{ID}/bloqueo"),
            format!("GET /api/editor/informes/{ID}/paquete"),
        ]
    );
    assert!(state.lock().unwrap().checkpoints.is_empty());
    let meta = workspace::read_project_meta(&root, &opened.proyecto_id).unwrap();
    assert_ne!(meta.client, "Cambio en la web");
    let status = sync::link_status(&root, &opened.proyecto_id, Some(client.servidor())).unwrap();
    assert_eq!(status.base_version, Some(1));

    // Sin cambios pendientes no hay aviso.
    state.lock().unwrap().draft_pending = false;
    let again = run(sync::open_informe(&client, &root, ID)).unwrap();
    assert_eq!(again.aviso, None);
}

#[test]
fn abrir_sin_versiones_con_bloqueo_ajeno_explica_version_requerida() {
    let state = Arc::new(Mutex::new(HubState::default()));
    {
        let mut st = state.lock().unwrap();
        st.draft = Some(empty_package(ID));
        st.draft_pending = true;
        st.lock_reply = Some(lock_held_by_other());
    }
    let mock = fake_hub(ID, Arc::clone(&state));
    let root = tmp("open-no-version");
    let error = run(sync::open_informe(&client_for(&mock), &root, ID)).unwrap_err();
    assert_eq!(error.codigo, "sin_version");
    assert!(
        error.mensaje.contains("ninguna version"),
        "{}",
        error.mensaje
    );
    assert_no_token(&error);
    assert_eq!(
        mock.calls(),
        [
            format!("POST /api/editor/informes/{ID}/bloqueo"),
            format!("GET /api/editor/informes/{ID}/paquete"),
        ]
    );
    assert!(workspace::list_projects(&root).unwrap().is_empty());

    // Con el bloqueo libre se crea la primera version.
    state.lock().unwrap().lock_reply = None;
    mock.requests.lock().unwrap().clear();
    let opened = run(sync::open_informe(&client_for(&mock), &root, ID)).unwrap();
    assert_eq!(opened.version, 1);
    assert_eq!(
        mock.calls(),
        [
            format!("POST /api/editor/informes/{ID}/bloqueo"),
            format!("GET /api/editor/informes/{ID}"),
            format!("POST /api/editor/informes/{ID}/versiones"),
            format!("GET /api/editor/informes/{ID}/paquete?version=1"),
            format!("DELETE /api/editor/informes/{ID}/bloqueo"),
        ]
    );
}

#[test]
fn paquete_con_cabecera_de_version_distinta_se_rechaza() {
    let mock = serve(Arc::new(|_req: &Request| {
        let mut reply = Reply::zip(initial_package(ID));
        reply.headers.push(("X-Informe-Version".into(), "7".into()));
        reply
    }));
    let error = run(client_for(&mock).package(ID, Some(1))).unwrap_err();
    assert_eq!(error.codigo, "respuesta_invalida");
}

#[test]
fn errores_mapeados_por_codigo_sin_token_ni_cuerpo() {
    let cases: Vec<(u16, Value, &str)> = vec![
        (401, json!({"error": "unauthorized"}), "no_autorizado"),
        (403, json!({"error": "forbidden"}), "prohibido"),
        (404, json!({"error": "not_found"}), "no_encontrado"),
        (
            409,
            json!({"error": "stale_revision", "revision": 9}),
            "revision_desactualizada",
        ),
        (409, json!({"error": "lock_required"}), "bloqueo_requerido"),
        (
            409,
            json!({"error": "unknown_base_version"}),
            "version_base_desconocida",
        ),
        (409, json!({"error": "version_required"}), "sin_version"),
        (413, json!({"error": "payload_too_large"}), "paquete_grande"),
        (
            415,
            json!({"error": "unsupported_media_type"}),
            "tipo_no_soportado",
        ),
        (422, json!({"error": "invalid_package"}), "paquete_invalido"),
        (422, json!({"error": "invalid_payload"}), "datos_invalidos"),
        (
            429,
            json!({"error": "rate_limit_exceeded", "retryAfter": 30}),
            "limite_peticiones",
        ),
        (
            503,
            json!({"error": "unavailable"}),
            "servicio_no_disponible",
        ),
        (
            418,
            json!({"error": "teapot", "detalle": "x"}),
            "error_servidor",
        ),
    ];
    for (status, body, expected) in cases {
        let reply_body = body.clone();
        let mock = serve(Arc::new(move |req: &Request| {
            // El servidor "refleja" el token: nunca debe llegar al mensaje.
            let mut echoed = reply_body.clone();
            echoed["detalle"] = json!(req.header("authorization").unwrap_or(""));
            Reply::json(status, echoed)
        }));
        let error = run(client_for(&mock).list(1, "")).unwrap_err();
        assert_eq!(error.codigo, expected, "{status} {body}");
        assert_no_token(&error);
        if expected == "limite_peticiones" {
            assert_eq!(error.extra.reintentar_en, Some(30));
        }
    }
}

#[test]
fn respuesta_demasiado_grande_se_corta() {
    let big = vec![b'x'; 4096];
    for chunked in [false, true] {
        let body = big.clone();
        let mock = serve(Arc::new(move |_req: &Request| Reply {
            chunked,
            ..Reply::zip(body.clone())
        }));
        let client = HubClient::with_limits(
            &mock.url,
            TOKEN,
            Limits {
                package_bytes: 1024,
                json_bytes: 1024,
                ..limits()
            },
        )
        .unwrap();
        let error = run(async { client.package(ID, Some(1)).await?.bytes().await }).unwrap_err();
        assert_eq!(error.codigo, "respuesta_grande", "chunked={chunked}");
        let error = run(client.list(1, "")).unwrap_err();
        assert_eq!(error.codigo, "respuesta_grande", "chunked={chunked}");
    }
}

#[test]
fn no_sigue_redirecciones_a_otro_origen() {
    let other = serve(Arc::new(|_req: &Request| Reply::json(200, json!({}))));
    let target = format!("{}/robar", other.url);
    let mock = serve(Arc::new(move |_req: &Request| Reply {
        status: 302,
        headers: vec![("Location".into(), target.clone())],
        body: Vec::new(),
        chunked: false,
    }));
    let client = client_for(&mock);
    let error = run(client.list(1, "")).unwrap_err();
    assert_eq!(error.codigo, "redireccion");
    assert_no_token(&error);
    let error = run(client.package(ID, None)).unwrap_err();
    assert_eq!(error.codigo, "redireccion");
    assert!(
        other.requests().is_empty(),
        "el token no viajo al otro origen"
    );
}

#[test]
fn errores_de_red_y_tls_explicados() {
    // Puerto cerrado.
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let client =
        HubClient::with_limits(&format!("http://127.0.0.1:{port}"), TOKEN, limits()).unwrap();
    let error = run(client.test()).unwrap_err();
    assert_eq!(error.codigo, "red");
    assert_no_token(&error);

    // https contra un servidor sin TLS.
    let mock = serve(Arc::new(|_req: &Request| Reply::json(200, json!({}))));
    let url = mock.url.replace("http://", "https://");
    let client = HubClient::with_limits(&url, TOKEN, limits()).unwrap();
    let error = run(client.test()).unwrap_err();
    assert_eq!(error.codigo, "tls", "{}", error.mensaje);
    assert!(error.mensaje.contains("certificado"));
    assert_no_token(&error);
}

#[test]
fn paquete_del_servidor_pasa_por_el_importador_estricto() {
    // Un ZIP que no es un paquete valido, y uno valido de otro informe.
    let state = Arc::new(Mutex::new(HubState::default()));
    state
        .lock()
        .unwrap()
        .packages
        .insert(1, b"PK\x03\x04no-es-zip".to_vec());
    let mock = fake_hub(ID, Arc::clone(&state));
    let root = tmp("strict");
    let error = run(sync::open_informe(&client_for(&mock), &root, ID)).unwrap_err();
    assert_eq!(error.codigo, "paquete_invalido");
    state
        .lock()
        .unwrap()
        .packages
        .insert(1, initial_package(NEW_ID));
    let error = run(sync::open_informe(&client_for(&mock), &root, ID)).unwrap_err();
    assert_eq!(error.codigo, "paquete_invalido");
    assert!(workspace::list_projects(&root).unwrap().is_empty());
}

#[test]
fn subir_como_informe_nuevo() {
    let state = Arc::new(Mutex::new(HubState::default()));
    let mock = fake_hub(NEW_ID, Arc::clone(&state));
    let client = client_for(&mock);
    let root = tmp("upload");
    let (pid, _) = workspace::create_example_project(&root).unwrap();
    workspace::save_asset(&root, &pid, "png", PNG).unwrap();
    let outcome = run(sync::upload_new(&client, &root, &pid)).unwrap();
    assert_eq!(outcome.informe_id, NEW_ID);
    assert_eq!(outcome.version, 2);
    assert!(outcome.aplicado);
    assert_eq!(
        mock.calls(),
        [
            "POST /api/editor/informes".to_string(),
            // Sin versiones: punto de control del borrador recien creado.
            format!("POST /api/editor/informes/{NEW_ID}/bloqueo"),
            format!("GET /api/editor/informes/{NEW_ID}"),
            format!("POST /api/editor/informes/{NEW_ID}/versiones"),
            format!("GET /api/editor/informes/{NEW_ID}/paquete?version=1"),
            format!("DELETE /api/editor/informes/{NEW_ID}/bloqueo"),
            format!("POST /api/editor/informes/{NEW_ID}/bloqueo"),
            format!("POST /api/editor/informes/{NEW_ID}/sincronizar"),
            format!("DELETE /api/editor/informes/{NEW_ID}/bloqueo"),
            format!("GET /api/editor/informes/{NEW_ID}/paquete?version=2"),
        ]
    );
    let created: Value = serde_json::from_slice(&mock.requests()[0].body).unwrap();
    assert_eq!(created["tipo"], "pentest");
    assert!(!created["titulo"].as_str().unwrap().is_empty());
    let sent = manifest_of(&state.lock().unwrap().uploaded[0]);
    assert_eq!(sent.base_version, Some(1));
    assert!(sent.archivos.iter().any(|f| f.ruta.starts_with("assets/")));
    // Informe nuevo: con la marca del workspace.
    assert!(sent.archivos.iter().any(|f| f.ruta == "workspace.yaml"));
    let status = sync::link_status(&root, &pid, Some(client.servidor())).unwrap();
    assert_eq!(status.informe_id.as_deref(), Some(NEW_ID));
    assert!(!status.cambios_sin_enviar);
    // Ya vinculado: no se vuelve a crear.
    let error = run(sync::upload_new(&client, &root, &pid)).unwrap_err();
    assert_eq!(error.codigo, "ya_vinculado");
}

#[test]
fn otro_servidor_no_recibe_el_proyecto() {
    let (mock, _) = hub_with_v1();
    let root = tmp("other-server");
    let pid = run(sync::open_informe(&client_for(&mock), &root, ID))
        .unwrap()
        .proyecto_id;
    let other = HubClient::with_limits("http://localhost:1", TOKEN, limits()).unwrap();
    let error = run(sync::send_project(&other, &root, &pid, false)).unwrap_err();
    assert_eq!(error.codigo, "otro_servidor");
    let status = sync::link_status(&root, &pid, Some("http://localhost:1")).unwrap();
    assert!(status.vinculado && !status.mismo_servidor);
}
