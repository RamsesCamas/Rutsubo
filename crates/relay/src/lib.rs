//! Relay de Rutsubo (contrato C-2, ADR-006).
//!
//! Tubería pub/sub con enrutamiento por cuenta: el daemon mantiene una
//! conexión WebSocket saliente (`/v1/connect`) y los clientes se suscriben
//! (`/v1/subscribe`); el relay reenvía el tráfico C-3 como carga opaca sin
//! deserializar más allá del encabezado de enrutamiento (RNF-10). Persiste
//! únicamente cuentas, dispositivos, tokens y códigos de pairing.

pub mod auth;
pub mod config;
pub mod devices;
pub mod error;
pub mod google;
pub mod hub;
pub mod outbox;
pub mod pairing;
pub mod ws;

use axum::Router;
use axum::routing::{delete, get, post};
use sqlx::SqlitePool;
use std::sync::Arc;

#[derive(Clone)]
pub struct RelayState {
    pub pool: SqlitePool,
    pub hub: Arc<hub::Hub>,
    /// Verificador del id_token de Google (real JWKS o dev).
    pub verifier: Arc<google::Verifier>,
    /// Client IDs aceptados como `aud` (de `GOOGLE_CLIENT_IDS`).
    pub google_client_ids: Arc<Vec<String>>,
}

/// Arranque con la config por defecto (usado en tests: modo dev, sin client IDs).
pub async fn bootstrap(db_url: &str) -> Result<RelayState, Box<dyn std::error::Error>> {
    bootstrap_with(db_url, true, Vec::new()).await
}

pub async fn bootstrap_with(
    db_url: &str,
    google_dev: bool,
    google_client_ids: Vec<String>,
) -> Result<RelayState, Box<dyn std::error::Error>> {
    let pool = SqlitePool::connect(db_url).await?;
    sqlx::migrate!("./migrations").run(&pool).await?;
    Ok(RelayState {
        pool,
        hub: Arc::new(hub::Hub::default()),
        verifier: Arc::new(google::Verifier::from_config(google_dev)),
        google_client_ids: Arc::new(google_client_ids),
    })
}

pub fn router(state: RelayState) -> Router {
    // CORS permisivo: el relay no usa cookies (solo Bearer) y sus clientes
    // navegador legítimos llegan desde origins variados (tauri://localhost,
    // flutter web en puerto de desarrollo). Sin credenciales no hay CSRF.
    let cors = tower_http::cors::CorsLayer::permissive();

    Router::new()
        .route("/v1/health", get(health))
        // Identidad Google (sustituye register/token de password).
        .route("/v1/auth/google", post(auth::google))
        .route("/v1/auth/token/rotate", post(auth::rotate))
        .route("/v1/pairing/codes", post(pairing::create_code))
        .route("/v1/pairing/claim", post(pairing::claim))
        .route("/v1/devices", get(devices::list))
        .route("/v1/devices/{id}", delete(devices::revoke))
        // Buzón de tareas offline (ADR-009).
        .route("/v1/outbox", post(outbox::enqueue).get(outbox::list))
        .route("/v1/outbox/{id}", delete(outbox::cancel))
        .route("/v1/connect", get(ws::connect))
        .route("/v1/subscribe", get(ws::subscribe))
        .layer(cors)
        .with_state(state)
}

/// GET /v1/health — liveness del relay (sin auth, C-2).
async fn health() -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}
