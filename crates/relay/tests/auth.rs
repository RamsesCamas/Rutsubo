//! Cuentas, tokens y dispositivos (C-2): registro, login, rotación (RNF-07)
//! y revocación por dispositivo.

mod common;

use common::{register_and_login, spawn};

#[tokio::test]
async fn registro_login_rotacion_y_revocacion() {
    let relay = spawn().await;
    let http = reqwest::Client::new();

    // health es el único endpoint sin auth.
    let health = http
        .get(format!("{}/v1/health", relay.base))
        .send()
        .await
        .unwrap();
    assert_eq!(health.status(), 200);

    let (token, device_id) = register_and_login(&relay, "ana@example.com").await;
    assert!(token.starts_with("rtb_"), "prefijo de diagnóstico");

    // Registro duplicado → 422 validation_failed.
    let dup = http
        .post(format!("{}/v1/auth/register", relay.base))
        .json(&serde_json::json!({"email": "ana@example.com", "password": "secreta-123"}))
        .send()
        .await
        .unwrap();
    assert_eq!(dup.status(), 422);
    let body: serde_json::Value = dup.json().await.unwrap();
    assert_eq!(body["error"]["code"], "validation_failed");

    // Contraseña equivocada → 401 (misma respuesta que cuenta inexistente).
    let bad = http
        .post(format!("{}/v1/auth/token", relay.base))
        .json(&serde_json::json!({"email": "ana@example.com", "password": "incorrecta!"}))
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), 401);
    let ghost = http
        .post(format!("{}/v1/auth/token", relay.base))
        .json(&serde_json::json!({"email": "nadie@example.com", "password": "incorrecta!"}))
        .send()
        .await
        .unwrap();
    assert_eq!(ghost.status(), 401);

    // /v1/devices exige Bearer.
    let anon = http
        .get(format!("{}/v1/devices", relay.base))
        .send()
        .await
        .unwrap();
    assert_eq!(anon.status(), 401);

    let devices: serde_json::Value = http
        .get(format!("{}/v1/devices", relay.base))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let list = devices["devices"].as_array().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["id"], device_id.as_str());
    assert_eq!(list[0]["kind"], "client");
    assert_eq!(list[0]["current"], true);

    // Rotación: el token viejo muere en el acto (RNF-07).
    let rotated: serde_json::Value = http
        .post(format!("{}/v1/auth/token/rotate", relay.base))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let new_token = rotated["token"].as_str().unwrap();
    assert_ne!(new_token, token);
    let stale = http
        .get(format!("{}/v1/devices", relay.base))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(stale.status(), 401);

    // Segundo dispositivo y revocación: solo su token se invalida.
    let (second_token, second_id) = common::login(&relay, "ana@example.com").await;
    let revoke = http
        .delete(format!("{}/v1/devices/{second_id}", relay.base))
        .bearer_auth(new_token)
        .send()
        .await
        .unwrap();
    assert_eq!(revoke.status(), 204);
    let dead = http
        .get(format!("{}/v1/devices", relay.base))
        .bearer_auth(&second_token)
        .send()
        .await
        .unwrap();
    assert_eq!(dead.status(), 401);
    let alive = http
        .get(format!("{}/v1/devices", relay.base))
        .bearer_auth(new_token)
        .send()
        .await
        .unwrap();
    assert_eq!(alive.status(), 200);

    // Revocar un dispositivo ajeno responde 404 sin filtrar existencia.
    let (other_token, _) = register_and_login(&relay, "eva@example.com").await;
    let foreign = http
        .delete(format!("{}/v1/devices/{device_id}", relay.base))
        .bearer_auth(&other_token)
        .send()
        .await
        .unwrap();
    assert_eq!(foreign.status(), 404);
}

#[tokio::test]
async fn valida_correo_y_contrasena() {
    let relay = spawn().await;
    let http = reqwest::Client::new();
    for (email, password) in [("sin-arroba", "secreta-123"), ("ana@example.com", "corta")] {
        let res = http
            .post(format!("{}/v1/auth/register", relay.base))
            .json(&serde_json::json!({"email": email, "password": password}))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 422, "{email}/{password}");
    }
}
