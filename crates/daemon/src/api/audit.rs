//! GET /v1/audit (RF-05, RF-22): consulta paginada con filtros por sesión y
//! proveedor. Lectura pura.

use crate::error::{ApiError, ApiQuery};
use crate::state::App;
use crate::store;
use axum::Json;
use axum::extract::State;
use rutsubo_core::api::{AuditPage, AuditQuery};

pub async fn query(
    State(app): State<App>,
    ApiQuery(q): ApiQuery<AuditQuery>,
) -> Result<Json<AuditPage>, ApiError> {
    let limit = i64::from(q.limit.unwrap_or(50).clamp(1, 200));
    let (entries, next_cursor) = store::audit::query(&app.pool, &q, limit).await?;
    Ok(Json(AuditPage {
        entries,
        next_cursor,
    }))
}
