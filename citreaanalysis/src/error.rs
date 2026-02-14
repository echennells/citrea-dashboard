use std::fmt;

/// Unified error type for the citrea-dashboard application.
#[derive(Debug)]
pub enum AppError {
    /// Bitcoin Core RPC errors
    BitcoinRpc(bitcoincore_rpc::Error),
    /// HTTP client errors (Citrea RPC, batch explorer)
    Http(reqwest::Error),
    /// JSON serialization/deserialization errors
    Json(serde_json::Error),
    /// SQLite database errors
    Db(rusqlite::Error),
    /// Integer parsing errors (hex values from RPCs)
    ParseInt(std::num::ParseIntError),
    /// File I/O errors
    Io(std::io::Error),
    /// Citrea commitment decode errors
    Decode(crate::citrea_decoder::DecodeError),
    /// Signal handler errors
    Ctrlc(ctrlc::Error),
    /// Application-specific errors with context
    App(String),
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AppError::BitcoinRpc(e) => write!(f, "Bitcoin RPC: {}", e),
            AppError::Http(e) => write!(f, "HTTP: {}", e),
            AppError::Json(e) => write!(f, "JSON: {}", e),
            AppError::Db(e) => write!(f, "Database: {}", e),
            AppError::ParseInt(e) => write!(f, "Parse: {}", e),
            AppError::Io(e) => write!(f, "I/O: {}", e),
            AppError::Decode(e) => write!(f, "Decode: {}", e),
            AppError::Ctrlc(e) => write!(f, "Signal handler: {}", e),
            AppError::App(msg) => write!(f, "{}", msg),
        }
    }
}

impl std::error::Error for AppError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            AppError::BitcoinRpc(e) => Some(e),
            AppError::Http(e) => Some(e),
            AppError::Json(e) => Some(e),
            AppError::Db(e) => Some(e),
            AppError::ParseInt(e) => Some(e),
            AppError::Io(e) => Some(e),
            AppError::Decode(e) => Some(e),
            AppError::Ctrlc(e) => Some(e),
            AppError::App(_) => None,
        }
    }
}

impl From<bitcoincore_rpc::Error> for AppError {
    fn from(e: bitcoincore_rpc::Error) -> Self { AppError::BitcoinRpc(e) }
}
impl From<reqwest::Error> for AppError {
    fn from(e: reqwest::Error) -> Self { AppError::Http(e) }
}
impl From<serde_json::Error> for AppError {
    fn from(e: serde_json::Error) -> Self { AppError::Json(e) }
}
impl From<rusqlite::Error> for AppError {
    fn from(e: rusqlite::Error) -> Self { AppError::Db(e) }
}
impl From<std::num::ParseIntError> for AppError {
    fn from(e: std::num::ParseIntError) -> Self { AppError::ParseInt(e) }
}
impl From<std::io::Error> for AppError {
    fn from(e: std::io::Error) -> Self { AppError::Io(e) }
}
impl From<crate::citrea_decoder::DecodeError> for AppError {
    fn from(e: crate::citrea_decoder::DecodeError) -> Self { AppError::Decode(e) }
}
impl From<ctrlc::Error> for AppError {
    fn from(e: ctrlc::Error) -> Self { AppError::Ctrlc(e) }
}
impl From<String> for AppError {
    fn from(s: String) -> Self { AppError::App(s) }
}
impl From<&str> for AppError {
    fn from(s: &str) -> Self { AppError::App(s.to_string()) }
}

/// Convenience alias used throughout the crate.
pub type Result<T> = std::result::Result<T, AppError>;
