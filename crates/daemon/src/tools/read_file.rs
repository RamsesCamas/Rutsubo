//! `read_file` (RF-07): lectura de archivos dentro del workspace.

use super::{Tool, ToolCtx, ToolResult, str_arg};
use crate::llm::ToolSchema;
use async_trait::async_trait;
use rutsubo_core::paths::resolve_within;
use serde_json::{Value, json};

pub struct ReadFile;

#[async_trait]
impl Tool for ReadFile {
    fn name(&self) -> &'static str {
        "read_file"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "read_file".into(),
            description: "Lee un archivo de texto dentro del workspace".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Ruta relativa al workspace"}
                },
                "required": ["path"]
            }),
        }
    }

    fn requires_approval(&self) -> bool {
        false
    }

    async fn execute(&self, ctx: &ToolCtx, args: Value) -> ToolResult {
        let path = match str_arg(&args, "path") {
            Ok(p) => p,
            Err(e) => return ToolResult::fail(e),
        };
        let resolved = match resolve_within(&ctx.workspace, &path) {
            Ok(p) => p,
            Err(e) => return ToolResult::fail(format!("ruta rechazada (RNF-05): {e}")),
        };
        match tokio::fs::read_to_string(&resolved).await {
            Ok(content) => ToolResult::ok(content),
            Err(e) => ToolResult::fail(format!("no se pudo leer `{path}`: {e}")),
        }
    }
}
