// SPDX-License-Identifier: GPL-3.0-only
// Copyright (c) 2026 bc0d3

//! Pruebas de integracion del servicio: se ejercita el router completo (sin
//! abrir puertos) con peticiones HTTP reales.

use std::io::{Cursor, Write};
use std::path::PathBuf;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use pudureport_core::package::{self, PackageInfo};
use pudureport_core::workspace;
use pudureport_service::{router, AppState, Config, KEY_HEADER};
use serde_json::Value;
use tower::ServiceExt;

const KEY: &str = "clave-de-prueba-con-mas-de-32-caracteres";

/// Binario de Typst para las pruebas: `PUDU_TYPST_BIN`, el sidecar de la app
/// (`src-tauri/binaries/typst-*`) o `typst` del PATH.
fn typst_bin() -> PathBuf {
    if let Ok(path) = std::env::var("PUDU_TYPST_BIN") {
        return PathBuf::from(path);
    }
    let binaries = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("src-tauri/binaries");
    if let Ok(entries) = std::fs::read_dir(&binaries) {
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().starts_with("typst-") {
                return entry.path();
            }
        }
    }
    PathBuf::from("typst")
}

fn config() -> Config {
    let bin = typst_bin();
    let version = pudureport_service::typst_version(&bin).unwrap_or_default();
    Config::new(&[KEY.to_string()], bin, version)
}

fn app(cfg: Config) -> axum::Router {
    router(AppState::new(cfg))
}

fn example_package() -> Vec<u8> {
    let root = std::env::temp_dir().join(format!("pudu-svc-{}", uuid::Uuid::new_v4()));
    workspace::create_workspace(&root, "WS").unwrap();
    let (pid, _) = workspace::create_example_project(&root).unwrap();
    let pkg = package::export_package(&root, &pid, &PackageInfo::default(), true).unwrap();
    let _ = std::fs::remove_dir_all(&root);
    pkg.bytes
}

fn render_request(body: Vec<u8>, key: Option<&str>) -> Request<Body> {
    let mut builder = Request::post("/v1/render").header("content-type", "application/zip");
    if let Some(key) = key {
        builder = builder.header(KEY_HEADER, key);
    }
    builder.body(Body::from(body)).unwrap()
}

async fn send(
    app: axum::Router,
    req: Request<Body>,
) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let response = app.oneshot(req).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, headers, bytes.to_vec())
}

fn json(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes).unwrap()
}

#[tokio::test]
async fn renders_example_package_to_pdf() {
    let cfg = config();
    if cfg.typst_version.is_empty() {
        eprintln!("Typst no disponible; se omite el render de integracion");
        return;
    }
    let (status, headers, body) =
        send(app(cfg), render_request(example_package(), Some(KEY))).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    assert!(body.starts_with(b"%PDF"), "la respuesta no es un PDF");
    assert_eq!(headers["content-type"], "application/pdf");
    assert_eq!(headers["x-pudureport-plantilla"], "pentest");
    // PuduHub exige las cuatro cabeceras de version en cada render exitoso.
    for name in [
        "x-pudureport-version",
        "x-pudureport-plantillas",
        "x-pudureport-plantilla",
        "x-pudureport-typst",
    ] {
        let value = headers.get(name).unwrap_or_else(|| panic!("falta {name}"));
        assert!(!value.is_empty(), "{name} vacia");
    }
    assert!(headers["x-pudureport-typst"]
        .to_str()
        .unwrap()
        .starts_with("typst "));
    if let Some(dir) = std::env::var_os("PUDU_SERVICE_PDF_OUT") {
        std::fs::write(PathBuf::from(dir).join("servicio-ejemplo.pdf"), &body).unwrap();
    }

    // Con plantilla explicita.
    let req = Request::post("/v1/render?plantilla=ejecutivo")
        .header("content-type", "application/zip")
        .header(KEY_HEADER, KEY)
        .body(Body::from(example_package()))
        .unwrap();
    let (status, headers, body) = send(app(config()), req).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.starts_with(b"%PDF"));
    assert_eq!(headers["x-pudureport-plantilla"], "ejecutivo");
}

#[tokio::test]
async fn rejects_unknown_template() {
    let req = Request::post("/v1/render?plantilla=../../etc/passwd")
        .header("content-type", "application/zip")
        .header(KEY_HEADER, KEY)
        .body(Body::from(example_package()))
        .unwrap();
    let (status, _, body) = send(app(config()), req).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(json(&body)["error"], "unknown_template");
}

#[tokio::test]
async fn auth_is_required_and_checked() {
    let (status, _, body) = send(app(config()), render_request(example_package(), None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(json(&body)["error"], "unauthorized");

    let wrong = "otra-clave-que-no-corresponde-a-ninguna";
    let (status, _, _) = send(
        app(config()),
        render_request(example_package(), Some(wrong)),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let req = Request::post("/v1/cvss")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"vector":"CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:H"}"#,
        ))
        .unwrap();
    let (status, _, _) = send(app(config()), req).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // La sonda de salud no exige clave.
    let req = Request::get("/healthz").body(Body::empty()).unwrap();
    let (status, _, body) = send(app(config()), req).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&body)["estado"], "ok");
}

#[test]
fn keys_require_minimum_length_and_support_rotation() {
    assert!(pudureport_service::parse_keys("").is_err());
    assert!(pudureport_service::parse_keys("corta").is_err());
    let text = format!("# actual\n{KEY}\n\nsiguiente-clave-de-rotacion-de-32-caracteres\n");
    let keys = pudureport_service::parse_keys(&text).unwrap();
    assert_eq!(keys.len(), 2);
    let cfg = Config::new(&keys, PathBuf::from("typst"), String::new());
    assert!(cfg.accepts(KEY.as_bytes()));
    assert!(cfg.accepts(b"siguiente-clave-de-rotacion-de-32-caracteres"));
    assert!(!cfg.accepts(b"clave-de-prueba-con-mas-de-32-caracterez"));
    assert!(!cfg.accepts(b""));
}

#[tokio::test]
async fn enforces_body_size_limit() {
    let mut cfg = config();
    cfg.max_body_bytes = 1024;
    let (status, _, body) = send(app(cfg), render_request(vec![0u8; 4096], Some(KEY))).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(json(&body)["error"], "payload_too_large");
}

#[tokio::test]
async fn rejects_wrong_content_type() {
    let req = Request::post("/v1/render")
        .header("content-type", "text/html")
        .header(KEY_HEADER, KEY)
        .body(Body::from(example_package()))
        .unwrap();
    let (status, _, _) = send(app(config()), req).await;
    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
}

#[tokio::test]
async fn rejects_zip_slip_package() {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default();
    zip.start_file("manifest.json", opts).unwrap();
    zip.write_all(br#"{"formato":1,"archivos":[]}"#).unwrap();
    zip.start_file("../../etc/cron.d/evil", opts).unwrap();
    zip.write_all(b"* * * * * root id").unwrap();
    let bytes = zip.finish().unwrap().into_inner();

    let (status, _, body) = send(app(config()), render_request(bytes, Some(KEY))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let body = json(&body);
    assert_eq!(body["error"], "invalid_package");
    assert!(body["detalle"].as_str().unwrap().contains("ruta"));
}

#[tokio::test]
async fn rejects_garbage_package() {
    let (status, _, body) = send(
        app(config()),
        render_request(b"no es zip".to_vec(), Some(KEY)),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(json(&body)["error"], "invalid_package");
}

async fn cvss(body: &str) -> (StatusCode, Value) {
    let req = Request::post("/v1/cvss")
        .header("content-type", "application/json")
        .header(KEY_HEADER, KEY)
        .body(Body::from(body.to_string()))
        .unwrap();
    let (status, _, bytes) = send(app(config()), req).await;
    (status, json(&bytes))
}

#[tokio::test]
async fn computes_cvss_vectors() {
    let (status, body) = cvss(r#"{"vector":"CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:H"}"#).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["score"], 9.8);
    assert_eq!(body["severity"], "critical");
    assert_eq!(body["version"], "3.1");

    let (status, body) =
        cvss(r#"{"vector":"CVSS:4.0/AV:N/AC:L/AT:N/PR:L/UI:N/VC:H/VI:N/VA:N/SC:N/SI:N/SA:N"}"#)
            .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["score"], 7.1);
    assert_eq!(body["severity"], "high");
    assert_eq!(body["version"], "4.0");

    let (status, body) = cvss(r#"{"vector":"CVSS:3.1/AV:N"}"#).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"], "invalid_vector");

    let (status, body) = cvss(r#"{"otro":1}"#).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"], "invalid_payload");
}

async fn initial(body: &str, key: Option<&str>) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let mut builder =
        Request::post("/v1/proyecto-inicial").header("content-type", "application/json");
    if let Some(key) = key {
        builder = builder.header(KEY_HEADER, key);
    }
    send(
        app(config()),
        builder.body(Body::from(body.to_string())).unwrap(),
    )
    .await
}

#[tokio::test]
async fn initial_project_matches_desktop_creation() {
    let (status, headers, body) = initial(
        r#"{"tipo":"pentest","nombre":"Informe Web","cliente":"ACME"}"#,
        Some(KEY),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    assert_eq!(headers["content-type"], "application/yaml; charset=utf-8");
    for name in [
        "x-pudureport-version",
        "x-pudureport-plantillas",
        "x-pudureport-typst",
    ] {
        assert!(headers.contains_key(name), "falta {name}");
    }
    let yaml = String::from_utf8(body).unwrap();
    assert_eq!(
        yaml,
        workspace::initial_project_yaml("Informe Web", "ACME", "pentest").unwrap()
    );
    let meta: pudureport_core::models::ProjectMeta = serde_yaml::from_str(&yaml).unwrap();
    assert_eq!(meta.name, "Informe Web");
    assert_eq!(meta.client, "ACME");
    assert!(!meta.sections.is_empty());
    assert!(meta.layout.iter().any(|b| b.kind == "cover"));
    assert!(meta.finding_order.is_empty());

    // Cada tipo conocido, sin cliente.
    for tipo in pudureport_core::models::PROJECT_TYPES {
        let (status, _, body) =
            initial(&format!(r#"{{"tipo":"{tipo}","nombre":"X"}}"#), Some(KEY)).await;
        assert_eq!(status, StatusCode::OK, "{tipo}");
        let meta: pudureport_core::models::ProjectMeta = serde_yaml::from_slice(&body).unwrap();
        assert_eq!(meta.project_type, tipo);
        assert!(meta.client.is_empty());
    }
}

#[tokio::test]
async fn initial_project_validates_input_and_auth() {
    let (status, _, _) = initial(r#"{"tipo":"pentest","nombre":"X"}"#, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    for (body, code) in [
        (r#"{"tipo":"desconocido","nombre":"X"}"#, "invalid_type"),
        (r#"{"tipo":"pentest","nombre":"  "}"#, "invalid_payload"),
        (
            r#"{"tipo":"pentest","nombre":"a\u0000b"}"#,
            "invalid_payload",
        ),
        (
            r#"{"tipo":"pentest","nombre":"X","extra":1}"#,
            "invalid_payload",
        ),
        (r#"{"nombre":"X"}"#, "invalid_payload"),
    ] {
        let (status, _, bytes) = initial(body, Some(KEY)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(json(&bytes)["error"], code, "{body}");
    }
    let long = "x".repeat(201);
    let (status, _, _) = initial(
        &format!(r#"{{"tipo":"pentest","nombre":"{long}"}}"#),
        Some(KEY),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}
