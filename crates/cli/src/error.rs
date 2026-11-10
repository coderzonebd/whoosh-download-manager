//! Errors and exit codes. The table is documented in the README and in
//! `vayu --help`; scripts (and the benchmark harness) rely on it.

use vayu_proto::error_code;

pub mod exit {
    pub const OK: u8 = 0;
    pub const OTHER: u8 = 1;
    pub const USAGE: u8 = 2;
    pub const DAEMON_UNAVAILABLE: u8 = 3;
    pub const REJECTED: u8 = 4;
    pub const NOT_FOUND: u8 = 5;
    pub const INVALID_STATE: u8 = 6;
    pub const INVALID_ARGUMENT: u8 = 7;
    pub const NETWORK: u8 = 8;
    pub const IO: u8 = 9;
    pub const UNSUPPORTED: u8 = 10;
    pub const DOWNLOAD_FAILED: u8 = 11;
    pub const LINK_EXPIRED: u8 = 12;
    pub const CHECKSUM_MISMATCH: u8 = 13;
    pub const REMOVED: u8 = 14;
    pub const INTERRUPTED: u8 = 130;
}

pub const EXIT_CODES_HELP: &str = "\
EXIT CODES:
    0    success
    1    other error
    2    usage error (bad option or argument)
    3    the daemon is not running and could not be started, or the
         connection to it was lost
    4    the daemon refused this client (wrong token or protocol version)
    5    no such download (JSON-RPC -32002)
    6    invalid state, e.g. resuming a completed download (-32003)
    7    the daemon rejected an argument (-32602)
    8    network error, e.g. the server says 404 (-32005)
    9    I/O error, e.g. disk full (-32004)
    10   not supported by this daemon or protocol (-32006)
    11   get: the download failed
    12   get: the link expired (the download needs a fresh URL)
    13   get: the file does not match its checksum
    14   get: the download was removed while waiting
    130  interrupted (Ctrl-C)";

#[derive(Debug, thiserror::Error)]
pub enum CliError {
    #[error("{0}")]
    Usage(String),
    #[error("{0}")]
    DaemonUnavailable(String),
    #[error("{0}")]
    Rejected(String),
    #[error("{message}")]
    Rpc { code: i64, message: String },
    #[error("{message}")]
    Download { exit: u8, message: String },
    #[error("{0}")]
    Unsupported(String),
    #[error("interrupted")]
    Interrupted,
    #[error("{0}")]
    Other(String),
}

impl CliError {
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Usage(_) => exit::USAGE,
            Self::DaemonUnavailable(_) => exit::DAEMON_UNAVAILABLE,
            Self::Rejected(_) => exit::REJECTED,
            Self::Rpc { code, .. } => match i32::try_from(*code).unwrap_or(0) {
                error_code::NOT_FOUND => exit::NOT_FOUND,
                error_code::INVALID_STATE => exit::INVALID_STATE,
                -32602 | -32600 => exit::INVALID_ARGUMENT,
                error_code::NETWORK => exit::NETWORK,
                error_code::IO => exit::IO,
                error_code::UNSUPPORTED => exit::UNSUPPORTED,
                error_code::UNAUTHORIZED => exit::REJECTED,
                _ => exit::OTHER,
            },
            Self::Download { exit, .. } => *exit,
            Self::Unsupported(_) => exit::UNSUPPORTED,
            Self::Interrupted => exit::INTERRUPTED,
            Self::Other(_) => exit::OTHER,
        }
    }

    /// For `--json`: `{"error": {"exitCode", "message", "code"?}}`.
    pub fn to_json(&self) -> serde_json::Value {
        let mut error = serde_json::json!({
            "exitCode": self.exit_code(),
            "message": self.to_string(),
        });
        if let Self::Rpc { code, .. } = self {
            error["code"] = (*code).into();
        }
        serde_json::json!({ "error": error })
    }
}

pub type Result<T, E = CliError> = std::result::Result<T, E>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rpc_codes_map_to_exit_codes() {
        let rpc = |code| CliError::Rpc {
            code,
            message: String::new(),
        };
        assert_eq!(rpc(-32002).exit_code(), 5);
        assert_eq!(rpc(-32003).exit_code(), 6);
        assert_eq!(rpc(-32602).exit_code(), 7);
        assert_eq!(rpc(-32005).exit_code(), 8);
        assert_eq!(rpc(-32004).exit_code(), 9);
        assert_eq!(rpc(-32006).exit_code(), 10);
        assert_eq!(rpc(-32601).exit_code(), 1);
        assert_eq!(rpc(-32002).to_json()["error"]["code"], -32002);
        // Every code in the help text is a code we use, and vice versa.
        for code in [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 130] {
            assert!(
                EXIT_CODES_HELP.contains(&format!("\n    {code} ")),
                "{code} undocumented"
            );
        }
    }
}
