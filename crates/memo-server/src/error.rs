use axum::{http::StatusCode, response::IntoResponse, Json};
use serde::Serialize;

#[derive(Debug, Clone, Copy)]
pub(crate) enum ErrorCode {
    BadRequest,
    Database,
    InvalidPayload,
    UnsupportedProtocol,
}

impl ErrorCode {
    fn as_str(self) -> &'static str {
        match self {
            Self::BadRequest => "bad_request",
            Self::Database => "database_error",
            Self::InvalidPayload => "invalid_payload",
            Self::UnsupportedProtocol => "unsupported_protocol",
        }
    }

    fn status(self) -> StatusCode {
        match self {
            Self::BadRequest | Self::UnsupportedProtocol => StatusCode::BAD_REQUEST,
            Self::Database | Self::InvalidPayload => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct ErrorBody {
    code: &'static str,
    message: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("operation payload error: {0}")]
    Payload(#[from] serde_json::Error),
    #[error("unsupported sync protocol version {0}")]
    UnsupportedProtocol(u16),
    #[error("bad sync request: {0}")]
    BadRequest(String),
}

impl ServerError {
    pub(crate) fn bad_request(message: impl Into<String>) -> Self {
        Self::BadRequest(message.into())
    }

    fn code(&self) -> ErrorCode {
        match self {
            Self::Database(_) => ErrorCode::Database,
            Self::Payload(_) => ErrorCode::InvalidPayload,
            Self::UnsupportedProtocol(_) => ErrorCode::UnsupportedProtocol,
            Self::BadRequest(_) => ErrorCode::BadRequest,
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }
}

impl IntoResponse for ServerError {
    fn into_response(self) -> axum::response::Response {
        let code = self.code();
        let message = self.message();
        if code.status().is_server_error() {
            tracing::error!(error.code = code.as_str(), error.message = %message, "request failed");
        } else {
            tracing::warn!(error.code = code.as_str(), error.message = %message, "request rejected");
        }
        (
            code.status(),
            Json(ErrorBody {
                code: code.as_str(),
                message,
            }),
        )
            .into_response()
    }
}
