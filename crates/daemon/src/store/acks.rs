//! Dedup de tareas del buzón (ADR-009): el relay entrega at-least-once, así
//! que el daemon marca cada `outbox_id` procesado y salta los repetidos.

use chrono::Utc;
use sqlx::SqlitePool;

/// Marca una tarea como procesada. Devuelve `true` si es NUEVA (hay que
/// ejecutarla) o `false` si ya se había procesado (solo re-acusar).
pub async fn mark_new(pool: &SqlitePool, outbox_id: &str) -> Result<bool, sqlx::Error> {
    // Consulta runtime (no macro) para no tocar la caché offline de sqlx.
    let result = sqlx::query("INSERT OR IGNORE INTO outbox_acks (outbox_id, applied_at) VALUES (?, ?)")
        .bind(outbox_id)
        .bind(Utc::now().to_rfc3339())
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}
