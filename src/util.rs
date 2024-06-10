use thiserror::Error;

pub type Result<T> = std::result::Result<T, AdbError>;

#[derive(Error, Debug)]
pub enum AdbError {
    #[error(transparent)]
    IOError(#[from] std::io::Error),
    #[error(transparent)]
    Utf8StringError(#[from] std::str::Utf8Error),
    #[error(transparent)]
    ParseIntError(#[from] std::num::ParseIntError),
    #[error("FAILED response status: {0}")]
    FailedResponseStatus(String),
    #[error("Unknown response status: {0}")]
    UnknownResponseStatus(String),
    #[error(transparent)]
    AddrParseError(#[from] std::net::AddrParseError),
    #[error(transparent)]
    TcpReuniteError(#[from] tokio::net::tcp::ReuniteError),
    #[error(transparent)]
    UnixReuniteError(#[from] tokio::net::unix::ReuniteError),
}
