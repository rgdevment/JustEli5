use thiserror::Error;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("I/O error on {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unexpected format: {0}")]
    Format(String),
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("database schema version {found} is newer than this build knows ({known})")]
    SchemaTooNew { found: i64, known: i64 },
    #[error("import conflict: {0}")]
    Conflict(String),
}

impl StoreError {
    pub fn io(path: impl AsRef<std::path::Path>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.as_ref().display().to_string(),
            source,
        }
    }
}
