//! Where the daemon publishes its port and token (`docs/api/PROTOCOL.md` §1).
//! Shared with the daemon and the CLI through `whoosh-proto`, so all three
//! always agree on the data directory and the file's checks.

pub use whoosh_proto::runtime::*;
