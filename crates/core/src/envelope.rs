//! Sobre del protocolo (contrato C-3, §3.3.1).
//!
//! Todo mensaje, en ambas direcciones, viaja dentro del mismo sobre:
//!
//! ```json
//! { "v": 1,
//!   "type": "file_diff",
//!   "session_id": "01J1ZG7Q...",
//!   "seq": 419,
//!   "ts": "2026-07-06T18:03:52Z",
//!   "payload": { } }
//! ```
//!
//! `type` y `payload` los aporta el cuerpo (`Event` o `Command`, adjacently
//! tagged) vía `#[serde(flatten)]`. El daemon asigna `seq` monótono creciente
//! y sin huecos por sesión, persistido junto al evento; los comandos de
//! cliente no llevan `seq`.

use crate::ids::SessionId;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use ts_rs::TS;

/// Versión del protocolo que viaja en `v`. Cambiar el payload de un evento
/// existente exige incrementarla (y detenerse a consultar: rompe compatibilidad).
pub const PROTOCOL_VERSION: u16 = 1;

/// Sobre v1. `T` es `Event` (daemon → clientes) o `Command` (clientes → daemon).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, concrete(T = crate::events::Event), rename = "EventEnvelope")]
pub struct Envelope<T> {
    pub v: u16,
    #[serde(flatten)]
    pub body: T,
    /// `null` en eventos globales (p. ej. `daemon_unavailable`).
    pub session_id: Option<SessionId>,
    /// `Some` en eventos; `None` (omitido) en comandos.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub seq: Option<u64>,
    pub ts: DateTime<Utc>,
}

impl<T: Serialize + DeserializeOwned> Envelope<T> {
    /// Sobre de evento: `seq` asignado por el daemon.
    pub fn event(body: T, session_id: Option<SessionId>, seq: u64, ts: DateTime<Utc>) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            body,
            session_id,
            seq: Some(seq),
            ts,
        }
    }

    /// Sobre de comando: sin `seq` (lo asigna el daemon al efecto resultante).
    pub fn command(body: T, session_id: Option<SessionId>, ts: DateTime<Utc>) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            body,
            session_id,
            seq: None,
            ts,
        }
    }
}
