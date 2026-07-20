//! Endpoints web-only de archivos generados/subidos (modo remoto).
//!
//! Los archivos de cada sesión web viven en Postgres (`store::files`), fuente
//! de verdad frente al FS efímero de Railway. El proxy BFF de Vercel expone
//! estas rutas a la SPA tal cual. Respuestas ad-hoc (JSON/bytes), NO tipos del
//! contrato C-n: son exclusivas de la web y no ripplean a móvil/escritorio.

use crate::error::ApiError;
use crate::state::App;
use crate::store;
use axum::Json;
use axum::body::Body;
use axum::extract::{Multipart, Path, Query, State};
use axum::http::{HeaderValue, Response, StatusCode, header};
use rutsubo_core::api::ErrorCode;
use rutsubo_core::ids::SessionId;
use rutsubo_core::paths::resolve_within;
use serde::Deserialize;
use serde_json::json;
use std::str::FromStr;

/// Archivos de código de un tamaño razonable para revisión/preview.
const MAX_UPLOAD_BYTES: usize = 512 * 1024;

fn parse_id(raw: &str) -> Result<SessionId, ApiError> {
    SessionId::from_str(raw).map_err(|_| ApiError::not_found("sesión"))
}

/// El store de archivos vive en Postgres → solo existe en modo remoto.
fn pool(app: &App) -> Result<&sqlx::PgPool, ApiError> {
    app.remote_auth
        .as_ref()
        .ok_or_else(|| ApiError::validation("los archivos solo están disponibles en modo remoto", None))
}

/// GET /v1/sessions/{id}/files — lista de archivos (metadatos).
pub async fn list(
    State(app): State<App>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let sid = parse_id(&id)?;
    let files = store::files::list(pool(&app)?, &sid)
        .await
        .map_err(ApiError::internal)?;
    let items: Vec<_> = files
        .into_iter()
        .map(|f| json!({ "path": f.path, "mime": f.mime, "bytes": f.bytes, "updated_at": f.updated_at }))
        .collect();
    Ok(Json(json!({ "files": items })))
}

#[derive(Deserialize)]
pub struct RawQuery {
    pub path: String,
}

/// GET /v1/sessions/{id}/files/raw?path=… — contenido crudo con su MIME.
/// Sirve el preview (iframe) y la descarga. El BFF reenvía el `Content-Type`.
pub async fn raw(
    State(app): State<App>,
    Path(id): Path<String>,
    Query(q): Query<RawQuery>,
) -> Result<Response<Body>, ApiError> {
    let sid = parse_id(&id)?;
    let (content, mime) = store::files::get(pool(&app)?, &sid, &q.path)
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("archivo"))?;
    let ct = HeaderValue::from_str(&mime)
        .unwrap_or_else(|_| HeaderValue::from_static("text/plain; charset=utf-8"));
    let mut resp = Response::new(Body::from(content));
    resp.headers_mut().insert(header::CONTENT_TYPE, ct);
    Ok(resp)
}

/// POST /v1/sessions/{id}/files — sube un archivo (multipart, campo `file`).
/// Se guarda en Postgres y también en el workspace de la sesión para que el
/// agente pueda leerlo/analizarlo de inmediato (modo debugger).
pub async fn upload(
    State(app): State<App>,
    Path(id): Path<String>,
    mut multipart: Multipart,
) -> Result<Json<serde_json::Value>, ApiError> {
    let sid = parse_id(&id)?;
    let pg = pool(&app)?;
    let row = store::sessions::get(&app.pool, &sid)
        .await?
        .ok_or_else(|| ApiError::not_found("sesión"))?;
    let workspace = std::path::PathBuf::from(&row.workspace_path);
    let _ = tokio::fs::create_dir_all(&workspace).await;

    let mut saved: Vec<String> = Vec::new();
    while let Some(field) = multipart.next_field().await.map_err(ApiError::internal)? {
        if field.name() != Some("file") {
            continue;
        }
        let raw_name = field
            .file_name()
            .map(str::to_owned)
            .unwrap_or_else(|| "subido.txt".into());
        let bytes = field.bytes().await.map_err(ApiError::internal)?;
        if bytes.len() > MAX_UPLOAD_BYTES {
            return Err(ApiError {
                status: StatusCode::PAYLOAD_TOO_LARGE,
                code: ErrorCode::ValidationFailed,
                message: "el archivo excede 512 KB".into(),
                details: None,
            });
        }
        // Basename seguro (sin rutas ni traversal).
        let safe = std::path::Path::new(&raw_name)
            .file_name()
            .and_then(|s| s.to_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("subido.txt")
            .to_owned();
        if let Ok(target) = resolve_within(&workspace, &safe) {
            let _ = tokio::fs::write(&target, &bytes).await;
        }
        let mime = store::files::guess_mime(&safe);
        store::files::upsert(pg, &sid, &safe, &bytes, mime)
            .await
            .map_err(ApiError::internal)?;
        saved.push(safe);
    }
    if saved.is_empty() {
        return Err(ApiError::validation("falta el campo `file`", None));
    }
    Ok(Json(json!({ "saved": saved })))
}
