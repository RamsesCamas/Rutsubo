//! Aceptación Fase C — E2E sin red con MockProvider:
//! crear sesión → mensaje → deltas → tool_call → approval_request → aprobar
//! por API → tool_result → file_diff → message_completed → idle, verificando
//! `seq` consecutivo sin huecos y audit_log con provider_id (RF-22).

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use rutsubo_daemon::api;
use rutsubo_daemon::config::DaemonConfig;
use rutsubo_daemon::state::{App, AppState};
use serde_json::{Value, json};
use std::time::Duration;
use tower::util::ServiceExt;

async fn test_app() -> (App, Router, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let cfg = DaemonConfig {
        data_dir: dir.path().join("data"),
        bind: "127.0.0.1:0".parse().unwrap(),
        max_iterations: 20,
        spa_origin: None,
        groq_api_key: None,
    };
    let app = AppState::bootstrap(cfg).await.unwrap();
    let router = api::router(app.clone());
    (app, router, dir)
}

fn request(method: &str, uri: &str, token: &str, body: Option<Value>) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"));
    match body {
        Some(v) => builder
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(v.to_string()))
            .unwrap(),
        None => {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
            builder.body(Body::empty()).unwrap()
        }
    }
}

async fn send(router: &Router, req: Request<Body>) -> (StatusCode, Value) {
    let res = router.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

/// Espera (con timeout) a que un predicado sobre el replay de eventos se cumpla.
async fn wait_for_events<F>(router: &Router, token: &str, sid: &str, pred: F) -> Vec<Value>
where
    F: Fn(&[Value]) -> bool,
{
    for _ in 0..400 {
        let (_, body) = send(
            router,
            request(
                "GET",
                &format!("/v1/sessions/{sid}/events?limit=1000"),
                token,
                None,
            ),
        )
        .await;
        let events = body["events"].as_array().cloned().unwrap_or_default();
        if pred(&events) {
            return events;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("timeout esperando eventos de la sesión {sid}");
}

fn kinds(events: &[Value]) -> Vec<String> {
    events
        .iter()
        .map(|e| e["type"].as_str().unwrap_or("?").to_owned())
        .collect()
}

fn has_kind(events: &[Value], kind: &str) -> bool {
    events.iter().any(|e| e["type"] == kind)
}

fn assert_seq_sin_huecos(events: &[Value]) {
    let seqs: Vec<u64> = events.iter().map(|e| e["seq"].as_u64().unwrap()).collect();
    let expected: Vec<u64> = (1..=seqs.len() as u64).collect();
    assert_eq!(seqs, expected, "seq consecutivo sin huecos (C-3)");
}

#[tokio::test]
async fn turno_completo_con_aprobacion_de_write_file() {
    let (app, router, _dir) = test_app().await;
    let token = app.token.clone();
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(ws.path().join("main.rs"), "fn main() {}\n").unwrap();

    // Sesión + mensaje (sin "test": el guion va read_file → write_file).
    let (status, body) = send(
        &router,
        request(
            "POST",
            "/v1/sessions",
            &token,
            Some(json!({"workspace_path": ws.path().to_str().unwrap(), "title": "demo"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let sid = body["id"].as_str().unwrap().to_owned();

    let (status, _) = send(
        &router,
        request(
            "POST",
            &format!("/v1/sessions/{sid}/messages"),
            &token,
            Some(json!({"content": "Revisa main.rs y deja tus notas", "client_msg_id": "e2e-1"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);

    // El loop corre: deltas → read_file → write_file ⇒ approval_request y,
    // justo después, session_state(waiting_approval) — esperamos la
    // transición (transaccional con su evento) para evitar la carrera.
    let events = wait_for_events(&router, &token, &sid, |ev| {
        has_kind(ev, "approval_request")
            && ev.iter().any(|e| {
                e["type"] == "session_state" && e["payload"]["state"] == "waiting_approval"
            })
    })
    .await;
    assert!(has_kind(&events, "message_delta"));
    assert!(has_kind(&events, "tool_call_requested"));
    let first_tool = events
        .iter()
        .find(|e| e["type"] == "tool_call_requested")
        .unwrap();
    assert_eq!(first_tool["payload"]["tool"], "read_file");
    assert_eq!(first_tool["payload"]["args"]["path"], "main.rs");

    // La sesión quedó suspendida esperando decisión (RF-16).
    let (_, detail) = send(
        &router,
        request("GET", &format!("/v1/sessions/{sid}"), &token, None),
    )
    .await;
    assert_eq!(detail["state"], "waiting_approval");

    // La aprobación pendiente es del write_file.
    let (_, pending) = send(&router, request("GET", "/v1/approvals", &token, None)).await;
    let approval = &pending["approvals"][0];
    assert_eq!(approval["tool"], "write_file");
    let approval_id = approval["id"].as_str().unwrap().to_owned();

    // Aprobar por API → tool_result → file_diff → message_completed → idle.
    let (status, _) = send(
        &router,
        request(
            "POST",
            &format!("/v1/approvals/{approval_id}/decision"),
            &token,
            Some(json!({"decision": "approve"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let events = wait_for_events(&router, &token, &sid, |ev| {
        has_kind(ev, "message_completed")
            && ev
                .last()
                .is_some_and(|e| e["type"] == "session_state" && e["payload"]["state"] == "idle")
    })
    .await;

    // Cadena esperada (aceptación Fase C).
    let ks = kinds(&events);
    for kind in [
        "session_state",
        "message_delta",
        "tool_call_requested",
        "tool_result",
        "approval_request",
        "approval_resolved",
        "file_diff",
        "message_completed",
    ] {
        assert!(ks.contains(&kind.to_owned()), "falta {kind} en {ks:?}");
    }
    // El orden relativo clave: approval_request < approval_resolved <
    // file_diff < message_completed.
    let pos = |k: &str| ks.iter().position(|x| x == k).unwrap();
    assert!(pos("approval_request") < pos("approval_resolved"));
    assert!(pos("approval_resolved") < pos("file_diff"));
    assert!(pos("file_diff") < pos("message_completed"));

    // waiting_approval y vuelta a running quedaron registrados.
    let states: Vec<&str> = events
        .iter()
        .filter(|e| e["type"] == "session_state")
        .map(|e| e["payload"]["state"].as_str().unwrap())
        .collect();
    assert!(states.contains(&"waiting_approval"));
    assert_eq!(*states.last().unwrap(), "idle");

    // seq consecutivo sin huecos.
    assert_seq_sin_huecos(&events);

    // El archivo realmente se escribió y el diff refleja el alta.
    assert!(ws.path().join("RUTSUBO_NOTES.md").exists());
    let diff = events.iter().find(|e| e["type"] == "file_diff").unwrap();
    assert_eq!(diff["payload"]["path"], "RUTSUBO_NOTES.md");
    assert!(diff["payload"]["additions"].as_u64().unwrap() > 0);
    assert_eq!(diff["payload"]["deletions"], 0);

    // Audit log: cada llamada al modelo registró provider_id (RF-22).
    let (_, audit) = send(
        &router,
        request("GET", &format!("/v1/audit?session_id={sid}"), &token, None),
    )
    .await;
    let llm_calls: Vec<&Value> = audit["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "llm_call")
        .collect();
    assert!(llm_calls.len() >= 2, "una entrada por iteración del loop");
    for call in &llm_calls {
        assert!(
            call["detail"]["provider_id"]
                .as_str()
                .unwrap()
                .starts_with("groq:missing:")
        );
    }

    // El mensaje del asistente quedó persistido (RF-04).
    let (_, detail) = send(
        &router,
        request("GET", &format!("/v1/sessions/{sid}"), &token, None),
    )
    .await;
    assert_eq!(detail["message_count"], 2); // user + assistant
}

#[tokio::test]
async fn rechazo_no_aborta_el_turno() {
    let (app, router, _dir) = test_app().await;
    let token = app.token.clone();
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(ws.path().join("lib.rs"), "pub fn f() {}\n").unwrap();

    let (_, body) = send(
        &router,
        request(
            "POST",
            "/v1/sessions",
            &token,
            Some(json!({"workspace_path": ws.path().to_str().unwrap()})),
        ),
    )
    .await;
    let sid = body["id"].as_str().unwrap().to_owned();

    // Con "test" el guion añade run_shell antes del write_file.
    let (status, _) = send(
        &router,
        request(
            "POST",
            &format!("/v1/sessions/{sid}/messages"),
            &token,
            Some(json!({"content": "Corre los test de lib.rs", "client_msg_id": "e2e-2"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);

    // Primera aprobación: run_shell → RECHAZAR.
    wait_for_events(&router, &token, &sid, |ev| has_kind(ev, "approval_request")).await;
    let (_, pending) = send(&router, request("GET", "/v1/approvals", &token, None)).await;
    let first = &pending["approvals"][0];
    assert_eq!(first["tool"], "run_shell");
    assert_eq!(first["summary"], "cargo test -p core");
    let first_id = first["id"].as_str().unwrap().to_owned();
    send(
        &router,
        request(
            "POST",
            &format!("/v1/approvals/{first_id}/decision"),
            &token,
            Some(json!({"decision": "reject", "reason": "no en este workspace"})),
        ),
    )
    .await;

    // El turno CONTINÚA: aparece la segunda aprobación (write_file).
    let events = wait_for_events(&router, &token, &sid, |ev| {
        ev.iter()
            .filter(|e| e["type"] == "approval_request")
            .count()
            >= 2
    })
    .await;
    // El rechazo quedó como tool_result fallido, sin abortar la sesión.
    let rejected = events
        .iter()
        .find(|e| e["type"] == "tool_result" && e["payload"]["ok"] == false)
        .expect("tool_result del rechazo");
    assert!(
        rejected["payload"]["output_excerpt"]
            .as_str()
            .unwrap()
            .contains("rechazado")
    );

    // Aprobar el write_file y verificar cierre normal.
    let (_, pending) = send(&router, request("GET", "/v1/approvals", &token, None)).await;
    let second = &pending["approvals"][0];
    assert_eq!(second["tool"], "write_file");
    let second_id = second["id"].as_str().unwrap().to_owned();
    send(
        &router,
        request(
            "POST",
            &format!("/v1/approvals/{second_id}/decision"),
            &token,
            Some(json!({"decision": "approve"})),
        ),
    )
    .await;

    let events = wait_for_events(&router, &token, &sid, |ev| {
        has_kind(ev, "message_completed")
    })
    .await;
    assert_seq_sin_huecos(&events);
    assert!(has_kind(&events, "file_diff"));
}
