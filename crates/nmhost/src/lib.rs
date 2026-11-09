//! `vayu-nmhost`: the native messaging host that lets the browser extension
//! (Pro mode) talk to the vayu daemon without ever seeing its token.
//!
//! ```text
//! extension ──native messaging (stdio)──▶ vayu-nmhost ──WebSocket + token──▶ daemon
//! ```
//!
//! See `docs/api/PROTOCOL.md` §1 and ADR 0003.

pub mod framing;
pub mod log;
pub mod register;
pub mod relay;
pub mod runtime;

/// The native messaging host name browsers look up.
pub const HOST_NAME: &str = "com.coderzonebd.vayu";

/// The ID the development build of the Chromium extension gets from the
/// public `key` in `apps/extension/manifest` (the private key is not in the
/// repository). Store builds get their IDs from the stores.
pub const DEV_CHROMIUM_EXTENSION_ID: &str = "clhjgofaikdmabdnebgcmdffdjenagfl";

/// `browser_specific_settings.gecko.id` of the Firefox build.
pub const FIREFOX_EXTENSION_ID: &str = "vayu@coderzonebd.com";
