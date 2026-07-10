//! Aprobaciones (RF-14…RF-17). Resolución única: la primera decisión gana,
//! garantizado por `UPDATE … WHERE decision IS NULL` (atómico en SQLite).

use chrono::{DateTime, Utc};
use rutsubo_core::api::ApprovalDto;
use rutsubo_core::events::Decision;
use rutsubo_core::ids::{ApprovalId, SessionId, ToolCallId};
use sqlx::SqlitePool;
use std::str::FromStr;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ApprovalRow {
    pub id: String,
    pub session_id: String,
    pub tool_call_id: String,
    pub tool: String,
    pub summary: String,
    pub args: String,
    pub decision: Option<String>,
    pub resolved_by: Option<String>,
    pub created_at: String,
    pub resolved_at: Option<String>,
}

impl ApprovalRow {
    pub fn to_dto(&self) -> Option<ApprovalDto> {
        Some(ApprovalDto {
            id: ApprovalId::from_str(&self.id).ok()?,
            session_id: SessionId::from_str(&self.session_id).ok()?,
            tool_call_id: ToolCallId::from_str(&self.tool_call_id).ok()?,
            tool: self.tool.clone(),
            summary: self.summary.clone(),
            args: serde_json::from_str(&self.args).ok()?,
            decision: match self.decision.as_deref() {
                Some("approve") => Some(Decision::Approve),
                Some("reject") => Some(Decision::Reject),
                _ => None,
            },
            resolved_by: self.resolved_by.clone(),
            created_at: parse_ts(&self.created_at)?,
            resolved_at: self.resolved_at.as_deref().and_then(parse_ts),
        })
    }
}

fn parse_ts(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

pub fn decision_to_str(d: Decision) -> &'static str {
    match d {
        Decision::Approve => "approve",
        Decision::Reject => "reject",
    }
}

/// Datos de una aprobación nueva (evita una firma de 8 argumentos).
pub struct NewApproval<'a> {
    pub id: &'a ApprovalId,
    pub session_id: &'a SessionId,
    pub tool_call_id: &'a ToolCallId,
    pub tool: &'a str,
    pub summary: &'a str,
    pub args: &'a serde_json::Value,
    pub created_at: DateTime<Utc>,
}

pub async fn insert(pool: &SqlitePool, new: NewApproval<'_>) -> Result<(), sqlx::Error> {
    let NewApproval {
        id,
        session_id,
        tool_call_id,
        tool,
        summary,
        args,
        created_at,
    } = new;
    let id = id.to_string();
    let sid = session_id.to_string();
    let tcid = tool_call_id.to_string();
    let args = args.to_string();
    let ts = created_at.to_rfc3339();
    sqlx::query!(
        "INSERT INTO approvals (id, session_id, tool_call_id, tool, summary, args, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
        id,
        sid,
        tcid,
        tool,
        summary,
        args,
        ts,
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn get(pool: &SqlitePool, id: &ApprovalId) -> Result<Option<ApprovalRow>, sqlx::Error> {
    let id = id.to_string();
    sqlx::query_as!(
        ApprovalRow,
        r#"SELECT id as "id!", session_id as "session_id!", tool_call_id as "tool_call_id!",
                  tool as "tool!", summary as "summary!", args as "args!",
                  decision, resolved_by, created_at as "created_at!", resolved_at
           FROM approvals WHERE id = ?"#,
        id,
    )
    .fetch_optional(pool)
    .await
}

/// Pendientes de **todas** las sesiones, más antiguas primero (C-1).
pub async fn pending_all(pool: &SqlitePool) -> Result<Vec<ApprovalRow>, sqlx::Error> {
    sqlx::query_as!(
        ApprovalRow,
        r#"SELECT id as "id!", session_id as "session_id!", tool_call_id as "tool_call_id!",
                  tool as "tool!", summary as "summary!", args as "args!",
                  decision, resolved_by, created_at as "created_at!", resolved_at
           FROM approvals WHERE decision IS NULL ORDER BY id ASC"#,
    )
    .fetch_all(pool)
    .await
}

pub enum DecideOutcome {
    /// Esta llamada ganó la carrera: la decisión quedó registrada.
    Applied(ApprovalRow),
    /// Ya estaba resuelta (por quien sea): registro original.
    AlreadyResolved(ApprovalRow),
    NotFound,
}

/// Primera decisión gana. La condición `decision IS NULL` hace la operación
/// atómica: de dos decisiones concurrentes, exactamente una aplica.
pub async fn decide(
    pool: &SqlitePool,
    id: &ApprovalId,
    decision: Decision,
    resolved_by: &str,
    resolved_at: DateTime<Utc>,
) -> Result<DecideOutcome, sqlx::Error> {
    let id_s = id.to_string();
    let dec = decision_to_str(decision);
    let ts = resolved_at.to_rfc3339();
    let res = sqlx::query!(
        "UPDATE approvals SET decision = ?, resolved_by = ?, resolved_at = ?
         WHERE id = ? AND decision IS NULL",
        dec,
        resolved_by,
        ts,
        id_s,
    )
    .execute(pool)
    .await?;

    match get(pool, id).await? {
        None => Ok(DecideOutcome::NotFound),
        Some(row) if res.rows_affected() > 0 => Ok(DecideOutcome::Applied(row)),
        Some(row) => Ok(DecideOutcome::AlreadyResolved(row)),
    }
}
