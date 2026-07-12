//! Operación del pairing C-2 desde el lado del daemon (rutas protegidas):
//! `GET /v1/relay/status` expone la pubkey de pairing y el estado de la
//! conexión; `POST /v1/relay/pair {code}` reclama el código contra el relay.
//!
//! Flujo completo (C-2 §3.2.2): la superficie de escritorio, autenticada en
//! el relay, lee aquí la pubkey → `POST relay /v1/pairing/codes` → entrega el
//! código a este endpoint → el daemon firma y reclama → conexión saliente.

use crate::error::ApiError;
use crate::state::App;
use axum::Json;
use axum::extract::State;
use serde::Deserialize;
use std::sync::atomic::Ordering;

pub async fn status(State(app): State<App>) -> Result<Json<serde_json::Value>, ApiError> {
    let configured = app.cfg.relay_url.is_some();
    let pubkey = crate::relay::pubkey_b64(&app.cfg.data_dir).map_err(ApiError::internal)?;
    Ok(Json(serde_json::json!({
        "configured": configured,
        "connected": app.relay.connected.load(Ordering::Relaxed),
        "relay_url": app.cfg.relay_url,
        "pubkey_b64": pubkey,
    })))
}

#[derive(Deserialize)]
pub struct PairRequest {
    pub code: String,
}

pub async fn pair(
    State(app): State<App>,
    Json(req): Json<PairRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let code = req.code.trim();
    if code.is_empty() {
        return Err(ApiError::validation("code es obligatorio", None));
    }
    let linked = crate::relay::pair(&app, code).await?;
    Ok(Json(linked))
}
