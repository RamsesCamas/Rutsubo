//! `search` (RF-10): búsqueda de contenido sobre crates nativos de ripgrep
//! (`grep-searcher`/`grep-regex`/`ignore`). Respeta `.gitignore`. La ruta
//! base también pasa por `resolve_within` (RNF-05).

use super::{Tool, ToolCtx, ToolResult, str_arg};
use crate::llm::ToolSchema;
use async_trait::async_trait;
use grep_regex::RegexMatcher;
use grep_searcher::sinks::UTF8;
use grep_searcher::{BinaryDetection, SearcherBuilder};
use rutsubo_core::paths::resolve_within;
use serde_json::{Value, json};

pub struct Search;

const DEFAULT_MAX_RESULTS: usize = 50;
const MAX_MAX_RESULTS: usize = 200;

#[async_trait]
impl Tool for Search {
    fn name(&self) -> &'static str {
        "search"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "search".into(),
            description:
                "Busca un patrón (regex) en el contenido del workspace; lista rutas y líneas".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "pattern": {"type": "string", "description": "Expresión regular a buscar"},
                    "path": {"type": "string", "description": "Subdirectorio relativo donde buscar (opcional)"},
                    "max_results": {"type": "integer", "description": "Tope de coincidencias (default 50, máx 200)"}
                },
                "required": ["pattern"]
            }),
        }
    }

    fn requires_approval(&self) -> bool {
        false
    }

    async fn execute(&self, ctx: &ToolCtx, args: Value) -> ToolResult {
        let pattern = match str_arg(&args, "pattern") {
            Ok(p) => p,
            Err(e) => return ToolResult::fail(e),
        };
        let base = match args.get("path").and_then(|v| v.as_str()) {
            Some(sub) if !sub.is_empty() => match resolve_within(&ctx.workspace, sub) {
                Ok(p) => p,
                Err(e) => return ToolResult::fail(format!("ruta rechazada (RNF-05): {e}")),
            },
            _ => ctx.workspace.clone(),
        };
        let max_results = args
            .get("max_results")
            .and_then(|v| v.as_u64())
            .map(|n| (n as usize).clamp(1, MAX_MAX_RESULTS))
            .unwrap_or(DEFAULT_MAX_RESULTS);

        let matcher = match RegexMatcher::new(&pattern) {
            Ok(m) => m,
            Err(e) => return ToolResult::fail(format!("patrón inválido: {e}")),
        };

        let workspace = ctx.workspace.clone();
        // Búsqueda sincrónica de ripgrep → hilo de bloqueo dedicado.
        let result = tokio::task::spawn_blocking(move || {
            let mut hits: Vec<String> = Vec::new();
            let mut searcher = SearcherBuilder::new()
                .binary_detection(BinaryDetection::quit(b'\x00'))
                .line_number(true)
                .build();
            for entry in ignore::WalkBuilder::new(&base).build().flatten() {
                if hits.len() >= max_results {
                    break;
                }
                if !entry.file_type().is_some_and(|ft| ft.is_file()) {
                    continue;
                }
                let path = entry.path();
                let rel = path.strip_prefix(&workspace).unwrap_or(path).to_owned();
                let _ = searcher.search_path(
                    &matcher,
                    path,
                    UTF8(|line, text| {
                        if hits.len() >= max_results {
                            return Ok(false); // corta esta búsqueda
                        }
                        hits.push(format!("{}:{line}: {}", rel.display(), text.trim_end()));
                        Ok(true)
                    }),
                );
            }
            hits
        })
        .await;

        match result {
            Ok(hits) if hits.is_empty() => ToolResult::ok("sin coincidencias"),
            Ok(hits) => {
                let clipped = hits.len() >= max_results;
                let mut out = hits.join("\n");
                if clipped {
                    out.push_str("\n… (resultados truncados)");
                }
                ToolResult::ok(out)
            }
            Err(e) => ToolResult::fail(format!("búsqueda fallida: {e}")),
        }
    }
}
