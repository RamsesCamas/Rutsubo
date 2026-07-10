//! Audit log (RF-05, RF-22): toda acción del agente queda registrada y es
//! consultable desde cualquier cliente.

use chrono::{DateTime, Utc};
use rutsubo_core::api::{AuditEntry, AuditQuery};
use rutsubo_core::ids::{AuditId, SessionId};
use sqlx::SqlitePool;
use std::str::FromStr;

pub async fn insert(
    pool: &SqlitePool,
    session_id: Option<&SessionId>,
    kind: &str,
    detail: &serde_json::Value,
    ts: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    let id = AuditId::new().to_string();
    let sid = session_id.map(|s| s.to_string());
    let detail = detail.to_string();
    let ts = ts.to_rfc3339();
    sqlx::query!(
        "INSERT INTO audit_log (id, session_id, kind, detail, ts) VALUES (?, ?, ?, ?, ?)",
        id,
        sid,
        kind,
        detail,
        ts,
    )
    .execute(pool)
    .await?;
    Ok(())
}

#[derive(Debug, sqlx::FromRow)]
struct AuditRow {
    id: String,
    session_id: Option<String>,
    kind: String,
    detail: String,
    ts: String,
}

/// Consulta paginada por cursor (id ULID descendente: más recientes primero)
/// con filtros por sesión y proveedor (RF-22: `detail.provider_id`).
pub async fn query(
    pool: &SqlitePool,
    q: &AuditQuery,
    limit: i64,
) -> Result<(Vec<AuditEntry>, Option<String>), sqlx::Error> {
    let mut qb =
        sqlx::QueryBuilder::new("SELECT id, session_id, kind, detail, ts FROM audit_log WHERE 1=1");
    if let Some(cursor) = &q.cursor {
        qb.push(" AND id < ").push_bind(cursor);
    }
    if let Some(sid) = &q.session_id {
        qb.push(" AND session_id = ").push_bind(sid.to_string());
    }
    if let Some(provider) = &q.provider {
        qb.push(" AND json_extract(detail, '$.provider_id') = ")
            .push_bind(provider);
    }
    qb.push(" ORDER BY id DESC LIMIT ").push_bind(limit + 1);

    let rows: Vec<AuditRow> = qb.build_query_as().fetch_all(pool).await?;
    let has_more = rows.len() as i64 > limit;
    let mut entries: Vec<AuditEntry> = rows
        .into_iter()
        .take(limit as usize)
        .filter_map(|r| {
            Some(AuditEntry {
                id: AuditId::from_str(&r.id).ok()?,
                session_id: r
                    .session_id
                    .as_deref()
                    .and_then(|s| SessionId::from_str(s).ok()),
                kind: r.kind,
                detail: serde_json::from_str(&r.detail).ok()?,
                ts: DateTime::parse_from_rfc3339(&r.ts)
                    .ok()?
                    .with_timezone(&Utc),
            })
        })
        .collect();
    let next_cursor = if has_more {
        entries.last().map(|e| e.id.to_string())
    } else {
        None
    };
    if !has_more {
        entries.shrink_to_fit();
    }
    Ok((entries, next_cursor))
}
