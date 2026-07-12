//! Cuentas y tokens de dispositivo (C-2 §3.2): registro con Argon2id, emisión
//! de tokens opacos vinculados a (cuenta, dispositivo) y rotación (RNF-07).
//! Se persiste el sha256 del token, nunca el token en claro.

use crate::RelayState;
use crate::error::RelayError;
use argon2::Argon2;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
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

// ---- POST /v1/auth/register ----

#[derive(Deserialize)]
pub struct RegisterRequest {
    pub email: String,
    pub password: String,
}

#[derive(Serialize)]
pub struct RegisterResponse {
    pub account_id: String,
}

pub async fn register(
    State(state): State<RelayState>,
    Json(req): Json<RegisterRequest>,
) -> Result<(StatusCode, Json<RegisterResponse>), RelayError> {
    let email = req.email.trim().to_ascii_lowercase();
    if !email.contains('@') || email.len() < 3 {
        return Err(RelayError::validation("correo inválido"));
    }
    if req.password.len() < 8 {
        return Err(RelayError::validation(
            "la contraseña debe tener al menos 8 caracteres",
        ));
    }
    let mut salt_bytes = [0u8; 16];
    rand::rng().fill_bytes(&mut salt_bytes);
    let salt = SaltString::encode_b64(&salt_bytes).map_err(RelayError::internal)?;
    let hash = Argon2::default()
        .hash_password(req.password.as_bytes(), &salt)
        .map_err(RelayError::internal)?
        .to_string();
    let account_id = Ulid::new().to_string();
    let result = sqlx::query(
        "INSERT INTO accounts (id, email, password_hash, created_at) VALUES (?, ?, ?, ?)",
    )
    .bind(&account_id)
    .bind(&email)
    .bind(&hash)
    .bind(Utc::now().to_rfc3339())
    .execute(&state.pool)
    .await;
    match result {
        Ok(_) => Ok((StatusCode::CREATED, Json(RegisterResponse { account_id }))),
        Err(sqlx::Error::Database(db)) if db.is_unique_violation() => {
            Err(RelayError::validation("el correo ya está registrado"))
        }
        Err(err) => Err(err.into()),
    }
}

// ---- POST /v1/auth/token (login → token de dispositivo) ----

#[derive(Deserialize)]
pub struct TokenRequest {
    pub email: String,
    pub password: String,
    /// Nombre legible del dispositivo (p. ej. "iPhone de Ramsés").
    #[serde(default)]
    pub device_name: Option<String>,
}

#[derive(Serialize)]
pub struct TokenResponse {
    pub token: String,
    pub device_id: String,
    pub account_id: String,
}

pub async fn token(
    State(state): State<RelayState>,
    Json(req): Json<TokenRequest>,
) -> Result<Json<TokenResponse>, RelayError> {
    let email = req.email.trim().to_ascii_lowercase();
    let row = sqlx::query("SELECT id, password_hash FROM accounts WHERE email = ?")
        .bind(&email)
        .fetch_optional(&state.pool)
        .await?;
    // Credenciales malas y cuenta inexistente responden igual (401).
    let Some(row) = row else {
        return Err(RelayError::unauthorized());
    };
    let stored: String = row.get("password_hash");
    let parsed = PasswordHash::new(&stored).map_err(RelayError::internal)?;
    if Argon2::default()
        .verify_password(req.password.as_bytes(), &parsed)
        .is_err()
    {
        return Err(RelayError::unauthorized());
    }
    let account_id: String = row.get("id");

    let device_id = Ulid::new().to_string();
    let name = req.device_name.unwrap_or_default();
    sqlx::query(
        "INSERT INTO devices (id, account_id, name, kind, created_at) VALUES (?, ?, ?, 'client', ?)",
    )
    .bind(&device_id)
    .bind(&account_id)
    .bind(&name)
    .bind(Utc::now().to_rfc3339())
    .execute(&state.pool)
    .await?;

    let token = issue_token(&state, &device_id).await?;
    Ok(Json(TokenResponse {
        token,
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
