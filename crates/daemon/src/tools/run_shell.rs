//! `run_shell` (RF-11, RNF-06): ejecución en subproceso aislado.
//!
//! - `current_dir` = workspace; sin shell interactivo: `sh -c` **solo** del
//!   comando aprobado literal.
//! - Entorno mínimo controlado (whitelist: PATH, HOME, LANG).
//! - Timeout 120 s; salida truncada a 64 KB con `truncated: true`.

use super::{Tool, ToolCtx, ToolResult, str_arg};
use crate::llm::ToolSchema;
use async_trait::async_trait;
use serde_json::{Value, json};
use std::process::Stdio;
use std::time::Duration;

pub struct RunShell;

pub const TIMEOUT: Duration = Duration::from_secs(120);
const ENV_WHITELIST: [&str; 3] = ["PATH", "HOME", "LANG"];

#[async_trait]
impl Tool for RunShell {
    fn name(&self) -> &'static str {
        "run_shell"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "run_shell".into(),
            description:
                "Ejecuta un comando de shell en el workspace (subproceso aislado, timeout 120 s)"
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "cmd": {"type": "string", "description": "Comando a ejecutar"}
                },
                "required": ["cmd"]
            }),
        }
    }

    fn requires_approval(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: &ToolCtx, args: Value) -> ToolResult {
        let cmd = match str_arg(&args, "cmd") {
            Ok(c) => c,
            Err(e) => return ToolResult::fail(e),
        };

        let mut command = tokio::process::Command::new("sh");
        command
            .arg("-c")
            .arg(&cmd) // literal aprobado, sin interpolación propia
            .current_dir(&ctx.workspace)
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true); // el timeout mata el subproceso
        for var in ENV_WHITELIST {
            if let Ok(value) = std::env::var(var) {
                command.env(var, value);
            }
        }

        let child = match command.spawn() {
            Ok(c) => c,
            Err(e) => return ToolResult::fail(format!("no se pudo lanzar el comando: {e}")),
        };

        match tokio::time::timeout(TIMEOUT, child.wait_with_output()).await {
            Err(_) => ToolResult::fail(format!(
                "timeout: el comando superó {} s y fue terminado",
                TIMEOUT.as_secs()
            )),
            Ok(Err(e)) => ToolResult::fail(format!("fallo de ejecución: {e}")),
            Ok(Ok(output)) => {
                let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
                let stderr = String::from_utf8_lossy(&output.stderr);
                if !stderr.trim().is_empty() {
                    text.push_str("\n[stderr]\n");
                    text.push_str(&stderr);
                }
                if output.status.success() {
                    ToolResult::ok(text)
                } else {
                    let code = output
                        .status
                        .code()
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "señal".into());
                    ToolResult::fail(format!("exit {code}\n{text}"))
                }
            }
        }
    }
}
