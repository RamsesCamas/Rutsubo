//! Estado compartido del daemon.

use crate::config::DaemonConfig;
use crate::store;
use crate::store::events::AppendError;
use rutsubo_core::api::{ModelConfig, ProviderHealth, ProviderStatus};
use rutsubo_core::envelope::Envelope;
use rutsubo_core::events::{Event, SessionState};
use rutsubo_core::ids::SessionId;
use sqlx::SqlitePool;
use std::sync::Arc;
use tokio::sync::{RwLock, broadcast};

pub type App = Arc<AppState>;

pub struct AppState {
    pub cfg: DaemonConfig,
    pub pool: SqlitePool,
    pub token: String,
    /// Bus de eventos vivo: todo evento persistido se difunde aquí (WS local
    /// en Fase D; el relay de C-2 se colgaría del mismo bus en fase futura).
    pub bus: broadcast::Sender<Envelope<Event>>,
    /// Política vigente del adapter (C-1 `/v1/config/model`). Se lee al
    /// inicio de cada llamada al modelo: un PUT jamás interrumpe una
    /// generación en curso.
    pub model_config: RwLock<ModelConfig>,
    /// Proveedor activo reportado por `/v1/health`.
    pub provider_status: RwLock<ProviderStatus>,
    /// Compuerta de permisos: aprobaciones pendientes esperando decisión.
    pub gate: crate::gate::Gate,
}

impl AppState {
    /// Arranque completo: base de datos, token y configuración persistida.
    pub async fn bootstrap(cfg: DaemonConfig) -> Result<App, Box<dyn std::error::Error>> {
        let pool = store::open(&cfg.data_dir).await?;
        let token = crate::auth::load_or_create_token(&cfg.data_dir)?;
        let model_config = match store::config::load_model(&pool).await? {
            Some(cfg) => cfg,
            None => {
                let def = ModelConfig::default();
                store::config::save_model(&pool, &def).await?;
                def
            }
        };
        let provider_status = ProviderStatus {
            id: format!("local:mock:{}", model_config.local.model),
            health: ProviderHealth::Ready,
        };
        let (bus, _) = broadcast::channel(1024);
        Ok(Arc::new(Self {
            cfg,
            pool,
            token,
            bus,
            model_config: RwLock::new(model_config),
            provider_status: RwLock::new(provider_status),
            gate: crate::gate::Gate::default(),
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
