//! `whoosh-nmhost`: the native messaging host that lets the browser extension
//! (Pro mode) talk to the whoosh daemon without ever seeing its token.
//!
//! ```text
//! extension ──native messaging (stdio)──▶ whoosh-nmhost ──WebSocket + token──▶ daemon
//! ```
//!
//! See `docs/api/PROTOCOL.md` §1 and ADR 0003.

pub mod detect;
pub mod external;
pub mod framing;
pub mod ids;
pub mod log;
pub mod register;
pub mod relay;
pub mod runtime;

/// The native messaging host name browsers look up.
pub const HOST_NAME: &str = "com.coderzonebd.whoosh";

/// The ID the development build of the Chromium extension gets from the
/// public `key` in `apps/extension/manifest` (the private key is not in the
/// repository). The Chromium zip on GitHub Releases keeps that key, so an
/// unpacked install of it has this ID too. Store builds get their IDs from
/// the stores (`packaging/browsers/extension-ids.json`, [`ids`]).
pub const DEV_CHROMIUM_EXTENSION_ID: &str = "clhjgofaikdmabdnebgcmdffdjenagfl";

/// `browser_specific_settings.gecko.id` of the Firefox build.
pub const FIREFOX_EXTENSION_ID: &str = "whoosh@coderzonebd.com";
