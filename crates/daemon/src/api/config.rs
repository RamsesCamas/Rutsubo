//! GET/PUT /v1/config/model (C-1, ADR-008).
//!
//! El PUT reemplaza la política completa (sin PATCH: el objeto es pequeño y
//! el reemplazo atómico evita estados mixtos). Aplica a partir de la
//! siguiente llamada al modelo; nunca interrumpe una generación en curso
//! (el agent loop lee la política al inicio de cada llamada).

use crate::error::{ApiError, ApiJson};
use crate::state::App;
use crate::store;
use axum::Json;
use axum::extract::State;
use chrono::Utc;
use rutsubo_core::api::{ModelConfig, ModelPolicy};
use serde_json::json;

pub async fn get_model(State(app): State<App>) -> Json<ModelConfig> {
    Json(app.model_config.read().await.clone())
}

pub async fn put_model(
    State(app): State<App>,
    ApiJson(req): ApiJson<ModelConfig>,
) -> Result<Json<ModelConfig>, ApiError> {
    if req.policy == ModelPolicy::ExternalOnly && app.cfg.external_api_key.is_none() {
        return Err(ApiError::validation(
            "external_only requiere credenciales configuradas (RUTSUBO_EXTERNAL_API_KEY)",
            Some(json!({"field": "policy"})),
        ));
    }
    if !req.local.endpoint.starts_with("http://") && !req.local.endpoint.starts_with("https://") {
        return Err(ApiError::validation(
            "local.endpoint debe ser una URL http(s)",
            Some(json!({"field": "local.endpoint"})),
        ));
    }
    if req.fallback.failure_window == 0 {
        return Err(ApiError::validation(
            "fallback.failure_window debe ser ≥ 1",
            Some(json!({"field": "fallback.failure_window"})),
        ));
    }

    store::config::save_model(&app.pool, &req).await?;
    *app.model_config.write().await = req.clone();

    store::audit::insert(
        &app.pool,
        None,
        "config",
        &json!({
            "what": "model_policy_replaced",
            "policy": req.policy,
            "local_model": req.local.model,
            "external_model": req.external.model,
        }),
        Utc::now(),
    )
    .await?;

    Ok(Json(req))
}
