//! ASR proxied por el daemon: el navegador nunca recibe la clave Groq.
use async_openai::{
    Client,
    config::OpenAIConfig,
    types::{
        InputSource,
        audio::{AudioInput, CreateTranscriptionRequestArgs},
    },
};
use async_trait::async_trait;
use rutsubo_core::api::AsrResponse;

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
    client: Client<OpenAIConfig>,
}
impl GroqTranscriber {
    pub fn new(api_key: String) -> Self {
        Self {
            client: Client::with_config(
                OpenAIConfig::new()
                    .with_api_base("https://api.groq.com/openai/v1")
                    .with_api_key(api_key),
            ),
        }
    }
}
#[async_trait]
impl Transcriber for GroqTranscriber {
    async fn transcribe(
        &self,
        audio: Vec<u8>,
        _mime: &str,
        language: Option<&str>,
    ) -> Result<AsrResponse, TranscribeError> {
        let mut builder = CreateTranscriptionRequestArgs::default();
        builder.file(AudioInput {
            source: InputSource::VecU8 {
                filename: "audio.webm".into(),
                vec: audio,
            },
        });
        builder.model("whisper-large-v3");
        if let Some(language) = language {
            builder.language(language);
        }
        let req = builder.build().map_err(|_| TranscribeError::Upstream)?;
        let response = self
            .client
            .audio()
            .transcription()
            .create(req)
            .await
            .map_err(|_| TranscribeError::Upstream)?;
        Ok(AsrResponse {
            text: response.text,
            duration_ms: 0,
            model: "whisper-large-v3".into(),
        })
    }
}

/// Límite de seguridad del audio multipart, en bytes.
pub const MAX_AUDIO_BYTES: usize = 25 * 1024 * 1024;
