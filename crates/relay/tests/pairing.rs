//! Pairing C-2 §3.2.2: prueba de posesión con Ed25519, TTL 5 min, un solo
//! uso, y límite de 5 intentos fallidos por código.

mod common;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use common::{pair_daemon, register_and_login, spawn};
use ed25519_dalek::{Signer, SigningKey};
use rand::RngCore;

fn new_key() -> (SigningKey, String) {
    let mut seed = [0u8; 32];
    rand::rng().fill_bytes(&mut seed);
    let key = SigningKey::from_bytes(&seed);
    let pubkey = B64.encode(key.verifying_key().to_bytes());
    (key, pubkey)
}

async fn create_code(relay: &common::TestRelay, token: &str, pubkey: &str) -> serde_json::Value {
    let res = reqwest::Client::new()
        .post(format!("{}/v1/pairing/codes", relay.base))
        .bearer_auth(token)
        .json(&serde_json::json!({"daemon_pubkey": pubkey}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 201);
    res.json().await.unwrap()
}

#[tokio::test]
async fn pairing_feliz_y_un_solo_uso() {
    let relay = spawn().await;
    let http = reqwest::Client::new();
    let (token, _) = register_and_login(&relay, "ana@example.com").await;
    let (key, pubkey) = new_key();

    let created = create_code(&relay, &token, &pubkey).await;
    let code = created["code"].as_str().unwrap();
    // Formato XXX-XXX-XXX, alfabeto sin ambigüedades.
    assert_eq!(code.len(), 11);
    assert!(created["single_use"].as_bool().unwrap());
    for (i, ch) in code.chars().enumerate() {
        if i == 3 || i == 7 {
            assert_eq!(ch, '-');
        } else {
            assert!(
                "ABCDEFGHJKMNPQRSTVWXYZ23456789".contains(ch),
                "símbolo ambiguo: {ch}"
            );
        }
    }

    let signature = B64.encode(key.sign(code.as_bytes()).to_bytes());
    let claim = http
        .post(format!("{}/v1/pairing/claim", relay.base))
        .json(&serde_json::json!({"code": code, "signature": signature}))
        .send()
        .await
        .unwrap();
    assert_eq!(claim.status(), 200);
    let body: serde_json::Value = claim.json().await.unwrap();
    let daemon_token = body["daemon_token"].as_str().unwrap();
    assert!(daemon_token.starts_with("rtb_"));

    // El device del daemon queda vinculado a la cuenta.
    let devices: serde_json::Value = http
        .get(format!("{}/v1/devices", relay.base))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let kinds: Vec<&str> = devices["devices"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["kind"].as_str().unwrap())
        .collect();
    assert!(kinds.contains(&"daemon"));

    // Un solo uso: el segundo reclamo del mismo código → 410.
    let again = http
        .post(format!("{}/v1/pairing/claim", relay.base))
        .json(&serde_json::json!({"code": code, "signature": signature}))
        .send()
        .await
        .unwrap();
    assert_eq!(again.status(), 410);
}

#[tokio::test]
async fn firma_invalida_y_limite_de_intentos() {
    let relay = spawn().await;
    let http = reqwest::Client::new();
    let (token, _) = register_and_login(&relay, "ana@example.com").await;
    let (_, pubkey) = new_key();
    let (attacker_key, _) = new_key();

    let created = create_code(&relay, &token, &pubkey).await;
    let code = created["code"].as_str().unwrap();

    // Firma de una clave distinta → 422, cinco veces.
    let bad_signature = B64.encode(attacker_key.sign(code.as_bytes()).to_bytes());
    for _ in 0..5 {
        let res = http
            .post(format!("{}/v1/pairing/claim", relay.base))
            .json(&serde_json::json!({"code": code, "signature": bad_signature}))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 422);
    }
    // Sexto intento → 429 con Retry-After (aunque la firma fuera correcta).
    let res = http
        .post(format!("{}/v1/pairing/claim", relay.base))
        .json(&serde_json::json!({"code": code, "signature": bad_signature}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 429);
    assert!(res.headers().contains_key("retry-after"));
}

#[tokio::test]
async fn codigo_expirado_y_desconocido() {
    let relay = spawn().await;
    let http = reqwest::Client::new();
    let (token, _) = register_and_login(&relay, "ana@example.com").await;
    let (key, pubkey) = new_key();

    let created = create_code(&relay, &token, &pubkey).await;
    let code = created["code"].as_str().unwrap();
    // Retroceder la expiración directamente en la base (TTL vencido).
    sqlx::query("UPDATE pairing_codes SET expires_at = ? WHERE code = ?")
        .bind("2020-01-01T00:00:00Z")
        .bind(code)
        .execute(&relay.state.pool)
        .await
        .unwrap();
    let signature = B64.encode(key.sign(code.as_bytes()).to_bytes());
    let res = http
        .post(format!("{}/v1/pairing/claim", relay.base))
        .json(&serde_json::json!({"code": code, "signature": signature}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 410);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["error"]["code"], "pairing_expired");

    // Código desconocido → 404.
    let res = http
        .post(format!("{}/v1/pairing/claim", relay.base))
        .json(&serde_json::json!({"code": "XXX-XXX-XXX", "signature": signature}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 404);

    // Crear códigos exige auth.
    let anon = http
        .post(format!("{}/v1/pairing/codes", relay.base))
        .json(&serde_json::json!({"daemon_pubkey": pubkey}))
        .send()
        .await
        .unwrap();
    assert_eq!(anon.status(), 401);
}

#[tokio::test]
async fn el_pairing_habilita_el_canal_del_daemon() {
    // pair_daemon del helper cubre el flujo completo usado por forward.rs.
    let relay = spawn().await;
    let (token, _) = register_and_login(&relay, "ana@example.com").await;
    let (daemon_token, device_id) = pair_daemon(&relay, &token).await;
    assert!(daemon_token.starts_with("rtb_"));
    assert!(!device_id.is_empty());
}
