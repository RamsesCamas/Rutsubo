use crate::asr::MAX_AUDIO_BYTES;
use crate::error::ApiError;
use crate::state::App;
use axum::Json;
use axum::extract::{Multipart, State};
use chrono::Utc;
use rutsubo_core::api::AsrResponse;
use serde_json::json;

pub async fn transcribe(
    State(app): State<App>,
    mut multipart: Multipart,
) -> Result<Json<AsrResponse>, ApiError> {
    let mut audio = None;
    let mut mime = None;
    let mut language = None;
    while let Some(field) = multipart.next_field().await.map_err(ApiError::internal)? {
        match field.name() {
            Some("audio") => {
                mime = field.content_type().map(str::to_owned);
                let bytes = field.bytes().await.map_err(ApiError::internal)?;
                if bytes.len() > MAX_AUDIO_BYTES {
                    return Err(ApiError {
                        status: axum::http::StatusCode::PAYLOAD_TOO_LARGE,
                        code: rutsubo_core::api::ErrorCode::ValidationFailed,
                        message: "audio excede 25 MB".into(),
                        details: None,
                    });
                }
                audio = Some(bytes.to_vec());
            }
            Some("language") => language = Some(field.text().await.map_err(ApiError::internal)?),
            _ => {}
        }
    }
    let audio = audio.ok_or_else(|| ApiError::validation("falta el campo audio", None))?;
    let mime = mime.unwrap_or_default();
    if !matches!(
        mime.as_str(),
        "audio/webm"
            | "audio/webm;codecs=opus"
            | "audio/opus"
            | "audio/wav"
            | "audio/x-wav"
            | "audio/m4a"
            | "audio/mp4"
    ) {
        return Err(ApiError::validation(
            "tipo de audio no permitido",
            Some(json!({"mime": mime})),
        ));
    }
    let bytes = audio.len();
    let transcriber = app.transcriber.read().await.clone();
    let result = transcriber
        .transcribe(audio, &mime, language.as_deref())
        .await
        .map_err(|_| ApiError {
            status: axum::http::StatusCode::BAD_GATEWAY,
            code: rutsubo_core::api::ErrorCode::AsrFailed,
            message: "falló la transcripción".into(),
            details: None,
        })?;
    crate::store::audit::insert(
        &app.pool,
        None,
        "asr",
        &json!({"duration_ms": result.duration_ms, "bytes": bytes}),
        Utc::now(),
    )
    .await?;
    Ok(Json(result))
}
