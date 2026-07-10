//! Reglas estables de auto-aprobación (RF-18). Persistencia y CRUD por
//! contrato C-1; la evaluación en la compuerta es TODO(fase-3).

use chrono::{DateTime, Utc};
use rutsubo_core::api::{NewRule, Rule};
use rutsubo_core::ids::RuleId;
use sqlx::SqlitePool;
use std::str::FromStr;

pub async fn list(pool: &SqlitePool) -> Result<Vec<Rule>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"SELECT id as "id!: String", workspace_path as "workspace_path!: String",
                  tool as "tool!: String", pattern as "pattern!: String",
                  created_at as "created_at!: String"
           FROM rules ORDER BY id ASC"#,
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|r| {
            Some(Rule {
                id: RuleId::from_str(&r.id).ok()?,
                workspace_path: r.workspace_path,
                tool: r.tool,
                pattern: r.pattern,
                created_at: DateTime::parse_from_rfc3339(&r.created_at)
                    .ok()?
                    .with_timezone(&Utc),
            })
        })
        .collect())
}

pub async fn insert(
    pool: &SqlitePool,
    rule: &NewRule,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    let id = RuleId::new().to_string();
    let ts = now.to_rfc3339();
    sqlx::query!(
        "INSERT INTO rules (id, workspace_path, tool, pattern, created_at) VALUES (?, ?, ?, ?, ?)",
        id,
        rule.workspace_path,
        rule.tool,
        rule.pattern,
        ts,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// PUT /v1/rules: reemplazo completo del conjunto, en una transacción.
pub async fn replace_all(
    pool: &SqlitePool,
    rules: &[NewRule],
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query!("DELETE FROM rules").execute(&mut *tx).await?;
    let ts = now.to_rfc3339();
    for rule in rules {
        let id = RuleId::new().to_string();
        sqlx::query!(
            "INSERT INTO rules (id, workspace_path, tool, pattern, created_at) VALUES (?, ?, ?, ?, ?)",
            id,
            rule.workspace_path,
            rule.tool,
            rule.pattern,
            ts,
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await
}
