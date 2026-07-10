//! GET /v1/health — único endpoint sin auth (C-1).

use crate::state::App;
use axum::Json;
use axum::extract::State;
use rutsubo_core::api::{HealthResponse, ProviderHealth};

pub async fn health(State(app): State<App>) -> Json<HealthResponse> {
    let mut provider = app.llm.status().await;
    if app.cfg.groq_api_key.is_none() {
        provider.health = ProviderHealth::Down;
        provider.reason = Some("missing_api_key".into());
    }
    Json(HealthResponse {
        status: if app.cfg.groq_api_key.is_some() {
            "ok"
        } else {
            "down"
        }
        .into(),
        version: env!("CARGO_PKG_VERSION").into(),
        provider,
    })
}
