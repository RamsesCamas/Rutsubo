//! Adapter compuesto (C-4 §3.4.2, ADR-008): envuelve (primario, secundario)
//! y aplica la política de `/v1/config/model` con la máquina de fallback
//! normativa (tabla 6):
//!
//! | Disparador     | Condición                                        | Acción                                   |
//! |----------------|--------------------------------------------------|------------------------------------------|
//! | OOM            | primario devuelve `OutOfMemory`                  | fallback inmediato; abre el breaker       |
//! | TTFT           | sin primer item tras `ttft_threshold_ms`         | cancela primario; reintenta en secundario |
//! | Fallos         | `failure_window` errores consecutivos            | abre el breaker                           |
//! | Breaker abierto| durante `cooldown_s`                             | ruta directa al secundario; sondeo salud  |
//! | Recuperación   | `health() = Ready` tras el cooldown              | cierra el breaker; vuelve al primario     |
//!
//! Reglas adicionales: (1) **jamás** fallback a mitad de un streaming
//! iniciado — el mensaje termina con `error` y el reintento del turno
//! completo va al secundario; (2) todo cambio efectivo de proveedor produce
//! `model_provider_changed` + audit (el loop lo emite con el `SwitchInfo`
//! devuelto); (3) con `local_only` todo disparador se vuelve error visible;
//! (4) `Cancelled` jamás alimenta la ventana de fallos.

use super::{GenerationRequest, GenerationStream, LlmProvider, ProviderError, StreamItem};
use async_trait::async_trait;
use futures::{Stream, StreamExt};
use rutsubo_core::api::{ModelConfig, ModelPolicy, ProviderHealth, ProviderStatus};
use rutsubo_core::events::FallbackTrigger;
use rutsubo_core::ids::ProviderId;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use tokio::time::{Duration, Instant};

/// Cambio efectivo de proveedor: el loop lo traduce a `model_provider_changed`.
#[derive(Debug, Clone)]
pub struct SwitchInfo {
    pub from: ProviderId,
    pub to: ProviderId,
    pub trigger: FallbackTrigger,
}

pub struct GenerationOutcome {
    pub stream: GenerationStream,
    /// Quién atendió la llamada (audit log, RF-22).
    pub provider_id: ProviderId,
    pub switch: Option<SwitchInfo>,
}

impl std::fmt::Debug for GenerationOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GenerationOutcome")
            .field("provider_id", &self.provider_id)
            .field("switch", &self.switch)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
struct BreakerState {
    consecutive_failures: u32,
    /// `Some(t)` = breaker abierto hasta `t`.
    open_until: Option<Instant>,
    /// Fallo a mitad de streaming: el siguiente turno va al secundario
    /// (regla 1), sin empalmar la salida de dos modelos en un mensaje.
    force_secondary_next: bool,
    /// Último proveedor efectivo (para detectar cambios).
    current: ProviderId,
    /// Última vez que se sondeó la salud del primario con el breaker abierto.
    last_health_probe: Option<Instant>,
}

pub struct FallbackAdapter {
    primary: Arc<dyn LlmProvider>,
    secondary: Arc<dyn LlmProvider>,
    config: Arc<tokio::sync::RwLock<ModelConfig>>,
    state: Arc<Mutex<BreakerState>>,
}

/// Intervalo del sondeo `health()` del primario con el breaker abierto (C-4).
const HEALTH_PROBE_INTERVAL: Duration = Duration::from_secs(15);

enum Route {
    Primary,
    Secondary(FallbackTrigger),
}

impl FallbackAdapter {
    pub fn new(
        primary: Arc<dyn LlmProvider>,
        secondary: Arc<dyn LlmProvider>,
        config: Arc<tokio::sync::RwLock<ModelConfig>>,
    ) -> Self {
        let current = primary.id();
        Self {
            primary,
            secondary,
            config,
            state: Arc::new(Mutex::new(BreakerState {
                consecutive_failures: 0,
                open_until: None,
                force_secondary_next: false,
                current,
                last_health_probe: None,
            })),
        }
    }

    /// Estado para `/v1/health`.
    pub async fn status(&self) -> ProviderStatus {
        let current = self.state.lock().expect("state").current.clone();
        let health = if current == self.secondary.id() {
            self.secondary.health().await
        } else {
            self.primary.health().await
        };
        ProviderStatus {
            id: current.0,
            health,
        }
    }

    /// Decide la ruta de esta llamada según política y breaker.
    async fn route(&self, policy: ModelPolicy) -> Route {
        match policy {
            ModelPolicy::ExternalOnly => Route::Secondary(FallbackTrigger::Manual),
            // Regla 3: en local_only los disparadores se vuelven error
            // visible; jamás se enruta al secundario.
            ModelPolicy::LocalOnly => Route::Primary,
            ModelPolicy::LocalFirst => {
                let (breaker_open, needs_probe, forced) = {
                    let mut st = self.state.lock().expect("state");
                    let forced = std::mem::take(&mut st.force_secondary_next);
                    let now = Instant::now();
                    match st.open_until {
                        Some(until) if now < until => {
                            let needs_probe = st
                                .last_health_probe
                                .is_none_or(|t| now - t >= HEALTH_PROBE_INTERVAL);
                            if needs_probe {
                                st.last_health_probe = Some(now);
                            }
                            (true, needs_probe, forced)
                        }
                        Some(_) => (true, true, forced), // cooldown vencido: sondear ya
                        None => (false, false, forced),
                    }
                };

                if forced {
                    return Route::Secondary(FallbackTrigger::Failures);
                }
                if !breaker_open {
                    return Route::Primary;
                }
                // Breaker abierto: sondeo de recuperación.
                if needs_probe {
                    let cooldown_elapsed = {
                        let st = self.state.lock().expect("state");
                        st.open_until.map(|u| Instant::now() >= u).unwrap_or(true)
                    };
                    if cooldown_elapsed && self.primary.health().await == ProviderHealth::Ready {
                        // Recuperación: cierra el breaker; esta llamada vuelve
                        // al primario (C-4 tabla 6).
                        let mut st = self.state.lock().expect("state");
                        st.open_until = None;
                        st.consecutive_failures = 0;
                        st.last_health_probe = None;
                        return Route::Primary;
                    }
                }
                Route::Secondary(FallbackTrigger::Failures)
            }
        }
    }

    fn open_breaker(&self, cooldown_s: u32) {
        let mut st = self.state.lock().expect("state");
        st.open_until = Some(Instant::now() + Duration::from_secs(u64::from(cooldown_s)));
        st.last_health_probe = None;
    }

    /// Registra un fallo Transport/InvalidResponse; abre el breaker si se
    /// llenó la ventana. Devuelve `true` si el breaker quedó abierto.
    fn count_failure(&self, window: u32, cooldown_s: u32) -> bool {
        let opened = {
            let mut st = self.state.lock().expect("state");
            st.consecutive_failures += 1;
            st.consecutive_failures >= window
        };
        if opened {
            self.open_breaker(cooldown_s);
        }
        opened
    }

    fn note_success(&self, provider: &ProviderId, trigger: FallbackTrigger) -> Option<SwitchInfo> {
        let mut st = self.state.lock().expect("state");
        st.consecutive_failures = 0;
        if st.current != *provider {
            let from = std::mem::replace(&mut st.current, provider.clone());
            Some(SwitchInfo {
                from,
                to: provider.clone(),
                trigger,
            })
        } else {
            None
        }
    }

    /// Intenta generar en `provider` aplicando el umbral TTFT al primer item.
    async fn attempt(
        &self,
        provider: &Arc<dyn LlmProvider>,
        req: &GenerationRequest,
        ttft_ms: u64,
    ) -> Result<GenerationStream, ProviderError> {
        let child = req.cancel.child_token();
        let mut child_req = req.clone();
        child_req.cancel = child.clone();
        let mut stream = provider.generate(child_req).await?;

        match tokio::time::timeout(Duration::from_millis(ttft_ms), stream.next()).await {
            Err(_) => {
                // TTFT vencido: cancela el intento (cooperativo) y reporta.
                child.cancel();
                Err(ProviderError::Timeout { after_ms: ttft_ms })
            }
            Ok(None) => Err(ProviderError::InvalidResponse(
                "stream vacío del proveedor".into(),
            )),
            Ok(Some(Err(e))) => Err(e),
            Ok(Some(Ok(first))) => Ok(
                Box::pin(futures::stream::iter(vec![Ok(first)]).chain(stream)) as GenerationStream,
            ),
        }
    }

    /// Punto de entrada del agent loop: además del stream devuelve quién
    /// atendió (RF-22) y si hubo cambio de proveedor (C-3).
    pub async fn generate_with_info(
        &self,
        req: GenerationRequest,
    ) -> Result<GenerationOutcome, ProviderError> {
        let cfg = self.config.read().await.clone();
        let ttft = cfg.fallback.ttft_threshold_ms;
        let window = cfg.fallback.failure_window;
        let cooldown = cfg.fallback.cooldown_s;

        let (provider, trigger) = match self.route(cfg.policy).await {
            Route::Primary => (self.primary.clone(), None),
            Route::Secondary(t) => (self.secondary.clone(), Some(t)),
        };

        // Ruta directa (secundario por política/breaker, o primario en
        // local_only/local_first sano): un intento, errores clasificados.
        if provider.id() == self.secondary.id() {
            let stream = self.attempt(&provider, &req, ttft).await?;
            let id = provider.id();
            let switch = self.note_success(&id, trigger.unwrap_or(FallbackTrigger::Manual));
            return Ok(self.watched(stream, id, switch, window, cooldown));
        }

        // Primario. Clasificación de errores → máquina de fallback.
        match self.attempt(&provider, &req, ttft).await {
            Ok(stream) => {
                let id = provider.id();
                let switch = self.note_success(&id, FallbackTrigger::Manual);
                Ok(self.watched(stream, id, switch, window, cooldown))
            }
            Err(ProviderError::Cancelled) => Err(ProviderError::Cancelled), // regla 4
            Err(err) => {
                let fallback_trigger = match &err {
                    // OOM: fallback inmediato + abre el breaker.
                    ProviderError::OutOfMemory => {
                        self.open_breaker(cooldown);
                        Some(FallbackTrigger::Oom)
                    }
                    // TTFT: cancela primario, reintenta en secundario
                    // (sin abrir el breaker: tabla 6).
                    ProviderError::Timeout { .. } => Some(FallbackTrigger::TtftExceeded),
                    // Transport/InvalidResponse: cuentan para la ventana;
                    // solo al llenarla se abre el breaker y se enruta.
                    ProviderError::Transport(_) | ProviderError::InvalidResponse(_) => self
                        .count_failure(window, cooldown)
                        .then_some(FallbackTrigger::Failures),
                    ProviderError::Cancelled => unreachable!(),
                };

                match fallback_trigger {
                    // local_only: el disparador se vuelve error visible (regla 3).
                    Some(_) if cfg.policy == ModelPolicy::LocalOnly => Err(err),
                    Some(trigger) => {
                        let stream = self.attempt(&self.secondary, &req, ttft).await?;
                        let id = self.secondary.id();
                        let switch = self.note_success(&id, trigger);
                        Ok(self.watched(stream, id, switch, window, cooldown))
                    }
                    // Fallo que aún no llena la ventana: error visible.
                    None => Err(err),
                }
            }
        }
    }

    fn watched(
        &self,
        stream: GenerationStream,
        provider_id: ProviderId,
        switch: Option<SwitchInfo>,
        window: u32,
        cooldown_s: u32,
    ) -> GenerationOutcome {
        GenerationOutcome {
            stream: Box::pin(WatchedStream {
                inner: stream,
                state: self.state.clone(),
                window,
                cooldown_s,
            }),
            provider_id,
            switch,
        }
    }
}

/// Observa fallos a mitad de streaming: cuentan para la ventana y fuerzan el
/// siguiente turno al secundario (regla 1: jamás empalmar dos modelos en un
/// mismo mensaje — el error viaja al cliente tal cual).
struct WatchedStream {
    inner: GenerationStream,
    state: Arc<Mutex<BreakerState>>,
    window: u32,
    cooldown_s: u32,
}

impl Stream for WatchedStream {
    type Item = Result<StreamItem, ProviderError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let polled = self.inner.as_mut().poll_next(cx);
        if let Poll::Ready(Some(Err(err))) = &polled {
            match err {
                ProviderError::Transport(_)
                | ProviderError::InvalidResponse(_)
                | ProviderError::OutOfMemory
                | ProviderError::Timeout { .. } => {
                    let mut st = self.state.lock().expect("state");
                    st.force_secondary_next = true;
                    st.consecutive_failures += 1;
                    if st.consecutive_failures >= self.window {
                        st.open_until =
                            Some(Instant::now() + Duration::from_secs(u64::from(self.cooldown_s)));
                        st.last_health_probe = None;
                    }
                }
                ProviderError::Cancelled => {} // regla 4: cancelar no es fallar
            }
        }
        polled
    }
}

#[async_trait]
impl LlmProvider for FallbackAdapter {
    fn id(&self) -> ProviderId {
        self.state.lock().expect("state").current.clone()
    }

    async fn generate(&self, req: GenerationRequest) -> Result<GenerationStream, ProviderError> {
        Ok(self.generate_with_info(req).await?.stream)
    }

    async fn health(&self) -> ProviderHealth {
        self.status().await.health
    }
}
