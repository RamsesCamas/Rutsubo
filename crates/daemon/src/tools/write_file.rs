//! `write_file` (RF-08): creación/sobreescritura de archivos. Emite
//! `file_diff` (RF-27) vía el diff adjunto al resultado.

use super::{Tool, ToolCtx, ToolResult, str_arg};
use crate::llm::ToolSchema;
use async_trait::async_trait;
use rutsubo_core::diff::FileDiff;
use rutsubo_core::paths::resolve_within;
use serde_json::{Value, json};

pub struct WriteFile;

#[async_trait]
impl Tool for WriteFile {
    fn name(&self) -> &'static str {
        "write_file"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "write_file".into(),
            description: "Crea o reemplaza un archivo dentro del workspace".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Ruta relativa al workspace"},
                    "content": {"type": "string", "description": "Contenido completo del archivo"}
                },
                "required": ["path", "content"]
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
        let content = match args.get("content").and_then(|v| v.as_str()) {
            Some(c) => c.to_owned(),
            None => return ToolResult::fail("falta el argumento obligatorio `content`"),
        };
        let resolved = match resolve_within(&ctx.workspace, &path) {
            Ok(p) => p,
            Err(e) => return ToolResult::fail(format!("ruta rechazada (RNF-05): {e}")),
        };

        let old = tokio::fs::read_to_string(&resolved)
            .await
            .unwrap_or_default();
        if let Some(parent) = resolved.parent()
            && let Err(e) = tokio::fs::create_dir_all(parent).await
        {
            return ToolResult::fail(format!("no se pudo crear el directorio: {e}"));
        }
        if let Err(e) = tokio::fs::write(&resolved, &content).await {
            return ToolResult::fail(format!("no se pudo escribir `{path}`: {e}"));
        }
        let diff = FileDiff::compute(&path, &old, &content);
        ToolResult::with_diff(
            format!(
                "escrito `{path}` ({} bytes, +{} −{})",
                content.len(),
                diff.additions,
                diff.deletions
            ),
            diff,
        )
    }
}
