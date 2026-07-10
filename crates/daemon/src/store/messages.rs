//! Mensajes de la conversación (RF-02, RF-04) con idempotencia por
//! `client_msg_id` (C-1).

use chrono::{DateTime, Utc};
use rutsubo_core::ids::{MessageId, SessionId};
use sqlx::SqlitePool;

/// Resultado de insertar un mensaje de usuario.
pub enum InsertOutcome {
    /// Insertado; el turno debe iniciar.
    Inserted,
    /// Ya existía un mensaje con ese `client_msg_id` en la sesión: se
    /// devuelve el `message_id` original sin reprocesar (C-1).
    Duplicate(MessageId),
}

pub async fn insert_user(
    pool: &SqlitePool,
    session_id: &SessionId,
    message_id: &MessageId,
    content: &str,
    client_msg_id: &str,
    created_at: DateTime<Utc>,
) -> Result<InsertOutcome, sqlx::Error> {
    let sid = session_id.to_string();
    let mid = message_id.to_string();
    let ts = created_at.to_rfc3339();
    let res = sqlx::query!(
        "INSERT INTO messages (id, session_id, role, content, client_msg_id, created_at)
         VALUES (?, ?, 'user', ?, ?, ?)
         ON CONFLICT (session_id, client_msg_id) DO NOTHING",
        mid,
        sid,
        content,
        client_msg_id,
        ts,
    )
    .execute(pool)
    .await?;

    if res.rows_affected() > 0 {
        return Ok(InsertOutcome::Inserted);
    }
    let existing = sqlx::query_scalar!(
        r#"SELECT id as "id!: String" FROM messages WHERE session_id = ? AND client_msg_id = ?"#,
        sid,
        client_msg_id,
    )
    .fetch_one(pool)
    .await?;
    let original = existing
        .parse()
        .map_err(|e| sqlx::Error::Decode(Box::new(std::io::Error::other(format!("{e:?}")))))?;
    Ok(InsertOutcome::Duplicate(original))
}

pub async fn insert_assistant(
    pool: &SqlitePool,
    session_id: &SessionId,
    message_id: &MessageId,
    content: &str,
    created_at: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    let sid = session_id.to_string();
    let mid = message_id.to_string();
    let ts = created_at.to_rfc3339();
    sqlx::query!(
        "INSERT INTO messages (id, session_id, role, content, client_msg_id, created_at)
         VALUES (?, ?, 'assistant', ?, NULL, ?)",
        mid,
        sid,
        content,
        ts,
    )
    .execute(pool)
    .await?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct MessageRow {
    pub role: String,
    pub content: String,
}

/// Historial en orden de creación (id ULID ascendente) para construir el
/// contexto del modelo (RF-13).
pub async fn history(
    pool: &SqlitePool,
    session_id: &SessionId,
) -> Result<Vec<MessageRow>, sqlx::Error> {
    let sid = session_id.to_string();
    let rows = sqlx::query!(
        r#"SELECT role as "role!: String", content as "content!: String"
           FROM messages WHERE session_id = ? ORDER BY id ASC"#,
        sid,
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| MessageRow {
            role: r.role,
            content: r.content,
        })
        .collect())
}
