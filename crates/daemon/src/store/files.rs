//! Persistencia de archivos generados/subidos en modo remoto (Postgres).
//!
//! En el despliegue (Railway) el FS del contenedor es efímero, así que esta
//! tabla es la FUENTE DE VERDAD de los archivos de cada sesión web; el
//! workspace temporal se rehidrata desde aquí al iniciar cada turno. Solo se
//! usa en modo remoto (web); en local los archivos viven en el disco real.
//! Queries en runtime (no macros) para no depender de la caché `.sqlx`.

use chrono::{DateTime, Utc};
use rutsubo_core::ids::SessionId;
use rutsubo_core::paths::resolve_within;
use sqlx::{PgPool, Row};
use std::path::Path;

/// Metadatos de un archivo (sin el contenido) para el listado del panel Vista.
pub struct FileMeta {
    pub path: String,
    pub mime: String,
    pub bytes: i64,
    pub updated_at: DateTime<Utc>,
}

/// MIME por extensión, para servir con el `Content-Type` correcto (el preview
/// de HTML depende de ello) y para guardarlo junto al archivo.
pub fn guess_mime(path: &str) -> &'static str {
    let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml",
        "md" => "text/markdown; charset=utf-8",
        "xml" => "application/xml; charset=utf-8",
        _ => "text/plain; charset=utf-8",
    }
}

/// Inserta o reemplaza el archivo `(session_id, path)`.
pub async fn upsert(
    pool: &PgPool,
    session_id: &SessionId,
    path: &str,
    content: &[u8],
    mime: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO generated_files (session_id, path, content, mime, updated_at) \
         VALUES ($1, $2, $3, $4, now()) \
         ON CONFLICT (session_id, path) \
         DO UPDATE SET content = EXCLUDED.content, mime = EXCLUDED.mime, updated_at = now()",
    )
    .bind(session_id.to_string())
    .bind(path)
    .bind(content)
    .bind(mime)
    .execute(pool)
    .await?;
    Ok(())
}

/// Lista los archivos de una sesión (metadatos, sin contenido).
pub async fn list(pool: &PgPool, session_id: &SessionId) -> Result<Vec<FileMeta>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT path, mime, octet_length(content) AS bytes, updated_at \
         FROM generated_files WHERE session_id = $1 ORDER BY path",
    )
    .bind(session_id.to_string())
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| FileMeta {
            path: r.get("path"),
            mime: r.get("mime"),
            bytes: r.get::<i32, _>("bytes") as i64,
            updated_at: r.get("updated_at"),
        })
        .collect())
}

/// Contenido + MIME de un archivo, o `None` si no existe.
pub async fn get(
    pool: &PgPool,
    session_id: &SessionId,
    path: &str,
) -> Result<Option<(Vec<u8>, String)>, sqlx::Error> {
    let row = sqlx::query("SELECT content, mime FROM generated_files WHERE session_id = $1 AND path = $2")
        .bind(session_id.to_string())
        .bind(path)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(|r| (r.get::<Vec<u8>, _>("content"), r.get::<String, _>("mime"))))
}

/// Vuelca todos los archivos de la sesión al workspace temporal (tras un
/// reinicio de Railway el disco está vacío pero Postgres conserva todo).
/// Cada ruta se resuelve dentro del workspace (RNF-05); las inválidas se saltan.
pub async fn rehydrate(
    pool: &PgPool,
    session_id: &SessionId,
    workspace: &Path,
) -> Result<(), sqlx::Error> {
    let rows = sqlx::query("SELECT path, content FROM generated_files WHERE session_id = $1")
        .bind(session_id.to_string())
        .fetch_all(pool)
        .await?;
    for r in rows {
        let rel: String = r.get("path");
        let content: Vec<u8> = r.get("content");
        let Ok(target) = resolve_within(workspace, &rel) else {
            continue;
        };
        if let Some(parent) = target.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        let _ = tokio::fs::write(&target, &content).await;
    }
    Ok(())
}
