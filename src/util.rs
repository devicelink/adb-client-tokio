use thiserror::Error;

/// Result type for adb-client-tokio
pub type Result<T> = std::result::Result<T, AdbError>;

/// Error type for adb-client-tokio
#[derive(Error, Debug)]
pub enum AdbError {
    /// Indicates that an error occurred with I/O.
    #[error(transparent)]
    IOError(#[from] std::io::Error),
    /// Indicates that an error occurred during UTF-8 parsing.
    #[error(transparent)]
    Utf8StringError(#[from] std::str::Utf8Error),
    /// Indicates that an error occurred during integer parsing.
    #[error(transparent)]
    ParseIntError(#[from] std::num::ParseIntError),
    /// Indicates that an error occurred with the ADB server.
    #[error("FAILED response status: {0}")]
    FailedResponseStatus(String),
    /// Indicates that an unknown response status was received.
    #[error("Unknown response status: {0}")]
    UnknownResponseStatus(String),
    /// Indicates that an addr couldn;'t be parsed
    #[error(transparent)]
    AddrParseError(#[from] std::net::AddrParseError),
}
