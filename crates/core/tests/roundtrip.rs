//! Round-trip serde para cada evento y comando del catálogo C-3:
//! JSON (fixture con la forma exacta del contrato) → tipo → JSON idéntico.

use rutsubo_core::commands::CommandEnvelope;
use rutsubo_core::envelope::Envelope;
use rutsubo_core::events::Event;
use serde_json::{Value, json};

fn roundtrip_event(fixture: Value) {
    let parsed: Envelope<Event> =
        serde_json::from_value(fixture.clone()).expect("el fixture debe deserializar");
    let back = serde_json::to_value(&parsed).expect("el tipo debe serializar");
    assert_eq!(
        fixture, back,
        "el round-trip debe ser idéntico campo a campo"
    );
}

fn roundtrip_command(fixture: Value) {
    let parsed: CommandEnvelope =
        serde_json::from_value(fixture.clone()).expect("el fixture debe deserializar");
    let back = serde_json::to_value(&parsed).expect("el tipo debe serializar");
    assert_eq!(
        fixture, back,
        "el round-trip debe ser idéntico campo a campo"
    );
}

const SID: &str = "01J1ZG7QXW8Y2K3M4N5P6Q7R8S";
const TS: &str = "2026-07-06T18:03:52Z";

#[test]
fn session_state() {
    roundtrip_event(json!({
        "v": 1, "type": "session_state",
        "payload": {"state": "waiting_approval", "reason": "aprobación pendiente"},
        "session_id": SID, "seq": 422, "ts": TS
    }));
    // title opcional presente; reason ausente.
    roundtrip_event(json!({
        "v": 1, "type": "session_state",
        "payload": {"state": "idle", "title": "Refactor validación"},
        "session_id": SID, "seq": 1, "ts": TS
    }));
}

#[test]
fn message_delta() {
    roundtrip_event(json!({
        "v": 1, "type": "message_delta",
        "payload": {"message_id": "01J1ZH2K0000000000000000AA", "delta": "Voy a leer el"},
        "session_id": SID, "seq": 416, "ts": TS
    }));
}

#[test]
fn message_completed() {
    roundtrip_event(json!({
        "v": 1, "type": "message_completed",
        "payload": {
            "message_id": "01J1ZH2K0000000000000000AA",
            "stop_reason": "end_turn",
            "usage": {"prompt_tokens": 1200, "completion_tokens": 340}
        },
        "session_id": SID, "seq": 427, "ts": TS
    }));
}

#[test]
fn tool_call_requested() {
    roundtrip_event(json!({
        "v": 1, "type": "tool_call_requested",
        "payload": {
            "tool_call_id": "01J1ZJ9M0000000000000000BB",
            "tool": "read_file",
            "args": {"path": "src/ctrl.rs"}
        },
        "session_id": SID, "seq": 418, "ts": TS
    }));
}

#[test]
fn approval_request() {
    roundtrip_event(json!({
        "v": 1, "type": "approval_request",
        "payload": {
            "approval_id": "01J1ZJ9M0000000000000000CC",
            "tool_call_id": "01J1ZJ9M0000000000000000BB",
            "tool": "run_shell",
            "summary": "cargo test -p core",
            "args": {"cmd": "cargo test -p core"}
        },
        "session_id": SID, "seq": 421, "ts": TS
    }));
    // Con expires_at opcional.
    roundtrip_event(json!({
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
    }));
}

#[test]
fn approval_resolved() {
    roundtrip_event(json!({
        "v": 1, "type": "approval_resolved",
        "payload": {
            "approval_id": "01J1ZJ9M0000000000000000CC",
            "decision": "approve",
            "resolved_by": "device:01JOX00000000000000000DD00"
        },
        "session_id": SID, "seq": 423, "ts": TS
    }));
}

#[test]
fn tool_result() {
    roundtrip_event(json!({
        "v": 1, "type": "tool_result",
        "payload": {
            "tool_call_id": "01J1ZJ9M0000000000000000BB",
            "ok": true,
            "output_excerpt": "23 passed...",
            "truncated": false
        },
        "session_id": SID, "seq": 425, "ts": TS
    }));
}

#[test]
fn file_diff() {
    roundtrip_event(json!({
        "v": 1, "type": "file_diff",
        "payload": {
            "tool_call_id": "01J1ZJ9M0000000000000000BB",
            "path": "src/validators.rs",
            "diff_unified": "--- a/src/validators.rs\n+++ b/src/validators.rs\n@@ -1 +1 @@\n-a\n+b\n",
            "additions": 14,
            "deletions": 6
        },
        "session_id": SID, "seq": 426, "ts": TS
    }));
}

#[test]
fn model_provider_changed() {
    roundtrip_event(json!({
        "v": 1, "type": "model_provider_changed",
        "payload": {
            "from": "local:vllm:qwen3.5-8b",
            "to": "external:anthropic:claude-sonnet-4-6",
            "trigger": "ttft_exceeded"
        },
        "session_id": SID, "seq": 430, "ts": TS
    }));
}

#[test]
fn daemon_unavailable() {
    // Evento global: session_id = null, sin payload.
    roundtrip_event(json!({
        "v": 1, "type": "daemon_unavailable",
        "session_id": null, "seq": 7, "ts": TS
    }));
}

#[test]
fn error_event() {
    roundtrip_event(json!({
        "v": 1, "type": "error",
        "payload": {"code": "provider_failed", "message": "sin proveedor disponible", "fatal": true},
        "session_id": SID, "seq": 431, "ts": TS
    }));
}

// ---- Comandos (clientes → daemon) ----

#[test]
fn send_message() {
    roundtrip_command(json!({
        "v": 1, "type": "send_message",
        "payload": {
            "content": "Extrae la lógica de validación a un módulo separado",
            "client_msg_id": "a3f0c2d4-0000-4000-8000-000000000001"
        },
        "session_id": SID, "ts": TS
    }));
}

#[test]
fn resolve_approval() {
    roundtrip_command(json!({
        "v": 1, "type": "resolve_approval",
        "payload": {
            "approval_id": "01J1ZJ9M0000000000000000CC",
            "decision": "reject",
            "reason": "no es el módulo correcto",
            "remember_rule": false
        },
        "session_id": SID, "ts": TS
    }));
    // Opcionales ausentes.
    roundtrip_command(json!({
        "v": 1, "type": "resolve_approval",
        "payload": {"approval_id": "01J1ZJ9M0000000000000000CC", "decision": "approve"},
        "session_id": SID, "ts": TS
    }));
}

#[test]
fn subscribe_session() {
    roundtrip_command(json!({
        "v": 1, "type": "subscribe_session",
        "payload": {"session_id": SID, "after_seq": 418},
        "session_id": SID, "ts": TS
    }));
}

#[test]
fn unsubscribe_session() {
    roundtrip_command(json!({
        "v": 1, "type": "unsubscribe_session",
        "payload": {"session_id": SID},
        "session_id": SID, "ts": TS
    }));
}

#[test]
fn el_sobre_de_evento_expone_seq_y_version() {
    let env: Envelope<Event> = serde_json::from_value(json!({
        "v": 1, "type": "message_delta",
        "payload": {"message_id": "01J1ZH2K0000000000000000AA", "delta": "x"},
        "session_id": SID, "seq": 9, "ts": TS
    }))
    .unwrap();
    assert_eq!(env.v, 1);
    assert_eq!(env.seq, Some(9));
    assert_eq!(env.body.kind(), "message_delta");
}
