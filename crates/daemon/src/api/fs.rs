//! Explorador de directorios (GET /v1/fs/list) para el selector de carpeta de
//! la UI. Devuelve solo subdirectorios (se elige el workspace de una sesión).
//!
//! Es una lectura sin efectos, tras la auth de loopback. No añade superficie:
//! el daemon ya tiene acceso total al FS local para las herramientas del
//! agente; aquí solo se enumeran carpetas.

use crate::error::ApiError;
use crate::state::App;
use axum::Json;
use axum::extract::{Query, State};
use rutsubo_core::api::{DirEntry, DirListing};
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Deserialize)]
pub struct ListQuery {
    /// Ruta absoluta a listar. Vacía/ausente → el home del usuario.
    #[serde(default)]
    path: Option<String>,
}

pub async fn list(
    State(_app): State<App>,
    Query(query): Query<ListQuery>,
) -> Result<Json<DirListing>, ApiError> {
    let base = match query.path.filter(|p| !p.is_empty()) {
        Some(p) => PathBuf::from(p),
        None => home_dir(),
    };
    // Canonicaliza (resuelve symlinks y `..`) y confirma que es un directorio.
    let dir = base
        .canonicalize()
        .map_err(|e| ApiError::validation(format!("ruta inaccesible: {e}"), None))?;
    if !dir.is_dir() {
        return Err(ApiError::validation("la ruta no es un directorio", None));
    }

    let mut entries: Vec<DirEntry> = Vec::new();
    let read = std::fs::read_dir(&dir)
        .map_err(|e| ApiError::validation(format!("no se pudo leer el directorio: {e}"), None))?;
    for entry in read.flatten() {
        // Solo directorios; se saltan entradas sin permiso o rotas.
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if !is_dir {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        // Ocultar directorios que empiezan por punto reduce ruido sin esconder
        // proyectos (los repos se eligen por su carpeta, no por su `.git`).
        if name.starts_with('.') {
            continue;
        }
        entries.push(DirEntry {
            name,
            path: clean_path(&entry.path()),
        });
    }
    entries.sort_by_key(|e| e.name.to_lowercase());

    let parent = dir.parent().map(clean_path).filter(|p| !p.is_empty());

    Ok(Json(DirListing {
        path: clean_path(&dir),
        parent,
        entries,
    }))
}

/// Ruta legible para la UI: quita el prefijo `\\?\` que `canonicalize` añade en
/// Windows (paths de longitud extendida). En Unix es idéntica.
fn clean_path(p: &Path) -> String {
    let s = p.to_string_lossy();
    s.strip_prefix(r"\\?\").unwrap_or(&s).to_string()
}

fn home_dir() -> PathBuf {
    directories::UserDirs::new()
        .map(|d| d.home_dir().to_path_buf())
        .unwrap_or_else(|| Path::new(".").to_path_buf())
}
