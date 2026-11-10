//! The session token: 32 bytes from the OS random number generator,
//! base64url-encoded without padding (43 characters). It is a secret: its
//! `Debug` is redacted and it is never logged.

use std::fmt;

use subtle::ConstantTimeEq;

/// Number of random bytes in a token.
pub const TOKEN_BYTES: usize = 32;

#[derive(Clone, PartialEq, Eq)]
pub struct Token(String);

impl Token {
    /// A fresh token from the OS RNG.
    pub fn generate() -> Result<Self, getrandom::Error> {
        let mut bytes = [0u8; TOKEN_BYTES];
        getrandom::fill(&mut bytes)?;
        Ok(Self(base64url(&bytes)))
    }

    /// Wraps an existing token (from a runtime file).
    pub fn from_string(token: String) -> Self {
        Self(token)
    }

    /// The token text. Handle with care: never log it.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Compares `candidate` with this token in constant time (for equal
    /// lengths; the length of a token is not a secret).
    pub fn verify(&self, candidate: &str) -> bool {
        self.0.as_bytes().ct_eq(candidate.as_bytes()).into()
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token(<redacted>)")
    }
}

/// RFC 4648 §5 base64url, no padding.
pub fn base64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        let chars = chunk.len() + 1;
        for i in 0..chars {
            let index = (n >> (18 - 6 * i)) & 0x3f;
            out.push(char::from(ALPHABET[index as usize]));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64url_matches_rfc4648_vectors() {
        // RFC 4648 §10, unpadded, plus the two url-safe characters.
        assert_eq!(base64url(b""), "");
        assert_eq!(base64url(b"f"), "Zg");
        assert_eq!(base64url(b"fo"), "Zm8");
        assert_eq!(base64url(b"foo"), "Zm9v");
        assert_eq!(base64url(b"foob"), "Zm9vYg");
        assert_eq!(base64url(b"fooba"), "Zm9vYmE");
        assert_eq!(base64url(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64url(&[0xfb, 0xff]), "-_8");
    }

    #[test]
    fn tokens_are_43_url_safe_chars_and_differ() {
        let a = Token::generate().unwrap();
        let b = Token::generate().unwrap();
        assert_eq!(a.expose().len(), 43);
        assert!(
            a.expose()
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
        );
        assert_ne!(a, b);
    }

    #[test]
    fn verify_and_redaction() {
        let t = Token::from_string("secret-token".into());
        assert!(t.verify("secret-token"));
        assert!(!t.verify("secret-tokeN"));
        assert!(!t.verify("secret"));
        assert!(!t.verify(""));
        assert_eq!(format!("{t:?}"), "Token(<redacted>)");
    }
}
