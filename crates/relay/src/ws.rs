//! Canales WebSocket del relay (C-2): `/v1/connect` (daemon saliente, RF-23)
//! y `/v1/subscribe` (clientes web/móvil). El tráfico C-3 se reenvía opaco;
//! lo único que el relay interpreta es el sobre de enrutamiento
//! (`rutsubo_core::relay`). Ping cada 30 s; sin pong en 90 s → close 4002.

use crate::RelayState;
use crate::auth::{AuthedDevice, authenticate, bearer_from_headers};
use crate::error::RelayError;
use crate::hub::{CLOSE_IDLE, CLOSE_SUPERSEDED, close_frame};
use axum::extract::ws::{Message, Utf8Bytes, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use futures::{SinkExt, StreamExt};
use rutsubo_core::relay::{FromDaemon, ToDaemon};
use serde::Deserialize;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::Instant;
use ulid::Ulid;

const PING_INTERVAL: Duration = Duration::from_secs(30);
const PONG_DEADLINE: Duration = Duration::from_secs(90);
/// Cola de salida por conexión; si se llena, los frames se pierden y el
/// cliente repone por seq (C-3) — el relay no encola tránsito (RNF-10).
const OUTBOX_CAPACITY: usize = 256;

#[derive(Deserialize)]
pub struct WsQuery {
    token: Option<String>,
}

/// Auth del handshake: Bearer o `?token=` (los WebSocket de navegador no
/// permiten headers; excepción documentada, igual que en el daemon).
async fn ws_device(
    state: &RelayState,
    headers: &HeaderMap,
    query: WsQuery,
    expected_kind: &str,
) -> Result<AuthedDevice, RelayError> {
    let token = bearer_from_headers(headers)
        .or(query.token)
        .ok_or_else(RelayError::unauthorized)?;
    let device = authenticate(state, &token).await?;
    if device.kind != expected_kind {
        return Err(RelayError::forbidden(format!(
            "este canal exige un dispositivo `{expected_kind}`"
        )));
    }
    Ok(device)
}

// ---- GET /v1/connect — canal del daemon ----

pub async fn connect(
    State(state): State<RelayState>,
    Query(query): Query<WsQuery>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    let device = match ws_device(&state, &headers, query, "daemon").await {
        Ok(device) => device,
        Err(err) => return err.into_response(),
    };
    upgrade.on_upgrade(move |socket| daemon_connection(state, device, socket))
}

async fn daemon_connection(state: RelayState, device: AuthedDevice, socket: WebSocket) {
    let conn_id = Ulid::new().to_string();
    let (tx, mut rx) = mpsc::channel::<Message>(OUTBOX_CAPACITY);
    // A lo sumo un daemon por cuenta: el anterior recibe close 4001.
    if let Some(previous) =
        state
            .hub
            .register_daemon(&device.account_id, &conn_id, &device.device_id, tx)
    {
        let _ = previous.try_send(close_frame(CLOSE_SUPERSEDED, "superseded"));
    }
    tracing::info!(account = %device.account_id, device = %device.device_id, "daemon conectado");

    let (mut sink, mut incoming) = socket.split();
    let mut ping = tokio::time::interval(PING_INTERVAL);
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_pong = Instant::now();

    loop {
        tokio::select! {
            message = incoming.next() => {
                match message {
                    None | Some(Err(_)) | Some(Ok(Message::Close(_))) => break,
                    Some(Ok(Message::Pong(_))) => last_pong = Instant::now(),
                    Some(Ok(Message::Text(text))) => {
                        route_from_daemon(&state, &device.account_id, text.as_str());
                    }
                    Some(Ok(_)) => {}
                }
            }
            queued = rx.recv() => {
                match queued {
                    Some(Message::Close(frame)) => {
                        let _ = sink.send(Message::Close(frame)).await;
                        break;
                    }
                    Some(message) => {
                        if sink.send(message).await.is_err() {
                            break;
                        }
                    }
                    None => break,
                }
            }
            _ = ping.tick() => {
                if last_pong.elapsed() > PONG_DEADLINE {
                    let _ = sink.send(close_frame(CLOSE_IDLE, "idle")).await;
                    break;
                }
                if sink.send(Message::Ping(Vec::new().into())).await.is_err() {
                    break;
                }
            }
        }
    }

    state.hub.unregister_daemon(&device.account_id, &conn_id);
    tracing::info!(account = %device.account_id, "daemon desconectado");
}

/// Enruta un sobre `FromDaemon`: broadcast a la cuenta o unicast al device.
/// El `frame` interior jamás se deserializa (RNF-10).
fn route_from_daemon(state: &RelayState, account_id: &str, text: &str) {
    match serde_json::from_str::<FromDaemon>(text) {
        Ok(FromDaemon { dst: None, frame }) => state.hub.broadcast(account_id, &frame),
        Ok(FromDaemon {
            dst: Some(device_id),
            frame,
        }) => state.hub.send_to(account_id, &device_id, &frame),
        Err(err) => {
            tracing::warn!(%err, "sobre de enrutamiento inválido del daemon; descartado");
        }
    }
}

// ---- GET /v1/subscribe — canal de clientes ----

pub async fn subscribe(
    State(state): State<RelayState>,
    Query(query): Query<WsQuery>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    let device = match ws_device(&state, &headers, query, "client").await {
        Ok(device) => device,
        Err(err) => return err.into_response(),
    };
    upgrade.on_upgrade(move |socket| subscriber_connection(state, device, socket))
}

async fn subscriber_connection(state: RelayState, device: AuthedDevice, socket: WebSocket) {
    let conn_id = Ulid::new().to_string();
    let (tx, mut rx) = mpsc::channel::<Message>(OUTBOX_CAPACITY);
    state
        .hub
        .register_subscriber(&device.account_id, &conn_id, &device.device_id, tx);
    tracing::info!(account = %device.account_id, device = %device.device_id, "suscriptor conectado");

    let (mut sink, mut incoming) = socket.split();
    let mut ping = tokio::time::interval(PING_INTERVAL);
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_pong = Instant::now();

    loop {
        tokio::select! {
            message = incoming.next() => {
                match message {
                    None | Some(Err(_)) | Some(Ok(Message::Close(_))) => break,
                    Some(Ok(Message::Pong(_))) => last_pong = Instant::now(),
                    Some(Ok(Message::Text(text))) => {
                        if forward_to_daemon(&state, &device, text.as_str(), &mut sink)
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    Some(Ok(_)) => {}
                }
            }
            queued = rx.recv() => {
                match queued {
                    Some(Message::Close(frame)) => {
                        let _ = sink.send(Message::Close(frame)).await;
                        break;
                    }
                    Some(message) => {
                        if sink.send(message).await.is_err() {
                            break;
                        }
                    }
                    None => break,
                }
            }
            _ = ping.tick() => {
                if last_pong.elapsed() > PONG_DEADLINE {
                    let _ = sink.send(close_frame(CLOSE_IDLE, "idle")).await;
                    break;
                }
                if sink.send(Message::Ping(Vec::new().into())).await.is_err() {
                    break;
                }
            }
        }
    }

    state
        .hub
        .unregister_subscriber(&device.account_id, &conn_id);
    tracing::info!(account = %device.account_id, "suscriptor desconectado");
}

type Sink = futures::stream::SplitSink<WebSocket, Message>;

/// Reenvía el comando del cliente al daemon de la cuenta, envuelto en
/// `ToDaemon{src}`. Sin daemon conectado responde `daemon_unavailable`
/// (RNF-12) y descarta: el relay no encola.
async fn forward_to_daemon(
    state: &RelayState,
    device: &AuthedDevice,
    text: &str,
    sink: &mut Sink,
) -> Result<(), axum::Error> {
    if let Some(daemon) = state.hub.daemon_tx(&device.account_id) {
        let envelope = ToDaemon {
            src: device.device_id.clone(),
            frame: text.to_owned(),
        };
        let serialized = serde_json::to_string(&envelope).expect("ToDaemon siempre serializa");
        if daemon
            .try_send(Message::Text(Utf8Bytes::from(serialized)))
            .is_ok()
        {
            return Ok(());
        }
    }
    let unavailable = serde_json::json!({
        "v": 1,
        "type": "daemon_unavailable",
        "session_id": null,
        "ts": Utc::now(),
    });
    sink.send(Message::Text(Utf8Bytes::from(unavailable.to_string())))
        .await
}
