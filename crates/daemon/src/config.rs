//! Configuración de arranque del daemon.

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;

/// Puerto fijo del contrato C-1.
pub const DEFAULT_BIND: &str = "127.0.0.1:7431";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMode {
    Local,
    Remote,
}

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
    /// Local conserva el token del daemon; remote acepta únicamente al BFF.
    pub auth_mode: AuthMode,
    /// Secreto compartido exclusivamente entre Vercel y Railway.
    pub proxy_secret: Option<String>,
    /// Correos permitidos para el acceso remoto inicial.
    pub allowed_emails: Vec<String>,
    /// PostgreSQL administrado para identidad/sesiones del modo remoto.
    pub database_url: Option<String>,
    /// Base http(s) del relay C-2 (`RUTSUBO_RELAY_URL`). Si está configurada,
    /// el daemon mantiene la conexión WebSocket saliente de ADR-006.
    pub relay_url: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("RNF-04: el daemon solo puede escuchar en loopback; se pidió {0}")]
    NonLoopbackBind(SocketAddr),
    #[error("dirección de escucha inválida: {0}")]
    InvalidBind(String),
    #[error("no se pudo determinar el directorio de datos")]
    NoDataDir,
    #[error("RUTSUBO_PROXY_SECRET es obligatorio en modo remote")]
    MissingProxySecret,
    #[error("DATABASE_URL es obligatorio en modo remote")]
    MissingDatabaseUrl,
}

impl DaemonConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        let _ = dotenvy::dotenv();
        let bind_raw = std::env::var("RUTSUBO_BIND").unwrap_or_else(|_| {
            std::env::var("PORT")
                .map(|port| format!("0.0.0.0:{port}"))
                .unwrap_or_else(|_| DEFAULT_BIND.into())
        });
        let bind: SocketAddr = bind_raw
            .parse()
            .map_err(|_| ConfigError::InvalidBind(bind_raw.clone()))?;
        let allow_non_loopback =
            std::env::var("RUTSUBO_ALLOW_NON_LOOPBACK").is_ok_and(|v| v == "1");
        if !is_loopback(bind.ip()) && !allow_non_loopback {
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
        let auth_mode = match std::env::var("RUTSUBO_AUTH_MODE").as_deref() {
            Ok("remote") => AuthMode::Remote,
            _ => AuthMode::Local,
        };
        let proxy_secret = std::env::var("RUTSUBO_PROXY_SECRET")
            .ok()
            .filter(|v| !v.is_empty());
        if auth_mode == AuthMode::Remote && proxy_secret.is_none() {
            return Err(ConfigError::MissingProxySecret);
        }
        let database_url = std::env::var("DATABASE_URL").ok().filter(|v| !v.is_empty());
        if auth_mode == AuthMode::Remote && database_url.is_none() {
            return Err(ConfigError::MissingDatabaseUrl);
        }
        let relay_url = std::env::var("RUTSUBO_RELAY_URL")
            .ok()
            .filter(|v| !v.is_empty())
            .map(|v| v.trim_end_matches('/').to_owned());
        let allowed_emails = std::env::var("RUTSUBO_ALLOWED_EMAILS")
            .unwrap_or_default()
            .split(',')
            .map(|email| email.trim().to_ascii_lowercase())
            .filter(|email| !email.is_empty())
            .collect();

        Ok(Self {
            data_dir,
            bind,
            max_iterations,
            spa_origin,
            groq_api_key,
            auth_mode,
            proxy_secret,
            allowed_emails,
            database_url,
            relay_url,
        })
    }
}

impl DaemonConfig {
    /// Workspace real por sesión en modo remoto: `<data_dir>/remote-ws/<id>`.
    /// El FS es efímero (Railway); la fuente de verdad de los archivos es
    /// Postgres (`generated_files`), que se rehidrata a este directorio.
    pub fn remote_workspace(&self, session_id: &str) -> PathBuf {
        self.data_dir.join("remote-ws").join(session_id)
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
