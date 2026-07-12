//! Tests de integración del contrato C-1 (aceptación Fase B):
//! auth 401, ciclo de sesión completo, idempotencia de mensajes, carrera de
//! aprobaciones, replay con after_seq, session_busy, validaciones 422.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use rutsubo_daemon::api;
use rutsubo_daemon::config::DaemonConfig;
use rutsubo_daemon::state::{App, AppState};
use rutsubo_daemon::store;
use serde_json::{Value, json};
use tower::util::ServiceExt;

async fn test_app() -> (App, Router, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let cfg = DaemonConfig {
        data_dir: dir.path().join("data"),
        bind: "127.0.0.1:0".parse().unwrap(),
        max_iterations: 20,
        spa_origin: None,
        groq_api_key: None,
        auth_mode: rutsubo_daemon::config::AuthMode::Local,
        proxy_secret: None,
        allowed_emails: vec![],
        database_url: None,
        relay_url: None,
    };
    let app = AppState::bootstrap(cfg).await.unwrap();
    let router = api::router(app.clone());
    (app, router, dir)
}

fn request(method: &str, uri: &str, token: Option<&str>, body: Option<Value>) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(t) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    match body {
        Some(v) => builder
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(v.to_string()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    }
}

async fn send(router: &Router, req: Request<Body>) -> (StatusCode, Value) {
    let res = router.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}

fn multipart_audio(token: &str, mime: &str, bytes: &[u8]) -> Request<Body> {
    const BOUNDARY: &str = "rutsubo-test-boundary";
    let mut body = format!("--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"audio\"; filename=\"test.webm\"\r\nContent-Type: {mime}\r\n\r\n").into_bytes();
    body.extend_from_slice(bytes);
    body.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());
    Request::builder()
        .method("POST")
        .uri("/v1/asr/transcribe")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={BOUNDARY}"),
        )
        .body(Body::from(body))
        .unwrap()
}

/// Crea una sesión sobre un workspace temporal y devuelve su id.
async fn create_session(router: &Router, token: &str, ws: &std::path::Path) -> String {
    let (status, body) = send(
        router,
        request(
            "POST",
            "/v1/sessions",
            Some(token),
            Some(json!({"workspace_path": ws.to_str().unwrap(), "title": "test"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    body["id"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn health_sin_auth_y_con_headers_de_seguridad() {
    let (_app, router, _dir) = test_app().await;
    let res = router
        .clone()
        .oneshot(request("GET", "/v1/health", None, None))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers().get("x-content-type-options").unwrap(),
        "nosniff"
    );
    let body: Value =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(body["status"], "down");
    assert_eq!(body["provider"]["reason"], "missing_api_key");
    assert!(
        body["provider"]["id"]
            .as_str()
            .unwrap()
            .starts_with("groq:missing:")
    );
}

#[tokio::test]
async fn rutas_protegidas_exigen_bearer() {
    let (app, router, _dir) = test_app().await;
    // Sin token.
    let (status, body) = send(&router, request("GET", "/v1/sessions", None, None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["code"], "unauthorized");
    // Token incorrecto.
    let (status, body) = send(&router, request("GET", "/v1/sessions", Some("nope"), None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["code"], "unauthorized");
    // Token correcto.
    let (status, _) = send(
        &router,
        request("GET", "/v1/sessions", Some(&app.token), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn ciclo_de_sesion_completo() {
    let (app, router, _dir) = test_app().await;
    let ws = tempfile::tempdir().unwrap();
    let token = app.token.clone();

    // Crear: 201 + Location + estado idle + evento session_state seq 1.
    let res = router
        .clone()
        .oneshot(request(
            "POST",
            "/v1/sessions",
            Some(&token),
            Some(json!({"workspace_path": ws.path().to_str().unwrap(), "title": "Refactor"})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    let location = res
        .headers()
        .get(header::LOCATION)
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    let body: Value =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let sid = body["id"].as_str().unwrap().to_owned();
    assert_eq!(location, format!("/v1/sessions/{sid}"));
    assert_eq!(body["state"], "idle");
    assert_eq!(body["last_seq"], 1); // el session_state inicial ya cuenta

    // Detalle con contadores.
    let (status, body) = send(
        &router,
        request("GET", &format!("/v1/sessions/{sid}"), Some(&token), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["message_count"], 0);
    assert_eq!(body["pending_approvals"], 0);

    // Mensaje: 202 y transición a running.
    let (status, body) = send(
        &router,
        request(
            "POST",
            &format!("/v1/sessions/{sid}/messages"),
            Some(&token),
            Some(json!({"content": "hola", "client_msg_id": "u-1"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(body["session_state"], "running");
    assert!(body["message_id"].as_str().is_some());

    // Replay: session_state idle (creación) y running, en orden por seq.
    let (status, body) = send(
        &router,
        request(
            "GET",
            &format!("/v1/sessions/{sid}/events?after_seq=0"),
            Some(&token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let events = body["events"].as_array().unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["seq"], 1);
    assert_eq!(events[0]["type"], "session_state");
    assert_eq!(events[0]["payload"]["state"], "idle");
    assert_eq!(events[1]["seq"], 2);
    assert_eq!(events[1]["payload"]["state"], "running");
    assert_eq!(body["has_more"], false);

    // Listado con filtro por estado.
    let (status, body) = send(
        &router,
        request("GET", "/v1/sessions?state=running", Some(&token), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["sessions"].as_array().unwrap().len(), 1);

    // PATCH renombrar.
    let (status, body) = send(
        &router,
        request(
            "PATCH",
            &format!("/v1/sessions/{sid}"),
            Some(&token),
            Some(json!({"title": "Refactor validación"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["title"], "Refactor validación");
}

#[tokio::test]
async fn workspace_path_invalido_da_422() {
    let (app, router, _dir) = test_app().await;
    for ws in ["relativa/x", "/no/existe/jamas", "/tmp/../etc"] {
        let (status, body) = send(
            &router,
            request(
                "POST",
                "/v1/sessions",
                Some(&app.token),
                Some(json!({"workspace_path": ws})),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "caso: {ws}");
        assert_eq!(body["error"]["code"], "validation_failed");
        assert_eq!(body["error"]["details"]["field"], "workspace_path");
    }
}

#[tokio::test]
async fn idempotencia_de_mensajes_por_client_msg_id() {
    let (app, router, _dir) = test_app().await;
    let ws = tempfile::tempdir().unwrap();
    let sid = create_session(&router, &app.token, ws.path()).await;

    let payload = json!({"content": "repite", "client_msg_id": "misma-clave"});
    let (s1, b1) = send(
        &router,
        request(
            "POST",
            &format!("/v1/sessions/{sid}/messages"),
            Some(&app.token),
            Some(payload.clone()),
        ),
    )
    .await;
    let (s2, b2) = send(
        &router,
        request(
            "POST",
            &format!("/v1/sessions/{sid}/messages"),
            Some(&app.token),
            Some(payload),
        ),
    )
    .await;
    assert_eq!(s1, StatusCode::ACCEPTED);
    assert_eq!(s2, StatusCode::ACCEPTED);
    assert_eq!(
        b1["message_id"], b2["message_id"],
        "dos POST con el mismo client_msg_id devuelven el message_id original"
    );

    // Solo hay un mensaje persistido.
    let (_, detail) = send(
        &router,
        request(
            "GET",
            &format!("/v1/sessions/{sid}"),
            Some(&app.token),
            None,
        ),
    )
    .await;
    assert_eq!(detail["message_count"], 1);
}

#[tokio::test]
async fn validaciones_de_mensaje() {
    let (app, router, _dir) = test_app().await;
    let ws = tempfile::tempdir().unwrap();
    let sid = create_session(&router, &app.token, ws.path()).await;

    // 32 001 caracteres → 422.
    let long = "x".repeat(32_001);
    let (status, body) = send(
        &router,
        request(
            "POST",
            &format!("/v1/sessions/{sid}/messages"),
            Some(&app.token),
            Some(json!({"content": long, "client_msg_id": "c1"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["code"], "validation_failed");

    // client_msg_id vacío → 422.
    let (status, _) = send(
        &router,
        request(
            "POST",
            &format!("/v1/sessions/{sid}/messages"),
            Some(&app.token),
            Some(json!({"content": "ok", "client_msg_id": ""})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // Cuerpo malformado → 422 con el sobre estándar (no 400 de axum).
    let (status, body) = send(
        &router,
        request(
            "POST",
            &format!("/v1/sessions/{sid}/messages"),
            Some(&app.token),
            Some(json!({"content": "sin client_msg_id"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["code"], "validation_failed");
}

#[tokio::test]
async fn session_busy_en_waiting_approval() {
    let (app, router, _dir) = test_app().await;
    let ws = tempfile::tempdir().unwrap();
    let sid_str = create_session(&router, &app.token, ws.path()).await;
    let sid: rutsubo_core::ids::SessionId = sid_str.parse().unwrap();

    // Fuerza waiting_approval (como haría la compuerta en Fase C).
    store::events::append(
        &app.pool,
        sid,
        rutsubo_core::events::Event::SessionState {
            state: rutsubo_core::events::SessionState::WaitingApproval,
            title: None,
            reason: None,
        },
        Some(rutsubo_core::events::SessionState::WaitingApproval),
    )
    .await
    .unwrap();

    let (status, body) = send(
        &router,
        request(
            "POST",
            &format!("/v1/sessions/{sid_str}/messages"),
            Some(&app.token),
            Some(json!({"content": "hola", "client_msg_id": "c9"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "session_busy");
}

#[tokio::test]
async fn carrera_de_aprobaciones_exactamente_un_ganador() {
    let (app, router, _dir) = test_app().await;
    let ws = tempfile::tempdir().unwrap();
    let sid_str = create_session(&router, &app.token, ws.path()).await;
    let sid: rutsubo_core::ids::SessionId = sid_str.parse().unwrap();

    let approval_id = rutsubo_core::ids::ApprovalId::new();
    let tool_call_id = rutsubo_core::ids::ToolCallId::new();
    store::approvals::insert(
        &app.pool,
        store::approvals::NewApproval {
            id: &approval_id,
            session_id: &sid,
            tool_call_id: &tool_call_id,
            tool: "run_shell",
            summary: "cargo test",
            args: &json!({"cmd": "cargo test"}),
            created_at: chrono::Utc::now(),
        },
    )
    .await
    .unwrap();

    // GET /v1/approvals la lista como pendiente.
    let (status, body) = send(
        &router,
        request("GET", "/v1/approvals", Some(&app.token), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["approvals"].as_array().unwrap().len(), 1);

    // Dos decisiones opuestas concurrentes → exactamente un 200 y un 409.
    let uri = format!("/v1/approvals/{approval_id}/decision");
    let approve = send(
        &router,
        request(
            "POST",
            &uri,
            Some(&app.token),
            Some(json!({"decision": "approve"})),
        ),
    );
    let reject = send(
        &router,
        request(
            "POST",
            &uri,
            Some(&app.token),
            Some(json!({"decision": "reject"})),
        ),
    );
    let ((s1, b1), (s2, b2)) = tokio::join!(approve, reject);

    let oks = [s1, s2].iter().filter(|s| **s == StatusCode::OK).count();
    let conflicts = [(s1, &b1), (s2, &b2)]
        .iter()
        .filter(|(s, b)| *s == StatusCode::CONFLICT && b["error"]["code"] == "conflict")
        .count();
    assert_eq!((oks, conflicts), (1, 1), "b1={b1} b2={b2}");

    // El 409 lleva el registro original en details.
    let loser = if s1 == StatusCode::CONFLICT { &b1 } else { &b2 };
    let winner = if s1 == StatusCode::OK { &b1 } else { &b2 };
    assert_eq!(
        loser["error"]["details"]["decision"], winner["decision"],
        "details contiene la decisión original"
    );

    // Repetir la decisión ganadora → 200 con el registro original.
    let winning_decision = winner["decision"].as_str().unwrap();
    let (status, body) = send(
        &router,
        request(
            "POST",
            &uri,
            Some(&app.token),
            Some(json!({"decision": winning_decision})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["decision"], *winning_decision);

    // La resolución emitió approval_resolved (visible por replay).
    let (_, events) = send(
        &router,
        request(
            "GET",
            &format!("/v1/sessions/{sid_str}/events"),
            Some(&app.token),
            None,
        ),
    )
    .await;
    let kinds: Vec<&str> = events["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["type"].as_str().unwrap())
        .collect();
    assert!(kinds.contains(&"approval_resolved"));

    // Ya no hay pendientes.
    let (_, body) = send(
        &router,
        request("GET", "/v1/approvals", Some(&app.token), None),
    )
    .await;
    assert_eq!(body["approvals"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn replay_con_after_seq_y_limit() {
    let (app, router, _dir) = test_app().await;
    let ws = tempfile::tempdir().unwrap();
    let sid_str = create_session(&router, &app.token, ws.path()).await;
    let sid: rutsubo_core::ids::SessionId = sid_str.parse().unwrap();

    // Genera 30 eventos adicionales (seq 2..=31).
    for i in 0..30 {
        store::events::append(
            &app.pool,
            sid,
            rutsubo_core::events::Event::MessageDelta {
                message_id: rutsubo_core::ids::MessageId::new(),
                delta: format!("delta-{i}"),
            },
            None,
        )
        .await
        .unwrap();
    }

    // after_seq=11, limit=10 → seq 12..=21, has_more.
    let (status, body) = send(
        &router,
        request(
            "GET",
            &format!("/v1/sessions/{sid_str}/events?after_seq=11&limit=10"),
            Some(&app.token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let events = body["events"].as_array().unwrap();
    assert_eq!(events.len(), 10);
    let seqs: Vec<u64> = events.iter().map(|e| e["seq"].as_u64().unwrap()).collect();
    assert_eq!(seqs, (12..=21).collect::<Vec<u64>>());
    assert_eq!(body["last_seq"], 31);
    assert_eq!(body["has_more"], true);

    // Cola final: sin has_more.
    let (_, body) = send(
        &router,
        request(
            "GET",
            &format!("/v1/sessions/{sid_str}/events?after_seq=21&limit=1000"),
            Some(&app.token),
            None,
        ),
    )
    .await;
    assert_eq!(body["events"].as_array().unwrap().len(), 10);
    assert_eq!(body["has_more"], false);
}

#[tokio::test]
async fn config_model_get_y_put() {
    let (app, router, _dir) = test_app().await;
    let token = app.token.clone();

    // GET: defaults del contrato.
    let (status, body) = send(
        &router,
        request("GET", "/v1/config/model", Some(&token), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["primary"]["provider"], "groq");
    assert_eq!(body["thresholds"]["ttft_threshold_ms"], 5000);

    // PUT sin GROQ_API_KEY → 422 (el test app no tiene api key).
    let cfg = body.clone();
    let (status, body) = send(
        &router,
        request("PUT", "/v1/config/model", Some(&token), Some(cfg.clone())),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["code"], "validation_failed");
}

#[tokio::test]
async fn asr_mock_valida_mime_y_audita_sin_audio() {
    let (app, router, _dir) = test_app().await;
    let (status, body) = send(
        &router,
        multipart_audio(&app.token, "audio/webm", b"not-real-audio"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["text"], "transcripción de prueba");

    let (_, audit) = send(&router, request("GET", "/v1/audit", Some(&app.token), None)).await;
    let asr = audit["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["kind"] == "asr")
        .unwrap();
    assert!(asr["detail"].get("bytes").is_some());
    assert!(asr["detail"].get("audio").is_none());

    let (status, body) = send(&router, multipart_audio(&app.token, "text/plain", b"no")).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["code"], "validation_failed");
}

#[tokio::test]
async fn rules_put_reemplaza_y_valida() {
    let (app, router, _dir) = test_app().await;
    let token = app.token.clone();

    // Herramienta sin efectos secundarios → 422.
    let (status, _) = send(
        &router,
        request(
            "PUT",
            "/v1/rules",
            Some(&token),
            Some(json!({"rules": [{"workspace_path": "/w", "tool": "read_file", "pattern": "x"}]})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // Reemplazo completo válido.
    let (status, body) = send(
        &router,
        request(
            "PUT",
            "/v1/rules",
            Some(&token),
            Some(json!({"rules": [
                {"workspace_path": "/w", "tool": "run_shell", "pattern": "cargo test"},
                {"workspace_path": "/w", "tool": "run_shell", "pattern": "pytest"}
            ]})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["rules"].as_array().unwrap().len(), 2);

    // Segundo PUT reemplaza (no acumula).
    let (_, body) = send(
        &router,
        request(
            "PUT",
            "/v1/rules",
            Some(&token),
            Some(json!({"rules": [
                {"workspace_path": "/w", "tool": "write_file", "pattern": "src/*"}
            ]})),
        ),
    )
    .await;
    assert_eq!(body["rules"].as_array().unwrap().len(), 1);
    let (_, body) = send(&router, request("GET", "/v1/rules", Some(&token), None)).await;
    assert_eq!(body["rules"][0]["tool"], "write_file");
}

#[tokio::test]
async fn sesion_archivada_rechaza_mensajes() {
    let (app, router, _dir) = test_app().await;
    let ws = tempfile::tempdir().unwrap();
    let sid = create_session(&router, &app.token, ws.path()).await;

    // Estado inválido en PATCH → 422.
    let (status, _) = send(
        &router,
        request(
            "PATCH",
            &format!("/v1/sessions/{sid}"),
            Some(&app.token),
            Some(json!({"state": "running"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // Archivar.
    let (status, body) = send(
        &router,
        request(
            "PATCH",
            &format!("/v1/sessions/{sid}"),
            Some(&app.token),
            Some(json!({"state": "archived"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["state"], "archived");

    // Mensaje sobre archivada → 409 conflict.
    let (status, body) = send(
        &router,
        request(
            "POST",
            &format!("/v1/sessions/{sid}/messages"),
            Some(&app.token),
            Some(json!({"content": "hola", "client_msg_id": "c1"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "conflict");
}

#[tokio::test]
async fn recursos_inexistentes_dan_404() {
    let (app, router, _dir) = test_app().await;
    let missing = rutsubo_core::ids::SessionId::new();
    for uri in [
        format!("/v1/sessions/{missing}"),
        format!("/v1/sessions/{missing}/events"),
        "/v1/sessions/no-es-ulid".to_owned(),
    ] {
        let (status, body) = send(&router, request("GET", &uri, Some(&app.token), None)).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "uri: {uri}");
        assert_eq!(body["error"]["code"], "not_found");
    }
}
