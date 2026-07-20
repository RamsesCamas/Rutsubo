//! Agent loop (RF-06): construir contexto → `provider.generate()` → stream →
//! (delta ⇒ evento | tool_call ⇒ compuerta) → ejecutar → anexar resultado →
//! iterar, con tope `max_iterations`.
//!
//! El rechazo de una aprobación **no aborta** el turno: se anexa como
//! resultado de herramienta y el modelo decide el siguiente paso.

pub mod context;

use crate::llm::{ChatMessage, GenerationRequest, ProviderError, StreamItem, ToolCallRequest};
use crate::state::App;
use crate::store;
use crate::tools::{ToolCtx, ToolResult};
use chrono::Utc;
use futures::StreamExt;
use rutsubo_core::events::{Decision, Event, SessionState, StopReason, Usage};
use rutsubo_core::ids::{ApprovalId, MessageId, SessionId};
use serde_json::json;
use std::path::PathBuf;
use tokio_util::sync::CancellationToken;

/// Arranca el turno agéntico de la sesión si no hay uno en curso (RF-16:
/// cada sesión se suspende sola; las demás siguen atendiéndose).
pub fn ensure_running(app: App, session_id: SessionId) {
    {
        let mut running = app.running.lock().expect("running set");
        if !running.insert(session_id) {
            return; // ya hay un turno en curso; el loop verá el mensaje nuevo
        }
    }
    tokio::spawn(async move {
        if let Err(err) = run_turn(&app, session_id).await {
            tracing::error!(session = %session_id, error = %err, "turno abortado");
        }
        app.running.lock().expect("running set").remove(&session_id);
    });
}

async fn run_turn(app: &App, session_id: SessionId) -> Result<(), Box<dyn std::error::Error>> {
    let Some(row) = store::sessions::get(&app.pool, &session_id).await? else {
        return Ok(());
    };
    // Remoto (web): el FS de Railway es efímero. Antes de correr el turno,
    // rehidratar el workspace temporal desde Postgres (fuente de verdad de los
    // archivos), por si el contenedor reinició. Best-effort. Además listamos
    // los archivos para inyectarlos al contexto: el agente no tiene una
    // herramienta de "listar", así que sin esto no sabría qué archivos hay
    // (subidos/generados) y respondería que no existen.
    let mut workspace_files: Vec<String> = Vec::new();
    if let Some(pool) = &app.remote_auth {
        let workspace = std::path::Path::new(&row.workspace_path);
        let _ = tokio::fs::create_dir_all(workspace).await;
        if let Err(err) = store::files::rehydrate(pool, &session_id, workspace).await {
            tracing::warn!(%err, "no se pudo rehidratar el workspace desde Postgres");
        }
        if let Ok(files) = store::files::list(pool, &session_id).await {
            workspace_files = files.into_iter().map(|f| f.path).collect();
        }
    }
    let ctx = ToolCtx {
        workspace: PathBuf::from(&row.workspace_path),
        session_id,
    };
    let message_id = MessageId::new();
    let cancel = CancellationToken::new();
    let mut turn_msgs: Vec<ChatMessage> = Vec::new();
    let mut collected = String::new();

    for iteration in 0..app.cfg.max_iterations {
        let history = store::messages::history(&app.pool, &session_id).await?;
        let request = GenerationRequest {
            messages: context::build(&ctx.workspace, &workspace_files, &history, &turn_msgs),
            tools: app.tools.schemas(),
            max_tokens: 1024,
            temperature: 0.2,
            cancel: cancel.clone(),
        };

        // Clon del Arc del adapter vigente: si la key se reconfigura a mitad de
        // sesión, esta llamada usa el adapter que estaba activo al empezar.
        let llm = app.llm.read().await.clone();
        let outcome = match llm.generate_with_info(request).await {
            Ok(outcome) => outcome,
            Err(err) => {
                finish_with_error(app, session_id, &err).await?;
                return Ok(());
            }
        };

        // Cambio efectivo de proveedor → evento + audit (C-4 regla 2).
        if let Some(sw) = &outcome.switch {
            app.emit(
                session_id,
                Event::ModelProviderChanged {
                    from: sw.from.clone(),
                    to: sw.to.clone(),
                    trigger: sw.trigger,
                },
                None,
            )
            .await?;
        }
        // RF-22: qué proveedor atendió cada llamada.
        store::audit::insert(
            &app.pool,
            Some(&session_id),
            "llm_call",
            &json!({
                "provider_id": outcome.provider_id.0,
                "iteration": iteration,
                "message_id": message_id.to_string(),
            }),
            Utc::now(),
        )
        .await?;

        let mut stream = outcome.stream;
        let mut pending: Vec<ToolCallRequest> = Vec::new();
        let mut done: Option<(StopReason, Usage)> = None;

        while let Some(item) = stream.next().await {
            match item {
                Ok(StreamItem::Delta(delta)) => {
                    collected.push_str(&delta);
                    app.emit(session_id, Event::MessageDelta { message_id, delta }, None)
                        .await?;
                }
                Ok(StreamItem::ToolCall(tc)) => {
                    pending.push(tc);
                }
                Ok(StreamItem::Done(stop, usage)) => {
                    done = Some((stop, usage));
                }
                Err(ProviderError::Cancelled) => {
                    done = Some((
                        StopReason::Cancelled,
                        Usage {
                            prompt_tokens: 0,
                            completion_tokens: 0,
                        },
                    ));
                    break;
                }
                Err(err) => {
                    // Jamás se empalma otro proveedor a mitad de streaming
                    // (C-4 regla 1): el mensaje termina con error visible.
                    finish_with_error(app, session_id, &err).await?;
                    if !collected.is_empty() {
                        store::messages::insert_assistant(
                            &app.pool,
                            &session_id,
                            &message_id,
                            &collected,
                            Utc::now(),
                        )
                        .await?;
                    }
                    return Ok(());
                }
            }
        }

        if pending.is_empty()
            && let Some((stop_reason, usage)) = done
        {
            return finish_completed(app, session_id, message_id, &collected, stop_reason, usage)
                .await;
        }

        if pending.is_empty() {
            // Stream agotado sin Done: cierre defensivo.
            return finish_completed(
                app,
                session_id,
                message_id,
                &collected,
                StopReason::EndTurn,
                Usage {
                    prompt_tokens: 0,
                    completion_tokens: 0,
                },
            )
            .await;
        }
        turn_msgs.push(ChatMessage {
            role: "assistant".into(),
            content: String::new(),
            tool_calls: pending.clone(),
            tool_call_id: None,
            provider_tool_call_id: None,
        });
        for tc in pending {
            app.emit(
                session_id,
                Event::ToolCallRequested {
                    tool_call_id: tc.tool_call_id,
                    tool: tc.tool.clone(),
                    args: tc.args.clone(),
                },
                None,
            )
            .await?;
            let (result, rejected) = run_gated_tool(app, &ctx, &tc).await?;
            turn_msgs.push(ChatMessage {
                role: "tool".into(), content: json!({ "tool": tc.tool, "tool_call_id": tc.tool_call_id.to_string(), "ok": result.ok, "rejected": rejected, "output": result.output }).to_string(),
                tool_calls: vec![], tool_call_id: Some(tc.tool_call_id), provider_tool_call_id: tc.provider_call_id,
            });
        }
    }

    // Tope de iteraciones (RF-06).
    finish_completed(
        app,
        session_id,
        message_id,
        &collected,
        StopReason::MaxIterations,
        Usage {
            prompt_tokens: 0,
            completion_tokens: 0,
        },
    )
    .await
}

/// Ejecuta la herramienta pasando por la compuerta si aplica (RF-14…RF-17).
/// Devuelve `(resultado, fue_rechazada)`.
async fn run_gated_tool(
    app: &App,
    ctx: &ToolCtx,
    tc: &ToolCallRequest,
) -> Result<(ToolResult, bool), Box<dyn std::error::Error>> {
    let session_id = ctx.session_id;
    let Some(tool) = app.tools.get(&tc.tool) else {
        let result = ToolResult::fail(format!("herramienta desconocida: {}", tc.tool));
        emit_tool_result(app, session_id, tc, &result).await?;
        return Ok((result, false));
    };

    if tool.requires_approval() {
        // TODO(fase-3): evaluación de reglas de auto-aprobación (RF-18)
        // contra la tabla `rules` antes de exigir decisión humana.
        let approval_id = ApprovalId::new();
        let summary = summarize(tc);
        store::approvals::insert(
            &app.pool,
            store::approvals::NewApproval {
                id: &approval_id,
                session_id: &session_id,
                tool_call_id: &tc.tool_call_id,
                tool: &tc.tool,
                summary: &summary,
                args: &tc.args,
                created_at: Utc::now(),
            },
        )
        .await?;
        let decision_rx = app.gate.register(approval_id);
        app.emit(
            session_id,
            Event::ApprovalRequest {
                approval_id,
                tool_call_id: tc.tool_call_id,
                tool: tc.tool.clone(),
                summary,
                args: tc.args.clone(),
                expires_at: None,
            },
            None,
        )
        .await?;
        app.emit(
            session_id,
            Event::SessionState {
                state: SessionState::WaitingApproval,
                title: None,
                reason: None,
            },
            Some(SessionState::WaitingApproval),
        )
        .await?;

        // Suspensión de ESTA sesión (RF-16): la task espera su oneshot sin
        // bloquear a las demás. La primera decisión gana (endpoint/WS).
        let decision = decision_rx.await.unwrap_or(Decision::Reject);

        app.emit(
            session_id,
            Event::SessionState {
                state: SessionState::Running,
                title: None,
                reason: None,
            },
            Some(SessionState::Running),
        )
        .await?;

        if decision == Decision::Reject {
            // El rechazo NO aborta: es un resultado más (diagrama Etapa 1).
            let result = ToolResult::fail("rechazado por el usuario");
            emit_tool_result(app, session_id, tc, &result).await?;
            audit_tool(app, session_id, tc, false, true).await?;
            return Ok((result, true));
        }
    }

    let result = tool.execute(ctx, tc.args.clone()).await;
    emit_tool_result(app, session_id, tc, &result).await?;
    if let Some(diff) = &result.diff {
        app.emit(
            session_id,
            Event::FileDiff {
                tool_call_id: tc.tool_call_id,
                path: diff.path.clone(),
                diff_unified: diff.unified.clone(),
                additions: diff.additions,
                deletions: diff.deletions,
            },
            None,
        )
        .await?;
        // Remoto: persistir el archivo COMPLETO en Postgres (el diff no basta
        // para overwrites/edits). Se relee del workspace tras la escritura.
        // Best-effort: un fallo de Postgres no aborta el turno del agente.
        if let Some(pool) = &app.remote_auth
            && let Ok(target) = rutsubo_core::paths::resolve_within(&ctx.workspace, &diff.path)
        {
            match tokio::fs::read(&target).await {
                Ok(bytes) => {
                    let mime = store::files::guess_mime(&diff.path);
                    if let Err(err) =
                        store::files::upsert(pool, &session_id, &diff.path, &bytes, mime).await
                    {
                        tracing::warn!(%err, path = %diff.path, "no se pudo persistir el archivo en Postgres");
                    }
                }
                Err(err) => {
                    tracing::warn!(%err, path = %diff.path, "no se pudo releer el archivo para persistir")
                }
            }
        }
    }
    audit_tool(app, session_id, tc, result.ok, false).await?;
    Ok((result, false))
}

async fn emit_tool_result(
    app: &App,
    session_id: SessionId,
    tc: &ToolCallRequest,
    result: &ToolResult,
) -> Result<(), crate::store::events::AppendError> {
    app.emit(
        session_id,
        Event::ToolResult {
            tool_call_id: tc.tool_call_id,
            ok: result.ok,
            output_excerpt: result.output.clone(),
            truncated: result.truncated,
        },
        None,
    )
    .await?;
    Ok(())
}

async fn audit_tool(
    app: &App,
    session_id: SessionId,
    tc: &ToolCallRequest,
    ok: bool,
    rejected: bool,
) -> Result<(), sqlx::Error> {
    store::audit::insert(
        &app.pool,
        Some(&session_id),
        "tool_exec",
        &json!({
            "tool": tc.tool,
            "tool_call_id": tc.tool_call_id.to_string(),
            "ok": ok,
            "rejected": rejected,
        }),
        Utc::now(),
    )
    .await
}

async fn finish_completed(
    app: &App,
    session_id: SessionId,
    message_id: MessageId,
    collected: &str,
    stop_reason: StopReason,
    usage: Usage,
) -> Result<(), Box<dyn std::error::Error>> {
    if !collected.is_empty() {
        store::messages::insert_assistant(
            &app.pool,
            &session_id,
            &message_id,
            collected,
            Utc::now(),
        )
        .await?;
    }
    app.emit(
        session_id,
        Event::MessageCompleted {
            message_id,
            stop_reason,
            usage,
        },
        None,
    )
    .await?;
    app.emit(
        session_id,
        Event::SessionState {
            state: SessionState::Idle,
            title: None,
            reason: None,
        },
        Some(SessionState::Idle),
    )
    .await?;
    Ok(())
}

async fn finish_with_error(
    app: &App,
    session_id: SessionId,
    err: &ProviderError,
) -> Result<(), crate::store::events::AppendError> {
    // fatal = true implica transición a idle (C-3).
    app.emit(
        session_id,
        Event::Error {
            code: "provider_failed".into(),
            message: err.to_string(),
            fatal: true,
        },
        None,
    )
    .await?;
    app.emit(
        session_id,
        Event::SessionState {
            state: SessionState::Idle,
            title: None,
            reason: Some("fallo del proveedor de modelo".into()),
        },
        Some(SessionState::Idle),
    )
    .await?;
    Ok(())
}

fn summarize(tc: &ToolCallRequest) -> String {
    match tc.tool.as_str() {
        // Para run_shell el resumen ES el comando (así lo muestra la tarjeta
        // y así se guarda el patrón exacto de la regla estable, C-1).
        "run_shell" => tc
            .args
            .get("cmd")
            .and_then(|v| v.as_str())
            .unwrap_or("run_shell")
            .to_owned(),
        tool => {
            let path = tc.args.get("path").and_then(|v| v.as_str()).unwrap_or("?");
            format!("{tool}: {path}")
        }
    }
}
