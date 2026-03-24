use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Database error: {0}")]
    Database(#[from] rusqlite::Error),

    #[error("Remote database error: {0}")]
    RemoteDatabase(#[from] libsql::Error),

    #[error("Keystore error: {0}")]
    Keystore(String),

    #[error("Crypto error: {0}")]
    Crypto(String),

    #[error("Signal error: {0}")]
    Signal(String),

    #[error("Config error: {0}")]
    Config(String),

    #[error("Not initialized: identity key not found")]
    NotInitialized,

    #[error("Network error: {0}")]
    Network(#[from] reqwest::Error),

    #[error("Serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("client_outdated")]
    ClientOutdated,

    #[error("{0}")]
    Other(#[from] anyhow::Error),
}

// Tauri requires commands return serializable errors
impl Serialize for Error {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;
