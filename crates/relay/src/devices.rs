//! Dispositivos vinculados (C-2): listado con última conexión y revocación
//! por dispositivo (invalida solo su token, RNF-07).

use crate::RelayState;
use crate::auth::require_bearer;
use crate::error::RelayError;
use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use serde::Serialize;
use sqlx::Row;

#[derive(Serialize)]
pub struct DeviceDto {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_seen_at: Option<String>,
    /// true para el dispositivo que hace la consulta.
    pub current: bool,
}

#[derive(Serialize)]
pub struct DevicesResponse {
    pub devices: Vec<DeviceDto>,
}

pub async fn list(
    State(state): State<RelayState>,
    headers: HeaderMap,
) -> Result<Json<DevicesResponse>, RelayError> {
    let device = require_bearer(&state, &headers).await?;
    let rows = sqlx::query(
        "SELECT id, name, kind, created_at, last_seen_at FROM devices \
         WHERE account_id = ? AND revoked_at IS NULL ORDER BY created_at",
    )
    .bind(&device.account_id)
    .fetch_all(&state.pool)
    .await?;
    let devices = rows
        .into_iter()
        .map(|row| {
            let id: String = row.get("id");
            DeviceDto {
                current: id == device.device_id,
                id,
                name: row.get("name"),
                kind: row.get("kind"),
                created_at: row.get("created_at"),
                last_seen_at: row.get("last_seen_at"),
            }
        })
        .collect();
    Ok(Json(DevicesResponse { devices }))
}

pub async fn revoke(
    State(state): State<RelayState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<StatusCode, RelayError> {
    let device = require_bearer(&state, &headers).await?;
    // Solo dispositivos de la propia cuenta; ajenos responden 404 (no se
    // filtra su existencia).
    let updated = sqlx::query(
        "UPDATE devices SET revoked_at = ? WHERE id = ? AND account_id = ? AND revoked_at IS NULL",
    )
    .bind(chrono::Utc::now().to_rfc3339())
    .bind(&id)
    .bind(&device.account_id)
    .execute(&state.pool)
    .await?;
    if updated.rows_affected() == 0 {
        return Err(RelayError::not_found("dispositivo desconocido"));
    }
    sqlx::query("DELETE FROM tokens WHERE device_id = ?")
        .bind(&id)
        .execute(&state.pool)
        .await?;
    // Si estaba conectado, se le cierra el socket en el acto.
    state.hub.disconnect_device(&device.account_id, &id);
    Ok(StatusCode::NO_CONTENT)
}
