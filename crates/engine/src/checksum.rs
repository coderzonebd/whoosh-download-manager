//! Verifying `AddParams::checksum` in the `Verifying` state.

use std::io;

use md5::Md5;
use sha1::Sha1;
use sha2::{Digest, Sha256, Sha512};
use vayu_core::Storage;
use vayu_proto::Checksum;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Algo {
    Sha256,
    Sha512,
    Sha1,
    Md5,
}

impl Algo {
    fn digest_len(self) -> usize {
        match self {
            Self::Sha256 => 32,
            Self::Sha512 => 64,
            Self::Sha1 => 20,
            Self::Md5 => 16,
        }
    }
}

/// Validates a requested checksum: a supported algorithm and a hex value of
/// the right length. Returns the algorithm and the expected digest.
pub(crate) fn parse(c: &Checksum) -> Result<(Algo, Vec<u8>), String> {
    let algo = match c.algorithm.to_ascii_lowercase().replace('-', "").as_str() {
        "sha256" => Algo::Sha256,
        "sha512" => Algo::Sha512,
        "sha1" => Algo::Sha1,
        "md5" => Algo::Md5,
        other => return Err(format!("unsupported checksum algorithm {other:?}")),
    };
    let want = hex::decode(c.value.trim())
        .map_err(|_| "checksum value must be hexadecimal".to_string())?;
    if want.len() != algo.digest_len() {
        return Err(format!(
            "a {:?} checksum has {} hex digits",
            algo,
            algo.digest_len() * 2
        ));
    }
    Ok((algo, want))
}

enum Hasher {
    Sha256(Sha256),
    Sha512(Sha512),
    Sha1(Sha1),
    Md5(Md5),
}

impl Hasher {
    fn new(a: Algo) -> Self {
        match a {
            Algo::Sha256 => Self::Sha256(Sha256::new()),
            Algo::Sha512 => Self::Sha512(Sha512::new()),
            Algo::Sha1 => Self::Sha1(Sha1::new()),
            Algo::Md5 => Self::Md5(Md5::new()),
        }
    }

    fn update(&mut self, b: &[u8]) {
        match self {
            Self::Sha256(h) => h.update(b),
            Self::Sha512(h) => h.update(b),
            Self::Sha1(h) => h.update(b),
            Self::Md5(h) => h.update(b),
        }
    }

    fn finish(self) -> Vec<u8> {
        match self {
            Self::Sha256(h) => h.finalize().to_vec(),
            Self::Sha512(h) => h.finalize().to_vec(),
            Self::Sha1(h) => h.finalize().to_vec(),
            Self::Md5(h) => h.finalize().to_vec(),
        }
    }
}

/// Hashes the first `len` bytes of `storage`, reading `chunk` bytes at a
/// time and yielding to the runtime between reads.
pub(crate) async fn digest(
    storage: &dyn Storage,
    len: u64,
    algo: Algo,
    chunk: usize,
) -> io::Result<Vec<u8>> {
    let mut h = Hasher::new(algo);
    let mut at = 0u64;
    while at < len {
        let want = chunk.min((len - at) as usize);
        let b = storage.read_at(at, want).await?;
        if b.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "file is shorter than downloaded",
            ));
        }
        h.update(&b);
        at += b.len() as u64;
        tokio::task::yield_now().await;
    }
    Ok(h.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use vayu_core::testing::MemStorage;

    fn ck(a: &str, v: &str) -> Checksum {
        Checksum {
            algorithm: a.into(),
            value: v.into(),
        }
    }

    #[test]
    fn parse_validates() {
        assert!(parse(&ck("sha256", &"ab".repeat(32))).is_ok());
        assert!(parse(&ck("SHA-512", &"ab".repeat(64))).is_ok());
        assert!(parse(&ck("sha1", &"ab".repeat(20))).is_ok());
        assert!(parse(&ck("md5", &"ab".repeat(16))).is_ok());
        assert!(parse(&ck("crc32", "abcd")).is_err());
        assert!(parse(&ck("sha256", "xyz")).is_err());
        assert!(parse(&ck("sha256", "abcd")).is_err());
    }

    #[tokio::test]
    async fn digests_known_values() {
        let s = MemStorage::default();
        s.write_at(0, Bytes::from_static(b"abc")).await.unwrap();
        let sha = digest(&s, 3, Algo::Sha256, 2).await.unwrap();
        assert_eq!(
            hex::encode(sha),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let md5 = digest(&s, 3, Algo::Md5, 1).await.unwrap();
        assert_eq!(hex::encode(md5), "900150983cd24fb0d6963f7d28e17f72");
        assert!(digest(&s, 10, Algo::Sha1, 4).await.is_err());
    }
}
