// SPDX-License-Identifier: GPL-3.0-only
// Copyright (c) 2026 bc0d3

//! Binario `pudureport-service`. Ver `service/README.md`.

use pudureport_service::{log_event, router, AppState, Config};
use serde_json::json;

#[tokio::main]
async fn main() {
    let cfg = match Config::from_env() {
        Ok(cfg) => cfg,
        Err(e) => {
            // Sin clave (o con una invalida) el servicio no arranca.
            log_event("inicio_fallido", json!({ "motivo": e }));
            std::process::exit(1);
        }
    };
    let addr = std::env::var("PUDUREPORT_SERVICE_ADDR")
        .ok()
        .filter(|a| !a.is_empty())
        .unwrap_or_else(|| "0.0.0.0:8080".to_string());
    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(listener) => listener,
        Err(e) => {
            log_event(
                "inicio_fallido",
                json!({ "motivo": format!("no se pudo escuchar en {addr}: {e}") }),
            );
            std::process::exit(1);
        }
    };
    log_event(
        "inicio",
        json!({
            "direccion": addr,
            "version": env!("CARGO_PKG_VERSION"),
            "typst": cfg.typst_version,
            "plantillas": pudureport_service::templates_fingerprint(),
            "concurrencia": cfg.concurrency,
            "max_bytes": cfg.max_body_bytes,
            "timeout_s": cfg.render_timeout.as_secs(),
        }),
    );
    let app = router(AppState::new(cfg));
    if let Err(e) = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown())
        .await
    {
        log_event("error", json!({ "motivo": e.to_string() }));
        std::process::exit(1);
    }
    log_event("fin", json!({}));
}

/// Termina con Ctrl+C o SIGTERM (detencion del orquestador), dejando
/// terminar las peticiones en curso.
async fn shutdown() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = term => {},
    }
}
