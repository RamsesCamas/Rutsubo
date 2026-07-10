//! Aceptación Fase D — WebSocket local:
//! 50 eventos, corte a mitad, reconexión con `after_seq` y recepción
//! exactamente-una-vez en orden; comandos WS por el mismo código que REST;
//! 500+ eventos con `seq` sin huecos (DoD).

use chrono::Utc;
use futures::{SinkExt, StreamExt};
use rutsubo_core::events::{Event, SessionState};
use rutsubo_core::ids::{MessageId, SessionId};
use rutsubo_daemon::config::DaemonConfig;
use rutsubo_daemon::state::{App, AppState};
use rutsubo_daemon::store;
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn spawn_server() -> (App, SocketAddr, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let cfg = DaemonConfig {
        data_dir: dir.path().join("data"),
        bind: "127.0.0.1:0".parse().unwrap(),
        max_iterations: 20,
        spa_origin: None,
        external_api_key: None,
    };
    let app = AppState::bootstrap(cfg).await.unwrap();
    let router = rutsubo_daemon::api::router(app.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (app, addr, dir)
}

async fn create_session(app: &App, workspace: &std::path::Path) -> SessionId {
    let id = SessionId::new();
    store::sessions::create(
        &app.pool,
        &id,
        workspace.to_str().unwrap(),
        "ws-test",
        Utc::now(),
    )
    .await
    .unwrap();
    app.emit(
        id,
        Event::SessionState {
            state: SessionState::Idle,
            title: None,
            reason: None,
        },
        None,
    )
    .await
    .unwrap();
    id
}

async fn emit_delta(app: &App, sid: SessionId, i: usize) {
    app.emit(
        sid,
        Event::MessageDelta {
            message_id: MessageId::new(),
            delta: format!("d{i}"),
        },
        None,
    )
    .await
    .unwrap();
}

async fn connect(addr: SocketAddr, token: &str) -> WsStream {
    let (socket, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/v1/ws?token={token}"))
        .await
        .expect("handshake WS");
    socket
}

async fn subscribe(socket: &mut WsStream, sid: SessionId, after_seq: u64) {
    let cmd = json!({
        "v": 1, "type": "subscribe_session",
        "payload": {"session_id": sid.to_string(), "after_seq": after_seq},
        "session_id": sid.to_string(), "ts": Utc::now().to_rfc3339()
    });
    socket
        .send(Message::Text(cmd.to_string().into()))
        .await
        .unwrap();
}

/// Siguiente evento (ignora pings/pongs), con timeout.
async fn recv_event(socket: &mut WsStream) -> Value {
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(5), socket.next())
            .await
            .expect("timeout esperando evento WS")
            .expect("stream cerrado")
            .expect("error de socket");
        match msg {
            Message::Text(text) => return serde_json::from_str(text.as_str()).unwrap(),
            Message::Ping(_) | Message::Pong(_) => continue,
            other => panic!("frame inesperado: {other:?}"),
        }
    }
}

#[tokio::test]
async fn reconexion_con_after_seq_entrega_exactamente_una_vez() {
    let (app, addr, _dir) = spawn_server().await;
    let ws = tempfile::tempdir().unwrap();
    let sid = create_session(&app, ws.path()).await; // seq 1
    for i in 0..50 {
        emit_delta(&app, sid, i).await; // seq 2..=51
    }

    // Conexión 1: suscribirse desde 0 y leer solo la mitad.
    let mut socket = connect(addr, &app.token).await;
    subscribe(&mut socket, sid, 0).await;
    let mut received: Vec<u64> = Vec::new();
    for _ in 0..25 {
        let event = recv_event(&mut socket).await;
        received.push(event["seq"].as_u64().unwrap());
    }
    assert_eq!(received, (1..=25).collect::<Vec<u64>>());
    drop(socket); // corte a mitad

    // Mientras estaba desconectado llegan más eventos.
    for i in 50..60 {
        emit_delta(&app, sid, i).await; // seq 52..=61
    }

    // Conexión 2: reconecta con after_seq=25 → 26..=61 exactamente una vez.
    let mut socket = connect(addr, &app.token).await;
    subscribe(&mut socket, sid, 25).await;
    let mut received: Vec<u64> = Vec::new();
    while received.last() != Some(&61) {
        let event = recv_event(&mut socket).await;
        received.push(event["seq"].as_u64().unwrap());
    }
    assert_eq!(
        received,
        (26..=61).collect::<Vec<u64>>(),
        "en orden, sin huecos y sin duplicados"
    );

    // Empalme vivo: un evento nuevo llega por el bus sin duplicar seq.
    emit_delta(&app, sid, 99).await; // seq 62
    let event = recv_event(&mut socket).await;
    assert_eq!(event["seq"], 62);
}

#[tokio::test]
async fn comandos_ws_ejecutan_el_mismo_codigo_que_rest() {
    let (app, addr, _dir) = spawn_server().await;
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(ws.path().join("main.rs"), "fn main() {}\n").unwrap();
    let sid = create_session(&app, ws.path()).await;

    let mut socket = connect(addr, &app.token).await;
    subscribe(&mut socket, sid, 0).await;
    let _ = recv_event(&mut socket).await; // session_state inicial (seq 1)

    // send_message por WS (misma validación/idempotencia que REST).
    let cmd = json!({
        "v": 1, "type": "send_message",
        "payload": {"content": "Revisa main.rs y resume", "client_msg_id": "ws-1"},
        "session_id": sid.to_string(), "ts": Utc::now().to_rfc3339()
    });
    socket
        .send(Message::Text(cmd.to_string().into()))
        .await
        .unwrap();

    // El turno fluye en vivo hasta la aprobación del write_file.
    let approval_id = loop {
        let event = recv_event(&mut socket).await;
        if event["type"] == "approval_request" {
            assert_eq!(event["payload"]["tool"], "write_file");
            break event["payload"]["approval_id"].as_str().unwrap().to_owned();
        }
    };

    // resolve_approval por WS.
    let cmd = json!({
        "v": 1, "type": "resolve_approval",
        "payload": {"approval_id": approval_id, "decision": "approve"},
        "session_id": sid.to_string(), "ts": Utc::now().to_rfc3339()
    });
    socket
        .send(Message::Text(cmd.to_string().into()))
        .await
        .unwrap();

    // Hasta el cierre del turno: approval_resolved (resuelto por local:ws),
    // file_diff y message_completed, con seq estrictamente creciente.
    let mut resolved_by = String::new();
    let mut saw_diff = false;
    let mut last_seq = 1;
    loop {
        let event = recv_event(&mut socket).await;
        let seq = event["seq"].as_u64().unwrap();
        assert!(seq > last_seq, "seq estrictamente creciente sin duplicados");
        last_seq = seq;
        match event["type"].as_str().unwrap() {
            "approval_resolved" => {
                resolved_by = event["payload"]["resolved_by"].as_str().unwrap().to_owned();
            }
            "file_diff" => saw_diff = true,
            "message_completed" => break,
            _ => {}
        }
    }
    assert_eq!(resolved_by, "local:ws");
    assert!(saw_diff);

    // Idempotencia compartida: repetir el send_message no crea otro turno.
    let cmd = json!({
        "v": 1, "type": "send_message",
        "payload": {"content": "Revisa main.rs y resume", "client_msg_id": "ws-1"},
        "session_id": sid.to_string(), "ts": Utc::now().to_rfc3339()
    });
    socket
        .send(Message::Text(cmd.to_string().into()))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let history = store::messages::history(&app.pool, &sid).await.unwrap();
    assert_eq!(
        history.len(),
        2,
        "user + assistant: el duplicado no reprocesa"
    );
}

#[tokio::test]
async fn ws_sin_token_rechazado() {
    let (_app, addr, _dir) = spawn_server().await;
    let err = tokio_tungstenite::connect_async(format!("ws://{addr}/v1/ws"))
        .await
        .expect_err("el handshake debe fallar sin token");
    match err {
        tokio_tungstenite::tungstenite::Error::Http(response) => {
            assert_eq!(response.status(), 401);
        }
        other => panic!("error inesperado: {other:?}"),
    }
}

#[tokio::test]
async fn quinientos_eventos_sin_huecos() {
    let (app, addr, _dir) = spawn_server().await;
    let ws = tempfile::tempdir().unwrap();
    let sid = create_session(&app, ws.path()).await; // seq 1
    for i in 0..600 {
        emit_delta(&app, sid, i).await; // seq 2..=601
    }

    let mut socket = connect(addr, &app.token).await;
    subscribe(&mut socket, sid, 0).await;
    let mut seqs: Vec<u64> = Vec::with_capacity(601);
    while seqs.last() != Some(&601) {
        let event = recv_event(&mut socket).await;
        seqs.push(event["seq"].as_u64().unwrap());
    }
    assert_eq!(
        seqs,
        (1..=601).collect::<Vec<u64>>(),
        "seq sin huecos (DoD)"
    );
}
