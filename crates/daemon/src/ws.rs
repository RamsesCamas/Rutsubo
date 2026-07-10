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
    ticket: Option<String>,
}

/// POST /v1/ws/ticket — emite un ticket efímero de un solo uso para el
/// handshake del WS. Pasa por el middleware de auth normal: en modo remoto
/// solo llega vía BFF (proxy + allowlist); en local exige el Bearer del token.
pub async fn issue_ticket(
    State(app): State<App>,
) -> axum::Json<rutsubo_core::api::WsTicketResponse> {
    let (ticket, expires_in_s) = app.tickets.issue();
    axum::Json(rutsubo_core::api::WsTicketResponse {
        ticket,
        expires_in_s,
    })
}

/// Decisión de autorización del handshake (pura, testeable):
/// - Remote: SOLO ticket (el token local jamás cruza el BFF) y, si hay
///   `spa_origin` configurado, el Origin del navegador debe coincidir.
/// - Local: token del daemon o ticket, sin exigencia de Origin.
fn ws_authorized(
    mode: crate::config::AuthMode,
    ticket_ok: bool,
    token_ok: bool,
    origin_ok: bool,
) -> bool {
    match mode {
        crate::config::AuthMode::Remote => ticket_ok && origin_ok,
        crate::config::AuthMode::Local => token_ok || ticket_ok,
    }
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
    let token_ok = bearer
        .or(query.token)
        .map(|t| crate::auth::token_matches(&app.token, &t))
        .unwrap_or(false);
    // Consumir el ticket solo si viene: un solo uso incluso en intentos mixtos.
    let ticket_ok = query
        .ticket
        .as_deref()
        .is_some_and(|t| app.tickets.consume(t));
    // Defensa extra en remoto: el handshake debe venir de la SPA publicada.
    let origin_ok = match app.cfg.spa_origin.as_deref() {
        Some(expected) => headers
            .get(axum::http::header::ORIGIN)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|origin| origin == expected),
        None => true,
    };
    if !ws_authorized(app.cfg.auth_mode, ticket_ok, token_ok, origin_ok) {
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

#[cfg(test)]
mod tests {
    use super::ws_authorized;
    use crate::config::AuthMode;

    #[test]
    fn remoto_exige_ticket_y_origin() {
        // El token local jamás autoriza en remoto (no cruza el BFF).
        assert!(!ws_authorized(AuthMode::Remote, false, true, true));
        assert!(ws_authorized(AuthMode::Remote, true, false, true));
        // Con spa_origin configurado, un Origin ajeno se rechaza aun con ticket.
        assert!(!ws_authorized(AuthMode::Remote, true, false, false));
        assert!(!ws_authorized(AuthMode::Remote, false, false, true));
    }

    #[test]
    fn local_acepta_token_o_ticket() {
        assert!(ws_authorized(AuthMode::Local, false, true, true));
        assert!(ws_authorized(AuthMode::Local, true, false, true));
        // En local el Origin no aplica (spa_origin es para el modo remoto).
        assert!(ws_authorized(AuthMode::Local, true, false, false));
        assert!(!ws_authorized(AuthMode::Local, false, false, true));
    }
}
