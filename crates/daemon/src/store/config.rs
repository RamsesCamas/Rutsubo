//! Persistencia de la política del adapter LLM (C-1 `/v1/config/model`):
//! sobrevive reinicios del daemon.

use rutsubo_core::api::ModelConfig;
use sqlx::SqlitePool;

const MODEL_KEY: &str = "model";
const PROVIDER_KEY: &str = "groq_api_key";

/// Lee la API key de Groq persistida (configurada desde la UI). `None` si el
/// usuario nunca la guardó (entonces se usa la del entorno, si existe).
pub async fn load_provider_key(pool: &SqlitePool) -> Result<Option<String>, sqlx::Error> {
    let value: Option<String> = sqlx::query_scalar("SELECT value FROM config WHERE key = ?")
        .bind(PROVIDER_KEY)
        .fetch_optional(pool)
        .await?;
    Ok(value.filter(|v| !v.is_empty()))
}

/// Guarda (o borra, con `None`) la API key de Groq. Persiste en la DB local
/// del daemon; el secreto queda en claro, igual que el token local (local-first).
pub async fn save_provider_key(pool: &SqlitePool, key: Option<&str>) -> Result<(), sqlx::Error> {
    match key.filter(|k| !k.is_empty()) {
        Some(k) => {
            sqlx::query(
                "INSERT INTO config (key, value) VALUES (?, ?)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            )
            .bind(PROVIDER_KEY)
            .bind(k)
            .execute(pool)
            .await?;
        }
        None => {
            sqlx::query("DELETE FROM config WHERE key = ?")
                .bind(PROVIDER_KEY)
                .execute(pool)
                .await?;
        }
    }
    Ok(())
}

pub async fn load_model(pool: &SqlitePool) -> Result<Option<ModelConfig>, sqlx::Error> {
    let value = sqlx::query_scalar!(
        r#"SELECT value as "value!: String" FROM config WHERE key = ?"#,
        MODEL_KEY,
    )
    .fetch_optional(pool)
    .await?;
    Ok(value.and_then(|v| serde_json::from_str(&v).ok()))
}

pub async fn save_model(pool: &SqlitePool, cfg: &ModelConfig) -> Result<(), sqlx::Error> {
    let value = serde_json::to_string(cfg).expect("ModelConfig siempre serializa");
    sqlx::query!(
        "INSERT INTO config (key, value) VALUES (?, ?)
         ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        MODEL_KEY,
        value,
    )
    .execute(pool)
    .await?;
    Ok(())
}
