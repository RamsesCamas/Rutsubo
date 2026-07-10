//! Persistencia de la política del adapter LLM (C-1 `/v1/config/model`):
//! sobrevive reinicios del daemon.

use rutsubo_core::api::ModelConfig;
use sqlx::SqlitePool;

const MODEL_KEY: &str = "model";

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
