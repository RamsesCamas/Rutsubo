//! Sesiones (RF-01).

use chrono::{DateTime, Utc};
use rutsubo_core::api::{SessionDto, SessionsQuery};
use rutsubo_core::events::SessionState;
use rutsubo_core::ids::SessionId;
use sqlx::SqlitePool;
use std::str::FromStr;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SessionRow {
    pub id: String,
    pub workspace_path: String,
    pub title: String,
    pub state: String,
    pub created_at: String,
    pub last_seq: i64,
}

impl SessionRow {
    pub fn to_dto(&self) -> Option<SessionDto> {
        Some(SessionDto {
            id: SessionId::from_str(&self.id).ok()?,
            workspace_path: self.workspace_path.clone(),
            title: self.title.clone(),
            state: super::state_from_str(&self.state)?,
            created_at: DateTime::parse_from_rfc3339(&self.created_at)
                .ok()?
                .with_timezone(&Utc),
            last_seq: self.last_seq as u64,
        })
    }

    pub fn session_state(&self) -> Option<SessionState> {
        super::state_from_str(&self.state)
    }
}

pub async fn create(
    pool: &SqlitePool,
    id: &SessionId,
    workspace_path: &str,
    title: &str,
    created_at: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    let id = id.to_string();
    let ts = created_at.to_rfc3339();
    sqlx::query!(
        "INSERT INTO sessions (id, workspace_path, title, state, created_at, last_seq)
         VALUES (?, ?, ?, 'idle', ?, 0)",
        id,
        workspace_path,
        title,
        ts,
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn get(pool: &SqlitePool, id: &SessionId) -> Result<Option<SessionRow>, sqlx::Error> {
    let id = id.to_string();
    sqlx::query_as!(
        SessionRow,
        r#"SELECT id as "id!", workspace_path as "workspace_path!", title as "title!",
                  state as "state!", created_at as "created_at!", last_seq as "last_seq!"
           FROM sessions WHERE id = ?"#,
        id,
    )
    .fetch_optional(pool)
    .await
}

/// Listado paginado por cursor (id ULID descendente: más recientes primero)
/// con filtro opcional por estado. Dinámico → QueryBuilder (la verificación
/// en compilación cubre las consultas estáticas del resto del módulo).
pub async fn list(
    pool: &SqlitePool,
    query: &SessionsQuery,
    limit: i64,
) -> Result<Vec<SessionRow>, sqlx::Error> {
    let mut qb = sqlx::QueryBuilder::new(
        "SELECT id, workspace_path, title, state, created_at, last_seq FROM sessions WHERE 1=1",
    );
    if let Some(cursor) = &query.cursor {
        qb.push(" AND id < ").push_bind(cursor);
    }
    if let Some(state) = query.state {
        qb.push(" AND state = ")
            .push_bind(super::state_to_str(state));
    }
    qb.push(" ORDER BY id DESC LIMIT ").push_bind(limit);
    qb.build_query_as::<SessionRow>().fetch_all(pool).await
}

pub async fn set_title(
    pool: &SqlitePool,
    id: &SessionId,
    title: &str,
) -> Result<bool, sqlx::Error> {
    let id = id.to_string();
    let res = sqlx::query!("UPDATE sessions SET title = ? WHERE id = ?", title, id)
        .execute(pool)
        .await?;
    Ok(res.rows_affected() > 0)
}

pub async fn message_count(pool: &SqlitePool, id: &SessionId) -> Result<u64, sqlx::Error> {
    let id = id.to_string();
    let n = sqlx::query_scalar!(
        r#"SELECT COUNT(*) as "n!: i64" FROM messages WHERE session_id = ?"#,
        id,
    )
    .fetch_one(pool)
    .await?;
    Ok(n as u64)
}

pub async fn pending_approvals_count(
    pool: &SqlitePool,
    id: &SessionId,
) -> Result<u64, sqlx::Error> {
    let id = id.to_string();
    let n = sqlx::query_scalar!(
        r#"SELECT COUNT(*) as "n!: i64" FROM approvals WHERE session_id = ? AND decision IS NULL"#,
        id,
    )
    .fetch_one(pool)
    .await?;
    Ok(n as u64)
}
