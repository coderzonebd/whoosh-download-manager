//! Virtual file content: every byte is a pure function of `(seed, offset)`.
//!
//! Byte `o` of a file is byte `o % 8` (little-endian) of the 64-bit word
//! `splitmix64(seed, o / 8)`. Nothing is stored on disk, any size costs
//! nothing, and any range can be regenerated to verify what a client
//! received. Serving goes through [`BlockCache`], which keeps recently used
//! 64 KiB blocks as shared [`Bytes`] so hot blocks are handed to the socket
//! without copying.

use bytes::Bytes;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use vayu_core::ByteRange;

use crate::lock;

/// Generation and cache granularity.
pub const BLOCK_SIZE: usize = 64 * 1024;

const GOLDEN: u64 = 0x9E37_79B9_7F4A_7C15;

#[inline(always)]
fn finalize(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// The 64-bit word at index `idx` (bytes `8*idx .. 8*idx+8`) of file `seed`.
#[inline(always)]
pub fn word(seed: u64, idx: u64) -> u64 {
    finalize(seed.wrapping_add(idx.wrapping_add(1).wrapping_mul(GOLDEN)))
}

/// A well-mixed 64-bit hash of `v` (SplitMix64 finalizer).
pub fn mix64(v: u64) -> u64 {
    finalize(v.wrapping_add(GOLDEN))
}

/// Seed for a file that has no explicit seed: a hash of its name, so the same
/// name has the same bytes in every scenario.
pub fn seed_for_name(name: &str) -> u64 {
    // FNV-1a, then mixed.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in name.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    mix64(h)
}

/// Seed of version `version` of a file whose base seed is `seed`. Version 0
/// is the base seed itself.
pub fn seed_for_version(seed: u64, version: u64) -> u64 {
    if version == 0 {
        seed
    } else {
        mix64(seed ^ mix64(version))
    }
}

/// Writes the bytes of file `seed` starting at `offset` into `out`.
pub fn fill(seed: u64, offset: u64, out: &mut [u8]) {
    let mut pos = offset;
    let mut rest = out;
    // Unaligned head.
    let head = ((8 - (pos % 8)) % 8) as usize;
    if head > 0 {
        let n = head.min(rest.len());
        let w = word(seed, pos / 8).to_le_bytes();
        let skip = (pos % 8) as usize;
        rest[..n].copy_from_slice(&w[skip..skip + n]);
        rest = &mut rest[n..];
        pos += n as u64;
    }
    // Aligned middle.
    let mut idx = pos / 8;
    let (words, tail) = rest.as_chunks_mut::<8>();
    for c in words {
        *c = word(seed, idx).to_le_bytes();
        idx += 1;
    }
    // Tail.
    if !tail.is_empty() {
        let w = word(seed, idx).to_le_bytes();
        let n = tail.len();
        tail.copy_from_slice(&w[..n]);
    }
}

/// Generates block number `block` (64 KiB, aligned) of file `seed`.
pub fn generate_block(seed: u64, block: u64) -> Bytes {
    // Appending to an uninitialised buffer is ~60% faster than zeroing a
    // buffer and then overwriting it.
    const WORDS: u64 = (BLOCK_SIZE / 8) as u64;
    let base = block * WORDS;
    let mut buf = Vec::with_capacity(BLOCK_SIZE);
    for k in 0..WORDS {
        buf.extend_from_slice(&word(seed, base + k).to_le_bytes());
    }
    Bytes::from(buf)
}

/// Lower-case hex.
pub fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(DIGITS[(b >> 4) as usize] as char);
        s.push(DIGITS[(b & 15) as usize] as char);
    }
    s
}

/// The expected content of one version of a virtual file.
///
/// ```
/// use vayu_core::ByteRange;
/// use vayu_labserver::VirtualFile;
///
/// let f = VirtualFile::named("1m.bin", 1 << 20);
/// let head = f.bytes(ByteRange::new(0, 16).unwrap());
/// assert_eq!(f.verify(0, &head), Ok(()));
/// assert_eq!(f.sha256_hex(f.full()).len(), 64);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VirtualFile {
    pub seed: u64,
    pub size: u64,
}

impl VirtualFile {
    pub fn new(seed: u64, size: u64) -> Self {
        Self { seed, size }
    }

    /// The file a scenario serves under `name` when the spec sets no seed.
    pub fn named(name: &str, size: u64) -> Self {
        Self::new(seed_for_name(name), size)
    }

    /// The same file after `version` content changes.
    pub fn version(&self, version: u64) -> Self {
        Self::new(seed_for_version(self.seed, version), self.size)
    }

    /// The whole file as a range.
    pub fn full(&self) -> ByteRange {
        ByteRange {
            start: 0,
            end: self.size,
        }
    }

    fn clamp(&self, r: ByteRange) -> ByteRange {
        let end = r.end.min(self.size);
        ByteRange {
            start: r.start.min(end),
            end,
        }
    }

    /// Expected bytes of `range` (clamped to the file size).
    pub fn bytes(&self, range: ByteRange) -> Vec<u8> {
        let r = self.clamp(range);
        let mut v = vec![0; r.len() as usize];
        fill(self.seed, r.start, &mut v);
        v
    }

    /// SHA-256 of `range` (clamped to the file size).
    pub fn sha256(&self, range: ByteRange) -> [u8; 32] {
        let r = self.clamp(range);
        let mut h = Sha256::new();
        let mut buf = vec![0u8; BLOCK_SIZE];
        let mut pos = r.start;
        while pos < r.end {
            let n = ((r.end - pos) as usize).min(BLOCK_SIZE);
            fill(self.seed, pos, &mut buf[..n]);
            h.update(&buf[..n]);
            pos += n as u64;
        }
        h.finalize().into()
    }

    /// Hex SHA-256 of `range`.
    pub fn sha256_hex(&self, range: ByteRange) -> String {
        hex(&self.sha256(range))
    }

    /// Checks that `data` is exactly the file's content at `offset`.
    /// `Err` carries the absolute offset of the first wrong byte (or of the
    /// first byte past the end of the file).
    pub fn verify(&self, offset: u64, data: &[u8]) -> Result<(), u64> {
        let mut buf = [0u8; 4096];
        let mut pos = offset;
        for chunk in data.chunks(buf.len()) {
            let expected = &mut buf[..chunk.len()];
            fill(self.seed, pos, expected);
            if let Some(i) = chunk.iter().zip(expected.iter()).position(|(a, b)| a != b) {
                return Err(pos + i as u64);
            }
            pos += chunk.len() as u64;
        }
        if pos > self.size {
            return Err(self.size.max(offset));
        }
        Ok(())
    }
}

const SHARDS: usize = 16;

/// A sharded FIFO cache of generated blocks, keyed by `(seed, block)`.
#[derive(Debug)]
pub struct BlockCache {
    shards: Box<[Mutex<Shard>]>,
    per_shard: usize,
}

#[derive(Debug, Default)]
struct Shard {
    map: HashMap<(u64, u64), Bytes>,
    order: VecDeque<(u64, u64)>,
}

impl BlockCache {
    /// A cache holding at most `capacity_bytes` of blocks (0 disables it).
    pub fn new(capacity_bytes: usize) -> Self {
        let blocks = capacity_bytes / BLOCK_SIZE;
        let per_shard = blocks.div_ceil(SHARDS);
        let shards = (0..SHARDS).map(|_| Mutex::default()).collect();
        Self { shards, per_shard }
    }

    /// Block `block` of file `seed`, generated on a miss.
    pub fn get(&self, seed: u64, block: u64) -> Bytes {
        if self.per_shard == 0 {
            return generate_block(seed, block);
        }
        let key = (seed, block);
        let shard = &self.shards[(mix64(seed ^ block) as usize) % SHARDS];
        if let Some(b) = lock(shard).map.get(&key) {
            return b.clone();
        }
        let b = generate_block(seed, block);
        let mut s = lock(shard);
        if s.map.insert(key, b.clone()).is_none() {
            s.order.push_back(key);
            while s.order.len() > self.per_shard {
                if let Some(old) = s.order.pop_front() {
                    s.map.remove(&old);
                }
            }
        }
        b
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_is_offset_consistent() {
        let seed = seed_for_name("x.bin");
        let mut whole = vec![0u8; 1000];
        fill(seed, 0, &mut whole);
        for start in [0usize, 1, 7, 8, 9, 13, 500] {
            for len in [0usize, 1, 3, 8, 17, 200] {
                let mut part = vec![0u8; len];
                fill(seed, start as u64, &mut part);
                assert_eq!(part, whole[start..start + len], "start {start} len {len}");
            }
        }
    }

    #[test]
    fn blocks_match_fill() {
        let seed = 42;
        let b = generate_block(seed, 3);
        let mut exp = vec![0u8; BLOCK_SIZE];
        fill(seed, 3 * BLOCK_SIZE as u64, &mut exp);
        assert_eq!(&b[..], &exp[..]);
    }

    #[test]
    fn seeds_and_versions_differ() {
        assert_ne!(seed_for_name("a"), seed_for_name("b"));
        let s = seed_for_name("a");
        assert_eq!(seed_for_version(s, 0), s);
        assert_ne!(seed_for_version(s, 1), s);
        assert_ne!(seed_for_version(s, 1), seed_for_version(s, 2));
    }

    #[test]
    fn verify_finds_first_bad_byte() {
        let f = VirtualFile::named("v.bin", 10_000);
        let mut d = f.bytes(ByteRange::new(100, 9000).unwrap());
        assert_eq!(f.verify(100, &d), Ok(()));
        d[4321] ^= 1;
        assert_eq!(f.verify(100, &d), Err(4421));
        // Past the end.
        let tail = VirtualFile::new(f.seed, 20_000).bytes(ByteRange::new(9990, 10_010).unwrap());
        assert_eq!(f.verify(9990, &tail), Err(10_000));
    }

    #[test]
    fn content_does_not_look_trivial() {
        // A crude check that the generator is not obviously patterned:
        // every byte value appears, and counts are roughly uniform.
        let f = VirtualFile::named("stat.bin", 1 << 20);
        let d = f.bytes(f.full());
        let mut counts = [0u32; 256];
        for b in &d {
            counts[*b as usize] += 1;
        }
        let (min, max) = (counts.iter().min().unwrap(), counts.iter().max().unwrap());
        assert!(*min > 3600 && *max < 4600, "min {min} max {max}");
    }

    #[test]
    fn cache_evicts_and_returns_same_bytes() {
        let c = BlockCache::new(4 * BLOCK_SIZE * SHARDS);
        let a = c.get(1, 0);
        for i in 0..1000 {
            c.get(1, i);
        }
        assert_eq!(c.get(1, 0), a);
        let total: usize = c.shards.iter().map(|s| lock(s).map.len()).sum();
        assert!(total <= 4 * SHARDS);
        assert_eq!(BlockCache::new(0).get(1, 0), a);
    }

    #[test]
    fn sha256_known_answer_for_empty() {
        assert_eq!(
            VirtualFile::new(1, 0).sha256_hex(ByteRange::new(0, 0).unwrap()),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}
