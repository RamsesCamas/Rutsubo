//! GET /v1/health — único endpoint sin auth (C-1).

use crate::state::App;
use axum::Json;
use axum::extract::State;
use rutsubo_core::api::{HealthResponse, ProviderHealth, RelayStatus};
use std::sync::atomic::Ordering;

pub async fn health(State(app): State<App>) -> Json<HealthResponse> {
    let has_key = app.groq_key.read().await.is_some();
    let llm = app.llm.read().await.clone();
    let mut provider = llm.status().await;
    if !has_key {
        provider.health = ProviderHealth::Down;
        provider.reason = Some("missing_api_key".into());
    }
    Json(HealthResponse {
        status: if has_key { "ok" } else { "down" }.into(),
        version: env!("CARGO_PKG_VERSION").into(),
        provider,
        relay: Some(RelayStatus {
            configured: app.cfg.relay_url.is_some(),
            connected: app.relay.connected.load(Ordering::Relaxed),
        }),
    })
}
