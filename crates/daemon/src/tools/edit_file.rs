//! `edit_file` (RF-09): reemplazo localizado único, estilo str-replace.

use super::{Tool, ToolCtx, ToolResult, str_arg};
use crate::llm::ToolSchema;
use async_trait::async_trait;
use rutsubo_core::diff::FileDiff;
use rutsubo_core::paths::resolve_within;
use serde_json::{Value, json};

pub struct EditFile;

#[async_trait]
impl Tool for EditFile {
    fn name(&self) -> &'static str {
        "edit_file"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "edit_file".into(),
            description: "Modifica un archivo reemplazando una única ocurrencia exacta de old_str por new_str".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Ruta relativa al workspace"},
                    "old_str": {"type": "string", "description": "Texto exacto a reemplazar (debe aparecer exactamente una vez)"},
                    "new_str": {"type": "string", "description": "Texto de reemplazo"}
                },
                "required": ["path", "old_str", "new_str"]
            }),
        }
    }

    fn requires_approval(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: &ToolCtx, args: Value) -> ToolResult {
        let path = match str_arg(&args, "path") {
            Ok(p) => p,
            Err(e) => return ToolResult::fail(e),
        };
        let old_str = match str_arg(&args, "old_str") {
            Ok(s) => s,
            Err(e) => return ToolResult::fail(e),
        };
        let new_str = match args.get("new_str").and_then(|v| v.as_str()) {
            Some(s) => s.to_owned(),
            None => return ToolResult::fail("falta el argumento obligatorio `new_str`"),
        };
        let resolved = match resolve_within(&ctx.workspace, &path) {
            Ok(p) => p,
            Err(e) => return ToolResult::fail(format!("ruta rechazada (RNF-05): {e}")),
        };

        let old = match tokio::fs::read_to_string(&resolved).await {
            Ok(c) => c,
            Err(e) => return ToolResult::fail(format!("no se pudo leer `{path}`: {e}")),
        };
        // Reemplazo localizado ÚNICO: 0 o >1 coincidencias son error.
        match old.matches(&old_str).count() {
            0 => return ToolResult::fail(format!("`old_str` no aparece en `{path}`")),
            1 => {}
            n => {
                return ToolResult::fail(format!(
                    "`old_str` es ambiguo: {n} coincidencias en `{path}` (debe ser única)"
                ));
            }
        }
        let new = old.replacen(&old_str, &new_str, 1);
        if let Err(e) = tokio::fs::write(&resolved, &new).await {
            return ToolResult::fail(format!("no se pudo escribir `{path}`: {e}"));
        }
        let diff = FileDiff::compute(&path, &old, &new);
        ToolResult::with_diff(
            format!("editado `{path}` (+{} −{})", diff.additions, diff.deletions),
            diff,
        )
    }
}
