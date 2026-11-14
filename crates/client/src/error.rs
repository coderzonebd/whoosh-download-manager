//! What can go wrong between a client and the daemon.

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// No daemon, it could not be started, or the connection was lost.
    #[error("{0}")]
    DaemonUnavailable(String),
    /// A daemon answered and refused this client (token or protocol version).
    #[error("{0}")]
    Rejected(String),
    /// A JSON-RPC error answer.
    #[error("{message}")]
    Rpc { code: i64, message: String },
    #[error("{0}")]
    Other(String),
}

pub type Result<T, E = ClientError> = std::result::Result<T, E>;
