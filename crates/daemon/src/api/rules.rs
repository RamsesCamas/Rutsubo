//! GET/PUT /v1/rules (RF-18). Persistencia completa; la evaluación de reglas
//! en la compuerta queda como TODO(fase-3) documentado.

use crate::error::{ApiError, ApiJson};
use crate::state::App;
use crate::store;
use axum::Json;
use axum::extract::State;
use chrono::Utc;
use rutsubo_core::api::{PutRulesRequest, RulesPage};
use serde_json::json;

const ALLOWED_TOOLS: [&str; 3] = ["run_shell", "write_file", "edit_file"];

pub async fn get_rules(State(app): State<App>) -> Result<Json<RulesPage>, ApiError> {
    let rules = store::rules::list(&app.pool).await?;
    Ok(Json(RulesPage { rules }))
}

/// PUT: reemplazo completo del conjunto (mismas convenciones que
/// /v1/config/model: sin PATCH parcial).
pub async fn put_rules(
    State(app): State<App>,
    ApiJson(req): ApiJson<PutRulesRequest>,
) -> Result<Json<RulesPage>, ApiError> {
    for (i, rule) in req.rules.iter().enumerate() {
        if !ALLOWED_TOOLS.contains(&rule.tool.as_str()) {
            return Err(ApiError::validation(
                format!(
                    "rules[{i}].tool debe ser una herramienta con efectos secundarios: {ALLOWED_TOOLS:?}"
                ),
                Some(json!({"field": format!("rules[{i}].tool")})),
            ));
        }
        if rule.pattern.is_empty() || rule.workspace_path.is_empty() {
            return Err(ApiError::validation(
                format!("rules[{i}]: pattern y workspace_path son obligatorios"),
                Some(json!({"field": format!("rules[{i}]")})),
            ));
        }
    }
    store::rules::replace_all(&app.pool, &req.rules, Utc::now()).await?;
    let rules = store::rules::list(&app.pool).await?;
    Ok(Json(RulesPage { rules }))
}
