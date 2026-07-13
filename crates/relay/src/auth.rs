//! Cuentas y tokens de dispositivo (C-2 §3.2 enmendado): identidad Google,
//! emisión de tokens opacos vinculados a (cuenta, dispositivo) y rotación
//! (RNF-07). Se persiste el sha256 del token, nunca el token en claro.

use crate::RelayState;
use crate::error::RelayError;
use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, header};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::Utc;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::Row;
use ulid::Ulid;

// ---- Emisión y verificación de tokens ----

/// Prefijo de los tokens del relay (diagnóstico; no aporta seguridad).
const TOKEN_PREFIX: &str = "rtb_";

pub fn new_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    format!("{TOKEN_PREFIX}{}", URL_SAFE_NO_PAD.encode(bytes))
}

pub fn token_hash(token: &str) -> String {
    format!("{:x}", Sha256::digest(token.as_bytes()))
}

/// Dispositivo autenticado por Bearer (o `?token=` en handshakes WS).
#[derive(Debug, Clone)]
pub struct AuthedDevice {
    pub device_id: String,
    pub account_id: String,
    pub kind: String,
}

pub fn bearer_from_headers(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::to_owned)
}

/// Resuelve un token a su dispositivo, si no está revocado. Actualiza
/// `last_seen_at` (para `GET /v1/devices`).
pub async fn authenticate(state: &RelayState, token: &str) -> Result<AuthedDevice, RelayError> {
    let hash = token_hash(token);
    let row = sqlx::query(
        "SELECT d.id, d.account_id, d.kind FROM tokens t \
         JOIN devices d ON d.id = t.device_id \
         WHERE t.token_hash = ? AND d.revoked_at IS NULL",
    )
    .bind(&hash)
    .fetch_optional(&state.pool)
    .await?;
    let Some(row) = row else {
        return Err(RelayError::unauthorized());
    };
    let device = AuthedDevice {
        device_id: row.get("id"),
        account_id: row.get("account_id"),
        kind: row.get("kind"),
    };
    sqlx::query("UPDATE devices SET last_seen_at = ? WHERE id = ?")
        .bind(Utc::now().to_rfc3339())
        .bind(&device.device_id)
        .execute(&state.pool)
        .await?;
    Ok(device)
}

pub async fn require_bearer(
    state: &RelayState,
    headers: &HeaderMap,
) -> Result<AuthedDevice, RelayError> {
    let token = bearer_from_headers(headers).ok_or_else(RelayError::unauthorized)?;
    authenticate(state, &token).await
}

// ---- POST /v1/auth/google (canje id_token → device_token) ----

#[derive(Deserialize)]
pub struct GoogleRequest {
    pub id_token: String,
    #[serde(default)]
    pub device: DeviceInfo,
}

#[derive(Deserialize, Default)]
pub struct DeviceInfo {
    /// mobile | desktop | web (se guarda en `devices.platform`).
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Serialize)]
pub struct GoogleResponse {
    pub device_token: String,
    pub device_id: String,
    pub account_id: String,
}

pub async fn google(
    State(state): State<RelayState>,
    Json(req): Json<GoogleRequest>,
) -> Result<Json<GoogleResponse>, RelayError> {
    // Verifica el id_token (real JWKS o dev) contra los client IDs aceptados.
    let claims = state
        .verifier
        .verify(&req.id_token, &state.google_client_ids)
        .await?;

    // La cuenta se ancla al `sub` de Google (estable). Upsert.
    let existing: Option<String> = sqlx::query_scalar("SELECT id FROM accounts WHERE google_sub = ?")
        .bind(&claims.sub)
        .fetch_optional(&state.pool)
        .await?;
    let account_id = match existing {
        Some(id) => id,
        None => {
            let id = Ulid::new().to_string();
            // password_hash es NOT NULL sin uso en cuentas Google → placeholder ''.
            sqlx::query(
                "INSERT INTO accounts (id, email, password_hash, google_sub, created_at) \
                 VALUES (?, ?, '', ?, ?)",
            )
            .bind(&id)
            .bind(&claims.email)
            .bind(&claims.sub)
            .bind(Utc::now().to_rfc3339())
            .execute(&state.pool)
            .await?;
            id
        }
    };

    // Cada login crea un device `client` con su plataforma.
    let device_id = Ulid::new().to_string();
    let name = req.device.name.unwrap_or_default();
    let platform = req.device.kind.unwrap_or_else(|| "web".into());
    sqlx::query(
        "INSERT INTO devices (id, account_id, name, kind, platform, created_at) \
         VALUES (?, ?, ?, 'client', ?, ?)",
    )
    .bind(&device_id)
    .bind(&account_id)
    .bind(&name)
    .bind(&platform)
    .bind(Utc::now().to_rfc3339())
    .execute(&state.pool)
    .await?;

    let device_token = issue_token(&state, &device_id).await?;
    Ok(Json(GoogleResponse {
        device_token,
        device_id,
        account_id,
    }))
}

/// Inserta un token nuevo para el dispositivo y devuelve el valor en claro.
pub async fn issue_token(state: &RelayState, device_id: &str) -> Result<String, RelayError> {
    let token = new_token();
    sqlx::query("INSERT INTO tokens (id, device_id, token_hash, created_at) VALUES (?, ?, ?, ?)")
        .bind(Ulid::new().to_string())
        .bind(device_id)
        .bind(token_hash(&token))
        .bind(Utc::now().to_rfc3339())
        .execute(&state.pool)
        .await?;
    Ok(token)
}

// ---- POST /v1/auth/token/rotate ----

#[derive(Serialize)]
pub struct RotateResponse {
    pub token: String,
}

pub async fn rotate(
    State(state): State<RelayState>,
    headers: HeaderMap,
) -> Result<Json<RotateResponse>, RelayError> {
    let device = require_bearer(&state, &headers).await?;
    // El token anterior deja de valer en cuanto existe el nuevo (RNF-07).
    sqlx::query("DELETE FROM tokens WHERE device_id = ?")
        .bind(&device.device_id)
        .execute(&state.pool)
        .await?;
    let token = issue_token(&state, &device.device_id).await?;
    Ok(Json(RotateResponse { token }))
}
