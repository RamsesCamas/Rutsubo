//! Pairing daemon ↔ cuenta (C-2 §3.2.2, RF-24, RNF-08): prueba de posesión de
//! ambos extremos. La app de escritorio (autenticada) registra la pubkey
//! efímera del daemon y recibe un código de un solo uso; el daemon lo reclama
//! firmándolo con su clave privada. Un observador del relay no puede reclamar
//! el código sin la clave.

use crate::RelayState;
use crate::auth::{issue_token, require_bearer};
use crate::error::RelayError;
use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use chrono::{DateTime, Duration, Utc};
use ed25519_dalek::{Signature, VerifyingKey};
use rand::Rng;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use ulid::Ulid;

/// Alfabeto sin símbolos ambiguos (sin 0/O, 1/I/L, U/V confusables).
const CODE_ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTVWXYZ23456789";
/// TTL del código (C-2: 5 minutos).
const CODE_TTL_MINUTES: i64 = 5;
/// Intentos de reclamo fallidos permitidos por código; después 429.
const MAX_CLAIM_ATTEMPTS: i64 = 5;

fn generate_code() -> String {
    let mut rng = rand::rng();
    let mut symbol = || CODE_ALPHABET[rng.random_range(0..CODE_ALPHABET.len())] as char;
    let mut code = String::with_capacity(11);
    for group in 0..3 {
        if group > 0 {
            code.push('-');
        }
        for _ in 0..3 {
            code.push(symbol());
        }
    }
    code
}

// ---- POST /v1/pairing/codes (escritorio, autenticado) ----

#[derive(Deserialize)]
pub struct CreateCodeRequest {
    /// base64(clave pública Ed25519 efímera generada por el daemon).
    pub daemon_pubkey: String,
}

#[derive(Serialize)]
pub struct CreateCodeResponse {
    pub code: String,
    pub expires_at: DateTime<Utc>,
    pub single_use: bool,
}

pub async fn create_code(
    State(state): State<RelayState>,
    headers: HeaderMap,
    Json(req): Json<CreateCodeRequest>,
) -> Result<(StatusCode, Json<CreateCodeResponse>), RelayError> {
    let device = require_bearer(&state, &headers).await?;
    // Validar que la pubkey es Ed25519 bien formada antes de persistirla.
    parse_pubkey(&req.daemon_pubkey)?;

    let code = generate_code();
    let expires_at = Utc::now() + Duration::minutes(CODE_TTL_MINUTES);
    sqlx::query(
        "INSERT INTO pairing_codes (code, account_id, daemon_pubkey, expires_at, created_at) \
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(&code)
    .bind(&device.account_id)
    .bind(&req.daemon_pubkey)
    .bind(expires_at.to_rfc3339())
    .bind(Utc::now().to_rfc3339())
    .execute(&state.pool)
    .await?;

    Ok((
        StatusCode::CREATED,
        Json(CreateCodeResponse {
            code,
            expires_at,
            single_use: true,
        }),
    ))
}

fn parse_pubkey(b64: &str) -> Result<VerifyingKey, RelayError> {
    let bytes = B64
        .decode(b64)
        .map_err(|_| RelayError::validation("daemon_pubkey no es base64 válido"))?;
    let bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|_| RelayError::validation("daemon_pubkey debe ser Ed25519 (32 bytes)"))?;
    VerifyingKey::from_bytes(&bytes)
        .map_err(|_| RelayError::validation("daemon_pubkey no es una clave Ed25519 válida"))
}

// ---- POST /v1/pairing/claim (daemon, sin token aún) ----

#[derive(Deserialize)]
pub struct ClaimRequest {
    pub code: String,
    /// base64(sign(code, daemon_privkey)).
    pub signature: String,
}

#[derive(Serialize)]
pub struct ClaimResponse {
    pub daemon_token: String,
    pub account_id: String,
    pub device_id: String,
}

pub async fn claim(
    State(state): State<RelayState>,
    Json(req): Json<ClaimRequest>,
) -> Result<Json<ClaimResponse>, RelayError> {
    let row = sqlx::query(
        "SELECT account_id, daemon_pubkey, expires_at, used_at, attempts \
         FROM pairing_codes WHERE code = ?",
    )
    .bind(&req.code)
    .fetch_optional(&state.pool)
    .await?;
    let Some(row) = row else {
        return Err(RelayError::not_found("código de pairing desconocido"));
    };

    let attempts: i64 = row.get("attempts");
    if attempts >= MAX_CLAIM_ATTEMPTS {
        return Err(RelayError::rate_limited(300));
    }

    let used_at: Option<String> = row.get("used_at");
    let expires_at: String = row.get("expires_at");
    let expired = DateTime::parse_from_rfc3339(&expires_at)
        .map(|t| t < Utc::now())
        .unwrap_or(true);
    if used_at.is_some() || expired {
        return Err(RelayError::pairing_expired());
    }

    // Prueba de posesión: la firma del código con la privada cuya pública
    // registró el escritorio autenticado (RNF-08).
    let pubkey = parse_pubkey(&row.get::<String, _>("daemon_pubkey"))?;
    let signature_ok = B64
        .decode(&req.signature)
        .ok()
        .and_then(|bytes| Signature::from_slice(&bytes).ok())
        .is_some_and(|sig| pubkey.verify_strict(req.code.as_bytes(), &sig).is_ok());
    if !signature_ok {
        sqlx::query("UPDATE pairing_codes SET attempts = attempts + 1 WHERE code = ?")
            .bind(&req.code)
            .execute(&state.pool)
            .await?;
        return Err(RelayError::validation("firma inválida"));
    }

    // Reclamo exitoso: el código se consume y nace el device del daemon.
    let account_id: String = row.get("account_id");
    let device_id = Ulid::new().to_string();
    let mut tx = state.pool.begin().await?;
    let consumed =
        sqlx::query("UPDATE pairing_codes SET used_at = ? WHERE code = ? AND used_at IS NULL")
            .bind(Utc::now().to_rfc3339())
            .bind(&req.code)
            .execute(&mut *tx)
            .await?;
    if consumed.rows_affected() == 0 {
        // Carrera: otro reclamo llegó primero (un solo uso).
        return Err(RelayError::pairing_expired());
    }
    sqlx::query(
        "INSERT INTO devices (id, account_id, name, kind, created_at) \
         VALUES (?, ?, 'daemon', 'daemon', ?)",
    )
    .bind(&device_id)
    .bind(&account_id)
    .bind(Utc::now().to_rfc3339())
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    let daemon_token = issue_token(&state, &device_id).await?;
    Ok(Json(ClaimResponse {
        daemon_token,
        account_id,
        device_id,
    }))
}
