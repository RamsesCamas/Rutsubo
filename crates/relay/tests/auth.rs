//! Identidad Google, tokens de dispositivo, rotación (RNF-07) y revocación
//! por dispositivo (C-2 enmendado). El relay de test arranca en modo dev.

mod common;

use common::{google_login, spawn};

#[tokio::test]
async fn login_google_rotacion_y_revocacion() {
    let relay = spawn().await;
    let http = reqwest::Client::new();

    // health es el único endpoint sin auth.
    let health = http
        .get(format!("{}/v1/health", relay.base))
        .send()
        .await
        .unwrap();
    assert_eq!(health.status(), 200);

    let (token, device_id) = google_login(&relay, "ana@example.com").await;
    assert!(token.starts_with("rtb_"), "prefijo de diagnóstico");

    // id_token dev mal formado → 401.
    let bad = http
        .post(format!("{}/v1/auth/google", relay.base))
        .json(&serde_json::json!({"id_token": "basura", "device": {}}))
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), 401);

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

    // Segundo dispositivo de la MISMA cuenta (mismo correo → mismo sub) y
    // revocación: solo su token se invalida. `/v1/devices` debe listar 2.
    let (second_token, second_id) = google_login(&relay, "ana@example.com").await;
    let two: serde_json::Value = http
        .get(format!("{}/v1/devices", relay.base))
        .bearer_auth(new_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        two["devices"].as_array().unwrap().len(),
        2,
        "mismo sub = misma cuenta"
    );
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

    // Revocar un dispositivo ajeno (otra cuenta) responde 404.
    let (other_token, _) = google_login(&relay, "eva@example.com").await;
    let foreign = http
        .delete(format!("{}/v1/devices/{device_id}", relay.base))
        .bearer_auth(&other_token)
        .send()
        .await
        .unwrap();
    assert_eq!(foreign.status(), 404);
}

#[tokio::test]
async fn google_dev_valida_formato() {
    let relay = spawn().await;
    let http = reqwest::Client::new();
    for bad in ["dev::correo@x.com", "dev:sub:sin-arroba", "no-dev:sub:x@y.z"] {
        let res = http
            .post(format!("{}/v1/auth/google", relay.base))
            .json(&serde_json::json!({"id_token": bad, "device": {}}))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 401, "id_token: {bad}");
    }
}
