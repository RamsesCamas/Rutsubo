//! WebSocket local de eventos — `GET /v1/ws` (C-3, Fase D).
//!
//! - Auth: mismo Bearer que REST; se acepta `?token=` **solo** para el
//!   handshake del navegador (la API WebSocket del browser no permite
//!   headers). Excepción local documentada: en el relay (C-2) el token
//!   viaja en el handshake HTTPS.
//! - `subscribe_session {session_id, after_seq}`: el daemon responde con los
//!   eventos faltantes (≤1000; brecha mayor → el cliente completa por REST) y
//!   empalma el flujo vivo **sin duplicar `seq`** (la suscripción al bus
//!   ocurre al conectar, antes de leer el backlog; se deduplica por seq).
//! - `send_message` y `resolve_approval` ejecutan el mismo código interno que
//!   sus endpoints REST: un solo servicio, dos transportes.
//! - Ping cada 30 s; sin pong en 90 s → cierre.

use crate::error::ApiError;
use crate::state::App;
use axum::extract::ws::{Message, Utf8Bytes, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use futures::{SinkExt, StreamExt};
use rutsubo_core::api::{DecisionRequest, SendMessageRequest};
use rutsubo_core::commands::{Command, CommandEnvelope};
use rutsubo_core::envelope::{Envelope, PROTOCOL_VERSION};
use rutsubo_core::events::Event;
use rutsubo_core::ids::SessionId;
use serde::Deserialize;
use std::collections::HashMap;
use std::time::Duration;
use tokio::time::Instant;

const PING_INTERVAL: Duration = Duration::from_secs(30);
const PONG_DEADLINE: Duration = Duration::from_secs(90);
/// Backlog máximo por suscripción; brecha mayor → replay REST (C-3).
const MAX_BACKLOG: i64 = 1000;

/// Identidad del decisor cuando la orden llega por WS local.
const LOCAL_WS_RESOLVER: &str = "local:ws";

#[derive(Deserialize)]
pub struct WsQuery {
    token: Option<String>,
}

pub async fn ws_handler(
    State(app): State<App>,
    Query(query): Query<WsQuery>,
    headers: axum::http::HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    let bearer = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::to_owned);
    let presented = bearer.or(query.token);
    let authorized = presented
        .map(|t| crate::auth::token_matches(&app.token, &t))
        .unwrap_or(false);
    if !authorized {
        return ApiError::unauthorized().into_response();
    }
    upgrade.on_upgrade(move |socket| connection(app, socket))
}

async fn connection(app: App, socket: WebSocket) {
    let (mut sink, mut incoming) = socket.split();
    // Suscripción al bus ANTES de cualquier backlog: lo vivo se acumula aquí
    // mientras se hace replay y el dedup por seq garantiza exactamente-una-vez.
    let mut bus = app.bus.subscribe();
    // Sesiones suscritas → último seq enviado.
    let mut subs: HashMap<SessionId, u64> = HashMap::new();
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
                        if handle_command(&app, &mut sink, &mut subs, text.as_str())
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    Some(Ok(_)) => {} // binarios/pings: el runtime responde pings solo
                }
            }
            event = bus.recv() => {
                match event {
                    Ok(envelope) => {
                        if forward_live(&mut sink, &mut subs, &envelope).await.is_err() {
                            break;
                        }
                    }
                    // Cliente rezagado: se corta para que reconecte con
                    // after_seq y reponga por replay (C-3 §3.3.5).
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => break,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
            _ = ping.tick() => {
                if last_pong.elapsed() > PONG_DEADLINE {
                    break; // sin pong en 90 s
                }
                if sink.send(Message::Ping(Vec::new().into())).await.is_err() {
                    break;
                }
            }
        }
    }
}

type Sink = futures::stream::SplitSink<WebSocket, Message>;

async fn send_envelope(sink: &mut Sink, envelope: &Envelope<Event>) -> Result<(), axum::Error> {
    let text = serde_json::to_string(envelope).expect("los eventos siempre serializan");
    sink.send(Message::Text(Utf8Bytes::from(text))).await
}

/// Reenvía un evento vivo a las suscripciones activas, sin duplicar seq.
async fn forward_live(
    sink: &mut Sink,
    subs: &mut HashMap<SessionId, u64>,
    envelope: &Envelope<Event>,
) -> Result<(), axum::Error> {
    let (Some(session_id), Some(seq)) = (envelope.session_id, envelope.seq) else {
        return Ok(()); // globales: sin suscripción por sesión en fase local
    };
    if let Some(last_sent) = subs.get_mut(&session_id)
        && seq > *last_sent
    {
        *last_sent = seq;
        return send_envelope(sink, envelope).await;
    }
    Ok(())
}

/// Notificación de error de comando (no persistida: `seq = null`).
async fn send_command_error(
    sink: &mut Sink,
    session_id: Option<SessionId>,
    err: &ApiError,
) -> Result<(), axum::Error> {
    let envelope = Envelope {
        v: PROTOCOL_VERSION,
        body: Event::Error {
            code: format!("{:?}", err.code).to_lowercase(),
            message: err.message.clone(),
            fatal: false,
        },
        session_id,
        seq: None,
        ts: Utc::now(),
    };
    send_envelope(sink, &envelope).await
}

async fn handle_command(
    app: &App,
    sink: &mut Sink,
    subs: &mut HashMap<SessionId, u64>,
    text: &str,
) -> Result<(), axum::Error> {
    let envelope: CommandEnvelope = match serde_json::from_str(text) {
        Ok(env) => env,
        Err(err) => {
            let api_err = ApiError::validation(format!("comando inválido: {err}"), None);
            return send_command_error(sink, None, &api_err).await;
        }
    };

    match envelope.body {
        Command::SubscribeSession {
            session_id,
            after_seq,
        } => {
            subs.insert(session_id, after_seq);
            // Backlog ≤1000 y empalme: los eventos vivos que llegaron durante
            // el replay quedan en el canal y se filtran por seq.
            match crate::store::events::replay(&app.pool, &session_id, after_seq, MAX_BACKLOG).await
            {
                Ok(backlog) => {
                    for event in &backlog {
                        if let Some(seq) = event.seq {
                            let last = subs.entry(session_id).or_insert(after_seq);
                            if seq > *last {
                                *last = seq;
                                send_envelope(sink, event).await?;
                            }
                        }
                    }
                }
                Err(err) => {
                    send_command_error(sink, Some(session_id), &ApiError::internal(err)).await?;
                }
            }
        }
        Command::UnsubscribeSession { session_id } => {
            subs.remove(&session_id);
        }
        Command::SendMessage {
            content,
            client_msg_id,
        } => {
            let Some(session_id) = envelope.session_id else {
                let err =
                    ApiError::validation("send_message requiere session_id en el sobre", None);
                return send_command_error(sink, None, &err).await;
            };
            let req = SendMessageRequest {
                content,
                client_msg_id,
            };
            // Mismo código que POST /v1/sessions/{id}/messages.
            if let Err(err) =
                crate::api::sessions::send_message_inner(app, &session_id.to_string(), req).await
            {
                send_command_error(sink, Some(session_id), &err).await?;
            }
        }
        Command::ResolveApproval {
            approval_id,
            decision,
            reason,
            remember_rule,
        } => {
            let req = DecisionRequest {
                decision,
                reason,
                remember_rule,
            };
            // Mismo código que POST /v1/approvals/{id}/decision.
            if let Err(err) = crate::api::approvals::decide_inner(
                app,
                &approval_id.to_string(),
                req,
                LOCAL_WS_RESOLVER,
            )
            .await
            {
                send_command_error(sink, envelope.session_id, &err).await?;
            }
        }
    }
    Ok(())
}
