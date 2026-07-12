//! Router de la API REST (contrato C-1): doce endpoints bajo `/v1/`,
//! auth Bearer en todo salvo `/v1/health`, CORS mínimo y headers de
//! seguridad (listo para el escaneo ZAP del primer corte).

pub mod approvals;
pub mod asr;
pub mod audit;
pub mod config;
pub mod fs;
pub mod health;
pub mod provider;
pub mod relay;
pub mod rules;
pub mod sessions;

use crate::state::App;
use axum::Router;
use axum::http::{HeaderValue, Method, header};
use axum::middleware;
use axum::routing::{get, post};
use tower_http::cors::CorsLayer;
use tower_http::set_header::SetResponseHeaderLayer;

pub fn router(app: App) -> Router {
    let protected = Router::new()
        .route("/v1/sessions", get(sessions::list).post(sessions::create))
        .route(
            "/v1/sessions/{id}",
            get(sessions::detail).patch(sessions::patch),
        )
        .route("/v1/sessions/{id}/messages", post(sessions::post_message))
        .route("/v1/sessions/{id}/events", get(sessions::events))
        .route("/v1/approvals", get(approvals::list_pending))
        .route("/v1/approvals/{id}/decision", post(approvals::decide))
        .route("/v1/rules", get(rules::get_rules).put(rules::put_rules))
        .route(
            "/v1/config/model",
            get(config::get_model).put(config::put_model),
        )
        .route(
            "/v1/config/provider",
            get(provider::get_provider).put(provider::put_provider),
        )
        .route("/v1/fs/list", get(fs::list))
        .route("/v1/audit", get(audit::query))
        .route("/v1/ws/ticket", post(crate::ws::issue_ticket))
        .route("/v1/asr/transcribe", post(asr::transcribe))
        .route("/v1/relay/status", get(relay::status))
        .route("/v1/relay/pair", post(relay::pair))
        .route_layer(middleware::from_fn_with_state(
            app.clone(),
            crate::auth::require_bearer,
        ));

    let mut allowed_origins: Vec<HeaderValue> = vec![
        HeaderValue::from_static("http://localhost:5173"),
        HeaderValue::from_static("http://127.0.0.1:5173"),
        // Shell Tauri (ADR-002: misma SPA, origin propio por plataforma).
        HeaderValue::from_static("tauri://localhost"),
        HeaderValue::from_static("http://tauri.localhost"),
        // Flutter web en el puerto fijo de desarrollo (plan C del móvil).
        HeaderValue::from_static("http://localhost:5180"),
        HeaderValue::from_static("http://127.0.0.1:5180"),
    ];
    if let Some(origin) = app.cfg.spa_origin.as_deref()
        && let Ok(v) = HeaderValue::from_str(origin)
    {
        allowed_origins.push(v);
    }
    let cors = CorsLayer::new()
        .allow_origin(allowed_origins)
        .allow_methods([Method::GET, Method::POST, Method::PUT, Method::PATCH])
        .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE]);

    Router::new()
        .route("/v1/health", get(health::health))
        // /v1/ws hace su propia auth: Bearer o ?token= (excepción local
        // documentada para el handshake del navegador).
        .route("/v1/ws", get(crate::ws::ws_handler))
        .merge(protected)
        .layer(cors)
        .layer(SetResponseHeaderLayer::overriding(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::X_FRAME_OPTIONS,
            HeaderValue::from_static("DENY"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::REFERRER_POLICY,
            HeaderValue::from_static("no-referrer"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-store"),
        ))
        .with_state(app)
}
