//! Endpoints de sesiones, mensajes y replay de eventos (C-1).

use crate::error::{ApiError, ApiJson, ApiQuery};
use crate::state::App;
use crate::store;
use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use chrono::Utc;
use rutsubo_core::api::{
    CreateSessionRequest, EventsPage, EventsQuery, PatchSessionRequest, SendMessageRequest,
    SendMessageResponse, SessionDetail, SessionDto, SessionsPage, SessionsQuery,
};
use rutsubo_core::events::{Event, SessionState};
use rutsubo_core::ids::{MessageId, SessionId};
use serde_json::json;
use std::path::{Component, Path as FsPath};
use std::str::FromStr;

pub const MAX_TITLE_CHARS: usize = 120;
pub const MAX_CONTENT_CHARS: usize = 32_000;

fn parse_session_id(raw: &str) -> Result<SessionId, ApiError> {
    SessionId::from_str(raw).map_err(|_| ApiError::not_found("sesión"))
}

async fn load_session(app: &App, id: &SessionId) -> Result<store::sessions::SessionRow, ApiError> {
    store::sessions::get(&app.pool, id)
        .await?
        .ok_or_else(|| ApiError::not_found("sesión"))
}

fn dto_or_500(row: &store::sessions::SessionRow) -> Result<SessionDto, ApiError> {
    row.to_dto()
        .ok_or_else(|| ApiError::internal("fila de sesión corrupta"))
}

/// Crea una sesión desde una tarea del buzón (ADR-009), sin handler HTTP.
/// Sin workspace explícito: usa el directorio de trabajo del daemon (donde el
/// usuario lo lanzó, típicamente su proyecto). En modo remote: `remote://chat`.
pub async fn create_session_inner(
    app: &App,
    title: Option<String>,
) -> Result<SessionId, ApiError> {
    let title = title.unwrap_or_default();
    if title.chars().count() > MAX_TITLE_CHARS {
        return Err(ApiError::validation(
            format!("title supera {MAX_TITLE_CHARS} caracteres"),
            None,
        ));
    }
    let workspace_path = if app.cfg.auth_mode == crate::config::AuthMode::Remote {
        "remote://chat".to_string()
    } else {
        std::env::current_dir()
            .map(|p| p.to_string_lossy().into_owned())
            .map_err(ApiError::internal)?
    };
    let id = SessionId::new();
    store::sessions::create(&app.pool, &id, &workspace_path, &title, Utc::now()).await?;
    app.emit(
        id,
        Event::SessionState {
            state: SessionState::Idle,
            title: (!title.is_empty()).then(|| title.clone()),
            reason: None,
        },
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    Ok(id)
}

/// POST /v1/sessions — 201 + Location. No es idempotente: dos POST idénticos
/// crean dos sesiones (C-1).
pub async fn create(
    State(app): State<App>,
    ApiJson(req): ApiJson<CreateSessionRequest>,
) -> Result<(StatusCode, HeaderMap, Json<SessionDto>), ApiError> {
    let ws_error = |msg: &str| {
        ApiError::validation(
            format!("workspace_path inválido: {msg}"),
            Some(json!({"field": "workspace_path"})),
        )
    };

    let remote = app.cfg.auth_mode == crate::config::AuthMode::Remote;
    let ws = FsPath::new(&req.workspace_path);
    if !remote && !ws.is_absolute() {
        return Err(ws_error("debe ser una ruta absoluta"));
    }
    if !remote && ws.components().any(|c| c == Component::ParentDir) {
        return Err(ws_error(
            "no debe contener secuencias de traversal (RNF-05)",
        ));
    }
    if !remote && !ws.is_dir() {
        return Err(ws_error("debe existir y ser un directorio"));
    }
    let title = req.title.unwrap_or_default();
    if title.chars().count() > MAX_TITLE_CHARS {
        return Err(ApiError::validation(
            format!("title supera {MAX_TITLE_CHARS} caracteres"),
            Some(json!({"field": "title"})),
        ));
    }

    let id = SessionId::new();
    let workspace_path = if remote {
        "remote://chat"
    } else {
        &req.workspace_path
    };
    store::sessions::create(&app.pool, &id, workspace_path, &title, Utc::now()).await?;
    app.emit(
        id,
        Event::SessionState {
            state: SessionState::Idle,
            title: (!title.is_empty()).then(|| title.clone()),
            reason: None,
        },
        None,
    )
    .await
    .map_err(ApiError::internal)?;

    let row = load_session(&app, &id).await?;
    let mut headers = HeaderMap::new();
    headers.insert(
        header::LOCATION,
        HeaderValue::from_str(&format!("/v1/sessions/{id}")).map_err(ApiError::internal)?,
    );
    Ok((StatusCode::CREATED, headers, Json(dto_or_500(&row)?)))
}

/// GET /v1/sessions — paginado por cursor, filtro por estado (C-1).
pub async fn list(
    State(app): State<App>,
    ApiQuery(query): ApiQuery<SessionsQuery>,
) -> Result<Json<SessionsPage>, ApiError> {
    let limit = i64::from(query.limit.unwrap_or(50).clamp(1, 200));
    let rows = store::sessions::list(&app.pool, &query, limit).await?;
    let sessions: Vec<SessionDto> = rows.iter().filter_map(|r| r.to_dto()).collect();
    let next_cursor = (rows.len() as i64 == limit)
        .then(|| sessions.last().map(|s| s.id.to_string()))
        .flatten();
    Ok(Json(SessionsPage {
        sessions,
        next_cursor,
    }))
}

/// GET /v1/sessions/{id} — detalle con contadores (C-1).
pub async fn detail(
    State(app): State<App>,
    Path(id): Path<String>,
) -> Result<Json<SessionDetail>, ApiError> {
    let id = parse_session_id(&id)?;
    let row = load_session(&app, &id).await?;
    let message_count = store::sessions::message_count(&app.pool, &id).await?;
    let pending_approvals = store::sessions::pending_approvals_count(&app.pool, &id).await?;
    Ok(Json(SessionDetail {
        session: dto_or_500(&row)?,
        message_count,
        pending_approvals,
    }))
}

/// PATCH /v1/sessions/{id} — archivar / renombrar (C-1).
pub async fn patch(
    State(app): State<App>,
    Path(id): Path<String>,
    ApiJson(req): ApiJson<PatchSessionRequest>,
) -> Result<Json<SessionDto>, ApiError> {
    let id = parse_session_id(&id)?;
    let row = load_session(&app, &id).await?;
    let current = row
        .session_state()
        .ok_or_else(|| ApiError::internal("estado de sesión corrupto"))?;

    if let Some(state) = req.state
        && state != SessionState::Archived
    {
        return Err(ApiError::validation(
            "el único cambio de estado permitido es 'archived'",
            Some(json!({"field": "state"})),
        ));
    }
    if let Some(title) = &req.title {
        if title.chars().count() > MAX_TITLE_CHARS {
            return Err(ApiError::validation(
                format!("title supera {MAX_TITLE_CHARS} caracteres"),
                Some(json!({"field": "title"})),
            ));
        }
        store::sessions::set_title(&app.pool, &id, title).await?;
    }

    if req.state == Some(SessionState::Archived) && current != SessionState::Archived {
        app.emit(
            id,
            Event::SessionState {
                state: SessionState::Archived,
                title: req.title.clone(),
                reason: None,
            },
            Some(SessionState::Archived),
        )
        .await
        .map_err(ApiError::internal)?;
    } else if req.title.is_some() {
        // Renombrado sin cambio de estado: los clientes se enteran igual.
        app.emit(
            id,
            Event::SessionState {
                state: current,
                title: req.title.clone(),
                reason: None,
            },
            None,
        )
        .await
        .map_err(ApiError::internal)?;
    }

    let row = load_session(&app, &id).await?;
    Ok(Json(dto_or_500(&row)?))
}

/// POST /v1/sessions/{id}/messages — 202 siempre: el procesamiento es
/// asíncrono y el progreso fluye como eventos C-3; este endpoint jamás
/// bloquea esperando al modelo (C-1).
pub async fn post_message(
    State(app): State<App>,
    Path(id): Path<String>,
    ApiJson(req): ApiJson<SendMessageRequest>,
) -> Result<(StatusCode, Json<SendMessageResponse>), ApiError> {
    let response = send_message_inner(&app, &id, req).await?;
    Ok((StatusCode::ACCEPTED, Json(response)))
}

/// Núcleo compartido REST/WS del envío de mensajes (C-3: el comando
/// `send_message` es equivalente exacto del endpoint — misma validación,
/// misma idempotencia, mismo código).
pub async fn send_message_inner(
    app: &App,
    raw_id: &str,
    req: SendMessageRequest,
) -> Result<SendMessageResponse, ApiError> {
    let id = parse_session_id(raw_id)?;
    let row = load_session(app, &id).await?;
    let state = row
        .session_state()
        .ok_or_else(|| ApiError::internal("estado de sesión corrupto"))?;

    match state {
        SessionState::WaitingApproval => {
            return Err(ApiError::session_busy(
                "la decisión pendiente debe resolverse primero (RF-16)",
            ));
        }
        SessionState::Archived => {
            return Err(ApiError::conflict("la sesión está archivada", None));
        }
        SessionState::Idle | SessionState::Running => {}
    }
    if req.content.chars().count() > MAX_CONTENT_CHARS {
        return Err(ApiError::validation(
            format!("content supera {MAX_CONTENT_CHARS} caracteres"),
            Some(json!({"field": "content"})),
        ));
    }
    if req.client_msg_id.is_empty() || req.client_msg_id.len() > 128 {
        return Err(ApiError::validation(
            "client_msg_id es obligatorio (≤128 chars)",
            Some(json!({"field": "client_msg_id"})),
        ));
    }

    let message_id = MessageId::new();
    let now = Utc::now();
    let outcome = store::messages::insert_user(
        &app.pool,
        &id,
        &message_id,
        &req.content,
        &req.client_msg_id,
        now,
    )
    .await?;

    let (message_id, session_state) = match outcome {
        store::messages::InsertOutcome::Duplicate(original) => {
            // Idempotencia (C-1): mismo message_id, sin reprocesar.
            (original, state)
        }
        store::messages::InsertOutcome::Inserted => {
            let new_state = if state == SessionState::Idle {
                app.emit(
                    id,
                    Event::SessionState {
                        state: SessionState::Running,
                        title: None,
                        reason: None,
                    },
                    Some(SessionState::Running),
                )
                .await
                .map_err(ApiError::internal)?;
                SessionState::Running
            } else {
                state
            };
            crate::agent::ensure_running(app.clone(), id);
            (message_id, new_state)
        }
    };

    Ok(SendMessageResponse {
        message_id,
        session_state,
        accepted_at: now,
    })
}

/// GET /v1/sessions/{id}/events — replay (C-1): orden ascendente, `limit`
/// máx. 1000, lectura pura sin efectos secundarios.
pub async fn events(
    State(app): State<App>,
    Path(id): Path<String>,
    ApiQuery(query): ApiQuery<EventsQuery>,
) -> Result<Json<EventsPage>, ApiError> {
    let id = parse_session_id(&id)?;
    let row = load_session(&app, &id).await?;
    let after_seq = query.after_seq.unwrap_or(0);
    let limit = i64::from(query.limit.unwrap_or(500).clamp(1, 1000));
    let events = store::events::replay(&app.pool, &id, after_seq, limit).await?;
    let last_seq = row.last_seq as u64;
    let boundary = events.last().and_then(|e| e.seq).unwrap_or(after_seq);
    Ok(Json(EventsPage {
        events,
        last_seq,
        has_more: boundary < last_seq,
    }))
}
