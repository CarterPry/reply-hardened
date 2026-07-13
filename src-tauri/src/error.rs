use serde::Serialize;

/// Application error surfaced to the frontend. Serialized as a tagged
/// union so the UI can branch on `kind` - most importantly to render
/// the Full Disk Access prompt instead of a generic crash.
#[derive(Debug, thiserror::Error, Serialize, Clone)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AppError {
    #[error("Full Disk Access required to read iMessage history at {path}")]
    PermissionDenied { path: String },
    #[error("iMessage database not found at {path}")]
    NotFound { path: String },
    #[error("Database error: {message}")]
    Database { message: String },
    #[error("IO error: {message}")]
    Io { message: String },
}

impl From<rusqlite::Error> for AppError {
    fn from(e: rusqlite::Error) -> Self {
        AppError::Database {
            message: e.to_string(),
        }
    }
}

impl From<std::io::Error> for AppError {
    fn from(e: std::io::Error) -> Self {
        AppError::Io {
            message: e.to_string(),
        }
    }
}

pub type AppResult<T> = Result<T, AppError>;
