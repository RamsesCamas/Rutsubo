//! Persistencia (ADR-005): SQLite embebido en WAL, accedido con sqlx y
//! consultas verificadas en compilación (caché offline en `.sqlx/`,
//! regenerable con `just prepare`).

pub mod acks;
pub mod approvals;
pub mod audit;
pub mod config;
pub mod events;
pub mod messages;
pub mod rules;
pub mod sessions;

use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use std::path::Path;

pub const DB_FILE: &str = "daemon.db";

/// Abre (o crea) la base en `<data_dir>/daemon.db`, activa WAL y aplica las
/// migraciones pendientes (viajan con el binario).
pub async fn open(data_dir: &Path) -> Result<SqlitePool, sqlx::Error> {
    std::fs::create_dir_all(data_dir).map_err(sqlx::Error::Io)?;
    let opts = SqliteConnectOptions::new()
        .filename(data_dir.join(DB_FILE))
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(8)
        .connect_with(opts)
        .await?;
    sqlx::migrate!("./migrations").run(&pool).await?;
    Ok(pool)
}

/// Serialización estable de estados de sesión hacia la columna TEXT.
pub fn state_to_str(state: rutsubo_core::events::SessionState) -> &'static str {
    use rutsubo_core::events::SessionState::*;
    match state {
        Idle => "idle",
        Running => "running",
        WaitingApproval => "waiting_approval",
        Archived => "archived",
    }
}

pub fn state_from_str(s: &str) -> Option<rutsubo_core::events::SessionState> {
    use rutsubo_core::events::SessionState::*;
    match s {
        "idle" => Some(Idle),
        "running" => Some(Running),
        "waiting_approval" => Some(WaitingApproval),
        "archived" => Some(Archived),
        _ => None,
    }
}
