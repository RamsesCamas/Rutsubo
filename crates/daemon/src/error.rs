//! Sobre de error estándar (C-1, sección 1): la única forma de error
//! permitida en toda la API. Prohibido devolver errores con otra forma.

use axum::extract::rejection::JsonRejection;
use axum::extract::{FromRequest, FromRequestParts, Request};
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use rutsubo_core::api::{ErrorBody, ErrorCode, ErrorEnvelope};
use serde::de::DeserializeOwned;
use serde_json::Value;

#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: ErrorCode,
    pub message: String,
    pub details: Option<Value>,
}

impl ApiError {
    pub fn unauthorized() -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            code: ErrorCode::Unauthorized,
            message: "token ausente o inválido".into(),
            details: None,
        }
    }

    pub fn not_found(what: &str) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            code: ErrorCode::NotFound,
            message: format!("{what} no encontrado"),
            details: None,
        }
    }

    pub fn validation(message: impl Into<String>, details: Option<Value>) -> Self {
        Self {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            code: ErrorCode::ValidationFailed,
            message: message.into(),
            details,
        }
    }

    pub fn conflict(message: impl Into<String>, details: Option<Value>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            code: ErrorCode::Conflict,
            message: message.into(),
            details,
        }
    }

    pub fn session_busy(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            code: ErrorCode::SessionBusy,
            message: message.into(),
            details: None,
        }
    }

    /// 500 opaco: el detalle va al log interno, nunca al cliente (sin stack
    /// traces en errores; requisito del corte ZAP).
    pub fn internal(err: impl std::fmt::Display) -> Self {
        tracing::error!(error = %err, "error interno");
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: ErrorCode::Internal,
            message: "error interno".into(),
            details: None,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = ErrorEnvelope {
            error: ErrorBody {
                code: self.code,
                message: self.message,
                details: self.details,
            },
        };
        (self.status, axum::Json(body)).into_response()
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(err: sqlx::Error) -> Self {
        ApiError::internal(err)
    }
}

/// `Json` con rechazo conforme al contrato: cuerpo malformado → 422
/// `validation_failed` (axum devolvería 400/415 con otra forma).
pub struct ApiJson<T>(pub T);

impl<S, T> FromRequest<S> for ApiJson<T>
where
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        match axum::Json::<T>::from_request(req, state).await {
            Ok(axum::Json(value)) => Ok(ApiJson(value)),
            Err(rejection) => Err(match rejection {
                JsonRejection::JsonDataError(e) => ApiError::validation(e.body_text(), None),
                JsonRejection::JsonSyntaxError(e) => ApiError::validation(e.body_text(), None),
                other => ApiError::validation(other.body_text(), None),
            }),
        }
    }
}

/// `Query` con rechazo conforme al contrato (422 `validation_failed`).
pub struct ApiQuery<T>(pub T);

impl<S, T> FromRequestParts<S> for ApiQuery<T>
where
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        match axum::extract::Query::<T>::from_request_parts(parts, state).await {
            Ok(axum::extract::Query(value)) => Ok(ApiQuery(value)),
            Err(rejection) => Err(ApiError::validation(rejection.body_text(), None)),
        }
    }
}
