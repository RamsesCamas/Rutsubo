//! Fixtures canónicos del contrato C-3: la forma exacta en el cable de cada
//! evento y comando. Fuente única de verdad para dos consumidores:
//! `tests/roundtrip.rs` (round-trip serde) y `tests/contract_export.rs`
//! (materialización a `contract-export/fixtures/` para los repos de app).
//!
//! Cada entrada es `(nombre, json)`. El nombre es el discriminante del evento,
//! con sufijo `.variante` cuando hay más de un fixture por tipo. Los repos de
//! app (Flutter, Tauri) deben deserializar TODOS estos fixtures y
//! re-serializarlos idénticos: es su test de contrato.

use serde_json::{Value, json};

/// session_id de ejemplo compartido por los fixtures.
pub const SID: &str = "01J1ZG7QXW8Y2K3M4N5P6Q7R8S";
/// timestamp de ejemplo compartido por los fixtures.
pub const TS: &str = "2026-07-06T18:03:52Z";

/// Catálogo de fixtures de eventos (sobre completo `EventEnvelope`).
pub fn event_fixtures() -> Vec<(&'static str, Value)> {
    vec![
        (
            "session_state",
            json!({
                "v": 1, "type": "session_state",
                "payload": {"state": "waiting_approval", "reason": "aprobación pendiente"},
                "session_id": SID, "seq": 422, "ts": TS
            }),
        ),
        (
            // title opcional presente; reason ausente.
            "session_state.optional_title",
            json!({
                "v": 1, "type": "session_state",
                "payload": {"state": "idle", "title": "Refactor validación"},
                "session_id": SID, "seq": 1, "ts": TS
            }),
        ),
        (
            "message_delta",
            json!({
                "v": 1, "type": "message_delta",
                "payload": {"message_id": "01J1ZH2K0000000000000000AA", "delta": "Voy a leer el"},
                "session_id": SID, "seq": 416, "ts": TS
            }),
        ),
        (
            "message_completed",
            json!({
                "v": 1, "type": "message_completed",
                "payload": {
                    "message_id": "01J1ZH2K0000000000000000AA",
                    "stop_reason": "end_turn",
                    "usage": {"prompt_tokens": 1200, "completion_tokens": 340}
                },
                "session_id": SID, "seq": 427, "ts": TS
            }),
        ),
        (
            "tool_call_requested",
            json!({
                "v": 1, "type": "tool_call_requested",
                "payload": {
                    "tool_call_id": "01J1ZJ9M0000000000000000BB",
                    "tool": "read_file",
                    "args": {"path": "src/ctrl.rs"}
                },
                "session_id": SID, "seq": 418, "ts": TS
            }),
        ),
        (
            "approval_request",
            json!({
                "v": 1, "type": "approval_request",
                "payload": {
                    "approval_id": "01J1ZJ9M0000000000000000CC",
                    "tool_call_id": "01J1ZJ9M0000000000000000BB",
                    "tool": "run_shell",
                    "summary": "cargo test -p core",
                    "args": {"cmd": "cargo test -p core"}
                },
                "session_id": SID, "seq": 421, "ts": TS
            }),
        ),
        (
            "approval_request.expires_at",
            json!({
                "v": 1, "type": "approval_request",
                "payload": {
                    "approval_id": "01J1ZJ9M0000000000000000CC",
                    "tool_call_id": "01J1ZJ9M0000000000000000BB",
                    "tool": "write_file",
                    "summary": "escribir src/x.rs",
                    "args": {"path": "src/x.rs"},
                    "expires_at": "2026-07-06T18:07:00Z"
                },
                "session_id": SID, "seq": 500, "ts": TS
            }),
        ),
        (
            "approval_resolved",
            json!({
                "v": 1, "type": "approval_resolved",
                "payload": {
                    "approval_id": "01J1ZJ9M0000000000000000CC",
                    "decision": "approve",
                    "resolved_by": "device:01JOX00000000000000000DD00"
                },
                "session_id": SID, "seq": 423, "ts": TS
            }),
        ),
        (
            "tool_result",
            json!({
                "v": 1, "type": "tool_result",
                "payload": {
                    "tool_call_id": "01J1ZJ9M0000000000000000BB",
                    "ok": true,
                    "output_excerpt": "23 passed...",
                    "truncated": false
                },
                "session_id": SID, "seq": 425, "ts": TS
            }),
        ),
        (
            "file_diff",
            json!({
                "v": 1, "type": "file_diff",
                "payload": {
                    "tool_call_id": "01J1ZJ9M0000000000000000BB",
                    "path": "src/validators.rs",
                    "diff_unified": "--- a/src/validators.rs\n+++ b/src/validators.rs\n@@ -1 +1 @@\n-a\n+b\n",
                    "additions": 14,
                    "deletions": 6
                },
                "session_id": SID, "seq": 426, "ts": TS
            }),
        ),
        (
            "model_provider_changed",
            json!({
                "v": 1, "type": "model_provider_changed",
                "payload": {
                    "from": "local:vllm:qwen3.5-8b",
                    "to": "external:anthropic:claude-sonnet-4-6",
                    "trigger": "ttft_exceeded"
                },
                "session_id": SID, "seq": 430, "ts": TS
            }),
        ),
        (
            // Evento global: session_id = null, sin payload.
            "daemon_unavailable",
            json!({
                "v": 1, "type": "daemon_unavailable",
                "session_id": null, "seq": 7, "ts": TS
            }),
        ),
        (
            "error",
            json!({
                "v": 1, "type": "error",
                "payload": {"code": "provider_failed", "message": "sin proveedor disponible", "fatal": true},
                "session_id": SID, "seq": 431, "ts": TS
            }),
        ),
    ]
}

/// Catálogo de fixtures de comandos (sobre completo `CommandEnvelope`, sin `seq`).
pub fn command_fixtures() -> Vec<(&'static str, Value)> {
    vec![
        (
            "send_message",
            json!({
                "v": 1, "type": "send_message",
                "payload": {
                    "content": "Extrae la lógica de validación a un módulo separado",
                    "client_msg_id": "a3f0c2d4-0000-4000-8000-000000000001"
                },
                "session_id": SID, "ts": TS
            }),
        ),
        (
            "resolve_approval",
            json!({
                "v": 1, "type": "resolve_approval",
                "payload": {
                    "approval_id": "01J1ZJ9M0000000000000000CC",
                    "decision": "reject",
                    "reason": "no es el módulo correcto",
                    "remember_rule": false
                },
                "session_id": SID, "ts": TS
            }),
        ),
        (
            // Opcionales ausentes.
            "resolve_approval.minimal",
            json!({
                "v": 1, "type": "resolve_approval",
                "payload": {"approval_id": "01J1ZJ9M0000000000000000CC", "decision": "approve"},
                "session_id": SID, "ts": TS
            }),
        ),
        (
            "subscribe_session",
            json!({
                "v": 1, "type": "subscribe_session",
                "payload": {"session_id": SID, "after_seq": 418},
                "session_id": SID, "ts": TS
            }),
        ),
        (
            "unsubscribe_session",
            json!({
                "v": 1, "type": "unsubscribe_session",
                "payload": {"session_id": SID},
                "session_id": SID, "ts": TS
            }),
        ),
    ]
}
