//! Contrato C-4: interfaz del LLM Adapter (in-process).
//!
//! El agent loop solo conoce el trait [`LlmProvider`]; jamás un proveedor
//! concreto (RNF-18). Añadir un proveedor real (vLLM/Ollama/API externa,
//! fase posterior) es implementar este trait sin tocar el loop.

pub mod fallback;
pub mod mock;

use async_trait::async_trait;
use futures::Stream;
use rutsubo_core::api::ProviderHealth;
use rutsubo_core::events::{StopReason, Usage};
use rutsubo_core::ids::{ProviderId, ToolCallId};
use std::pin::Pin;
use tokio_util::sync::CancellationToken;

/// Mensaje del contexto ya gestionado (RF-13).
#[derive(Debug, Clone)]
pub struct ChatMessage {
    /// `system` | `user` | `assistant` | `tool`
    pub role: String,
    pub content: String,
}

/// Esquema JSON de los argumentos de una herramienta (del Tool Registry, RF-12).
#[derive(Debug, Clone)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    /// JSON Schema de `args`.
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct GenerationRequest {
    /// Historial ya gestionado (RF-13).
    pub messages: Vec<ChatMessage>,
    /// Del Tool Registry (RF-12).
    pub tools: Vec<ToolSchema>,
    pub max_tokens: u32,
    pub temperature: f32,
    /// Cancelación cooperativa.
    pub cancel: CancellationToken,
}

/// El modelo pidió una herramienta.
#[derive(Debug, Clone)]
pub struct ToolCallRequest {
    pub tool_call_id: ToolCallId,
    pub tool: String,
    pub args: serde_json::Value,
}

/// El stream emite items tipados; el loop los traduce a eventos C-3.
#[derive(Debug, Clone)]
pub enum StreamItem {
    /// → `message_delta`
    Delta(String),
    /// → `tool_call_requested`
    ToolCall(ToolCallRequest),
    /// → `message_completed`
    Done(StopReason, Usage),
}

/// Taxonomía de errores: es la ENTRADA del clasificador de fallback (C-4).
#[derive(Debug, Clone, thiserror::Error)]
pub enum ProviderError {
    /// Dispara fallback inmediato (RF-21).
    #[error("proveedor sin memoria (OOM)")]
    OutOfMemory,
    #[error("timeout tras {after_ms} ms")]
    Timeout { after_ms: u64 },
    /// Red/HTTP; cuenta para la ventana de fallos.
    #[error("transporte: {0}")]
    Transport(String),
    /// Cuenta para la ventana de fallos.
    #[error("respuesta inválida: {0}")]
    InvalidResponse(String),
    /// NO cuenta: fue voluntad del usuario.
    #[error("cancelado por el usuario")]
    Cancelled,
}

/// Un stream de generación puede fallar a mitad (tras emitir deltas); por eso
/// los items van envueltos en `Result`.
pub type GenerationStream = Pin<Box<dyn Stream<Item = Result<StreamItem, ProviderError>> + Send>>;

/// Contrato que todo proveedor de modelo debe implementar (RNF-18).
#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// Identificador estable para el audit log (RF-22),
    /// p. ej. `local:vllm:qwen3.5-8b`.
    fn id(&self) -> ProviderId;

    /// Inicia una generación en streaming. Errores tipados: el clasificador
    /// de fallback depende de esta taxonomía.
    async fn generate(&self, req: GenerationRequest) -> Result<GenerationStream, ProviderError>;

    /// Sondeo barato de salud; usado por el circuit breaker.
    async fn health(&self) -> ProviderHealth;
}
