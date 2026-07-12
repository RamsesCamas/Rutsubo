//! Sobre de error del contrato C-2: mismo formato que C-1
//! (`{"error": {"code", "message"}}`) con el catálogo cerrado de C-2.

use axum::Json;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Code {
    Unauthorized,
    Forbidden,
    NotFound,
    ValidationFailed,
    PairingExpired,
    RateLimited,
    Internal,
}

#[derive(Debug)]
pub struct RelayError {
    pub code: Code,
    pub message: String,
    /// Solo para `rate_limited`: segundos sugeridos de espera (Retry-After).
    pub retry_after_s: Option<u32>,
}

impl RelayError {
    pub fn unauthorized() -> Self {
        Self::new(Code::Unauthorized, "credenciales inválidas o ausentes")
    }
    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(Code::Forbidden, message)
    }
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(Code::NotFound, message)
    }
    pub fn validation(message: impl Into<String>) -> Self {
        Self::new(Code::ValidationFailed, message)
    }
    pub fn pairing_expired() -> Self {
        Self::new(Code::PairingExpired, "el código expiró o ya fue usado")
    }
    pub fn rate_limited(retry_after_s: u32) -> Self {
        Self {
            code: Code::RateLimited,
            message: "demasiados intentos".into(),
            retry_after_s: Some(retry_after_s),
        }
    }
    pub fn internal(err: impl std::fmt::Display) -> Self {
        tracing::error!(%err, "error interno del relay");
        Self::new(Code::Internal, "error interno")
    }

    fn new(code: Code, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            retry_after_s: None,
        }
    }

    fn status(&self) -> StatusCode {
        match self.code {
            Code::Unauthorized => StatusCode::UNAUTHORIZED,
            Code::Forbidden => StatusCode::FORBIDDEN,
            Code::NotFound => StatusCode::NOT_FOUND,
            Code::ValidationFailed => StatusCode::UNPROCESSABLE_ENTITY,
            Code::PairingExpired => StatusCode::GONE,
            Code::RateLimited => StatusCode::TOO_MANY_REQUESTS,
            Code::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl From<sqlx::Error> for RelayError {
    fn from(err: sqlx::Error) -> Self {
        Self::internal(err)
    }
}

impl IntoResponse for RelayError {
    fn into_response(self) -> Response {
        let body = serde_json::json!({
            "error": { "code": self.code, "message": self.message }
        });
        let mut response = (self.status(), Json(body)).into_response();
        if let Some(seconds) = self.retry_after_s
            && let Ok(value) = HeaderValue::from_str(&seconds.to_string())
        {
            response.headers_mut().insert(header::RETRY_AFTER, value);
        }
        response
    }
}
