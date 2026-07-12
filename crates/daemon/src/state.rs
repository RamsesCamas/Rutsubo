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
    /// Adapter LLM compuesto (C-4). Swappable: al configurar la API key desde
    /// la UI se reconstruye en caliente (`reconfigure_provider`).
    pub llm: RwLock<Arc<FallbackAdapter>>,
    /// Registro de las 5 herramientas (RF-12).
    pub tools: Arc<ToolRegistry>,
    /// Transcriptor ASR; swappable junto con el adapter al cambiar la key.
    pub transcriber: RwLock<Arc<dyn crate::asr::Transcriber>>,
    /// API key de Groq efectiva (DB > entorno). La UI la actualiza en caliente.
    pub groq_key: RwLock<Option<String>>,
    /// Tickets efímeros de un solo uso para el handshake del WS remoto.
    pub tickets: crate::tickets::TicketStore,
    /// Sesiones con turno agéntico en curso (RF-16: suspensión por sesión).
    pub running: std::sync::Mutex<HashSet<SessionId>>,
    /// Estado de la conexión saliente al relay C-2 (ADR-006) + despertador
    /// para reintentar tras el pairing.
    pub relay: Arc<crate::relay::RelayControl>,
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
        // Key efectiva: la persistida desde la UI tiene prioridad sobre el
        // entorno (permite ambas rutas: .env legado o configurar en Ajustes).
        let groq_key = match store::config::load_provider_key(&pool).await? {
            Some(k) => Some(k),
            None => cfg.groq_api_key.clone(),
        };
        let model_config = Arc::new(RwLock::new(model_config));
        let (llm, transcriber) = build_providers(
            groq_key.as_deref(),
            &*model_config.read().await,
            &model_config,
        );
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
            llm: RwLock::new(llm),
            tools,
            transcriber: RwLock::new(transcriber),
            groq_key: RwLock::new(groq_key),
            tickets: crate::tickets::TicketStore::default(),
            running: std::sync::Mutex::new(HashSet::new()),
            relay: Arc::new(crate::relay::RelayControl::default()),
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

    /// Reconfigura el proveedor de modelo con una nueva API key (o `None` para
    /// borrarla → modo degradado). Persiste la key y reconstruye el adapter y
    /// el transcriptor en caliente: la siguiente llamada al modelo usa la key
    /// nueva sin reiniciar el daemon.
    pub async fn reconfigure_provider(&self, key: Option<String>) -> Result<(), sqlx::Error> {
        store::config::save_provider_key(&self.pool, key.as_deref()).await?;
        let model_config = self.model_config.read().await.clone();
        let (llm, transcriber) = build_providers(key.as_deref(), &model_config, &self.model_config);
        *self.llm.write().await = llm;
        *self.transcriber.write().await = transcriber;
        *self.groq_key.write().await = key;
        Ok(())
    }
}

/// Construye el adapter LLM y el transcriptor a partir de la API key efectiva
/// y la política de modelos. Sin key: proveedores mock (modo degradado), la
/// app abre igual y health reporta `missing_api_key`.
fn build_providers(
    key: Option<&str>,
    model_config: &ModelConfig,
    model_config_ref: &Arc<RwLock<ModelConfig>>,
) -> (Arc<FallbackAdapter>, Arc<dyn crate::asr::Transcriber>) {
    let primary: Arc<dyn crate::llm::LlmProvider> = match key {
        Some(k) => Arc::new(GroqProvider::new(
            model_config.primary.model.clone(),
            k.to_owned(),
        )),
        None => Arc::new(MockProvider::new(format!(
            "groq:missing:{}",
            model_config.primary.model
        ))),
    };
    let secondary: Arc<dyn crate::llm::LlmProvider> = match key {
        Some(k) => Arc::new(GroqProvider::new(
            model_config.fallback.model.clone(),
            k.to_owned(),
        )),
        None => Arc::new(MockProvider::new(format!(
            "groq:missing:{}",
            model_config.fallback.model
        ))),
    };
    let llm = Arc::new(FallbackAdapter::new(
        primary,
        secondary,
        model_config_ref.clone(),
    ));
    let transcriber: Arc<dyn crate::asr::Transcriber> = match key {
        Some(k) => Arc::new(crate::asr::GroqTranscriber::new(k.to_owned())),
        None => Arc::new(crate::asr::MockTranscriber),
    };
    (llm, transcriber)
}
