//! GET /v1/health — único endpoint sin auth (C-1).

use crate::state::App;
use axum::Json;
use axum::extract::State;
use rutsubo_core::api::HealthResponse;

pub async fn health(State(app): State<App>) -> Json<HealthResponse> {
    let provider = app.provider_status.read().await.clone();
    Json(HealthResponse {
        status: "ok".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        provider,
    })
}
