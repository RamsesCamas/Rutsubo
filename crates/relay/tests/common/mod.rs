// Compartido por varios binarios de test: no todos usan todos los helpers.
#![allow(dead_code)]

//! Arranque de un relay real en puerto efímero para los tests de integración.

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use ed25519_dalek::{Signer, SigningKey};
use rand::RngCore;
use rutsubo_relay::RelayState;

pub struct TestRelay {
    pub state: RelayState,
    pub base: String,
    pub ws_base: String,
    _dir: tempfile::TempDir,
}

pub async fn spawn() -> TestRelay {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_url = format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("relay.db").display()
    );
    let state = rutsubo_relay::bootstrap(&db_url).await.expect("bootstrap");
    let router = rutsubo_relay::router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        axum::serve(listener, router).await.expect("serve");
    });
    TestRelay {
        state,
        base: format!("http://{addr}"),
        ws_base: format!("ws://{addr}"),
        _dir: dir,
    }
}

pub async fn register_and_login(relay: &TestRelay, email: &str) -> (String, String) {
    let http = reqwest::Client::new();
    let status = http
        .post(format!("{}/v1/auth/register", relay.base))
        .json(&serde_json::json!({"email": email, "password": "secreta-123"}))
        .send()
        .await
        .expect("register")
        .status();
    assert_eq!(status, 201);
    login(relay, email).await
}

/// Devuelve `(token, device_id)` de un dispositivo cliente nuevo.
pub async fn login(relay: &TestRelay, email: &str) -> (String, String) {
    let http = reqwest::Client::new();
    let body: serde_json::Value = http
        .post(format!("{}/v1/auth/token", relay.base))
        .json(&serde_json::json!({"email": email, "password": "secreta-123"}))
        .send()
        .await
        .expect("token")
        .json()
        .await
        .expect("json");
    (
        body["token"].as_str().expect("token").to_owned(),
        body["device_id"].as_str().expect("device_id").to_owned(),
    )
}

/// Flujo de pairing completo (C-2 §3.2.2): devuelve `(daemon_token, device_id)`.
pub async fn pair_daemon(relay: &TestRelay, client_token: &str) -> (String, String) {
    let http = reqwest::Client::new();
    let mut seed = [0u8; 32];
    rand::rng().fill_bytes(&mut seed);
    let key = SigningKey::from_bytes(&seed);
    let pubkey = B64.encode(key.verifying_key().to_bytes());

    let created: serde_json::Value = http
        .post(format!("{}/v1/pairing/codes", relay.base))
        .bearer_auth(client_token)
        .json(&serde_json::json!({"daemon_pubkey": pubkey}))
        .send()
        .await
        .expect("codes")
        .json()
        .await
        .expect("json");
    let code = created["code"].as_str().expect("code");

    let signature = B64.encode(key.sign(code.as_bytes()).to_bytes());
    let claimed: serde_json::Value = http
        .post(format!("{}/v1/pairing/claim", relay.base))
        .json(&serde_json::json!({"code": code, "signature": signature}))
        .send()
        .await
        .expect("claim")
        .json()
        .await
        .expect("json");
    (
        claimed["daemon_token"]
            .as_str()
            .expect("daemon_token")
            .to_owned(),
        claimed["device_id"].as_str().expect("device_id").to_owned(),
    )
}
