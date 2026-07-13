//! Buzón de tareas offline (ADR-009): encolar, límites, cancelar, drenaje al
//! conectar el daemon, acuse (borrado) y dedup at-least-once.

mod common;

use common::{google_login, pair_daemon, spawn};
use futures::{SinkExt, StreamExt};
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

type Ws = tokio_tungstenite::WebSocketStream<
    tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
>;

async fn ws(url: &str) -> Ws {
    tokio_tungstenite::connect_async(url).await.expect("ws").0
}

async fn next_text(socket: &mut Ws) -> Option<String> {
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(3), socket.next())
            .await
            .ok()??;
        match frame.ok()? {
            Message::Text(t) => return Some(t.to_string()),
            Message::Ping(_) | Message::Pong(_) => continue,
            _ => return None,
        }
    }
}

fn enqueue_body(content: &str, client_msg_id: &str) -> serde_json::Value {
    serde_json::json!({
        "target": {"session_id": null, "new_session_title": "desde el móvil"},
        "payload_kind": "plaintext",
        "payload": content,
        "client_msg_id": client_msg_id
    })
}

#[tokio::test]
async fn encolar_offline_y_drenar_al_conectar_el_daemon() {
    let relay = spawn().await;
    let http = reqwest::Client::new();
    let (client_token, _) = google_login(&relay, "ana@example.com").await;
    let (daemon_token, _) = pair_daemon(&relay, &client_token).await;

    // Sin daemon conectado: la tarea queda `queued`.
    let accepted: serde_json::Value = http
        .post(format!("{}/v1/outbox", relay.base))
        .bearer_auth(&client_token)
        .json(&enqueue_body("agrega docstrings al módulo X", "c-1"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(accepted["state"], "queued");
    let outbox_id = accepted["outbox_id"].as_str().unwrap().to_owned();

    // GET /v1/outbox la muestra.
    let page: serde_json::Value = http
        .get(format!("{}/v1/outbox", relay.base))
        .bearer_auth(&client_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(page["items"].as_array().unwrap().len(), 1);

    // Conectar el daemon → debe recibir la tarea drenada (ToDaemon con outbox_id).
    let mut daemon = ws(&format!("{}/v1/connect?token={daemon_token}", relay.ws_base)).await;
    let drained = next_text(&mut daemon).await.expect("tarea drenada");
    let frame: serde_json::Value = serde_json::from_str(&drained).unwrap();
    assert_eq!(frame["outbox_id"], outbox_id.as_str());
    assert_eq!(frame["new_session_title"], "desde el móvil");
    // El frame interior es un CommandEnvelope::SendMessage.
    let inner: serde_json::Value = serde_json::from_str(frame["frame"].as_str().unwrap()).unwrap();
    assert_eq!(inner["type"], "send_message");
    assert_eq!(inner["payload"]["content"], "agrega docstrings al módulo X");

    // El daemon acusa → el relay borra la fila.
    daemon
        .send(Message::text(
            serde_json::json!({"ack_outbox_id": outbox_id, "frame": ""}).to_string(),
        ))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let page2: serde_json::Value = http
        .get(format!("{}/v1/outbox", relay.base))
        .bearer_auth(&client_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        page2["items"].as_array().unwrap().len(),
        0,
        "el ack borra la tarea"
    );
}

#[tokio::test]
async fn entrega_inmediata_si_el_daemon_esta_conectado() {
    let relay = spawn().await;
    let http = reqwest::Client::new();
    let (client_token, _) = google_login(&relay, "ana@example.com").await;
    let (daemon_token, _) = pair_daemon(&relay, &client_token).await;
    let mut daemon = ws(&format!("{}/v1/connect?token={daemon_token}", relay.ws_base)).await;

    let accepted: serde_json::Value = http
        .post(format!("{}/v1/outbox", relay.base))
        .bearer_auth(&client_token)
        .json(&enqueue_body("hola", "c-1"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(accepted["state"], "delivered", "daemon online = entrega inmediata");
    let frame = next_text(&mut daemon).await.expect("tarea");
    assert!(frame.contains("outbox_id"));
}

#[tokio::test]
async fn cancelar_e_idempotencia() {
    let relay = spawn().await;
    let http = reqwest::Client::new();
    let (client_token, _) = google_login(&relay, "ana@example.com").await;

    let a: serde_json::Value = http
        .post(format!("{}/v1/outbox", relay.base))
        .bearer_auth(&client_token)
        .json(&enqueue_body("tarea", "c-1"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = a["outbox_id"].as_str().unwrap().to_owned();

    // Reintento con el mismo client_msg_id → misma tarea (idempotencia).
    let b: serde_json::Value = http
        .post(format!("{}/v1/outbox", relay.base))
        .bearer_auth(&client_token)
        .json(&enqueue_body("tarea", "c-1"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(b["outbox_id"], id.as_str());

    // Cancelar la borra; segundo intento → 404.
    let del = http
        .delete(format!("{}/v1/outbox/{id}", relay.base))
        .bearer_auth(&client_token)
        .send()
        .await
        .unwrap();
    assert_eq!(del.status(), 204);
    let del2 = http
        .delete(format!("{}/v1/outbox/{id}", relay.base))
        .bearer_auth(&client_token)
        .send()
        .await
        .unwrap();
    assert_eq!(del2.status(), 404);
}

#[tokio::test]
async fn limite_de_20_tareas() {
    let relay = spawn().await;
    let http = reqwest::Client::new();
    let (client_token, _) = google_login(&relay, "ana@example.com").await;
    for i in 0..20 {
        let res = http
            .post(format!("{}/v1/outbox", relay.base))
            .bearer_auth(&client_token)
            .json(&enqueue_body("t", &format!("c-{i}")))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 200, "tarea {i}");
    }
    let full = http
        .post(format!("{}/v1/outbox", relay.base))
        .bearer_auth(&client_token)
        .json(&enqueue_body("t", "c-21"))
        .send()
        .await
        .unwrap();
    assert_eq!(full.status(), 422, "buzón lleno");
}

#[tokio::test]
async fn outbox_exige_auth() {
    let relay = spawn().await;
    let res = reqwest::Client::new()
        .post(format!("{}/v1/outbox", relay.base))
        .json(&enqueue_body("t", "c-1"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 401);
}
