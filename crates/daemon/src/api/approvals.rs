//! Endpoints de aprobaciones (C-1, RF-14…RF-18).

use crate::error::{ApiError, ApiJson};
use crate::state::App;
use crate::store;
use crate::store::approvals::DecideOutcome;
use axum::Json;
use axum::extract::{Path, State};
use chrono::Utc;
use rutsubo_core::api::{ApprovalsPage, DecisionRequest, DecisionResponse, NewRule};
use rutsubo_core::events::{Decision, Event};
use rutsubo_core::ids::ApprovalId;
use serde_json::json;
use std::str::FromStr;

pub const MAX_REASON_CHARS: usize = 500;

/// Identidad local del decisor (fase sin pairing; C-2 traerá device IDs).
pub const LOCAL_REST_RESOLVER: &str = "local:rest";

/// GET /v1/approvals — pendientes de todas las sesiones, más antiguas primero.
pub async fn list_pending(State(app): State<App>) -> Result<Json<ApprovalsPage>, ApiError> {
    let rows = store::approvals::pending_all(&app.pool).await?;
    Ok(Json(ApprovalsPage {
        approvals: rows.iter().filter_map(|r| r.to_dto()).collect(),
    }))
}

/// POST /v1/approvals/{id}/decision — resolución única (RF-17): la primera
/// decisión gana; repetir la misma decisión devuelve 200 con el registro
/// original; una decisión contraria recibe 409 con la original en `details`.
pub async fn decide(
    State(app): State<App>,
    Path(id): Path<String>,
    ApiJson(req): ApiJson<DecisionRequest>,
) -> Result<Json<DecisionResponse>, ApiError> {
    decide_inner(&app, &id, req, LOCAL_REST_RESOLVER)
        .await
        .map(Json)
}

/// Núcleo compartido REST/WS (Fase D reutiliza con otro `resolved_by`):
/// misma validación, misma semántica.
pub async fn decide_inner(
    app: &App,
    raw_id: &str,
    req: DecisionRequest,
    resolved_by: &str,
) -> Result<DecisionResponse, ApiError> {
    let id = ApprovalId::from_str(raw_id).map_err(|_| ApiError::not_found("aprobación"))?;
    if let Some(reason) = &req.reason
        && reason.chars().count() > MAX_REASON_CHARS
    {
        return Err(ApiError::validation(
            format!("reason supera {MAX_REASON_CHARS} caracteres"),
            Some(json!({"field": "reason"})),
        ));
    }

    let now = Utc::now();
    match store::approvals::decide(&app.pool, &id, req.decision, resolved_by, now).await? {
        DecideOutcome::NotFound => Err(ApiError::not_found("aprobación")),
        DecideOutcome::Applied(row) => {
            let dto = row
                .to_dto()
                .ok_or_else(|| ApiError::internal("fila de aprobación corrupta"))?;

            // Difusión a todos los clientes: retiran la tarjeta aunque no
            // hayan decidido ellos (RF-17).
            app.emit(
                dto.session_id,
                Event::ApprovalResolved {
                    approval_id: id,
                    decision: req.decision,
                    resolved_by: resolved_by.to_owned(),
                },
                None,
            )
            .await
            .map_err(ApiError::internal)?;

            store::audit::insert(
                &app.pool,
                Some(&dto.session_id),
                "approval",
                &json!({
                    "approval_id": id.to_string(),
                    "tool": dto.tool,
                    "decision": store::approvals::decision_to_str(req.decision),
                    "resolved_by": resolved_by,
                    "reason": req.reason,
                }),
                now,
            )
            .await?;

            // Regla estable (RF-18): patrón exacto del comando y workspace
            // actual. La evaluación de reglas es TODO(fase-3).
            if req.remember_rule == Some(true)
                && req.decision == Decision::Approve
                && dto.tool == "run_shell"
                && let Some(session) = store::sessions::get(&app.pool, &dto.session_id).await?
            {
                let pattern = dto
                    .args
                    .get("cmd")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&dto.summary)
                    .to_owned();
                store::rules::insert(
                    &app.pool,
                    &NewRule {
                        workspace_path: session.workspace_path,
                        tool: dto.tool.clone(),
                        pattern,
                    },
                    now,
                )
                .await?;
            }

            // Despierta a la sesión suspendida en la compuerta (RF-16).
            app.gate.resolve(&id, req.decision);

            Ok(DecisionResponse {
                approval_id: id,
                resolved: true,
                decision: req.decision,
                resolved_by: resolved_by.to_owned(),
                resolved_at: now,
            })
        }
        DecideOutcome::AlreadyResolved(row) => {
            let dto = row
                .to_dto()
                .ok_or_else(|| ApiError::internal("fila de aprobación corrupta"))?;
            let original_decision = dto
                .decision
                .ok_or_else(|| ApiError::internal("aprobación resuelta sin decisión"))?;
            if original_decision == req.decision {
                // Idempotencia práctica: misma decisión → registro original.
                Ok(DecisionResponse {
                    approval_id: id,
                    resolved: true,
                    decision: original_decision,
                    resolved_by: dto.resolved_by.unwrap_or_default(),
                    resolved_at: dto.resolved_at.unwrap_or(now),
                })
            } else {
                Err(ApiError::conflict(
                    "la aprobación ya fue resuelta con la decisión contraria",
                    Some(serde_json::to_value(&dto).unwrap_or(json!({}))),
                ))
            }
        }
    }
}
