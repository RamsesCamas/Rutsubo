//! Representación de cambios a archivos (RF-27, RF-28).
//!
//! `write_file` y `edit_file` calculan aquí el diff unificado que viaja en el
//! evento `file_diff` (C-3). Solo lectura del lado cliente: el visor lo
//! renderiza con líneas +/− y el contador `+a / −d`.

use serde::{Deserialize, Serialize};
use similar::{ChangeTag, TextDiff};
use ts_rs::TS;

/// Diff unificado de un archivo del workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct FileDiff {
    /// Ruta relativa al workspace, tal como la pidió la herramienta.
    pub path: String,
    /// Diff en formato unified (con encabezados `---`/`+++` y hunks `@@`).
    pub unified: String,
    pub additions: u32,
    pub deletions: u32,
}

impl FileDiff {
    /// Calcula el diff entre `old` y `new` para `path`. Para archivos nuevos,
    /// `old` es la cadena vacía.
    pub fn compute(path: &str, old: &str, new: &str) -> Self {
        let diff = TextDiff::from_lines(old, new);
        let mut additions = 0u32;
        let mut deletions = 0u32;
        for change in diff.iter_all_changes() {
            match change.tag() {
                ChangeTag::Insert => additions += 1,
                ChangeTag::Delete => deletions += 1,
                ChangeTag::Equal => {}
            }
        }
        let unified = diff
            .unified_diff()
            .context_radius(3)
            .header(&format!("a/{path}"), &format!("b/{path}"))
            .to_string();
        Self {
            path: path.to_owned(),
            unified,
            additions,
            deletions,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cuenta_adiciones_y_eliminaciones() {
        let old = "a\nb\nc\n";
        let new = "a\nB\nc\nd\n";
        let d = FileDiff::compute("x.txt", old, new);
        assert_eq!(d.additions, 2); // "B" y "d"
        assert_eq!(d.deletions, 1); // "b"
        assert!(d.unified.contains("-b"));
        assert!(d.unified.contains("+B"));
        assert!(d.unified.contains("a/x.txt"));
    }

    #[test]
    fn archivo_nuevo_solo_adiciones() {
        let d = FileDiff::compute("nuevo.txt", "", "uno\ndos\n");
        assert_eq!(d.additions, 2);
        assert_eq!(d.deletions, 0);
    }

    #[test]
    fn sin_cambios_produce_diff_vacio() {
        let d = FileDiff::compute("x.txt", "igual\n", "igual\n");
        assert_eq!(d.additions, 0);
        assert_eq!(d.deletions, 0);
        assert!(!d.unified.contains('@'));
    }
}
