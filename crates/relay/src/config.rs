//! Configuración de arranque del relay.

use std::net::SocketAddr;

/// Puerto de desarrollo/LAN acordado en el plan de fase (docker publica 8443).
pub const DEFAULT_BIND: &str = "127.0.0.1:8443";

#[derive(Debug, Clone)]
pub struct RelayConfig {
    pub bind: SocketAddr,
    /// URL sqlx de SQLite. `mode=rwc` crea el archivo si no existe.
    pub db_url: String,
    /// Client IDs de Google aceptados como `aud` del id_token (`GOOGLE_CLIENT_IDS`,
    /// separados por coma). Vacío = solo modo dev.
    pub google_client_ids: Vec<String>,
    /// `RELAY_GOOGLE_DEV=1`: acepta id_tokens de prueba `dev:{sub}:{email}`.
    pub google_dev: bool,
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
        let google_client_ids = std::env::var("GOOGLE_CLIENT_IDS")
            .unwrap_or_default()
            .split(',')
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .collect();
        let google_dev = std::env::var("RELAY_GOOGLE_DEV").is_ok_and(|v| v == "1");
        Ok(Self {
            bind,
            db_url,
            google_client_ids,
            google_dev,
        })
    }
}
