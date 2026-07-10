//! GET /v1/health — único endpoint sin auth (C-1).

use crate::state::App;
use axum::Json;
use axum::extract::State;
use rutsubo_core::api::HealthResponse;

pub async fn health(State(app): State<App>) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        provider: app.llm.status().await,
    })
}
