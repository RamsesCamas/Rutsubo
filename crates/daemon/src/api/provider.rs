//! Credencial del proveedor de modelo (GET/PUT /v1/config/provider).
//!
//! Permite configurar la API key de Groq desde la UI en lugar de depender de
//! una variable de entorno / `.env`. La key se persiste en la DB local del
//! daemon y el adapter se reconstruye en caliente (`reconfigure_provider`).
//! Nunca se devuelve la key: solo su estado.

use crate::error::{ApiError, ApiJson};
use crate::state::App;
use axum::Json;
use axum::extract::State;
use chrono::Utc;
use rutsubo_core::api::{ProviderKeyStatus, SetProviderKeyRequest};
use serde_json::json;

pub async fn get_provider(State(app): State<App>) -> Json<ProviderKeyStatus> {
    let has_key = app.groq_key.read().await.is_some();
    // `stored` si hay una key persistida; si no, `env` cuando el proceso la
    // heredó del entorno; `none` si no hay ninguna.
    let stored = crate::store::config::load_provider_key(&app.pool)
        .await
        .ok()
        .flatten()
        .is_some();
    let source = if stored {
        "stored"
    } else if has_key {
        "env"
    } else {
        "none"
    };
    Json(ProviderKeyStatus {
        configured: has_key,
        source: source.into(),
    })
}

pub async fn put_provider(
    State(app): State<App>,
    ApiJson(req): ApiJson<SetProviderKeyRequest>,
) -> Result<Json<ProviderKeyStatus>, ApiError> {
    let key = req.groq_api_key.filter(|k| !k.trim().is_empty());
    // Validación mínima de forma para evitar guardar basura obvia.
    if let Some(k) = &key
        && !k.starts_with("gsk_")
    {
        return Err(ApiError::validation(
            "la API key de Groq debe empezar por `gsk_`",
            Some(json!({"field": "groq_api_key"})),
        ));
    }

    app.reconfigure_provider(key.clone())
        .await
        .map_err(ApiError::internal)?;

    crate::store::audit::insert(
        &app.pool,
        None,
        "config",
        &json!({"what": "provider_key_changed", "configured": key.is_some()}),
        Utc::now(),
    )
    .await
    .map_err(ApiError::internal)?;

    Ok(Json(ProviderKeyStatus {
        configured: key.is_some(),
        source: if key.is_some() { "stored" } else { "none" }.into(),
    }))
}
