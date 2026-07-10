//! Persistencia y emisión de eventos (C-3).
//!
//! Única puerta de escritura de eventos: `append` incrementa `last_seq` **en
//! la misma transacción** que inserta el evento — esa atomicidad es lo que
//! garantiza `seq` monótono y sin huecos por sesión.

use chrono::Utc;
use rutsubo_core::envelope::Envelope;
use rutsubo_core::events::{Event, SessionState};
use rutsubo_core::ids::SessionId;
use sqlx::SqlitePool;

#[derive(Debug, thiserror::Error)]
pub enum AppendError {
    #[error("sesión no encontrada")]
    SessionNotFound,
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// Persiste `event` con el siguiente `seq` de la sesión y devuelve el sobre
/// completo. Si `new_state` viene, la transición de estado ocurre en la misma
/// transacción (consistencia estado ↔ evento `session_state`).
pub async fn append(
    pool: &SqlitePool,
    session_id: SessionId,
    event: Event,
    new_state: Option<SessionState>,
) -> Result<Envelope<Event>, AppendError> {
    let sid = session_id.to_string();
    let mut tx = pool.begin().await?;

    if let Some(state) = new_state {
        let state = super::state_to_str(state);
        sqlx::query!("UPDATE sessions SET state = ? WHERE id = ?", state, sid)
            .execute(&mut *tx)
            .await?;
    }

    let seq = sqlx::query_scalar!(
        r#"UPDATE sessions SET last_seq = last_seq + 1 WHERE id = ? RETURNING last_seq as "last_seq!: i64""#,
        sid,
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppendError::SessionNotFound)?;

    let envelope = Envelope::event(event, Some(session_id), seq as u64, Utc::now());
    let kind = envelope.body.kind();
    let payload = serde_json::to_string(&envelope).expect("los eventos siempre serializan");
    let ts = envelope.ts.to_rfc3339();
    sqlx::query!(
        "INSERT INTO events (session_id, seq, type, payload, ts) VALUES (?, ?, ?, ?, ?)",
        sid,
        seq,
        kind,
        payload,
        ts,
    )
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(envelope)
}

/// Replay: eventos con `seq` estrictamente mayor a `after_seq`, ascendente,
/// hasta `limit`. Lectura pura, sin efectos secundarios (C-1).
pub async fn replay(
    pool: &SqlitePool,
    session_id: &SessionId,
    after_seq: u64,
    limit: i64,
) -> Result<Vec<Envelope<Event>>, sqlx::Error> {
    let sid = session_id.to_string();
    let after = after_seq as i64;
    let rows = sqlx::query_scalar!(
        r#"SELECT payload as "payload!: String" FROM events
           WHERE session_id = ? AND seq > ?
           ORDER BY seq ASC LIMIT ?"#,
        sid,
        after,
        limit,
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .iter()
        .filter_map(|p| serde_json::from_str(p).ok())
        .collect())
}
