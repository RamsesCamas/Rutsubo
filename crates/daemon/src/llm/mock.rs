//! Proveedores mock (C-4, handoff §5.4).
//!
//! [`MockProvider`] sigue un guion determinista para desarrollo y demo:
//! deltas de texto → `read_file` del archivo mencionado → (si el mensaje
//! contiene "test") `run_shell` que dispara la compuerta → `write_file` de
//! notas (dispara compuerta y produce `file_diff`) → `Done` con `Usage`
//! sintético. Respeta `CancellationToken`.
//!
//! [`FailingMock`] falla bajo demanda para testear el `FallbackAdapter`.

use super::{
    ChatMessage, GenerationRequest, GenerationStream, LlmProvider, ProviderError, StreamItem,
    ToolCallRequest,
};
use async_trait::async_trait;
use futures::StreamExt;
use rutsubo_core::api::ProviderHealth;
use rutsubo_core::events::{StopReason, Usage};
use rutsubo_core::ids::{ProviderId, ToolCallId};
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

pub struct MockProvider {
    id: ProviderId,
    /// Pausa entre items para que el streaming sea observable en la UI.
    pub delay: Duration,
}

impl MockProvider {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: ProviderId(id.into()),
            delay: Duration::from_millis(30),
        }
    }

    /// Detecta un archivo mencionado en el mensaje (token con `/` o `.`).
    fn mentioned_file(content: &str) -> String {
        content
            .split_whitespace()
            .map(|t| {
                t.trim_matches(|c: char| {
                    !c.is_alphanumeric() && c != '/' && c != '.' && c != '_' && c != '-'
                })
            })
            .find(|t| {
                !t.is_empty()
                    && !t.starts_with("http")
                    && (t.contains('/') || (t.contains('.') && !t.ends_with('.')))
            })
            .unwrap_or("README.md")
            .to_owned()
    }

    /// ¿Qué herramientas ya corrieron en este turno? (el loop anexa los
    /// resultados como mensajes `tool` con JSON `{tool, ok, ...}`).
    fn tools_ran(messages: &[ChatMessage]) -> Vec<String> {
        messages
            .iter()
            .filter(|m| m.role == "tool")
            .filter_map(|m| serde_json::from_str::<serde_json::Value>(&m.content).ok())
            .filter_map(|v| v.get("tool").and_then(|t| t.as_str()).map(str::to_owned))
            .collect()
    }

    fn script(&self, req: &GenerationRequest) -> Vec<StreamItem> {
        let last_user = req
            .messages
            .iter()
            .rev()
            .find(|m| m.role == "user")
            .map(|m| m.content.clone())
            .unwrap_or_default();
        let ran = Self::tools_ran(&req.messages);
        let file = Self::mentioned_file(&last_user);
        let wants_shell = last_user.to_lowercase().contains("test");

        // Fase 1: aún no se leyó el archivo.
        if !ran.iter().any(|t| t == "read_file") {
            let deltas = [
                "Voy a leer ",
                "el archivo ",
                &format!("`{file}` "),
                "mencionado.",
            ]
            .map(|d| StreamItem::Delta(d.to_owned()));
            let mut items = deltas.to_vec();
            items.push(StreamItem::ToolCall(ToolCallRequest {
                tool_call_id: ToolCallId::new(),
                tool: "read_file".into(),
                args: json!({"path": file}),
                provider_call_id: None,
            }));
            return items;
        }

        // Fase 2: correr los tests si el mensaje lo pide (dispara la compuerta).
        if wants_shell && !ran.iter().any(|t| t == "run_shell") {
            return vec![
                StreamItem::Delta("Ahora ejecuto la suite de tests.".into()),
                StreamItem::ToolCall(ToolCallRequest {
                    tool_call_id: ToolCallId::new(),
                    tool: "run_shell".into(),
                    args: json!({"cmd": "cargo test -p core"}),
                    provider_call_id: None,
                }),
            ];
        }

        // Fase 3: escribir notas (compuerta + file_diff).
        if !ran.iter().any(|t| t == "write_file") {
            return vec![
                StreamItem::Delta("Dejo mis notas del análisis en el workspace.".into()),
                StreamItem::ToolCall(ToolCallRequest {
                    tool_call_id: ToolCallId::new(),
                    tool: "write_file".into(),
                    args: json!({
                        "path": "RUTSUBO_NOTES.md",
                        "content": format!(
                            "# Notas de Rutsubo\n\n- Archivo revisado: {file}\n- Petición: {}\n",
                            last_user.chars().take(120).collect::<String>()
                        ),
                    }),
                    provider_call_id: None,
                }),
            ];
        }

        // Fase final: cierre con Usage sintético.
        let prompt_tokens: u32 = req
            .messages
            .iter()
            .map(|m| m.content.len() as u32 / 4)
            .sum();
        vec![
            StreamItem::Delta("Listo: archivo revisado".into()),
            StreamItem::Delta(" y notas escritas.".into()),
            StreamItem::Done(
                StopReason::EndTurn,
                Usage {
                    prompt_tokens,
                    completion_tokens: 42,
                },
            ),
        ]
    }
}

#[async_trait]
impl LlmProvider for MockProvider {
    fn id(&self) -> ProviderId {
        self.id.clone()
    }

    async fn generate(&self, req: GenerationRequest) -> Result<GenerationStream, ProviderError> {
        if req.cancel.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        let items = self.script(&req);
        let cancel = req.cancel.clone();
        let delay = self.delay;
        let stream = futures::stream::iter(items).then(move |item| {
            let cancel = cancel.clone();
            async move {
                tokio::time::sleep(delay).await;
                if cancel.is_cancelled() {
                    Err(ProviderError::Cancelled)
                } else {
                    Ok(item)
                }
            }
        });
        Ok(Box::pin(stream))
    }

    async fn health(&self) -> ProviderHealth {
        ProviderHealth::Ready
    }
}

// ---- Mock que falla bajo demanda (para el FallbackAdapter) ----

/// Modo de fallo del [`FailingMock`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailMode {
    /// Sano: responde un guion mínimo.
    Ok,
    /// `generate` devuelve OutOfMemory.
    Oom,
    /// `generate` devuelve Transport.
    Transport,
    /// `generate` devuelve InvalidResponse.
    InvalidResponse,
    /// El proveedor respondió 429 con Retry-After.
    RateLimited,
    /// El primer item del stream tarda más que cualquier TTFT razonable.
    SlowFirstItem,
    /// Emite deltas y luego falla a mitad de streaming.
    FailMidStream,
}

pub struct FailingMock {
    id: ProviderId,
    mode: std::sync::Mutex<FailMode>,
    pub calls: AtomicUsize,
    health: std::sync::Mutex<ProviderHealth>,
}

impl FailingMock {
    pub fn new(id: impl Into<String>, mode: FailMode) -> Arc<Self> {
        Arc::new(Self {
            id: ProviderId(id.into()),
            mode: std::sync::Mutex::new(mode),
            calls: AtomicUsize::new(0),
            health: std::sync::Mutex::new(ProviderHealth::Ready),
        })
    }

    pub fn set_mode(&self, mode: FailMode) {
        *self.mode.lock().unwrap() = mode;
    }

    pub fn set_health(&self, health: ProviderHealth) {
        *self.health.lock().unwrap() = health;
    }

    fn ok_stream() -> GenerationStream {
        Box::pin(futures::stream::iter(vec![
            Ok(StreamItem::Delta("ok".into())),
            Ok(StreamItem::Done(
                StopReason::EndTurn,
                Usage {
                    prompt_tokens: 1,
                    completion_tokens: 1,
                },
            )),
        ]))
    }
}

#[async_trait]
impl LlmProvider for FailingMock {
    fn id(&self) -> ProviderId {
        self.id.clone()
    }

    async fn generate(&self, req: GenerationRequest) -> Result<GenerationStream, ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mode = *self.mode.lock().unwrap();
        match mode {
            FailMode::Ok => Ok(Self::ok_stream()),
            FailMode::Oom => Err(ProviderError::OutOfMemory),
            FailMode::Transport => Err(ProviderError::Transport("conexión rehusada".into())),
            FailMode::InvalidResponse => {
                Err(ProviderError::InvalidResponse("JSON malformado".into()))
            }
            FailMode::RateLimited => Err(ProviderError::RateLimited { retry_after_s: 30 }),
            FailMode::SlowFirstItem => {
                let cancel = req.cancel.clone();
                Ok(Box::pin(futures::stream::once(async move {
                    // Duerme hasta que el TTFT del adapter lo cancele.
                    tokio::select! {
                        _ = cancel.cancelled() => Err(ProviderError::Cancelled),
                        _ = tokio::time::sleep(Duration::from_secs(3600)) => {
                            Ok(StreamItem::Delta("tarde".into()))
                        }
                    }
                })))
            }
            FailMode::FailMidStream => Ok(Box::pin(futures::stream::iter(vec![
                Ok(StreamItem::Delta("empecé bien ".into())),
                Ok(StreamItem::Delta("pero ".into())),
                Err(ProviderError::Transport("se cayó a mitad".into())),
            ]))),
        }
    }

    async fn health(&self) -> ProviderHealth {
        *self.health.lock().unwrap()
    }
}
