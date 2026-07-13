//! Sobre de enrutamiento daemon ↔ relay (contrato C-2, canal `/v1/connect`).
//!
//! El relay reenvía tráfico C-3 **sin deserializarlo** (RNF-10): `frame` es el
//! JSON del sobre C-3 tal cual, opaco para el relay. Lo único que el relay
//! interpreta es este encabezado mínimo de enrutamiento:
//!
//! - Cliente → relay → daemon: el relay envuelve el comando en [`ToDaemon`]
//!   con el `device_id` de origen (el daemon lo usa como identidad
//!   `device:{src}` en `resolved_by`).
//! - Daemon → relay → clientes: [`FromDaemon`] con `dst = None` difunde a
//!   todos los suscriptores de la cuenta (eventos vivos); `dst = Some(id)`
//!   entrega solo a ese dispositivo (backlog de `subscribe_session`). El
//!   solape entre backlog unicast y flujo vivo broadcast lo resuelve el dedup
//!   por `seq` del cliente (C-3).
//!
//! Estos tipos son internos Rust↔Rust (daemon y relay): no se exportan a
//! bindings TS ni al schema del contrato de apps.

use serde::{Deserialize, Serialize};

/// Comando de un cliente remoto rumbo al daemon.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToDaemon {
    /// `device_id` del cliente que emitió el comando.
    pub src: String,
    /// Sobre C-3 (`CommandEnvelope`) serializado, opaco para el relay.
    pub frame: String,
    /// `Some` cuando este `ToDaemon` es una tarea drenada del buzón (ADR-009):
    /// el daemon deduplica por este id y responde con `ack_outbox_id`. El
    /// `frame` es siempre un `CommandEnvelope::SendMessage`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outbox_id: Option<String>,
    /// Título para crear una sesión nueva cuando la tarea encolada apunta a
    /// `session_id = null`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_session_title: Option<String>,
}

/// Evento del daemon rumbo a los clientes de la cuenta.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FromDaemon {
    /// `None` = difundir a todos los suscriptores; `Some(device_id)` = unicast.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dst: Option<String>,
    /// Sobre C-3 (`EventEnvelope`) serializado, opaco para el relay.
    pub frame: String,
    /// `Some` cuando este mensaje es el acuse de una tarea del buzón: el relay
    /// borra la fila del outbox y NO reenvía `frame` (que va vacío).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ack_outbox_id: Option<String>,
}
