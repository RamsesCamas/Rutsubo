//! Cliente del relay C-2 (ADR-006): conexión WebSocket **saliente**
//! persistente hacia `/v1/connect`, con reconexión por backoff exponencial +
//! jitter (espejo de la capa del cliente web). El estado de replay vive aquí
//! (C-3), no en el relay.
//!
//! - Salida: todo evento del bus interno se difunde a la cuenta
//!   (`FromDaemon{dst: None}`); el backlog de `subscribe_session` va unicast
//!   (`dst: Some(device)`), y el dedup por seq del cliente resuelve el solape.
//! - Entrada: los comandos llegan como `ToDaemon{src, frame}` y ejecutan el
//!   MISMO código interno que sus endpoints REST/WS (un servicio, tres
//!   transportes); el decisor queda auditado como `device:{src}` (RF-17).
//! - Pairing (C-2 §3.2.2): clave Ed25519 efímera en `<data_dir>/relay_key`
//!   (0600); el claim guarda el token en `<data_dir>/relay_token` (0600) y
//!   despierta la task.

use crate::error::ApiError;
use crate::state::App;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use chrono::Utc;
use ed25519_dalek::{Signer, SigningKey};
use futures::{SinkExt, StreamExt};
use rand::{Rng, RngCore};
use rutsubo_core::api::{DecisionRequest, SendMessageRequest, SessionsQuery};
use rutsubo_core::commands::{Command, CommandEnvelope};
use rutsubo_core::envelope::{Envelope, PROTOCOL_VERSION};
use rutsubo_core::events::Event;
use rutsubo_core::ids::SessionId;
use rutsubo_core::relay::{FromDaemon, ToDaemon};
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

pub const RELAY_KEY_FILE: &str = "relay_key";
pub const RELAY_TOKEN_FILE: &str = "relay_token";

const BACKOFF_BASE_MS: u64 = 1_000;
const BACKOFF_CAP_MS: u64 = 30_000;
/// Backlog máximo por suscripción, igual que el WS local (C-3).
const MAX_BACKLOG: i64 = 1000;
/// Close C-2: otro daemon de la cuenta tomó el canal.
const CLOSE_SUPERSEDED: u16 = 4001;

/// Estado observable de la conexión + despertador de la task (el pairing
/// exitoso la saca de la espera sin reiniciar el daemon).
#[derive(Default)]
pub struct RelayControl {
    pub connected: AtomicBool,
    pub wake: tokio::sync::Notify,
}

// ---- Claves y token ----

/// Clave Ed25519 del daemon para el pairing (seed 32 B, base64, 0600).
pub fn load_or_create_signing_key(data_dir: &Path) -> io::Result<SigningKey> {
    let path = data_dir.join(RELAY_KEY_FILE);
    if path.exists() {
        let encoded = std::fs::read_to_string(&path)?;
        let bytes = B64
            .decode(encoded.trim())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let seed: [u8; 32] = bytes
            .try_into()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "seed inválido"))?;
        return Ok(SigningKey::from_bytes(&seed));
    }
    std::fs::create_dir_all(data_dir)?;
    let mut seed = [0u8; 32];
    rand::rng().fill_bytes(&mut seed);
    write_0600(&path, &B64.encode(seed))?;
    tracing::info!(path = %path.display(), "clave de pairing generada");
    Ok(SigningKey::from_bytes(&seed))
}

pub fn pubkey_b64(data_dir: &Path) -> io::Result<String> {
    let key = load_or_create_signing_key(data_dir)?;
    Ok(B64.encode(key.verifying_key().to_bytes()))
}

fn read_token(data_dir: &Path) -> Option<String> {
    let token = std::fs::read_to_string(data_dir.join(RELAY_TOKEN_FILE)).ok()?;
    let token = token.trim().to_owned();
    (!token.is_empty()).then_some(token)
}

/// Misma disciplina de permisos que el token local (`auth::write_0600`).
fn write_0600(path: &Path, contents: &str) -> io::Result<()> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    // 0600 solo en Unix; en Windows, permisos del usuario (daemon local).
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    f.write_all(contents.as_bytes())
}

// ---- Pairing (lado daemon) ----

/// Reclama un código de pairing contra el relay configurado y persiste el
/// `daemon_token`. Lo invoca `POST /v1/relay/pair`.
pub async fn pair(app: &App, code: &str) -> Result<serde_json::Value, ApiError> {
    let Some(base) = app.cfg.relay_url.as_deref() else {
        return Err(ApiError::validation(
            "RUTSUBO_RELAY_URL no está configurada",
            None,
        ));
    };
    let key = load_or_create_signing_key(&app.cfg.data_dir).map_err(ApiError::internal)?;
    let signature = B64.encode(key.sign(code.as_bytes()).to_bytes());

    let response = reqwest::Client::new()
        .post(format!("{}/v1/pairing/claim", base.trim_end_matches('/')))
        .json(&serde_json::json!({"code": code, "signature": signature}))
        .send()
        .await
        .map_err(|e| ApiError::validation(format!("no se pudo alcanzar el relay: {e}"), None))?;
    let status = response.status();
    let body: serde_json::Value = response.json().await.map_err(ApiError::internal)?;
    if !status.is_success() {
        let message = body["error"]["message"]
            .as_str()
            .unwrap_or("claim rechazado");
        return Err(ApiError::validation(
            format!("pairing rechazado ({status}): {message}"),
            None,
        ));
    }
    let token = body["daemon_token"]
        .as_str()
        .ok_or_else(|| ApiError::internal("respuesta de claim sin daemon_token"))?;
    write_0600(&app.cfg.data_dir.join(RELAY_TOKEN_FILE), token).map_err(ApiError::internal)?;
    tracing::info!("pairing completado; conectando al relay");
    app.relay.wake.notify_waiters();
    Ok(serde_json::json!({
        "account_id": body["account_id"],
        "device_id": body["device_id"],
    }))
}

// ---- Task de conexión saliente ----

pub fn spawn(app: App) {
    if app.cfg.relay_url.is_none() {
        return;
    }
    tokio::spawn(run(app));
}

enum SessionEnd {
    /// Reconectar con backoff (caída de red, cierre del relay).
    Retry,
    /// Otro daemon tomó el canal (4001): esperar re-pairing explícito.
    Superseded,
}

async fn run(app: App) {
    let base = app.cfg.relay_url.clone().expect("spawn valida relay_url");
    let ws_url = format!(
        "{}/v1/connect",
        base.trim_end_matches('/')
            .replacen("http://", "ws://", 1)
            .replacen("https://", "wss://", 1)
    );
    let mut attempts: u32 = 0;

    loop {
        let Some(token) = read_token(&app.cfg.data_dir) else {
            tracing::info!("relay configurado sin pairing; esperando POST /v1/relay/pair");
            app.relay.wake.notified().await;
            continue;
        };

        match connect(&ws_url, &token).await {
            Ok(socket) => {
                attempts = 0;
                app.relay.connected.store(true, Ordering::Relaxed);
                tracing::info!(url = %ws_url, "conectado al relay");
                let end = session(&app, socket).await;
                app.relay.connected.store(false, Ordering::Relaxed);
                if let SessionEnd::Superseded = end {
                    tracing::warn!(
                        "desplazado por otro daemon (4001 superseded); \
                         no se reintenta hasta un nuevo pairing"
                    );
                    app.relay.wake.notified().await;
                    continue;
                }
            }
            Err(err) => {
                tracing::warn!(%err, "conexión al relay fallida");
            }
        }

        // Backoff exponencial + jitter (mismas constantes que la SPA).
        attempts = attempts.saturating_add(1);
        let exp = BACKOFF_BASE_MS
            .saturating_mul(1u64 << attempts.min(5))
            .min(BACKOFF_CAP_MS);
        let jitter = rand::rng().random_range(0..=exp * 3 / 10);
        tokio::time::sleep(Duration::from_millis(exp + jitter)).await;
    }
}

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

async fn connect(
    ws_url: &str,
    token: &str,
) -> Result<Socket, Box<dyn std::error::Error + Send + Sync>> {
    let mut request = ws_url.into_client_request()?;
    request.headers_mut().insert(
        reqwest::header::AUTHORIZATION,
        format!("Bearer {token}").parse()?,
    );
    let (socket, _) = tokio_tungstenite::connect_async(request).await?;
    Ok(socket)
}

async fn session(app: &App, socket: Socket) -> SessionEnd {
    let (mut sink, mut incoming) = socket.split();
    let mut bus = app.bus.subscribe();

    // Anunciar las sesiones existentes a los suscriptores ya conectados: sin
    // esto, un móvil/web que entró antes que el daemon no vería las sesiones
    // creadas en el escritorio hasta que emitieran un evento vivo.
    if announce_sessions(app, &mut sink, None).await.is_err() {
        return SessionEnd::Retry;
    }

    loop {
        tokio::select! {
            message = incoming.next() => {
                match message {
                    None | Some(Err(_)) => return SessionEnd::Retry,
                    Some(Ok(Message::Close(frame))) => {
                        let superseded = frame
                            .as_ref()
                            .is_some_and(|f| u16::from(f.code) == CLOSE_SUPERSEDED);
                        return if superseded {
                            SessionEnd::Superseded
                        } else {
                            SessionEnd::Retry
                        };
                    }
                    // El relay hace ping cada 30 s; sin pong nos cierra (4002).
                    Some(Ok(Message::Ping(payload))) => {
                        if sink.send(Message::Pong(payload)).await.is_err() {
                            return SessionEnd::Retry;
                        }
                    }
                    Some(Ok(Message::Text(text))) => {
                        if handle_to_daemon(app, &mut sink, text.as_str()).await.is_err() {
                            return SessionEnd::Retry;
                        }
                    }
                    Some(Ok(_)) => {}
                }
            }
            event = bus.recv() => {
                match event {
                    Ok(envelope) => {
                        let frame = serde_json::to_string(&envelope)
                            .expect("los eventos siempre serializan");
                        if send_from_daemon(&mut sink, None, frame).await.is_err() {
                            return SessionEnd::Retry;
                        }
                    }
                    // Rezago del bus: los clientes reponen por seq (C-3);
                    // se sigue con el flujo vivo.
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        return SessionEnd::Retry;
                    }
                }
            }
            // Un nuevo pairing escribió otro token (posiblemente OTRA cuenta):
            // cortar esta sesión y reconectar con el token fresco. Sin esto,
            // re-vincular con la app abierta dejaba al daemon en la cuenta
            // vieja mientras el token nuevo dormía en disco.
            _ = app.relay.wake.notified() => {
                tracing::info!("nuevo pairing: reconectando al relay con el token nuevo");
                return SessionEnd::Retry;
            }
        }
    }
}

type Sink = futures::stream::SplitSink<Socket, Message>;

/// Snapshot de sesiones para poblar la lista de un cliente remoto:
/// `session_state` sintéticos SIN `seq` — no entran al secuenciador C-3 (que
/// es por-sesión y con seq contiguos); los clientes hacen upsert de su lista.
/// `dst = None` difunde (al conectar el daemon); `dst = Some(device)` unicasta
/// (empujón del relay cuando un suscriptor entra tarde).
async fn announce_sessions(
    app: &App,
    sink: &mut Sink,
    dst: Option<String>,
) -> Result<(), tokio_tungstenite::tungstenite::Error> {
    let query = SessionsQuery {
        cursor: None,
        limit: None,
        state: None,
    };
    let rows = match crate::store::sessions::list(&app.pool, &query, 200).await {
        Ok(rows) => rows,
        Err(err) => {
            tracing::warn!(%err, "no se pudo listar sesiones para el anuncio");
            return Ok(());
        }
    };
    for row in rows {
        let Some(state) = row.session_state() else {
            continue;
        };
        let Ok(session_id) = row.id.parse::<SessionId>() else {
            continue;
        };
        let envelope: Envelope<Event> = Envelope {
            v: PROTOCOL_VERSION,
            body: Event::SessionState {
                state,
                title: Some(row.title.clone()),
                reason: None,
            },
            session_id: Some(session_id),
            seq: None,
            ts: Utc::now(),
        };
        let frame = serde_json::to_string(&envelope).expect("los eventos siempre serializan");
        send_from_daemon(sink, dst.clone(), frame).await?;
    }
    Ok(())
}

async fn send_from_daemon(
    sink: &mut Sink,
    dst: Option<String>,
    frame: String,
) -> Result<(), tokio_tungstenite::tungstenite::Error> {
    let envelope = FromDaemon {
        dst,
        frame,
        ack_outbox_id: None,
    };
    let text = serde_json::to_string(&envelope).expect("FromDaemon siempre serializa");
    sink.send(Message::text(text)).await
}

/// Acuse de una tarea del buzón: el relay borra la fila al recibirlo.
async fn send_task_ack(
    sink: &mut Sink,
    outbox_id: String,
) -> Result<(), tokio_tungstenite::tungstenite::Error> {
    let envelope = FromDaemon {
        dst: None,
        frame: String::new(),
        ack_outbox_id: Some(outbox_id),
    };
    let text = serde_json::to_string(&envelope).expect("FromDaemon siempre serializa");
    sink.send(Message::text(text)).await
}

/// Error de comando remoto: mismo sobre no persistido (`seq = null`) que el
/// WS local, entregado unicast al dispositivo de origen.
async fn send_command_error(
    sink: &mut Sink,
    src: &str,
    session_id: Option<SessionId>,
    err: &ApiError,
) -> Result<(), tokio_tungstenite::tungstenite::Error> {
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
    let frame = serde_json::to_string(&envelope).expect("los eventos siempre serializan");
    send_from_daemon(sink, Some(src.to_owned()), frame).await
}

/// Ejecuta un comando llegado por el relay reutilizando el mismo código
/// interno que REST y el WS local (espejo de `ws::handle_command`).
async fn handle_to_daemon(
    app: &App,
    sink: &mut Sink,
    text: &str,
) -> Result<(), tokio_tungstenite::tungstenite::Error> {
    let Ok(ToDaemon {
        src,
        frame,
        outbox_id,
        new_session_title,
        announce_sessions: announce,
    }) = serde_json::from_str::<ToDaemon>(text)
    else {
        tracing::warn!("sobre de enrutamiento inválido del relay; descartado");
        return Ok(());
    };
    // Empujón del relay: `src` acaba de suscribirse → unicasta el snapshot de
    // sesiones (el `frame` va vacío; no hay comando que parsear).
    if announce == Some(true) {
        return announce_sessions(app, sink, Some(src)).await;
    }
    let envelope: CommandEnvelope = match serde_json::from_str(&frame) {
        Ok(env) => env,
        Err(err) => {
            let api_err = ApiError::validation(format!("comando inválido: {err}"), None);
            return send_command_error(sink, &src, None, &api_err).await;
        }
    };

    // Tarea drenada del buzón (ADR-009): dedup + crear/inyectar + acuse.
    if let Some(outbox_id) = outbox_id {
        return handle_queued_task(app, sink, &src, envelope, outbox_id, new_session_title).await;
    }

    match envelope.body {
        Command::SubscribeSession {
            session_id,
            after_seq,
        } => {
            // Backlog unicast; el flujo vivo llega por broadcast y el cliente
            // deduplica por seq (C-3).
            match crate::store::events::replay(&app.pool, &session_id, after_seq, MAX_BACKLOG).await
            {
                Ok(backlog) => {
                    for event in &backlog {
                        let frame =
                            serde_json::to_string(event).expect("los eventos siempre serializan");
                        send_from_daemon(sink, Some(src.clone()), frame).await?;
                    }
                }
                Err(err) => {
                    send_command_error(sink, &src, Some(session_id), &ApiError::internal(err))
                        .await?;
                }
            }
        }
        // Modelo broadcast: no hay suscripción por dispositivo que retirar.
        Command::UnsubscribeSession { .. } => {}
        Command::SendMessage {
            content,
            client_msg_id,
        } => {
            let Some(session_id) = envelope.session_id else {
                let err =
                    ApiError::validation("send_message requiere session_id en el sobre", None);
                return send_command_error(sink, &src, None, &err).await;
            };
            let req = SendMessageRequest {
                content,
                client_msg_id,
            };
            if let Err(err) =
                crate::api::sessions::send_message_inner(app, &session_id.to_string(), req).await
            {
                send_command_error(sink, &src, Some(session_id), &err).await?;
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
            // Identidad del decisor: el device_id que asignó el relay (RF-17).
            let resolver = format!("device:{src}");
            if let Err(err) =
                crate::api::approvals::decide_inner(app, &approval_id.to_string(), req, &resolver)
                    .await
            {
                send_command_error(sink, &src, envelope.session_id, &err).await?;
            }
        }
    }
    Ok(())
}

/// Procesa una tarea drenada del buzón (ADR-009): dedup por `outbox_id` (el
/// relay entrega at-least-once), resuelve/crea la sesión, la inyecta por el
/// MISMO pipeline `send_message_inner`, emite `task_dequeued` y SIEMPRE acusa
/// (para que el relay borre la fila, incluso si la tarea no aplica).
async fn handle_queued_task(
    app: &App,
    sink: &mut Sink,
    src: &str,
    envelope: CommandEnvelope,
    outbox_id: String,
    new_session_title: Option<String>,
) -> Result<(), tokio_tungstenite::tungstenite::Error> {
    // Dedup: si ya se procesó, solo re-acusar (no reejecutar).
    match crate::store::acks::mark_new(&app.pool, &outbox_id).await {
        Ok(true) => {}
        Ok(false) => return send_task_ack(sink, outbox_id).await,
        Err(err) => {
            tracing::error!(%err, "no se pudo deduplicar la tarea del buzón");
            return send_task_ack(sink, outbox_id).await;
        }
    }

    // El buzón solo transporta send_message (ADR-007: nada de aprobaciones).
    let Command::SendMessage {
        content,
        client_msg_id,
    } = envelope.body
    else {
        tracing::warn!("tarea del buzón con comando no permitido; descartada");
        return send_task_ack(sink, outbox_id).await;
    };

    // Sesión objetivo: la del sobre, o una nueva.
    let session_id = match envelope.session_id {
        Some(sid) => sid,
        None => match crate::api::sessions::create_session_inner(app, new_session_title).await {
            Ok(sid) => sid,
            Err(err) => {
                send_command_error(sink, src, None, &err).await?;
                return send_task_ack(sink, outbox_id).await;
            }
        },
    };

    let req = SendMessageRequest {
        content,
        client_msg_id,
    };
    match crate::api::sessions::send_message_inner(app, &session_id.to_string(), req).await {
        Ok(resp) => {
            // "tu tarea encolada ya corre" (RF-17, visible en todos los clientes).
            let _ = app
                .emit(
                    session_id,
                    Event::TaskDequeued {
                        outbox_id: outbox_id.clone(),
                        message_id: resp.message_id,
                    },
                    None,
                )
                .await;
            let _ = crate::store::audit::insert(
                &app.pool,
                Some(&session_id),
                "queued_task",
                &serde_json::json!({"outbox_id": outbox_id, "enqueued_by": src}),
                Utc::now(),
            )
            .await;
        }
        Err(err) => {
            send_command_error(sink, src, Some(session_id), &err).await?;
        }
    }
    send_task_ack(sink, outbox_id).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn la_clave_de_pairing_persiste_con_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let first = load_or_create_signing_key(dir.path()).unwrap();
        let second = load_or_create_signing_key(dir.path()).unwrap();
        assert_eq!(
            first.verifying_key(),
            second.verifying_key(),
            "la clave debe ser estable entre arranques"
        );
        let mode = std::fs::metadata(dir.path().join(RELAY_KEY_FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
