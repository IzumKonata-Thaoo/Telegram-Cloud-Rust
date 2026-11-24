use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("{0}")]
    BadRequest(String),
    #[error("authentication required")]
    Unauthorized,
    #[error("{0}")]
    Forbidden(String),
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    PayloadTooLarge(String),
    #[error("range not satisfiable")]
    RangeNotSatisfiable { size: u64 },
    #[error("too many requests")]
    RateLimited { retry_after: u64 },
    #[error("storage backend unavailable")]
    Backend(String),
    #[error("internal error")]
    Internal(String),
}

pub type ApiResult<T> = Result<T, ApiError>;

impl ApiError {
    pub fn bad(msg: impl Into<String>) -> Self {
        Self::BadRequest(msg.into())
    }
    pub fn not_found(what: &str) -> Self {
        Self::NotFound(format!("{what} not found"))
    }
    fn code(&self) -> (StatusCode, &'static str) {
        match self {
            Self::BadRequest(_) => (StatusCode::BAD_REQUEST, "bad_request"),
            Self::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized"),
            Self::Forbidden(_) => (StatusCode::FORBIDDEN, "forbidden"),
            Self::NotFound(_) => (StatusCode::NOT_FOUND, "not_found"),
            Self::Conflict(_) => (StatusCode::CONFLICT, "conflict"),
            Self::PayloadTooLarge(_) => (StatusCode::PAYLOAD_TOO_LARGE, "payload_too_large"),
            Self::RangeNotSatisfiable { .. } => (StatusCode::RANGE_NOT_SATISFIABLE, "range_not_satisfiable"),
            Self::RateLimited { .. } => (StatusCode::TOO_MANY_REQUESTS, "rate_limited"),
            Self::Backend(_) => (StatusCode::BAD_GATEWAY, "backend_error"),
            Self::Internal(_) => (StatusCode::INTERNAL_SERVER_ERROR, "internal"),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code) = self.code();
        // Internal and backend details go to the log, never to the client.
        match &self {
            Self::Internal(d) => tracing::error!(detail = %d, "internal error"),
            Self::Backend(d) => tracing::warn!(detail = %d, "backend error"),
            _ => {}
        }
        let mut res = (status, Json(json!({ "error": { "code": code, "message": self.to_string() } }))).into_response();
        match self {
            Self::RangeNotSatisfiable { size } => {
                if let Ok(v) = HeaderValue::from_str(&format!("bytes */{size}")) {
                    res.headers_mut().insert("content-range", v);
                }
            }
            Self::RateLimited { retry_after } => {
                res.headers_mut().insert("retry-after", HeaderValue::from(retry_after));
            }
            Self::Unauthorized => {
                res.headers_mut().insert("www-authenticate", HeaderValue::from_static("Bearer"));
            }
            _ => {}
        }
        res
    }
}

impl From<rusqlite::Error> for ApiError {
    fn from(e: rusqlite::Error) -> Self {
        if let rusqlite::Error::SqliteFailure(f, _) = &e {
            if f.code == rusqlite::ErrorCode::ConstraintViolation {
                return Self::Conflict("resource already exists".into());
            }
        }
        Self::Internal(format!("db: {e}"))
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        Self::Internal(format!("{e:#}"))
    }
}
