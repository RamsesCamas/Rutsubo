//! Arranque del daemon de Rutsubo.
//!
//! Bind exclusivo en loopback (RNF-04): si la configuración pide otra
//! interfaz, el proceso se niega a arrancar con error explícito.

use rutsubo_daemon::api;
use rutsubo_daemon::config::DaemonConfig;
use rutsubo_daemon::state::AppState;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,sqlx=warn".into()),
        )
        .init();

    let cfg = match DaemonConfig::from_env() {
        Ok(cfg) => cfg,
        Err(err) => {
            // RNF-04: negativa explícita (p. ej. bind fuera de loopback).
            eprintln!("rutsubo-daemon: {err}");
            std::process::exit(1);
        }
    };

    let app = match AppState::bootstrap(cfg.clone()).await {
        Ok(app) => app,
        Err(err) => {
            eprintln!("rutsubo-daemon: fallo de arranque: {err}");
            std::process::exit(1);
        }
    };

    let router = api::router(app);
    let listener = match tokio::net::TcpListener::bind(cfg.bind).await {
        Ok(l) => l,
        Err(err) => {
            eprintln!("rutsubo-daemon: no se pudo escuchar en {}: {err}", cfg.bind);
            std::process::exit(1);
        }
    };
    tracing::info!(bind = %cfg.bind, data_dir = %cfg.data_dir.display(), "daemon listo");

    if let Err(err) = axum::serve(listener, router).await {
        eprintln!("rutsubo-daemon: error del servidor: {err}");
        std::process::exit(1);
    }
}
