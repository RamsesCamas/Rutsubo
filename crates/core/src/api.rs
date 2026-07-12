//! Esquemas de request/response y filtros de la API REST del daemon
//! (contrato C-1). Declarados aquí —no en el daemon— para que los clientes
//! consuman los mismos tipos vía bindings generados (RNF-17).

use crate::envelope::Envelope;
use crate::events::{Decision, Event, SessionState};
use crate::ids::{ApprovalId, AuditId, MessageId, RuleId, SessionId, ToolCallId};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

// ---- Sobre de error (C-1, sección 1 del contrato) ----

/// Catálogo cerrado de códigos de error de C-1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ErrorCode {
    Unauthorized,
    NotFound,
    ValidationFailed,
    Conflict,
    SessionBusy,
    AsrFailed,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ErrorBody {
    pub code: ErrorCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

/// Única forma de error permitida: `{ "error": { code, message, details } }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ErrorEnvelope {
    pub error: ErrorBody,
}

// ---- Salud (GET /v1/health) ----

/// Salud de un proveedor de modelo (C-4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ProviderHealth {
    Ready,
    Degraded,
    Down,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ProviderStatus {
    pub id: String,
    pub health: ProviderHealth,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Estado de la conexión saliente al relay C-2 (ADR-006).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RelayStatus {
    /// `RUTSUBO_RELAY_URL` está configurada.
    pub configured: bool,
    /// La conexión WebSocket saliente está viva.
    pub connected: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
    pub provider: ProviderStatus,
    /// Ausente en builds previos al relay; los clientes lo tratan opcional.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub relay: Option<RelayStatus>,
}

// ---- Sesiones ----

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionDto {
    pub id: SessionId,
    pub workspace_path: String,
    pub title: String,
    pub state: SessionState,
    #[ts(type = "string")]
    pub created_at: DateTime<Utc>,
    #[ts(type = "number")]
    pub last_seq: u64,
}

/// Detalle con contadores (GET /v1/sessions/{id}).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionDetail {
    #[serde(flatten)]
    pub session: SessionDto,
    #[ts(type = "number")]
    pub message_count: u64,
    #[ts(type = "number")]
    pub pending_approvals: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CreateSessionRequest {
    /// Ruta absoluta a un directorio existente, sin secuencias de traversal.
    pub workspace_path: String,
    /// Opcional, ≤ 120 caracteres.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// PATCH /v1/sessions/{id}: archivar / renombrar. Reemplazo por campo presente.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PatchSessionRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Único cambio de estado permitido por API: `archived`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<SessionState>,
}

/// Filtros de GET /v1/sessions (paginación por cursor, filtro por estado).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionsQuery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub limit: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<SessionState>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionsPage {
    pub sessions: Vec<SessionDto>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

// ---- Mensajes ----

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SendMessageRequest {
    /// ≤ 32 000 caracteres.
    pub content: String,
    /// UUID generado por el cliente; clave de idempotencia por sesión.
    pub client_msg_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SendMessageResponse {
    pub message_id: MessageId,
    pub session_state: SessionState,
    #[ts(type = "string")]
    pub accepted_at: DateTime<Utc>,
}

// ---- Eventos (replay) ----

/// Filtros de GET /v1/sessions/{id}/events.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct EventsQuery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub after_seq: Option<u64>,
    /// Máximo 1000.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct EventsPage {
    pub events: Vec<Envelope<Event>>,
    #[ts(type = "number")]
    pub last_seq: u64,
    pub has_more: bool,
}

// ---- Aprobaciones ----

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ApprovalDto {
    pub id: ApprovalId,
    pub session_id: SessionId,
    pub tool_call_id: ToolCallId,
    pub tool: String,
    pub summary: String,
    pub args: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<Decision>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_by: Option<String>,
    #[ts(type = "string")]
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, as = "Option<String>")]
    pub resolved_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ApprovalsPage {
    /// Pendientes de todas las sesiones, más antiguas primero.
    pub approvals: Vec<ApprovalDto>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct DecisionRequest {
    pub decision: Decision,
    /// Opcional, ≤ 500 caracteres.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Crear regla estable (RF-18).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remember_rule: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct DecisionResponse {
    pub approval_id: ApprovalId,
    pub resolved: bool,
    pub decision: Decision,
    pub resolved_by: String,
    #[ts(type = "string")]
    pub resolved_at: DateTime<Utc>,
}

// ---- Reglas de auto-aprobación (RF-18) ----

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Rule {
    pub id: RuleId,
    pub workspace_path: String,
    pub tool: String,
    pub pattern: String,
    #[ts(type = "string")]
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RulesPage {
    pub rules: Vec<Rule>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct NewRule {
    pub workspace_path: String,
    pub tool: String,
    pub pattern: String,
}

/// PUT /v1/rules: reemplazo completo del conjunto.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PutRulesRequest {
    pub rules: Vec<NewRule>,
}

// ---- Configuración del adapter LLM (GET/PUT /v1/config/model) ----

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ModelRef {
    pub provider: String,
    pub model: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Thresholds {
    #[ts(type = "number")]
    pub ttft_threshold_ms: u64,
    pub failure_window: u32,
    pub cooldown_s: u32,
}

/// Configuración primaria/fallback remota. El PUT aplica al siguiente turno.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ModelConfig {
    pub primary: ModelRef,
    pub fallback: ModelRef,
    pub thresholds: Thresholds,
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            primary: ModelRef {
                provider: "groq".into(),
                model: "qwen/qwen3.6-27b".into(),
            },
            fallback: ModelRef {
                provider: "groq".into(),
                model: "openai/gpt-oss-120b".into(),
            },
            thresholds: Thresholds {
                ttft_threshold_ms: 5000,
                failure_window: 3,
                cooldown_s: 60,
            },
        }
    }
}

// ---- Credencial del proveedor (GET/PUT /v1/config/provider) ----

/// Estado de la API key del proveedor. Nunca expone la key en sí (RNF-07);
/// solo si hay una configurada y de dónde vino.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ProviderKeyStatus {
    /// Hay una API key efectiva (la app puede llamar al modelo).
    pub configured: bool,
    /// `stored` (persistida desde la UI), `env` (variable de entorno) o `none`.
    pub source: String,
}

/// PUT /v1/config/provider: configura o borra la API key de Groq.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SetProviderKeyRequest {
    /// `null` o vacío borra la key (vuelve a modo degradado / a la del entorno).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub groq_api_key: Option<String>,
}

// ---- Explorador de directorios (GET /v1/fs/list) ----

/// Una entrada de directorio (solo se listan carpetas: se elige el workspace).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct DirEntry {
    pub name: String,
    /// Ruta absoluta de la entrada.
    pub path: String,
}

/// Listado de un directorio para el selector de carpeta de la UI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct DirListing {
    /// Ruta absoluta que se listó.
    pub path: String,
    /// Directorio padre (`null` en una raíz).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// Subdirectorios, ordenados por nombre.
    pub entries: Vec<DirEntry>,
}

/// Resultado de la transcripción ASR. El audio nunca se persiste.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AsrResponse {
    pub text: String,
    #[ts(type = "number")]
    pub duration_ms: u64,
    pub model: String,
}

// ---- Audit log (GET /v1/audit) ----

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AuditEntry {
    pub id: AuditId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    /// tool_exec | llm_call | approval | config
    pub kind: String,
    pub detail: serde_json::Value,
    #[ts(type = "string")]
    pub ts: DateTime<Utc>,
}

/// Filtros de GET /v1/audit.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AuditQuery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub limit: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AuditPage {
    pub entries: Vec<AuditEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

// ---- Ticket temporal para el WebSocket remoto (POST /v1/ws/ticket) ----

/// El navegador no puede mandar `Authorization` en el handshake WS y el proxy
/// BFF (serverless) no reenvía WebSockets: el cliente remoto pide un ticket
/// efímero por REST (autenticado vía BFF) y abre el WS directo al daemon con
/// `?ticket=`. Un solo uso, caduca en `expires_in_s` segundos.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct WsTicketResponse {
    pub ticket: String,
    pub expires_in_s: u32,
}
