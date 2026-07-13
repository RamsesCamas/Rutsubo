//! Buzón de tareas offline (ADR-009): store-and-forward de `send_message`
//! diferidos. Es la ÚNICA excepción de persistencia de contenido del relay
//! (RNF-10); en M2 el payload va en claro → el relay no sale de LAN.
//!
//! Encolar (`POST /v1/outbox`) inserta la tarea; si el daemon está conectado
//! se entrega en el acto, si no queda `queued` y se drena al reconectar
//! (`drain_on_connect`). El daemon acusa cada tarea y el relay borra la fila
//! (`spawn_ack`). El buzón solo transporta mensajes (ADR-007: nada de
//! aprobaciones ni control).

use crate::RelayState;
use crate::auth::require_bearer;
use crate::error::RelayError;
use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::extract::ws::{Message, Utf8Bytes};
use chrono::{DateTime, Duration, Utc};
use rutsubo_core::api::{OutboxAccepted, OutboxItem, OutboxPage, OutboxRequest, OutboxTarget};
use rutsubo_core::commands::{Command, CommandEnvelope};
use rutsubo_core::envelope::PROTOCOL_VERSION;
use rutsubo_core::ids::SessionId;
use rutsubo_core::relay::ToDaemon;
use sqlx::Row;
use std::str::FromStr;
use ulid::Ulid;

const MAX_PAYLOAD_BYTES: usize = 32 * 1024;
const MAX_QUEUED_PER_ACCOUNT: i64 = 20;

/// TTL del buzón. Default 7 días; `RELAY_OUTBOX_TTL_SECS` lo acorta para probar
/// la expiración en dev.
fn ttl() -> Duration {
    std::env::var("RELAY_OUTBOX_TTL_SECS")
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .map(Duration::seconds)
        .unwrap_or_else(|| Duration::days(7))
}

/// Marca expiradas las tareas `queued` vencidas (purga perezosa).
async fn expire_stale(state: &RelayState, account_id: &str) -> Result<(), RelayError> {
    sqlx::query(
        "UPDATE outbox SET state = 'expired' \
         WHERE account_id = ? AND state = 'queued' AND expires_at < ?",
    )
    .bind(account_id)
    .bind(Utc::now().to_rfc3339())
    .execute(&state.pool)
    .await?;
    Ok(())
}

// ---- POST /v1/outbox ----

pub async fn enqueue(
    State(state): State<RelayState>,
    headers: HeaderMap,
    Json(req): Json<OutboxRequest>,
) -> Result<Json<OutboxAccepted>, RelayError> {
    let device = require_bearer(&state, &headers).await?;
    if req.payload.len() > MAX_PAYLOAD_BYTES {
        return Err(RelayError::validation("payload supera 32 KB"));
    }
    if req.payload.trim().is_empty() {
        return Err(RelayError::validation("payload vacío"));
    }
    if req.client_msg_id.is_empty() {
        return Err(RelayError::validation("client_msg_id requerido"));
    }
    expire_stale(&state, &device.account_id).await?;

    // Límite de tareas encoladas por cuenta.
    let queued: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM outbox WHERE account_id = ? AND state = 'queued'")
            .bind(&device.account_id)
            .fetch_one(&state.pool)
            .await?;
    if queued >= MAX_QUEUED_PER_ACCOUNT {
        return Err(RelayError::validation("buzón lleno (máx. 20 tareas)"));
    }

    let now = Utc::now();
    let expires_at = now + ttl();
    let id = Ulid::new().to_string();
    let session_id = req.target.session_id.as_ref().map(|s| s.to_string());

    // Idempotencia por (account, client_msg_id): un reintento devuelve la misma.
    let insert = sqlx::query(
        "INSERT INTO outbox (id, account_id, enqueued_by, target_session_id, new_session_title, \
         payload_kind, payload, client_msg_id, state, created_at, expires_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'queued', ?, ?)",
    )
    .bind(&id)
    .bind(&device.account_id)
    .bind(&device.device_id)
    .bind(&session_id)
    .bind(&req.target.new_session_title)
    .bind(&req.payload_kind)
    .bind(req.payload.as_bytes())
    .bind(&req.client_msg_id)
    .bind(now.to_rfc3339())
    .bind(expires_at.to_rfc3339())
    .execute(&state.pool)
    .await;
    if let Err(sqlx::Error::Database(db)) = &insert
        && db.is_unique_violation()
    {
        // Reintento del mismo client_msg_id: devolver la tarea existente.
        let existing: (String, String) = sqlx::query_as(
            "SELECT id, state FROM outbox WHERE account_id = ? AND client_msg_id = ?",
        )
        .bind(&device.account_id)
        .bind(&req.client_msg_id)
        .fetch_one(&state.pool)
        .await?;
        return Ok(Json(OutboxAccepted {
            outbox_id: existing.0,
            state: existing.1,
            expires_at,
        }));
    }
    insert?;

    // Entrega inmediata si el daemon de la cuenta está conectado.
    let state_str = if let Some(daemon) = state.hub.daemon_tx(&device.account_id) {
        let frame = build_frame(
            &device.device_id,
            &id,
            session_id.as_deref(),
            req.target.new_session_title.as_deref(),
            &req.payload,
            &req.client_msg_id,
        );
        let _ = daemon.try_send(Message::Text(Utf8Bytes::from(frame)));
        "delivered"
    } else {
        "queued"
    };

    Ok(Json(OutboxAccepted {
        outbox_id: id,
        state: state_str.into(),
        expires_at,
    }))
}

// ---- GET /v1/outbox ----

pub async fn list(
    State(state): State<RelayState>,
    headers: HeaderMap,
) -> Result<Json<OutboxPage>, RelayError> {
    let device = require_bearer(&state, &headers).await?;
    expire_stale(&state, &device.account_id).await?;
    let rows = sqlx::query(
        "SELECT id, target_session_id, new_session_title, state, payload_kind, created_at, expires_at \
         FROM outbox WHERE account_id = ? ORDER BY created_at DESC",
    )
    .bind(&device.account_id)
    .fetch_all(&state.pool)
    .await?;
    let items = rows
        .into_iter()
        .map(|row| {
            let session_id: Option<String> = row.get("target_session_id");
            OutboxItem {
                id: row.get("id"),
                target: OutboxTarget {
                    session_id: session_id.and_then(|s| SessionId::from_str(&s).ok()),
                    new_session_title: row.get("new_session_title"),
                },
                state: row.get("state"),
                payload_kind: row.get("payload_kind"),
                enqueued_at: parse_ts(row.get("created_at")),
                expires_at: parse_ts(row.get("expires_at")),
            }
        })
        .collect();
    Ok(Json(OutboxPage { items }))
}

// ---- DELETE /v1/outbox/{id} ----

pub async fn cancel(
    State(state): State<RelayState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<StatusCode, RelayError> {
    let device = require_bearer(&state, &headers).await?;
    // Solo se cancela si sigue `queued` y es de la cuenta.
    let deleted = sqlx::query(
        "DELETE FROM outbox WHERE id = ? AND account_id = ? AND state = 'queued'",
    )
    .bind(&id)
    .bind(&device.account_id)
    .execute(&state.pool)
    .await?;
    if deleted.rows_affected() == 0 {
        return Err(RelayError::not_found("tarea inexistente o ya entregada"));
    }
    Ok(StatusCode::NO_CONTENT)
}

// ---- Drenaje al conectar el daemon (llamado desde ws::daemon_connection) ----

/// Envía al daemon recién conectado todas las tareas `queued` de la cuenta en
/// orden FIFO. El daemon deduplica por `outbox_id` y acusa cada una.
pub async fn drain_on_connect(state: &RelayState, account_id: &str, tx: &crate::hub::Tx) {
    let _ = expire_stale(state, account_id).await;
    let rows = match sqlx::query(
        "SELECT id, enqueued_by, target_session_id, new_session_title, payload, client_msg_id \
         FROM outbox WHERE account_id = ? AND state = 'queued' ORDER BY created_at ASC",
    )
    .bind(account_id)
    .fetch_all(&state.pool)
    .await
    {
        Ok(rows) => rows,
        Err(err) => {
            tracing::error!(%err, "no se pudo leer el buzón al conectar el daemon");
            return;
        }
    };
    for row in rows {
        let payload: Vec<u8> = row.get("payload");
        let frame = build_frame(
            &row.get::<String, _>("enqueued_by"),
            &row.get::<String, _>("id"),
            row.get::<Option<String>, _>("target_session_id").as_deref(),
            row.get::<Option<String>, _>("new_session_title").as_deref(),
            &String::from_utf8_lossy(&payload),
            &row.get::<String, _>("client_msg_id"),
        );
        let _ = tx.try_send(Message::Text(Utf8Bytes::from(frame)));
    }
}

/// Acuse de una tarea entregada: borra la fila (llamado desde `route_from_daemon`,
/// que es síncrono, por eso se hace en una task).
pub fn spawn_ack(state: RelayState, account_id: String, outbox_id: String) {
    tokio::spawn(async move {
        let _ = sqlx::query("DELETE FROM outbox WHERE id = ? AND account_id = ?")
            .bind(&outbox_id)
            .bind(&account_id)
            .execute(&state.pool)
            .await;
    });
}

/// Construye el `ToDaemon{outbox_id, frame:SendMessage}` serializado.
fn build_frame(
    enqueued_by: &str,
    outbox_id: &str,
    session_id: Option<&str>,
    new_session_title: Option<&str>,
    payload: &str,
    client_msg_id: &str,
) -> String {
    let cmd = CommandEnvelope {
        v: PROTOCOL_VERSION,
        body: Command::SendMessage {
            content: payload.to_owned(),
            client_msg_id: client_msg_id.to_owned(),
        },
        session_id: session_id.and_then(|s| SessionId::from_str(s).ok()),
        ts: Utc::now(),
    };
    let frame = serde_json::to_string(&cmd).expect("CommandEnvelope siempre serializa");
    let to_daemon = ToDaemon {
        src: enqueued_by.to_owned(),
        frame,
        outbox_id: Some(outbox_id.to_owned()),
        new_session_title: new_session_title.map(str::to_owned),
        announce_sessions: None,
    };
    serde_json::to_string(&to_daemon).expect("ToDaemon siempre serializa")
}

fn parse_ts(s: String) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(&s)
        .map(|t| t.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now())
}
