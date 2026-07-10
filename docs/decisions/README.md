# Decisiones de implementación — backend Rutsubo

Los documentos normativos del proyecto son el *Documento de Requerimientos y
Diseño* (RF-01…RF-31, RNF-01…RNF-18) y *ADRs y Contratos de Servicio*
(ADR-001…ADR-008, contratos C-1…C-4). Jerarquía ante ambigüedad:
**contrato C-n > ADR-n > handoff de implementación**.

## Trazabilidad ADR ↔ componente

| ADR | Componente en este repo |
|---|---|
| ADR-001 (daemon local) | `crates/daemon` — bind exclusivo loopback (`config.rs`, RNF-04) |
| ADR-003 (Rust/Axum/tokio/sqlx) | todo el workspace; búsqueda con crates de ripgrep (`tools/search.rs`) |
| ADR-004 (contrato único en `core` + ts-rs) | `crates/core` + `bindings/` generados (`just bindings`) |
| ADR-005 (SQLite embebido, WAL) | `store/` + `migrations/`; consultas verificadas en compilación (caché `.sqlx/`) |
| ADR-008 (adapter local-first con fallback) | `llm/fallback.rs` según tabla normativa C-4 |

ADR-002/006/007 (superficies, relay, móvil) aplican a otros repos/fases; el
bus de eventos interno (`state.rs`) es el punto donde el relay de C-2 se
colgará en la fase siguiente.

## Resoluciones tomadas durante la implementación

1. **Sobre C-3 *adjacently tagged*.** El sketch del handoff usaba
   `#[serde(tag = "type")]` con flatten, que inlinearía el payload; el
   ejemplo normativo de C-3 §3.3.1 muestra un campo `payload` separado. Por
   jerarquía gana C-3: los enums de eventos/comandos usan
   `#[serde(tag = "type", content = "payload")]` (`core/src/events.rs`).
2. **Migración 0002 (`rules`, `config`).** El esquema 0001 del handoff no
   contemplaba persistencia para `GET/PUT /v1/rules` ni para
   `/v1/config/model`, que C-1 sí exige. Se añadió la migración 0002; la
   *evaluación* de reglas en la compuerta sigue siendo `TODO(fase-3)` (RF-18),
   como permite el alcance.
3. **`resolved_by` en fase local.** Sin pairing (C-2 es fase futura) no hay
   device IDs; las decisiones registran `local:rest` o `local:ws` según el
   transporte que las originó.
4. **`model_provider_changed` en cambios no degradantes.** El trigger del
   evento es un enum cerrado (`oom|ttft_exceeded|failures|manual`). Los
   cambios de proveedor que no provienen de un disparador de degradación
   (recuperación del breaker, cambio de política) se reportan como `manual`.
5. **Sondeo de salud del breaker.** C-4 pide sondear `health()` cada 15 s con
   el breaker abierto. Se implementa de forma perezosa sobre la siguiente
   llamada (con marca de tiempo del último sondeo): sin llamadas no hay nada
   que enrutar, y la recuperación igualmente exige cooldown vencido + Ready.
6. **Telemetría (RF-30/31).** Instrumentación con `tracing`; el export OTLP a
   Arize Phoenix queda `TODO(fase-3)` — fuera del alcance del handoff de esta
   fase.
7. **Dos repositorios.** El monorepo del handoff se dividió por decisión del
   usuario: backend (este repo) y
   [Rutsubo-Webapp](https://github.com/RamsesCamas/Rutsubo-Webapp). Los
   bindings ts-rs se generan aquí y la webapp vendoriza una copia commiteada
   (`npm run sync:bindings` + check de drift), preservando RNF-17 entre repos.
8. **Truncado de salida uniforme.** El handoff exige 64 KB para `run_shell`;
   se aplica el mismo tope a toda herramienta (`tools/mod.rs`):
   `output_excerpt` es un extracto por contrato.
