//! Validación de rutas del workspace (RNF-05).
//!
//! Toda ruta que una herramienta reciba del modelo pasa por
//! [`resolve_within`]. Sin excepciones: también `search`. La función
//! canonicaliza y rechaza todo lo que resuelva fuera del workspace:
//! secuencias `..`, rutas absolutas, symlinks salientes y prefijos engañosos
//! (`/ws-otro` no está dentro de `/ws`).

use std::path::{Component, Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PathError {
    #[error("el workspace no existe o no es accesible: {0}")]
    InvalidWorkspace(String),
    #[error("se rechazan rutas absolutas dentro del workspace")]
    AbsolutePath,
    #[error("la ruta contiene componentes de traversal (`..`)")]
    Traversal,
    #[error("la ruta resuelve fuera del workspace")]
    OutsideWorkspace,
    #[error("ruta inválida: {0}")]
    Invalid(String),
}

/// Resuelve `requested` (relativa) dentro de `workspace` y garantiza que el
/// resultado final —tras canonicalizar symlinks— sigue dentro del workspace.
///
/// La ruta devuelta puede no existir todavía (caso `write_file`): en ese caso
/// se canonicaliza el ancestro existente más profundo y se verifica que esté
/// contenido; los componentes restantes se anexan tal cual (ya se comprobó
/// que no contienen `..` ni son absolutos).
pub fn resolve_within(workspace: &Path, requested: &str) -> Result<PathBuf, PathError> {
    if requested.is_empty() {
        return Err(PathError::Invalid("ruta vacía".into()));
    }
    if requested.contains('\0') {
        return Err(PathError::Invalid("la ruta contiene NUL".into()));
    }

    let ws = workspace
        .canonicalize()
        .map_err(|e| PathError::InvalidWorkspace(format!("{}: {e}", workspace.display())))?;
    if !ws.is_dir() {
        return Err(PathError::InvalidWorkspace(format!(
            "{}: no es un directorio",
            ws.display()
        )));
    }

    let req = Path::new(requested);
    if req.is_absolute() {
        return Err(PathError::AbsolutePath);
    }

    // Rechazo sintáctico previo a tocar el filesystem: `..` en cualquier
    // posición (cubre `../x`, `a/../../x` y `..%2f` ya decodificado).
    let mut candidate = ws.clone();
    for comp in req.components() {
        match comp {
            Component::Normal(part) => candidate.push(part),
            Component::CurDir => {}
            Component::ParentDir => return Err(PathError::Traversal),
            Component::RootDir | Component::Prefix(_) => return Err(PathError::AbsolutePath),
        }
    }

    // Canonicaliza el ancestro existente más profundo: si algún componente ya
    // creado es un symlink que apunta fuera, aquí se detecta. `starts_with`
    // compara por componentes, no por prefijo de cadena, así que `/ws-otro`
    // jamás pasa como interior de `/ws`.
    let mut existing = candidate.clone();
    let mut remainder: Vec<std::ffi::OsString> = Vec::new();
    loop {
        if existing.exists() || existing == ws {
            break;
        }
        match (existing.file_name(), existing.parent()) {
            (Some(name), Some(parent)) => {
                remainder.push(name.to_owned());
                existing = parent.to_path_buf();
            }
            _ => return Err(PathError::OutsideWorkspace),
        }
    }
    let mut resolved = existing
        .canonicalize()
        .map_err(|e| PathError::Invalid(format!("{}: {e}", existing.display())))?;
    if !resolved.starts_with(&ws) {
        return Err(PathError::OutsideWorkspace);
    }
    for part in remainder.iter().rev() {
        resolved.push(part);
    }
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn ws() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().join("ws");
        fs::create_dir_all(ws.join("src")).unwrap();
        fs::write(ws.join("src/main.rs"), "fn main() {}\n").unwrap();
        (dir, ws)
    }

    #[test]
    fn acepta_rutas_relativas_normales() {
        let (_d, ws) = ws();
        let p = resolve_within(&ws, "src/main.rs").unwrap();
        assert!(p.ends_with("src/main.rs"));
        assert!(p.starts_with(ws.canonicalize().unwrap()));
    }

    #[test]
    fn acepta_rutas_que_no_existen_todavia() {
        let (_d, ws) = ws();
        let p = resolve_within(&ws, "src/nuevo/modulo.rs").unwrap();
        assert!(p.starts_with(ws.canonicalize().unwrap()));
    }

    #[test]
    fn rechaza_parent_dir() {
        let (_d, ws) = ws();
        assert_eq!(
            resolve_within(&ws, "../fuera.txt"),
            Err(PathError::Traversal)
        );
        assert_eq!(
            resolve_within(&ws, "src/../../fuera.txt"),
            Err(PathError::Traversal)
        );
    }

    #[test]
    fn rechaza_traversal_ya_decodificado() {
        // `..%2fetc/passwd` decodificado por la capa HTTP llega como `../etc/passwd`.
        let (_d, ws) = ws();
        assert_eq!(
            resolve_within(&ws, "../etc/passwd"),
            Err(PathError::Traversal)
        );
    }

    #[test]
    fn rechaza_rutas_absolutas() {
        let (_d, ws) = ws();
        assert_eq!(
            resolve_within(&ws, "/etc/passwd"),
            Err(PathError::AbsolutePath)
        );
    }

    #[test]
    fn rechaza_symlink_saliente() {
        let (dir, ws) = ws();
        let outside = dir.path().join("secreto");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("clave.txt"), "42").unwrap();
        std::os::unix::fs::symlink(&outside, ws.join("atajo")).unwrap();
        assert_eq!(
            resolve_within(&ws, "atajo/clave.txt"),
            Err(PathError::OutsideWorkspace)
        );
    }

    #[test]
    fn rechaza_prefijo_enganoso() {
        // `/tmp/.../ws-otro` comparte prefijo de cadena con `/tmp/.../ws`,
        // pero no es interior suyo.
        let (dir, ws) = ws();
        let sibling = dir.path().join("ws-otro");
        fs::create_dir_all(&sibling).unwrap();
        fs::write(sibling.join("x.txt"), "x").unwrap();
        std::os::unix::fs::symlink(&sibling, ws.join("hermano")).unwrap();
        assert_eq!(
            resolve_within(&ws, "hermano/x.txt"),
            Err(PathError::OutsideWorkspace)
        );
    }

    #[test]
    fn rechaza_workspace_inexistente() {
        let err = resolve_within(Path::new("/no/existe/jamas"), "a.txt").unwrap_err();
        assert!(matches!(err, PathError::InvalidWorkspace(_)));
    }

    #[test]
    fn curdir_es_inocuo() {
        let (_d, ws) = ws();
        let p = resolve_within(&ws, "./src/./main.rs").unwrap();
        assert!(p.ends_with("src/main.rs"));
    }
}
