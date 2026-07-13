//! Arranque del relay C-2. Escucha en `RELAY_BIND` (default 127.0.0.1:8443;
//! en docker/LAN se levanta con 0.0.0.0:8443) y persiste en `RELAY_DB`.

use rutsubo_relay::config::RelayConfig;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,sqlx=warn".into()),
        )
        .init();

    let cfg = match RelayConfig::from_env() {
        Ok(cfg) => cfg,
        Err(err) => {
            eprintln!("rutsubo-relay: {err}");
            std::process::exit(1);
        }
    };

    let state = match rutsubo_relay::bootstrap_with(
        &cfg.db_url,
        cfg.google_dev,
        cfg.google_client_ids.clone(),
    )
    .await
    {
        Ok(state) => state,
        Err(err) => {
            eprintln!("rutsubo-relay: fallo de arranque: {err}");
            std::process::exit(1);
        }
    };

    let router = rutsubo_relay::router(state);
    let listener = match tokio::net::TcpListener::bind(cfg.bind).await {
        Ok(l) => l,
        Err(err) => {
            eprintln!("rutsubo-relay: no se pudo escuchar en {}: {err}", cfg.bind);
            std::process::exit(1);
        }
    };
    tracing::info!(bind = %cfg.bind, db = %cfg.db_url, "relay listo");

    if let Err(err) = axum::serve(listener, router).await {
        eprintln!("rutsubo-relay: error del servidor: {err}");
        std::process::exit(1);
    }
}
