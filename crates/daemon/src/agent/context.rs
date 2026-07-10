//! Construcción del contexto del modelo (RF-13).
//!
//! Gestión mínima para esta fase: system prompt + historial persistido
//! (user/assistant) acotado a las últimas `MAX_HISTORY` entradas + los
//! intercambios de herramientas del turno en curso (en memoria; su registro
//! durable vive en events/audit_log, RF-04/RF-05).
//! TODO(fase-3): truncado/resumen por ventana de contexto del modelo activo.

use crate::llm::ChatMessage;
use crate::store::messages::MessageRow;
use std::path::Path;

const MAX_HISTORY: usize = 40;

pub fn build(workspace: &Path, history: &[MessageRow], turn: &[ChatMessage]) -> Vec<ChatMessage> {
    let mut messages = Vec::with_capacity(history.len() + turn.len() + 1);
    messages.push(ChatMessage {
        role: "system".into(),
        content: format!(
            "Eres Rutsubo, un agente de código local-first. Trabajas dentro del \
             workspace `{}` usando las herramientas disponibles. Toda ruta es \
             relativa al workspace.",
            workspace.display()
        ),
        tool_calls: vec![],
        tool_call_id: None,
        provider_tool_call_id: None,
    });
    let skip = history.len().saturating_sub(MAX_HISTORY);
    messages.extend(history.iter().skip(skip).map(|m| ChatMessage {
        role: m.role.clone(),
        content: m.content.clone(),
        tool_calls: vec![],
        tool_call_id: None,
        provider_tool_call_id: None,
    }));
    messages.extend(turn.iter().cloned());
    messages
}
