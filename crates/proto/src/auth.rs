//! Mutual authentication in `session.hello` (PROTOCOL.md §1).
//!
//! The token proves the client to the daemon: only a process that can read
//! `runtime.json` knows it. The *proof* proves the daemon to the client: the
//! client sends a fresh random nonce, and the daemon answers with
//! `HMAC-SHA256(key = token, message = PROOF_CONTEXT ‖ nonce)`. A process that
//! took over the port of a stale `runtime.json` (the daemon crashed, the port
//! was freed) cannot compute it, so the client stops before it sends any
//! request, cookie or header. The daemon generates a new token at every start,
//! so the stale token the impostor sees in the hello is worthless.

use hmac::{Hmac, Mac};
use sha2::Sha256;

/// Domain separation for the proof, so the token's HMAC is never reused for
/// anything else.
pub const PROOF_CONTEXT: &str = "vayu-hello-proof-v1\n";

/// Random bytes in a nonce made by [`new_nonce`] (hex-encoded: 32 characters).
pub const NONCE_BYTES: usize = 16;

/// Bounds on the nonce length, in hex characters (PROTOCOL.md §1).
pub const NONCE_MIN_LEN: usize = 32;
pub const NONCE_MAX_LEN: usize = 128;

type HmacSha256 = Hmac<Sha256>;

/// A fresh nonce from the OS random number generator, lower-case hex.
pub fn new_nonce() -> std::io::Result<String> {
    let mut bytes = [0u8; NONCE_BYTES];
    getrandom::fill(&mut bytes).map_err(|e| std::io::Error::other(e.to_string()))?;
    Ok(to_hex(&bytes))
}

/// Whether `nonce` is acceptable: 32–128 lower-case hex characters.
pub fn is_valid_nonce(nonce: &str) -> bool {
    (NONCE_MIN_LEN..=NONCE_MAX_LEN).contains(&nonce.len())
        && nonce
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn mac(token: &str, nonce: &str) -> HmacSha256 {
    // HMAC accepts keys of any length; this cannot fail.
    let mut mac = HmacSha256::new_from_slice(token.as_bytes()).expect("HMAC takes any key length");
    mac.update(PROOF_CONTEXT.as_bytes());
    mac.update(nonce.as_bytes());
    mac
}

/// The daemon's answer to `nonce`, lower-case hex (64 characters).
pub fn hello_proof(token: &str, nonce: &str) -> String {
    to_hex(&mac(token, nonce).finalize().into_bytes())
}

/// Whether `proof` is the right answer to `nonce` for `token`. Compares in
/// constant time.
pub fn verify_hello_proof(token: &str, nonce: &str, proof: &str) -> bool {
    match from_hex(proof) {
        Some(bytes) => mac(token, nonce).verify_slice(&bytes).is_ok(),
        None => false,
    }
}

fn to_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(DIGITS[usize::from(b >> 4)] as char);
        s.push(DIGITS[usize::from(b & 0xf)] as char);
    }
    s
}

fn from_hex(s: &str) -> Option<Vec<u8>> {
    fn digit(c: u8) -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            _ => None,
        }
    }
    if !s.len().is_multiple_of(2) {
        return None;
    }
    s.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&[hi, lo]| Some(digit(hi)? << 4 | digit(lo)?))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shared with `packages/api-client` (ws.test.ts) so both sides agree.
    /// Computed independently with Python's `hmac` module.
    const VECTOR_TOKEN: &str = "test-token";
    const VECTOR_NONCE: &str = "00112233445566778899aabbccddeeff";
    const VECTOR_PROOF: &str = "e24aa162373347d76ec158c8507ca1a4157a8c521990640275c6d6c50ad548e4";

    #[test]
    fn proof_matches_the_shared_vector() {
        assert_eq!(hello_proof(VECTOR_TOKEN, VECTOR_NONCE), VECTOR_PROOF);
        assert!(verify_hello_proof(VECTOR_TOKEN, VECTOR_NONCE, VECTOR_PROOF));
    }

    #[test]
    fn wrong_token_nonce_or_proof_fails() {
        let proof = hello_proof("t", VECTOR_NONCE);
        assert!(verify_hello_proof("t", VECTOR_NONCE, &proof));
        assert!(!verify_hello_proof("u", VECTOR_NONCE, &proof));
        assert!(!verify_hello_proof("t", &"0".repeat(32), &proof));
        assert!(!verify_hello_proof(
            "t",
            VECTOR_NONCE,
            &proof.to_uppercase()
        ));
        assert!(!verify_hello_proof("t", VECTOR_NONCE, &proof[..62]));
        assert!(!verify_hello_proof("t", VECTOR_NONCE, ""));
    }

    #[test]
    fn nonces_are_fresh_and_valid() {
        let a = new_nonce().unwrap();
        let b = new_nonce().unwrap();
        assert_ne!(a, b);
        assert!(is_valid_nonce(&a));
        assert!(!is_valid_nonce(&"a".repeat(31)));
        assert!(!is_valid_nonce(&"a".repeat(129)));
        assert!(!is_valid_nonce(&"A".repeat(32)));
        assert!(!is_valid_nonce(&"g".repeat(32)));
    }
}
