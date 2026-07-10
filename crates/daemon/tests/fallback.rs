//! Tests del FallbackAdapter contra la tabla normativa de C-4 (§3.4.2),
//! usando FailingMock (falla bajo demanda) y el reloj pausado de tokio.

use futures::StreamExt;
use rutsubo_core::api::{ModelConfig, ProviderHealth, Thresholds};
use rutsubo_core::events::FallbackTrigger;
use rutsubo_daemon::llm::fallback::FallbackAdapter;
use rutsubo_daemon::llm::mock::{FailMode, FailingMock, MockProvider};
use rutsubo_daemon::llm::{GenerationRequest, ProviderError, StreamItem};
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

fn config(window: u32, cooldown_s: u32) -> Arc<RwLock<ModelConfig>> {
    let cfg = ModelConfig {
        thresholds: Thresholds {
            ttft_threshold_ms: 5000,
            failure_window: window,
            cooldown_s,
        },
        ..ModelConfig::default()
    };
    Arc::new(RwLock::new(cfg))
}

fn request() -> GenerationRequest {
    GenerationRequest {
        messages: vec![],
        tools: vec![],
        max_tokens: 128,
        temperature: 0.0,
        cancel: CancellationToken::new(),
    }
}

async fn drain(
    mut stream: rutsubo_daemon::llm::GenerationStream,
) -> Vec<Result<StreamItem, ProviderError>> {
    let mut items = Vec::new();
    while let Some(item) = stream.next().await {
        items.push(item);
    }
    items
}

#[tokio::test]
async fn oom_hace_fallback_inmediato_y_abre_el_breaker() {
    let primary = FailingMock::new("local:mock:p", FailMode::Oom);
    let secondary = FailingMock::new("external:mock:s", FailMode::Ok);
    let adapter = FallbackAdapter::new(primary.clone(), secondary.clone(), config(3, 60));

    let outcome = adapter.generate_with_info(request()).await.unwrap();
    assert_eq!(outcome.provider_id.0, "external:mock:s");
    let switch = outcome.switch.expect("hubo cambio de proveedor");
    assert_eq!(switch.trigger, FallbackTrigger::Oom);
    assert_eq!(switch.from.0, "local:mock:p");

    // Breaker abierto: la siguiente llamada va directa al secundario.
    let outcome = adapter.generate_with_info(request()).await.unwrap();
    assert_eq!(outcome.provider_id.0, "external:mock:s");
    assert!(
        outcome.switch.is_none(),
        "sin cambio: ya estaba en secundario"
    );
    assert_eq!(
        primary.calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "el primario no se toca con el breaker abierto"
    );
}

#[tokio::test(start_paused = true)]
async fn ttft_cancela_al_primario_y_reintenta_sin_abrir_breaker() {
    let primary = FailingMock::new("local:mock:p", FailMode::SlowFirstItem);
    let secondary = FailingMock::new("external:mock:s", FailMode::Ok);
    let adapter = FallbackAdapter::new(primary.clone(), secondary.clone(), config(3, 60));

    let outcome = adapter.generate_with_info(request()).await.unwrap();
    assert_eq!(outcome.provider_id.0, "external:mock:s");
    assert_eq!(
        outcome.switch.unwrap().trigger,
        FallbackTrigger::TtftExceeded
    );

    // TTFT no abre el breaker (tabla 6): la siguiente llamada reintenta el
    // primario, ya sano.
    primary.set_mode(FailMode::Ok);
    let outcome = adapter.generate_with_info(request()).await.unwrap();
    assert_eq!(outcome.provider_id.0, "local:mock:p");
    assert_eq!(primary.calls.load(std::sync::atomic::Ordering::SeqCst), 2);
}

#[tokio::test(start_paused = true)]
async fn rate_limited_va_al_fallback_y_no_abre_breaker() {
    let primary = FailingMock::new("groq:p", FailMode::RateLimited);
    let secondary = FailingMock::new("groq:s", FailMode::Ok);
    let adapter = FallbackAdapter::new(primary.clone(), secondary.clone(), config(1, 60));

    let outcome = adapter.generate_with_info(request()).await.unwrap();
    assert_eq!(outcome.provider_id.0, "groq:s");
    assert_eq!(
        outcome.switch.unwrap().trigger,
        FallbackTrigger::RateLimited
    );

    // Antes de Retry-After se salta el primario, sin abrir el breaker.
    let _ = adapter.generate_with_info(request()).await.unwrap();
    assert_eq!(primary.calls.load(std::sync::atomic::Ordering::SeqCst), 1);

    tokio::time::advance(std::time::Duration::from_secs(31)).await;
    primary.set_mode(FailMode::Ok);
    let outcome = adapter.generate_with_info(request()).await.unwrap();
    assert_eq!(outcome.provider_id.0, "groq:p");
    assert_eq!(primary.calls.load(std::sync::atomic::Ordering::SeqCst), 2);
}

#[tokio::test]
async fn la_ventana_de_fallos_abre_el_breaker() {
    let primary = FailingMock::new("local:mock:p", FailMode::Transport);
    let secondary = FailingMock::new("external:mock:s", FailMode::Ok);
    let adapter = FallbackAdapter::new(primary.clone(), secondary.clone(), config(3, 60));

    // Fallos 1 y 2: error visible (aún no se llena la ventana).
    for _ in 0..2 {
        let err = adapter.generate_with_info(request()).await.unwrap_err();
        assert!(matches!(err, ProviderError::Transport(_)));
    }
    // Fallo 3: llena la ventana → abre el breaker → esta llamada ya va al
    // secundario.
    let outcome = adapter.generate_with_info(request()).await.unwrap();
    assert_eq!(outcome.provider_id.0, "external:mock:s");
    assert_eq!(outcome.switch.unwrap().trigger, FallbackTrigger::Failures);

    // Breaker abierto: ruta directa.
    let _ = adapter.generate_with_info(request()).await.unwrap();
    assert_eq!(
        primary.calls.load(std::sync::atomic::Ordering::SeqCst),
        3,
        "tras abrir el breaker no se reintenta el primario"
    );
}

#[tokio::test(start_paused = true)]
async fn recuperacion_health_ready_tras_cooldown_cierra_el_breaker() {
    let primary = FailingMock::new("local:mock:p", FailMode::Oom);
    let secondary = FailingMock::new("external:mock:s", FailMode::Ok);
    let adapter = FallbackAdapter::new(primary.clone(), secondary.clone(), config(3, 60));

    let _ = adapter.generate_with_info(request()).await.unwrap(); // abre breaker
    primary.set_mode(FailMode::Ok);

    // Durante el cooldown: sigue en secundario.
    tokio::time::advance(std::time::Duration::from_secs(30)).await;
    let outcome = adapter.generate_with_info(request()).await.unwrap();
    assert_eq!(outcome.provider_id.0, "external:mock:s");

    // Cooldown vencido + health() = Ready → cierra el breaker y vuelve al
    // primario.
    tokio::time::advance(std::time::Duration::from_secs(31)).await;
    let outcome = adapter.generate_with_info(request()).await.unwrap();
    assert_eq!(outcome.provider_id.0, "local:mock:p");
    assert!(outcome.switch.is_some(), "el regreso también se notifica");
}

#[tokio::test(start_paused = true)]
async fn sin_health_ready_no_hay_recuperacion() {
    let primary = FailingMock::new("local:mock:p", FailMode::Oom);
    let secondary = FailingMock::new("external:mock:s", FailMode::Ok);
    let adapter = FallbackAdapter::new(primary.clone(), secondary.clone(), config(3, 60));

    let _ = adapter.generate_with_info(request()).await.unwrap();
    primary.set_mode(FailMode::Ok);
    primary.set_health(ProviderHealth::Down);

    tokio::time::advance(std::time::Duration::from_secs(61)).await;
    let outcome = adapter.generate_with_info(request()).await.unwrap();
    assert_eq!(
        outcome.provider_id.0, "external:mock:s",
        "sin Ready el breaker sigue abierto"
    );
}

#[tokio::test]
async fn cancelled_jamas_alimenta_la_ventana() {
    // Ventana de 1: cualquier fallo contado abriría el breaker.
    let primary = Arc::new(MockProvider::new("local:mock:p"));
    let secondary = FailingMock::new("external:mock:s", FailMode::Ok);
    let adapter = FallbackAdapter::new(primary, secondary.clone(), config(1, 60));

    let req = request();
    req.cancel.cancel();
    let err = adapter.generate_with_info(req).await.unwrap_err();
    assert!(matches!(err, ProviderError::Cancelled));

    // El breaker sigue cerrado: la siguiente llamada va al primario.
    let outcome = adapter.generate_with_info(request()).await.unwrap();
    assert_eq!(outcome.provider_id.0, "local:mock:p");
    assert_eq!(secondary.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[tokio::test]
async fn jamas_fallback_a_mitad_de_streaming() {
    let primary = FailingMock::new("local:mock:p", FailMode::FailMidStream);
    let secondary = FailingMock::new("external:mock:s", FailMode::Ok);
    let adapter = FallbackAdapter::new(primary.clone(), secondary.clone(), config(3, 60));

    let outcome = adapter.generate_with_info(request()).await.unwrap();
    assert_eq!(outcome.provider_id.0, "local:mock:p");
    let items = drain(outcome.stream).await;
    // El error viaja tal cual al final del stream: nada del secundario se
    // empalma en este mensaje.
    assert!(matches!(
        items.last(),
        Some(Err(ProviderError::Transport(_)))
    ));
    assert_eq!(
        secondary.calls.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "ningún empalme a mitad de streaming"
    );

    // El reintento del turno completo va al secundario.
    let outcome = adapter.generate_with_info(request()).await.unwrap();
    assert_eq!(outcome.provider_id.0, "external:mock:s");
    assert_eq!(outcome.switch.unwrap().trigger, FallbackTrigger::Failures);
    assert_eq!(primary.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
}
