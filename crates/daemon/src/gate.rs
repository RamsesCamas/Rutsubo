//! Permission gate (RF-14…RF-17): registro en memoria de las aprobaciones
//! pendientes cuya sesión está suspendida esperando decisión.
//!
//! El agent loop registra un canal antes de emitir `approval_request` y se
//! suspende esperándolo **sin bloquear otras sesiones** (RF-16). El endpoint
//! REST y el comando WS de decisión resuelven por aquí; la primera decisión
//! gana (la unicidad la garantiza el UPDATE condicional en la base).

use rutsubo_core::events::Decision;
use rutsubo_core::ids::ApprovalId;
use std::collections::HashMap;
use std::sync::Mutex;
use tokio::sync::oneshot;

#[derive(Default)]
pub struct Gate {
    pending: Mutex<HashMap<ApprovalId, oneshot::Sender<Decision>>>,
}

impl Gate {
    /// Registra una aprobación pendiente; el receptor despierta a la sesión.
    pub fn register(&self, id: ApprovalId) -> oneshot::Receiver<Decision> {
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .expect("gate mutex envenenado")
            .insert(id, tx);
        rx
    }

    /// Entrega la decisión a la sesión suspendida. Devuelve `false` si nadie
    /// esperaba (p. ej. decisión llegada tras reinicio del daemon).
    pub fn resolve(&self, id: &ApprovalId, decision: Decision) -> bool {
        let sender = self
            .pending
            .lock()
            .expect("gate mutex envenenado")
            .remove(id);
        match sender {
            Some(tx) => tx.send(decision).is_ok(),
            None => false,
        }
    }

    /// Descarta un registro (sesión cancelada mientras esperaba).
    pub fn discard(&self, id: &ApprovalId) {
        self.pending
            .lock()
            .expect("gate mutex envenenado")
            .remove(id);
    }
}
