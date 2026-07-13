//! Catálogo de eventos daemon → clientes (contrato C-3, tabla 5).
//!
//! El sobre en el cable lleva el discriminante en `type` y el cuerpo en
//! `payload` (ver ejemplo del contrato §3.3.1), por eso el enum es
//! *adjacently tagged*. Añadir un evento es cambio menor (los clientes
//! ignoran `type` desconocido); cambiar el payload de uno existente exige
//! incrementar `v` en el sobre.

use crate::ids::{ApprovalId, MessageId, ProviderId, ToolCallId};
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Estado de una sesión (C-1 / C-3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum SessionState {
    Idle,
    Running,
    WaitingApproval,
    Archived,
}

/// Decisión sobre una aprobación (C-1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum Decision {
    Approve,
    Reject,
}

/// Motivo de cierre de un mensaje del modelo (C-4 `StreamItem::Done`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum StopReason {
    EndTurn,
    MaxTokens,
    MaxIterations,
    Cancelled,
    Error,
}

/// Conteo de tokens de una generación (RF-22, RF-31).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[ts(export)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
}

/// Disparador de un cambio de proveedor (C-3 `model_provider_changed`, C-4 tabla 6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum FallbackTrigger {
    Oom,
    RateLimited,
    TtftExceeded,
    Failures,
    Manual,
}

/// Catálogo de eventos v1 (contrato C-3, tabla 5). Un variante por fila.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
#[ts(export)]
pub enum Event {
    /// Cambio de estado de la sesión.
    SessionState {
        state: SessionState,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// Fragmento de streaming del modelo (RF-03). Alta frecuencia; los
    /// clientes concatenan por `message_id`.
    MessageDelta {
        message_id: MessageId,
        delta: String,
    },
    /// Cierre de un mensaje del modelo.
    MessageCompleted {
        message_id: MessageId,
        stop_reason: StopReason,
        usage: Usage,
    },
    /// El modelo pidió una herramienta. Informativo (la compuerta decide si
    /// requiere aprobación).
    ToolCallRequested {
        tool_call_id: ToolCallId,
        tool: String,
        args: serde_json::Value,
    },
    /// La compuerta exige decisión humana (RF-15). Difundido a todos los clientes.
    ApprovalRequest {
        approval_id: ApprovalId,
        tool_call_id: ToolCallId,
        tool: String,
        summary: String,
        args: serde_json::Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional, as = "Option<String>")]
        expires_at: Option<DateTime<Utc>>,
    },
    /// Decisión tomada (RF-17). Los clientes retiran la tarjeta aunque no
    /// hayan decidido ellos.
    ApprovalResolved {
        approval_id: ApprovalId,
        decision: Decision,
        resolved_by: String,
    },
    /// Resultado de ejecución de una herramienta.
    ToolResult {
        tool_call_id: ToolCallId,
        ok: bool,
        output_excerpt: String,
        truncated: bool,
    },
    /// Cambio a archivo (RF-27).
    FileDiff {
        tool_call_id: ToolCallId,
        path: String,
        diff_unified: String,
        additions: u32,
        deletions: u32,
    },
    /// El adapter cambió de proveedor (ADR-008).
    ModelProviderChanged {
        from: ProviderId,
        to: ProviderId,
        trigger: FallbackTrigger,
    },
    /// Emitido por el relay cuando no hay daemon conectado (RNF-12).
    /// Global: viaja con `session_id = null`. En la fase local solo existe el tipo.
    DaemonUnavailable,
    /// Error asociado a la sesión. `fatal = true` implica transición a `idle`.
    Error {
        code: String,
        message: String,
        fatal: bool,
    },
    /// Una tarea encolada en el buzón (ADR-009) fue drenada y convertida en un
    /// mensaje real: "tu tarea encolada ya corre". `session_id` viaja en el
    /// sobre (puede ser una sesión recién creada).
    TaskDequeued {
        outbox_id: String,
        message_id: MessageId,
    },
}

impl Event {
    /// Discriminante en el cable (`type` del sobre). Útil para persistencia
    /// (columna `events.type`) y logs.
    pub fn kind(&self) -> &'static str {
        match self {
            Event::SessionState { .. } => "session_state",
            Event::MessageDelta { .. } => "message_delta",
            Event::MessageCompleted { .. } => "message_completed",
            Event::ToolCallRequested { .. } => "tool_call_requested",
            Event::ApprovalRequest { .. } => "approval_request",
            Event::ApprovalResolved { .. } => "approval_resolved",
            Event::ToolResult { .. } => "tool_result",
            Event::FileDiff { .. } => "file_diff",
            Event::ModelProviderChanged { .. } => "model_provider_changed",
            Event::DaemonUnavailable => "daemon_unavailable",
            Event::Error { .. } => "error",
            Event::TaskDequeued { .. } => "task_dequeued",
        }
    }
}
