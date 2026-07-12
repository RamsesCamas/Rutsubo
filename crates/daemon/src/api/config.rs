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
use rutsubo_core::api::ModelConfig;
use serde_json::json;

pub async fn get_model(State(app): State<App>) -> Json<ModelConfig> {
    Json(app.model_config.read().await.clone())
}

pub async fn put_model(
    State(app): State<App>,
    ApiJson(req): ApiJson<ModelConfig>,
) -> Result<Json<ModelConfig>, ApiError> {
    if app.groq_key.read().await.is_none() {
        return Err(ApiError::validation(
            "GROQ_API_KEY no está configurada",
            Some(json!({"field": "GROQ_API_KEY"})),
        ));
    }
    if req.primary.provider != "groq" || req.fallback.provider != "groq" {
        return Err(ApiError::validation(
            "primary.provider y fallback.provider deben ser groq",
            Some(json!({"field": "provider"})),
        ));
    }
    if req.primary.model.trim().is_empty() || req.fallback.model.trim().is_empty() {
        return Err(ApiError::validation(
            "los modelos no pueden estar vacíos",
            Some(json!({"field": "model"})),
        ));
    }
    if req.thresholds.failure_window == 0 {
        return Err(ApiError::validation(
            "thresholds.failure_window debe ser ≥ 1",
            Some(json!({"field": "thresholds.failure_window"})),
        ));
    }

    store::config::save_model(&app.pool, &req).await?;
    *app.model_config.write().await = req.clone();

    store::audit::insert(
        &app.pool,
        None,
        "config",
        &json!({
            "what": "model_config_replaced",
            "primary_model": req.primary.model,
            "fallback_model": req.fallback.model,
        }),
        Utc::now(),
    )
    .await?;

    Ok(Json(req))
}
