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

/// Login con Google en modo dev: el `sub` se deriva del correo, así que
/// llamadas repetidas con el mismo correo caen en la MISMA cuenta (cada una
/// crea un device nuevo, como el login de antes). El relay de test arranca en
/// modo `google_dev`.
pub async fn google_login(relay: &TestRelay, email: &str) -> (String, String) {
    let id_token = format!("dev:sub-{email}:{email}");
    let body: serde_json::Value = reqwest::Client::new()
        .post(format!("{}/v1/auth/google", relay.base))
        .json(&serde_json::json!({
            "id_token": id_token,
            "device": {"kind": "mobile", "name": "test"}
        }))
        .send()
        .await
        .expect("google")
        .json()
        .await
        .expect("json");
    (
        body["device_token"].as_str().expect("device_token").to_owned(),
        body["device_id"].as_str().expect("device_id").to_owned(),
    )
}

/// Alias histórico (antes register+login); ahora un login Google dev.
pub async fn register_and_login(relay: &TestRelay, email: &str) -> (String, String) {
    google_login(relay, email).await
}

/// Alias histórico; otro device de la misma cuenta.
pub async fn login(relay: &TestRelay, email: &str) -> (String, String) {
    google_login(relay, email).await
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
