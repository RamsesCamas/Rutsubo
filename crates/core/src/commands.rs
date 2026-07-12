//! Comandos cliente → daemon (contrato C-3, §3.3.3).
//!
//! Mismo sobre y mismo mecanismo de tag que los eventos. Los comandos no
//! llevan `seq` (lo asigna el daemon al efecto resultante); `send_message`
//! lleva `client_msg_id` para idempotencia. `send_message` y
//! `resolve_approval` son equivalentes exactos de sus endpoints REST en C-1:
//! misma validación, misma idempotencia, mismo código interno.

use crate::events::Decision;
use crate::ids::{ApprovalId, SessionId};
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Comandos v1 (contrato C-3). La sesión objetivo de `send_message` viaja en
/// `session_id` del sobre.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
#[ts(export)]
pub enum Command {
    /// Enviar mensaje e iniciar iteración del loop (equivalente REST:
    /// `POST /v1/sessions/{id}/messages`).
    SendMessage {
        content: String,
        client_msg_id: String,
    },
    /// Aprobar o rechazar una solicitud (equivalente REST:
    /// `POST /v1/approvals/{id}/decision`).
    ResolveApproval {
        approval_id: ApprovalId,
        decision: Decision,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        remember_rule: Option<bool>,
    },
    /// Suscribirse al flujo vivo de una sesión, con replay desde `after_seq`.
    SubscribeSession {
        session_id: SessionId,
        #[ts(type = "number")]
        after_seq: u64,
    },
    /// Dejar de recibir eventos de una sesión.
    UnsubscribeSession { session_id: SessionId },
}

/// Sobre v1 en dirección cliente → daemon. Es la misma forma que
/// [`crate::Envelope`] instanciada con [`Command`]; existe como struct propio
/// porque `ts-rs` solo permite fijar una instanciación concreta por genérico
/// (la otra es `EventEnvelope`). Sin `seq`: lo asigna el daemon al efecto.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
#[ts(export)]
pub struct CommandEnvelope {
    pub v: u16,
    #[serde(flatten)]
    pub body: Command,
    /// Sesión objetivo (`send_message`); `null` en comandos globales.
    pub session_id: Option<SessionId>,
    pub ts: DateTime<Utc>,
}
