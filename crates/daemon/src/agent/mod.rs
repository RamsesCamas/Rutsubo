//! Agent loop (RF-06). Se implementa en la Fase C; este módulo existe desde
//! la Fase B porque `POST /v1/sessions/{id}/messages` debe disparar el turno.

use crate::state::App;
use rutsubo_core::ids::SessionId;

/// Arranca el turno agéntico de la sesión si no hay uno en curso.
/// TODO(fase-C): loop completo (contexto → generate → stream → gate → iterar).
pub fn ensure_running(_app: App, _session_id: SessionId) {
    // Fase B: aceptar el mensaje (202) sin procesamiento es suficiente para
    // el contrato REST; el loop llega en la fase C.
}
