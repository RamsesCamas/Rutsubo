//! Herramientas del agente (RF-07…RF-12).
//!
//! Interfaz única: añadir una herramienta es implementar [`Tool`] y
//! registrarla, sin modificar el ciclo agéntico (RF-12, RNF-18). **Toda ruta**
//! pasa por `rutsubo_core::paths::resolve_within` (RNF-05) — sin excepciones,
//! tampoco en `search`.

mod edit_file;
mod read_file;
mod run_shell;
mod search;
mod write_file;

pub use edit_file::EditFile;
pub use read_file::ReadFile;
pub use run_shell::RunShell;
pub use search::Search;
pub use write_file::WriteFile;

use crate::llm::ToolSchema;
use async_trait::async_trait;
use rutsubo_core::diff::FileDiff;
use rutsubo_core::ids::SessionId;
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

/// Tope uniforme de salida (el handoff lo exige para `run_shell`; se aplica a
/// todas las herramientas: `output_excerpt` es un extracto por contrato).
pub const MAX_OUTPUT_BYTES: usize = 64 * 1024;

pub struct ToolCtx {
    pub workspace: PathBuf,
    pub session_id: SessionId,
}

/// Resultado de ejecución. Los fallos van como `ok = false` con el motivo en
/// `output` (el modelo decide el siguiente paso; no se aborta el turno).
pub struct ToolResult {
    pub ok: bool,
    pub output: String,
    pub truncated: bool,
    /// `write_file`/`edit_file` adjuntan el diff (RF-27) → evento `file_diff`.
    pub diff: Option<FileDiff>,
}

impl ToolResult {
    pub fn ok(output: impl Into<String>) -> Self {
        Self::truncating(true, output.into(), None)
    }

    pub fn fail(output: impl Into<String>) -> Self {
        Self::truncating(false, output.into(), None)
    }

    pub fn with_diff(output: impl Into<String>, diff: FileDiff) -> Self {
        Self::truncating(true, output.into(), Some(diff))
    }

    /// Trunca a [`MAX_OUTPUT_BYTES`] marcando `truncated` (RNF-06).
    fn truncating(ok: bool, mut output: String, diff: Option<FileDiff>) -> Self {
        let truncated = output.len() > MAX_OUTPUT_BYTES;
        if truncated {
            let mut cut = MAX_OUTPUT_BYTES;
            while !output.is_char_boundary(cut) {
                cut -= 1;
            }
            output.truncate(cut);
        }
        Self {
            ok,
            output,
            truncated,
            diff,
        }
    }
}

/// Trait único (RF-12).
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &'static str;
    /// JSON Schema de args.
    fn schema(&self) -> ToolSchema;
    /// `write_file`, `edit_file`, `run_shell` → true (RF-14).
    fn requires_approval(&self) -> bool;
    async fn execute(&self, ctx: &ToolCtx, args: Value) -> ToolResult;
}

#[derive(Default)]
pub struct ToolRegistry {
    tools: HashMap<&'static str, Arc<dyn Tool>>,
}

impl ToolRegistry {
    /// Registro con las 5 herramientas del MVP (RF-07…RF-11).
    pub fn standard() -> Self {
        let mut registry = Self::default();
        registry.register(Arc::new(ReadFile));
        registry.register(Arc::new(WriteFile));
        registry.register(Arc::new(EditFile));
        registry.register(Arc::new(Search));
        registry.register(Arc::new(RunShell));
        registry
    }

    /// Registro para el modo remoto (web desplegada): herramientas de archivos
    /// sobre el workspace temporal de la sesión, SIN `run_shell` (no hay shell
    /// útil en el contenedor y se evita ejecución arbitraria en el servidor).
    /// Los archivos escritos se persisten aparte en Postgres (FS efímero).
    pub fn remote() -> Self {
        let mut registry = Self::default();
        registry.register(Arc::new(ReadFile));
        registry.register(Arc::new(WriteFile));
        registry.register(Arc::new(EditFile));
        registry.register(Arc::new(Search));
        registry
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.name(), tool);
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    pub fn schemas(&self) -> Vec<ToolSchema> {
        let mut schemas: Vec<ToolSchema> = self.tools.values().map(|t| t.schema()).collect();
        schemas.sort_by(|a, b| a.name.cmp(&b.name));
        schemas
    }
}

/// Extrae un argumento string obligatorio.
fn str_arg(args: &Value, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(str::to_owned)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("falta el argumento obligatorio `{key}`"))
}
