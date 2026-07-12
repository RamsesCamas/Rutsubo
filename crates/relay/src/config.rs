//! Configuración de arranque del relay.

use std::net::SocketAddr;

/// Puerto de desarrollo/LAN acordado en el plan de fase (docker publica 8443).
pub const DEFAULT_BIND: &str = "127.0.0.1:8443";

#[derive(Debug, Clone)]
pub struct RelayConfig {
    pub bind: SocketAddr,
    /// URL sqlx de SQLite. `mode=rwc` crea el archivo si no existe.
    pub db_url: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("dirección de escucha inválida: {0}")]
    InvalidBind(String),
}

impl RelayConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        let _ = dotenvy::dotenv();
        let bind_raw = std::env::var("RELAY_BIND").unwrap_or_else(|_| DEFAULT_BIND.into());
        let bind: SocketAddr = bind_raw
            .parse()
            .map_err(|_| ConfigError::InvalidBind(bind_raw.clone()))?;
        let db_url =
            std::env::var("RELAY_DB").unwrap_or_else(|_| "sqlite://relay.db?mode=rwc".into());
        Ok(Self { bind, db_url })
    }
}
