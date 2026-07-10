//! Configuración de arranque del daemon.

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;

/// Puerto fijo del contrato C-1.
pub const DEFAULT_BIND: &str = "127.0.0.1:7431";

#[derive(Debug, Clone)]
pub struct DaemonConfig {
    /// Directorio de datos (token, daemon.db). `RUTSUBO_DATA_DIR` lo
    /// sobreescribe (necesario para tests y ejecuciones aisladas).
    pub data_dir: PathBuf,
    /// Dirección de escucha. RNF-04: si no es loopback, el daemon se niega a
    /// arrancar con error explícito.
    pub bind: SocketAddr,
    /// Tope de iteraciones del agent loop por turno (RF-06).
    pub max_iterations: u32,
    /// Origin adicional permitido por CORS (producción de la SPA).
    pub spa_origin: Option<String>,
    /// Credencial Groq, capturada al arranque y nunca expuesta por la API.
    pub groq_api_key: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("RNF-04: el daemon solo puede escuchar en loopback; se pidió {0}")]
    NonLoopbackBind(SocketAddr),
    #[error("dirección de escucha inválida: {0}")]
    InvalidBind(String),
    #[error("no se pudo determinar el directorio de datos")]
    NoDataDir,
}

impl DaemonConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        let _ = dotenvy::dotenv();
        let bind_raw = std::env::var("RUTSUBO_BIND").unwrap_or_else(|_| DEFAULT_BIND.into());
        let bind: SocketAddr = bind_raw
            .parse()
            .map_err(|_| ConfigError::InvalidBind(bind_raw.clone()))?;
        if !is_loopback(bind.ip()) {
            return Err(ConfigError::NonLoopbackBind(bind));
        }

        let data_dir = match std::env::var("RUTSUBO_DATA_DIR") {
            Ok(dir) if !dir.is_empty() => PathBuf::from(dir),
            _ => directories::ProjectDirs::from("dev", "rutsubo", "rutsubo")
                .map(|d| d.data_dir().to_path_buf())
                .ok_or(ConfigError::NoDataDir)?,
        };

        let max_iterations = std::env::var("RUTSUBO_MAX_ITERATIONS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(20);

        let spa_origin = std::env::var("RUTSUBO_SPA_ORIGIN")
            .ok()
            .filter(|s| !s.is_empty());

        let groq_api_key = std::env::var("GROQ_API_KEY").ok().filter(|v| !v.is_empty());

        Ok(Self {
            data_dir,
            bind,
            max_iterations,
            spa_origin,
            groq_api_key,
        })
    }
}

fn is_loopback(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback(),
        IpAddr::V6(v6) => v6.is_loopback(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rechaza_bind_no_loopback() {
        assert!(!is_loopback("0.0.0.0".parse().unwrap()));
        assert!(!is_loopback("192.168.1.10".parse().unwrap()));
        assert!(is_loopback("127.0.0.1".parse().unwrap()));
        assert!(is_loopback("::1".parse().unwrap()));
    }
}
