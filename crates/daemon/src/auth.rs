//! Autenticación por token local (C-1).
//!
//! El token se genera en el primer arranque (32 bytes aleatorios, base64url),
//! se guarda en `<data_dir>/token` con permisos 0600 y se exige como
//! `Authorization: Bearer <token>` en toda ruta salvo `/v1/health`.
//! La comparación es en tiempo constante.

use crate::error::ApiError;
use crate::state::App;
use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngCore;
use std::io;
use std::path::Path;
use subtle::ConstantTimeEq;

pub const TOKEN_FILE: &str = "token";

/// Lee el token del directorio de datos, o lo genera si no existe.
pub fn load_or_create_token(data_dir: &Path) -> io::Result<String> {
    let path = data_dir.join(TOKEN_FILE);
    if path.exists() {
        let token = std::fs::read_to_string(&path)?;
        let token = token.trim().to_owned();
        if !token.is_empty() {
            return Ok(token);
        }
    }
    std::fs::create_dir_all(data_dir)?;
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    let token = URL_SAFE_NO_PAD.encode(bytes);
    write_0600(&path, &token)?;
    tracing::info!(path = %path.display(), "token local generado");
    Ok(token)
}

fn write_0600(path: &Path, contents: &str) -> io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(contents.as_bytes())
}

/// Comparación en tiempo constante (misma longitud requerida).
pub fn token_matches(expected: &str, presented: &str) -> bool {
    let a = expected.as_bytes();
    let b = presented.as_bytes();
    if a.len() != b.len() {
        return false;
    }
    a.ct_eq(b).into()
}

/// Middleware: exige `Authorization: Bearer <token>` en las rutas protegidas.
pub async fn require_bearer(
    State(app): State<App>,
    req: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let presented = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    match presented {
        Some(t) if token_matches(&app.token, t) => Ok(next.run(req).await),
        _ => Err(ApiError::unauthorized()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn genera_token_con_permisos_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let token = load_or_create_token(dir.path()).unwrap();
        assert!(token.len() >= 40); // 32 bytes en base64url ≈ 43 chars
        let meta = std::fs::metadata(dir.path().join(TOKEN_FILE)).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        // Segundo arranque: mismo token.
        assert_eq!(load_or_create_token(dir.path()).unwrap(), token);
    }

    #[test]
    fn comparacion_de_tokens() {
        assert!(token_matches("abc123", "abc123"));
        assert!(!token_matches("abc123", "abc124"));
        assert!(!token_matches("abc123", "abc12"));
        assert!(!token_matches("abc123", ""));
    }
}
