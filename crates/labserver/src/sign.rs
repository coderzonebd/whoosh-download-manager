//! Expiring signed URLs.
//!
//! A signed path is `<path>?expires=<unix seconds>&sig=<hex>` where
//! `sig = HMAC-SHA256(secret, "<path>\n<expires>")` and `<path>` is the
//! canonical `/s/<scenario>/<file>` (redirect hops keep the query string, so
//! one signature is valid at every hop).

use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::content::hex;

/// HMAC-SHA256 (RFC 2104).
pub fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    const B: usize = 64;
    let mut k = [0u8; B];
    if key.len() > B {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; B];
    let mut opad = [0x5cu8; B];
    for i in 0..B {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }
    let inner = Sha256::new()
        .chain_update(ipad)
        .chain_update(msg)
        .finalize();
    Sha256::new()
        .chain_update(opad)
        .chain_update(inner)
        .finalize()
        .into()
}

/// The `sig` value for `path` expiring at `expires`.
pub fn signature(secret: &str, path: &str, expires: u64) -> String {
    hex(&hmac_sha256(
        secret.as_bytes(),
        format!("{path}\n{expires}").as_bytes(),
    ))
}

/// `path?expires=…&sig=…`.
pub fn signed_path(secret: &str, path: &str, expires: u64) -> String {
    format!(
        "{path}?expires={expires}&sig={}",
        signature(secret, path, expires)
    )
}

/// Seconds since the Unix epoch.
pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Outcome of checking a request's signature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SigCheck {
    Valid,
    Missing,
    Invalid,
    Expired,
}

/// Checks `query` (without the `?`) against `path` at time `now`.
pub fn check(secret: &str, path: &str, query: Option<&str>, now: u64) -> SigCheck {
    let (mut expires, mut sig) = (None, None);
    for pair in query.unwrap_or("").split('&') {
        match pair.split_once('=') {
            Some(("expires", v)) => expires = Some(v),
            Some(("sig", v)) => sig = Some(v),
            _ => {}
        }
    }
    let (Some(expires), Some(sig)) = (expires, sig) else {
        return SigCheck::Missing;
    };
    let Ok(exp) = expires.parse::<u64>() else {
        return SigCheck::Invalid;
    };
    let want = signature(secret, path, exp);
    // Constant-time compare; not that a test server needs it.
    let ok = want.len() == sig.len()
        && want
            .bytes()
            .zip(sig.bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0;
    if !ok {
        SigCheck::Invalid
    } else if now > exp {
        SigCheck::Expired
    } else {
        SigCheck::Valid
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc4231_case_2() {
        assert_eq!(
            hex(&hmac_sha256(b"Jefe", b"what do ya want for nothing?")),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn rfc4231_long_key() {
        // Case 6: 131-byte key, hashed first.
        assert_eq!(
            hex(&hmac_sha256(
                &[0xaa; 131],
                b"Test Using Larger Than Block-Size Key - Hash Key First"
            )),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }

    #[test]
    fn round_trip_and_failures() {
        let p = signed_path("k", "/s/x/a.bin", 100);
        let q = p.split_once('?').unwrap().1;
        assert_eq!(check("k", "/s/x/a.bin", Some(q), 100), SigCheck::Valid);
        assert_eq!(check("k", "/s/x/a.bin", Some(q), 101), SigCheck::Expired);
        assert_eq!(check("k", "/s/x/b.bin", Some(q), 50), SigCheck::Invalid);
        assert_eq!(check("other", "/s/x/a.bin", Some(q), 50), SigCheck::Invalid);
        let tampered = q.replace("expires=100", "expires=999");
        assert_eq!(
            check("k", "/s/x/a.bin", Some(&tampered), 50),
            SigCheck::Invalid
        );
        assert_eq!(check("k", "/s/x/a.bin", None, 50), SigCheck::Missing);
        assert_eq!(
            check("k", "/s/x/a.bin", Some("expires=1"), 0),
            SigCheck::Missing
        );
    }
}
