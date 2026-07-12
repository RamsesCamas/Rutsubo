//! Reenvío pub/sub (ADR-006, RNF-10): broadcast a la cuenta, unicast por
//! dispositivo, `daemon_unavailable` sin encolar y desplazamiento 4001.

mod common;

use common::{pair_daemon, register_and_login, spawn};
use futures::{SinkExt, StreamExt};
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn ws(url: &str) -> Ws {
    let (socket, _) = tokio_tungstenite::connect_async(url).await.expect("ws");
    socket
}

/// Siguiente frame de texto, ignorando pings, con timeout.
async fn next_text(socket: &mut Ws) -> Option<String> {
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(3), socket.next())
            .await
            .ok()??;
        match frame.ok()? {
            Message::Text(text) => return Some(text.to_string()),
            Message::Ping(_) | Message::Pong(_) => continue,
            Message::Close(_) => return None,
            _ => continue,
        }
    }
}

/// Verifica que NO llega ningún frame de texto en un lapso corto.
async fn assert_silence(socket: &mut Ws) {
    let result = tokio::time::timeout(Duration::from_millis(300), async {
        loop {
            match socket.next().await {
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
                other => return other,
            }
        }
    })
    .await;
    assert!(result.is_err(), "no debía llegar nada: {result:?}");
}

#[tokio::test]
async fn broadcast_unicast_y_comandos() {
    let relay = spawn().await;
    let (token, _) = register_and_login(&relay, "ana@example.com").await;
    let (daemon_token, _) = pair_daemon(&relay, &token).await;
    let (tok1, dev1) = common::login(&relay, "ana@example.com").await;
    let (tok2, _dev2) = common::login(&relay, "ana@example.com").await;

    let mut daemon = ws(&format!(
        "{}/v1/connect?token={daemon_token}",
        relay.ws_base
    ))
    .await;
    let mut sub1 = ws(&format!("{}/v1/subscribe?token={tok1}", relay.ws_base)).await;
    let mut sub2 = ws(&format!("{}/v1/subscribe?token={tok2}", relay.ws_base)).await;

    // dst: null → broadcast a todos los suscriptores de la cuenta. El frame
    // es opaco: el relay lo entrega tal cual.
    let evento = r#"{"v":1,"type":"message_delta","payload":{},"session_id":null,"seq":1,"ts":"2026-07-06T18:03:52Z"}"#;
    daemon
        .send(Message::text(
            serde_json::json!({"frame": evento}).to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(next_text(&mut sub1).await.as_deref(), Some(evento));
    assert_eq!(next_text(&mut sub2).await.as_deref(), Some(evento));

    // dst: device → unicast (backlog de subscribe_session).
    let backlog = r#"{"v":1,"type":"session_state","seq":2}"#;
    daemon
        .send(Message::text(
            serde_json::json!({"dst": dev1, "frame": backlog}).to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(next_text(&mut sub1).await.as_deref(), Some(backlog));
    assert_silence(&mut sub2).await;

    // Comando del cliente → daemon envuelto en ToDaemon{src}.
    let comando = r#"{"v":1,"type":"send_message","payload":{"content":"hola","client_msg_id":"x"},"session_id":null,"ts":"2026-07-06T18:03:52Z"}"#;
    sub1.send(Message::text(comando)).await.unwrap();
    let recibido = next_text(&mut daemon).await.expect("ToDaemon");
    let sobre: serde_json::Value = serde_json::from_str(&recibido).unwrap();
    assert_eq!(sobre["src"], dev1.as_str());
    assert_eq!(sobre["frame"], comando);

    // Otra cuenta no recibe el broadcast de esta.
    let (token_b, _) = register_and_login(&relay, "eva@example.com").await;
    let (tok_b, _) = common::login(&relay, "eva@example.com").await;
    let _ = token_b;
    let mut sub_extranjero = ws(&format!("{}/v1/subscribe?token={tok_b}", relay.ws_base)).await;
    daemon
        .send(Message::text(
            serde_json::json!({"frame": "solo-cuenta-a"}).to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(next_text(&mut sub1).await.as_deref(), Some("solo-cuenta-a"));
    assert_silence(&mut sub_extranjero).await;
}

#[tokio::test]
async fn daemon_unavailable_sin_daemon_conectado() {
    let relay = spawn().await;
    let (token, _) = register_and_login(&relay, "ana@example.com").await;
    let (tok1, _) = common::login(&relay, "ana@example.com").await;
    let _ = token;

    let mut sub = ws(&format!("{}/v1/subscribe?token={tok1}", relay.ws_base)).await;
    sub.send(Message::text(r#"{"v":1,"type":"send_message"}"#))
        .await
        .unwrap();
    let respuesta = next_text(&mut sub).await.expect("daemon_unavailable");
    let evento: serde_json::Value = serde_json::from_str(&respuesta).unwrap();
    assert_eq!(evento["type"], "daemon_unavailable");
    assert_eq!(evento["session_id"], serde_json::Value::Null);
    assert!(evento.get("seq").is_none(), "no persistido: sin seq");
}

#[tokio::test]
async fn segundo_daemon_desplaza_al_primero_con_4001() {
    let relay = spawn().await;
    let (token, _) = register_and_login(&relay, "ana@example.com").await;
    let (daemon_token, _) = pair_daemon(&relay, &token).await;

    let mut primero = ws(&format!(
        "{}/v1/connect?token={daemon_token}",
        relay.ws_base
    ))
    .await;
    let mut segundo = ws(&format!(
        "{}/v1/connect?token={daemon_token}",
        relay.ws_base
    ))
    .await;

    // El primero recibe close 4001 superseded.
    let close = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match primero.next().await {
                Some(Ok(Message::Close(frame))) => return frame,
                Some(Ok(_)) => continue,
                other => panic!("se esperaba close: {other:?}"),
            }
        }
    })
    .await
    .expect("close a tiempo")
    .expect("close con frame");
    assert_eq!(u16::from(close.code), 4001);
    assert_eq!(close.reason.as_str(), "superseded");

    // El segundo queda como daemon activo: un cliente le llega.
    let (tok1, dev1) = common::login(&relay, "ana@example.com").await;
    let mut sub = ws(&format!("{}/v1/subscribe?token={tok1}", relay.ws_base)).await;
    sub.send(Message::text("cmd")).await.unwrap();
    let recibido = next_text(&mut segundo).await.expect("ToDaemon");
    let sobre: serde_json::Value = serde_json::from_str(&recibido).unwrap();
    assert_eq!(sobre["src"], dev1.as_str());
}

#[tokio::test]
async fn el_canal_de_daemon_rechaza_dispositivos_cliente() {
    let relay = spawn().await;
    let (_token, _) = register_and_login(&relay, "ana@example.com").await;
    let (client_token, _) = common::login(&relay, "ana@example.com").await;

    // Un token de cliente en /v1/connect → 403 en el handshake.
    let err = tokio_tungstenite::connect_async(format!(
        "{}/v1/connect?token={client_token}",
        relay.ws_base
    ))
    .await
    .expect_err("handshake rechazado");
    match err {
        tokio_tungstenite::tungstenite::Error::Http(response) => {
            assert_eq!(response.status(), 403);
        }
        other => panic!("se esperaba error HTTP: {other:?}"),
    }

    // Y sin token → 401.
    let err = tokio_tungstenite::connect_async(format!("{}/v1/subscribe", relay.ws_base))
        .await
        .expect_err("handshake rechazado");
    match err {
        tokio_tungstenite::tungstenite::Error::Http(response) => {
            assert_eq!(response.status(), 401);
        }
        other => panic!("se esperaba error HTTP: {other:?}"),
    }
}
