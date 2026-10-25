// SPDX-License-Identifier: GPL-3.0-only
// Copyright (c) 2026 bc0d3

//! El proceso de Typst no hereda el entorno del servicio: la clave compartida
//! nunca le llega. Va en su propio binario de pruebas porque modifica el
//! entorno del proceso y no debe correr en paralelo con otras pruebas.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use pudureport_core::package::{self, PackageInfo};
use pudureport_core::workspace;
use pudureport_service::{router, AppState, Config, KEY_HEADER};
use tower::ServiceExt;

const KEY: &str = "clave-de-prueba-que-no-debe-llegar-a-typst";

#[tokio::test]
async fn typst_child_does_not_inherit_the_service_key() {
    std::env::set_var("PUDUREPORT_SERVICE_KEY", KEY);
    std::env::set_var("PUDUREPORT_PRUEBA_SECRETO", "otro-secreto-del-servicio");

    // Typst falso: vuelca su entorno y sus argumentos y, al compilar, escribe
    // un PDF minimo en la salida (ultimo argumento, terminado en `.pdf`).
    let dir = std::env::temp_dir().join(format!("pudu-svc-env-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let fake = dir.join("typst");
    let script = format!(
        "#!/bin/sh\n/usr/bin/env > '{0}/env.txt'\necho \"$@\" > '{0}/args.txt'\nfor last; do :; done\ncase \"$last\" in *.pdf) printf '%%PDF-1.7 falso' > \"$last\";; esac\n",
        dir.display()
    );
    std::fs::write(&fake, script).unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

    // `typst --version` tambien corre sin el entorno del servicio.
    pudureport_service::typst_version(&fake).unwrap();
    let env = std::fs::read_to_string(dir.join("env.txt")).unwrap();
    assert!(!env.contains(KEY), "--version recibio la clave");

    let root = dir.join("ws");
    workspace::create_workspace(&root, "WS").unwrap();
    let (pid, _) = workspace::create_example_project(&root).unwrap();
    let pkg = package::export_package(&root, &pid, &PackageInfo::default(), true).unwrap();

    let cfg = Config::new(&[KEY.to_string()], fake, "typst falso".to_string());
    let req = Request::post("/v1/render")
        .header("content-type", "application/zip")
        .header(KEY_HEADER, KEY)
        .body(Body::from(pkg.bytes))
        .unwrap();
    let response = router(AppState::new(cfg)).oneshot(req).await.unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));

    let env = std::fs::read_to_string(dir.join("env.txt")).unwrap();
    assert!(!env.contains(KEY), "Typst recibio la clave del servicio");
    assert!(
        !env.contains("PUDUREPORT"),
        "Typst recibio variables del servicio"
    );
    assert!(!env.contains("otro-secreto-del-servicio"));
    let args = std::fs::read_to_string(dir.join("args.txt")).unwrap();
    assert!(args.contains("--jobs 1"), "{args}");
    let _ = std::fs::remove_dir_all(&dir);
}
