//! Error types for the crate.

use serde_json::Value;
use thiserror::Error;

/// Convenience alias used throughout the crate.
pub type Result<T> = std::result::Result<T, Error>;

/// The single error type for all operations.
///
/// Mirrors the shape of `playwright-rust`'s error enum, drops the driver-only
/// variants, and adds CDP-specific ones (`CdpError`, `NotImplemented`, `Http`).
#[derive(Debug, Error)]
pub enum Error {
    #[error("browser launch failed: {0}")]
    LaunchFailed(String),

    #[error("connection failed: {0}")]
    ConnectionFailed(String),

    #[error("transport error: {0}")]
    TransportError(String),

    #[error("protocol error: {0}")]
    ProtocolError(String),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Json(#[from] serde_json::Error),

    #[error("timeout: {0}")]
    Timeout(String),

    #[error("navigation to {url} timed out after {duration_ms}ms")]
    NavigationTimeout { url: String, duration_ms: u64 },

    #[error("target closed: {target_type} ({context})")]
    TargetClosed { target_type: String, context: String },

    #[error("element not found: {0}")]
    ElementNotFound(String),

    #[error("channel closed")]
    ChannelClosed,

    #[error("invalid argument: {0}")]
    InvalidArgument(String),

    /// A CDP method returned an error response.
    #[error("CDP error {code}: {message}")]
    CdpError {
        code: i64,
        message: String,
        #[allow(dead_code)]
        data: Option<Value>,
    },

    /// A method that exists for API completeness but is not yet implemented.
    #[error("not yet implemented: {0}")]
    NotImplemented(&'static str),

    #[error("http error: {0}")]
    Http(String),

    /// Wrap an existing error with additional context.
    #[error("{0}: {1}")]
    Context(String, Box<Error>),
}

impl Error {
    /// Attach a human-readable context string, mirroring `playwright-rust`.
    pub fn context(self, msg: impl Into<String>) -> Self {
        Error::Context(msg.into(), Box::new(self))
    }
}
