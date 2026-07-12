//! ASR proxied por el daemon: el navegador nunca recibe la clave Groq.
use async_trait::async_trait;
use rutsubo_core::api::AsrResponse;
use serde::Deserialize;

const GROQ_ASR_URL: &str = "https://api.groq.com/openai/v1/audio/transcriptions";
const ASR_MODEL: &str = "whisper-large-v3";

/// Respuesta de transcripción de Groq, deserializada con struct propio laxo:
/// Groq devuelve `{"text": "...", "x_groq": {...}}` SIN el campo `usage` que el
/// deserializador estricto de async-openai exige. Solo se extrae `text`.
#[derive(Deserialize)]
struct GroqTranscription {
    text: String,
}

#[derive(Debug, thiserror::Error)]
pub enum TranscribeError {
    #[error("upstream ASR failed")]
    Upstream,
}

#[async_trait]
pub trait Transcriber: Send + Sync {
    async fn transcribe(
        &self,
        audio: Vec<u8>,
        mime: &str,
        language: Option<&str>,
    ) -> Result<AsrResponse, TranscribeError>;
}

/// Implementación determinista para pruebas sin red.
pub struct MockTranscriber;
#[async_trait]
impl Transcriber for MockTranscriber {
    async fn transcribe(
        &self,
        _audio: Vec<u8>,
        _mime: &str,
        _language: Option<&str>,
    ) -> Result<AsrResponse, TranscribeError> {
        Ok(AsrResponse {
            text: "transcripción de prueba".into(),
            duration_ms: 0,
            model: "whisper-large-v3".into(),
        })
    }
}

pub struct GroqTranscriber {
    api_key: String,
    http: reqwest::Client,
}
impl GroqTranscriber {
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            http: reqwest::Client::new(),
        }
    }
}
#[async_trait]
impl Transcriber for GroqTranscriber {
    async fn transcribe(
        &self,
        audio: Vec<u8>,
        mime: &str,
        language: Option<&str>,
    ) -> Result<AsrResponse, TranscribeError> {
        // Petición multipart directa: Groq/whisper detecta el formato por la
        // extensión del filename, derivada del MIME que envió el cliente.
        let filename = format!("audio.{}", ext_for(mime));
        let part = reqwest::multipart::Part::bytes(audio)
            .file_name(filename)
            .mime_str(mime.split(';').next().unwrap_or("audio/webm"))
            .map_err(|_| TranscribeError::Upstream)?;
        let mut form = reqwest::multipart::Form::new()
            .part("file", part)
            .text("model", ASR_MODEL);
        if let Some(language) = language {
            form = form.text("language", language.to_owned());
        }

        let response = self
            .http
            .post(GROQ_ASR_URL)
            .bearer_auth(&self.api_key)
            .multipart(form)
            .send()
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "groq asr transport");
                TranscribeError::Upstream
            })?;
        if !response.status().is_success() {
            tracing::error!(status = %response.status(), "groq asr rechazó el audio");
            return Err(TranscribeError::Upstream);
        }
        let parsed: GroqTranscription = response.json().await.map_err(|e| {
            tracing::error!(error = %e, "groq asr respuesta ilegible");
            TranscribeError::Upstream
        })?;
        Ok(AsrResponse {
            text: parsed.text,
            duration_ms: 0,
            model: ASR_MODEL.into(),
        })
    }
}

/// Extensión de archivo para el MIME del audio (whisper detecta el formato por
/// ella). Default `webm`, que es lo que graba el navegador/WebView.
fn ext_for(mime: &str) -> &'static str {
    match mime.split(';').next().unwrap_or("").trim() {
        "audio/wav" | "audio/x-wav" => "wav",
        "audio/m4a" | "audio/mp4" => "m4a",
        "audio/opus" => "ogg",
        _ => "webm",
    }
}

/// Límite de seguridad del audio multipart, en bytes.
pub const MAX_AUDIO_BYTES: usize = 25 * 1024 * 1024;

#[cfg(test)]
mod tests {
    use super::GroqTranscription;

    #[test]
    fn deserializa_respuesta_de_groq_sin_campo_usage() {
        // Groq devuelve `x_groq` y omite `usage`, lo que el struct de
        // async-openai rechazaba. El struct laxo solo necesita `text`.
        let raw = r#"{"text":" hola mundo","x_groq":{"id":"req_1"}}"#;
        let parsed: GroqTranscription = serde_json::from_str(raw).expect("debe deserializar");
        assert_eq!(parsed.text, " hola mundo");
    }
}
