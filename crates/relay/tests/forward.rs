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

/// Igual que `next_text` pero omite los frames de presencia
/// (`daemon_unavailable`) que el relay empuja al conectar sin daemon o al caerse
/// el daemon. Los tests de enrutamiento asertan sobre los frames enrutados, no
/// sobre la presencia (que carrera con el registro del daemon).
async fn next_routed(socket: &mut Ws) -> Option<String> {
    loop {
        let text = next_text(socket).await?;
        let is_presence = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| v.get("type").and_then(|t| t.as_str().map(str::to_owned)))
            .as_deref()
            == Some("daemon_unavailable");
        if !is_presence {
            return Some(text);
        }
    }
}

/// Siguiente `ToDaemon` en el canal del daemon, saltando los empujones de
/// snapshot (`announce_sessions`) que el relay manda cuando entra un suscriptor.
async fn next_command(socket: &mut Ws) -> Option<String> {
    loop {
        let text = next_text(socket).await?;
        let is_nudge = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| v.get("announce_sessions").and_then(|a| a.as_bool()))
            == Some(true);
        if !is_nudge {
            return Some(text);
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
    assert_eq!(next_routed(&mut sub1).await.as_deref(), Some(evento));
    assert_eq!(next_routed(&mut sub2).await.as_deref(), Some(evento));

    // dst: device → unicast (backlog de subscribe_session).
    let backlog = r#"{"v":1,"type":"session_state","seq":2}"#;
    daemon
        .send(Message::text(
            serde_json::json!({"dst": dev1, "frame": backlog}).to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(next_routed(&mut sub1).await.as_deref(), Some(backlog));
    assert_silence(&mut sub2).await;

    // Comando del cliente → daemon envuelto en ToDaemon{src}.
    let comando = r#"{"v":1,"type":"send_message","payload":{"content":"hola","client_msg_id":"x"},"session_id":null,"ts":"2026-07-06T18:03:52Z"}"#;
    sub1.send(Message::text(comando)).await.unwrap();
    let recibido = next_command(&mut daemon).await.expect("ToDaemon");
    let sobre: serde_json::Value = serde_json::from_str(&recibido).unwrap();
    assert_eq!(sobre["src"], dev1.as_str());
    assert_eq!(sobre["frame"], comando);

    // Otra cuenta no recibe el broadcast de esta.
    let (token_b, _) = register_and_login(&relay, "eva@example.com").await;
    let (tok_b, _) = common::login(&relay, "eva@example.com").await;
    let _ = token_b;
    let mut sub_extranjero = ws(&format!("{}/v1/subscribe?token={tok_b}", relay.ws_base)).await;
    // La cuenta B no tiene daemon: su suscriptor recibe el frame de presencia
    // inicial `daemon_unavailable`. Se consume antes de asertar el aislamiento.
    let presencia = next_text(&mut sub_extranjero).await.expect("presencia B");
    assert!(presencia.contains("daemon_unavailable"), "presencia: {presencia}");
    daemon
        .send(Message::text(
            serde_json::json!({"frame": "solo-cuenta-a"}).to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(next_routed(&mut sub1).await.as_deref(), Some("solo-cuenta-a"));
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
async fn presencia_al_conectar_sin_daemon() {
    // Un suscriptor que entra sin daemon conectado recibe `daemon_unavailable`
    // de inmediato, sin mandar ningún comando: así el indicador queda honesto
    // ("escritorio offline") y el compositor pasa a "Encolar".
    let relay = spawn().await;
    let (token, _) = register_and_login(&relay, "ana@example.com").await;
    let (tok1, _) = common::login(&relay, "ana@example.com").await;
    let _ = token;

    let mut sub = ws(&format!("{}/v1/subscribe?token={tok1}", relay.ws_base)).await;
    let text = next_text(&mut sub).await.expect("presencia inicial");
    let ev: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(ev["type"], "daemon_unavailable");
    assert_eq!(ev["session_id"], serde_json::Value::Null);
}

#[tokio::test]
async fn presencia_por_difusion_cuando_el_daemon_se_cae() {
    // Con el daemon en línea el suscriptor no está "offline"; cuando el daemon
    // se desconecta, el relay difunde `daemon_unavailable` para que el móvil
    // vuelva a "Encolar" sin tener que reintentar un comando.
    let relay = spawn().await;
    let (token, _) = register_and_login(&relay, "ana@example.com").await;
    let (daemon_token, _) = pair_daemon(&relay, &token).await;
    let (tok1, _) = common::login(&relay, "ana@example.com").await;

    let mut daemon = ws(&format!("{}/v1/connect?token={daemon_token}", relay.ws_base)).await;
    let mut sub = ws(&format!("{}/v1/subscribe?token={tok1}", relay.ws_base)).await;

    // Sincronizar: un evento enrutado confirma que ambos están registrados y
    // consume cualquier presencia inicial (carrera de registro).
    daemon
        .send(Message::text(serde_json::json!({"frame": "ping"}).to_string()))
        .await
        .unwrap();
    assert_eq!(next_routed(&mut sub).await.as_deref(), Some("ping"));

    // El daemon se cae → el suscriptor recibe la presencia por difusión.
    drop(daemon);
    let text = next_text(&mut sub).await.expect("presencia por caída");
    assert!(text.contains("daemon_unavailable"), "presencia: {text}");
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
    let recibido = next_command(&mut segundo).await.expect("ToDaemon");
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

#[tokio::test]
async fn empujon_de_snapshot_al_suscribirse_con_daemon() {
    // Con daemon conectado, un suscriptor nuevo provoca un ToDaemon con
    // `announce_sessions: true` y `src` = su device (el daemon le unicasta
    // su lista de sesiones; el relay no fabrica datos, RNF-10).
    let relay = spawn().await;
    let (token, _) = register_and_login(&relay, "ana@example.com").await;
    let (daemon_token, _) = pair_daemon(&relay, &token).await;
    let mut daemon = ws(&format!("{}/v1/connect?token={daemon_token}", relay.ws_base)).await;

    let (tok1, dev1) = common::login(&relay, "ana@example.com").await;
    let _sub = ws(&format!("{}/v1/subscribe?token={tok1}", relay.ws_base)).await;

    let nudge = next_text(&mut daemon).await.expect("empujón");
    let sobre: serde_json::Value = serde_json::from_str(&nudge).unwrap();
    assert_eq!(sobre["announce_sessions"], true);
    assert_eq!(sobre["src"], dev1.as_str());
    assert_eq!(sobre["frame"], "");
}
