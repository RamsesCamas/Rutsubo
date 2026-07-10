//! Estado compartido del daemon.

use crate::config::DaemonConfig;
use crate::llm::fallback::FallbackAdapter;
use crate::llm::groq::GroqProvider;
use crate::llm::mock::MockProvider;
use crate::store;
use crate::store::events::AppendError;
use crate::tools::ToolRegistry;
use rutsubo_core::api::ModelConfig;
use rutsubo_core::envelope::Envelope;
use rutsubo_core::events::{Event, SessionState};
use rutsubo_core::ids::SessionId;
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, SqlitePool};
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::{RwLock, broadcast};

pub type App = Arc<AppState>;

pub struct AppState {
    pub cfg: DaemonConfig,
    pub pool: SqlitePool,
    pub token: String,
    pub remote_auth: Option<PgPool>,
    /// Bus de eventos vivo: todo evento persistido se difunde aquí (WS local
    /// en Fase D; el relay de C-2 se colgaría del mismo bus en fase futura).
    pub bus: broadcast::Sender<Envelope<Event>>,
    /// Política vigente del adapter (C-1 `/v1/config/model`). El adapter la
    /// lee al inicio de cada llamada: un PUT jamás interrumpe una generación
    /// en curso.
    pub model_config: Arc<RwLock<ModelConfig>>,
    /// Compuerta de permisos: aprobaciones pendientes esperando decisión.
    pub gate: crate::gate::Gate,
    /// Adapter LLM compuesto (C-4): MockProvider en esta fase; enchufar
    /// vLLM/Ollama/API real es implementar `LlmProvider` sin tocar el loop.
    pub llm: Arc<FallbackAdapter>,
    /// Registro de las 5 herramientas (RF-12).
    pub tools: Arc<ToolRegistry>,
    pub transcriber: Arc<dyn crate::asr::Transcriber>,
    /// Sesiones con turno agéntico en curso (RF-16: suspensión por sesión).
    pub running: std::sync::Mutex<HashSet<SessionId>>,
}

impl AppState {
    /// Arranque completo: base de datos, token, configuración persistida y
    /// adapter LLM.
    pub async fn bootstrap(cfg: DaemonConfig) -> Result<App, Box<dyn std::error::Error>> {
        let pool = store::open(&cfg.data_dir).await?;
        let remote_auth = if cfg.auth_mode == crate::config::AuthMode::Remote {
            let pool = PgPoolOptions::new()
                .max_connections(8)
                .connect(cfg.database_url.as_deref().expect("validated config"))
                .await?;
            crate::auth::migrate_remote_auth(&pool).await?;
            Some(pool)
        } else {
            None
        };
        let token = crate::auth::load_or_create_token(&cfg.data_dir)?;
        let model_config = match store::config::load_model(&pool).await? {
            Some(cfg) => cfg,
            None => {
                let def = ModelConfig::default();
                store::config::save_model(&pool, &def).await?;
                def
            }
        };
        let primary: Arc<dyn crate::llm::LlmProvider> = match &cfg.groq_api_key {
            Some(key) => Arc::new(GroqProvider::new(
                model_config.primary.model.clone(),
                key.clone(),
            )),
            None => Arc::new(MockProvider::new(format!(
                "groq:missing:{}",
                model_config.primary.model
            ))),
        };
        let secondary: Arc<dyn crate::llm::LlmProvider> = match &cfg.groq_api_key {
            Some(key) => Arc::new(GroqProvider::new(
                model_config.fallback.model.clone(),
                key.clone(),
            )),
            None => Arc::new(MockProvider::new(format!(
                "groq:missing:{}",
                model_config.fallback.model
            ))),
        };
        let model_config = Arc::new(RwLock::new(model_config));
        let llm = Arc::new(FallbackAdapter::new(
            primary,
            secondary,
            model_config.clone(),
        ));

        let transcriber: Arc<dyn crate::asr::Transcriber> = match &cfg.groq_api_key {
            Some(key) => Arc::new(crate::asr::GroqTranscriber::new(key.clone())),
            None => Arc::new(crate::asr::MockTranscriber),
        };
        let tools = Arc::new(if cfg.auth_mode == crate::config::AuthMode::Remote {
            ToolRegistry::default()
        } else {
            ToolRegistry::standard()
        });
        let (bus, _) = broadcast::channel(1024);
        Ok(Arc::new(Self {
            cfg,
            pool,
            token,
            remote_auth,
            bus,
            model_config,
            gate: crate::gate::Gate::default(),
            llm,
            tools,
            transcriber,
            running: std::sync::Mutex::new(HashSet::new()),
        }))
    }

    /// Única puerta de emisión de eventos: persiste (seq atómico, C-3) y
    /// difunde al bus. `new_state` aplica la transición de sesión en la misma
    /// transacción.
    pub async fn emit(
        &self,
        session_id: SessionId,
        event: Event,
        new_state: Option<SessionState>,
    ) -> Result<Envelope<Event>, AppendError> {
        let envelope = store::events::append(&self.pool, session_id, event, new_state).await?;
        // Sin receptores conectados no es un error: el historial ya quedó
        // persistido y el replay REST lo sirve.
        let _ = self.bus.send(envelope.clone());
        Ok(envelope)
    }
}
