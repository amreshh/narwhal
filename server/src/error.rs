use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use narwhal_shared::ErrorBody;
use std::{fmt, io};

/// An error returned to the client as `{"error": "..."}` with a status code.
#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub message: String,
}

pub type ApiResult<T> = Result<T, ApiError>;

impl ApiError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self { status, message: message.into() }
    }
    pub fn bad_request(m: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, m)
    }
    pub fn forbidden(m: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, m)
    }
    pub fn not_found(m: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, m)
    }
    pub fn conflict(m: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, m)
    }
    pub fn internal(m: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, m)
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.message, self.status)
    }
}

impl From<io::Error> for ApiError {
    fn from(e: io::Error) -> Self {
        use io::ErrorKind as K;
        let (status, message) = match e.kind() {
            K::NotFound => (StatusCode::NOT_FOUND, "No such file or folder"),
            K::PermissionDenied => (StatusCode::FORBIDDEN, "Permission denied"),
            K::AlreadyExists => (StatusCode::CONFLICT, "An item with that name already exists"),
            K::NotADirectory => (StatusCode::BAD_REQUEST, "Not a folder"),
            K::IsADirectory => (StatusCode::BAD_REQUEST, "That is a folder"),
            K::DirectoryNotEmpty => (StatusCode::CONFLICT, "The folder is not empty"),
            K::InvalidFilename => (StatusCode::BAD_REQUEST, "Invalid or too long name"),
            K::ReadOnlyFilesystem => (StatusCode::FORBIDDEN, "The storage is read-only"),
            K::StorageFull | K::QuotaExceeded => (StatusCode::INSUFFICIENT_STORAGE, "Not enough free space"),
            K::InvalidInput => (StatusCode::BAD_REQUEST, "Invalid request"),
            _ if crate::confined::errno(&e) == Some(rustix::io::Errno::LOOP) => {
                (StatusCode::BAD_REQUEST, "Too many levels of symbolic links")
            }
            _ => return Self::internal(e.to_string()),
        };
        // Errors made by the server itself carry a more precise message
        // (e.g. "Path is outside the storage root").
        Self::new(status, e.get_ref().map_or_else(|| message.to_string(), |inner| inner.to_string()))
    }
}

impl From<tokio::task::JoinError> for ApiError {
    fn from(e: tokio::task::JoinError) -> Self {
        Self::internal(format!("background task failed: {e}"))
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        if self.status.is_server_error() {
            tracing::error!(status = %self.status, "{}", self.message);
        }
        (self.status, Json(ErrorBody { error: self.message })).into_response()
    }
}
