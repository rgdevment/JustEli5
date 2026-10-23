use thiserror::Error;

#[derive(Debug, Error)]
pub enum MemoryError {
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("memory schema version {found} is newer than this build knows ({known})")]
    SchemaTooNew { found: i64, known: i64 },
    #[error("no entry {0} in the memory")]
    Missing(i64),
    #[error("{0}")]
    Invalid(String),
}
