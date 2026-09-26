use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use dataset_meta::MetaError;
use dataset_storage::StorageError;
use serde_json::json;

#[derive(Debug)]
pub enum AppError {
    NotFound,
    Unauthorized,
    Forbidden,
    BadRequest(String),
    Conflict(String),
    MissingBlobs(Vec<dataset_core::BlobHash>),
    RangeNotSatisfiable,
    Internal(anyhow::Error),
}

pub type AppResult<T> = Result<T, AppError>;

impl From<MetaError> for AppError {
    fn from(e: MetaError) -> Self {
        match e {
            MetaError::NotFound => AppError::NotFound,
            MetaError::Conflict(m) => AppError::Conflict(m),
            MetaError::Invalid(m) => AppError::BadRequest(m),
            MetaError::MissingBlobs(b) => AppError::MissingBlobs(b),
            MetaError::Db(e) => AppError::Internal(e.into()),
        }
    }
}

impl From<StorageError> for AppError {
    fn from(e: StorageError) -> Self {
        match e {
            StorageError::NotFound => AppError::NotFound,
            StorageError::InvalidRange => AppError::RangeNotSatisfiable,
            e @ (StorageError::HashMismatch { .. } | StorageError::SizeMismatch { .. } | StorageError::Invalid(_)) => {
                AppError::BadRequest(e.to_string())
            }
            StorageError::Io(e) => AppError::Internal(e.into()),
        }
    }
}

impl From<anyhow::Error> for AppError {
    fn from(e: anyhow::Error) -> Self {
        AppError::Internal(e)
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, body) = match self {
            AppError::NotFound => (StatusCode::NOT_FOUND, json!({ "error": "not found" })),
            AppError::Unauthorized => {
                return (
                    StatusCode::UNAUTHORIZED,
                    [(header::WWW_AUTHENTICATE, "Bearer, Basic realm=\"datasets\"")],
                    Json(json!({ "error": "unauthorized" })),
                )
                    .into_response()
            }
            AppError::Forbidden => (StatusCode::FORBIDDEN, json!({ "error": "forbidden" })),
            AppError::BadRequest(m) => (StatusCode::BAD_REQUEST, json!({ "error": m })),
            AppError::Conflict(m) => (StatusCode::CONFLICT, json!({ "error": m })),
            AppError::MissingBlobs(b) => (
                StatusCode::CONFLICT,
                json!({ "error": "missing blobs; upload them first", "missing": b }),
            ),
            AppError::RangeNotSatisfiable => (StatusCode::RANGE_NOT_SATISFIABLE, json!({ "error": "invalid range" })),
            AppError::Internal(e) => {
                tracing::error!("internal error: {e:#}");
                (StatusCode::INTERNAL_SERVER_ERROR, json!({ "error": "internal error" }))
            }
        };
        (status, Json(body)).into_response()
    }
}
