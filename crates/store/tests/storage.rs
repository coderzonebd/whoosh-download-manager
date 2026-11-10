//! `FileStorage` against the `Storage` contract, with the in-memory double
//! from `vayu_core::testing` as the reference model.

use std::io;
use std::path::Path;
use std::sync::Arc;

use bytes::Bytes;
use vayu_core::testing::MemStorageProvider;
use vayu_core::{Storage, StorageProvider};
use vayu_store::{FileStorageProvider, StorageOptions};

fn provider() -> FileStorageProvider {
    FileStorageProvider::new(StorageOptions::default())
}

/// Deterministic xorshift, so failures reproduce.
struct Rng(u64);
impl Rng {
    fn next(&mut self, m: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % m
    }
}

fn same(a: &Arc<dyn Storage>, b: &Arc<dyn Storage>) -> bool {
    std::ptr::addr_eq(Arc::as_ptr(a), Arc::as_ptr(b))
}

#[tokio::test]
async fn writes_reads_flush_and_finish() {
    let d = tempfile::tempdir().unwrap();
    let p = provider();
    let part = d.path().join("sub/dir/file.bin.vayu-part");
    let s = p.open(&part).await.unwrap();
    s.write_at(3, Bytes::from_static(b"lo, world"))
        .await
        .unwrap();
    s.write_at(0, Bytes::from_static(b"hel")).await.unwrap();
    // Seen before any flush.
    assert_eq!(s.read_at(0, 100).await.unwrap(), "hello, world");
    assert_eq!(s.read_at(7, 3).await.unwrap(), "wor");
    assert_eq!(s.read_at(50, 3).await.unwrap(), "");
    s.write_at(5, Bytes::new()).await.unwrap();
    s.flush().await.unwrap();

    // The final name is taken: AlreadyExists, nothing replaced, and the
    // storage stays usable for another name.
    let taken = d.path().join("sub/dir/file.bin");
    std::fs::write(&taken, b"someone else's").unwrap();
    let e = s.finish(&taken).await.unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(std::fs::read(&taken).unwrap(), b"someone else's");
    s.write_at(12, Bytes::from_static(b"!")).await.unwrap();

    let other = d.path().join("elsewhere/file (1).bin");
    s.finish(&other).await.unwrap();
    assert_eq!(std::fs::read(&other).unwrap(), b"hello, world!");
    assert!(!part.exists());
    assert_eq!(std::fs::read(&taken).unwrap(), b"someone else's");

    // Unusable afterwards.
    assert!(s.write_at(0, Bytes::from_static(b"x")).await.is_err());
    assert!(s.read_at(0, 1).await.is_err());
    assert!(s.flush().await.is_err());
    assert!(s.finish(&d.path().join("again")).await.is_err());

    // The same in-progress name can be used again (a new download of the
    // same file) while the finished storage is still held.
    let again = p.open(&part).await.unwrap();
    assert!(!same(&s, &again));
    assert_eq!(again.read_at(0, 10).await.unwrap(), "");
    again.write_at(0, Bytes::from_static(b"new")).await.unwrap();
    again.finish(&d.path().join("new")).await.unwrap();
    assert_eq!(std::fs::read(d.path().join("new")).unwrap(), b"new");
    assert_eq!(std::fs::read(&other).unwrap(), b"hello, world!");
}

#[tokio::test]
async fn rejects_offsets_past_u64() {
    let d = tempfile::tempdir().unwrap();
    let s = provider().open(&d.path().join("f")).await.unwrap();
    let e = s
        .write_at(u64::MAX - 1, Bytes::from_static(b"abc"))
        .await
        .unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
}

#[tokio::test]
async fn open_returns_the_live_storage_and_resumes_after_drop() {
    let d = tempfile::tempdir().unwrap();
    let p = provider();
    let path = d.path().join("f.vayu-part");
    let a = p.open(&path).await.unwrap();
    let b = p.open(&path).await.unwrap();
    assert!(same(&a, &b));
    drop(b);
    // Dropped without a flush: the buffer is still written out before the
    // path can be opened again.
    let big = Bytes::from(vec![0x5a; 8 << 20]);
    a.write_at(0, big.clone()).await.unwrap();
    a.write_at(8 << 20, Bytes::from_static(b"tail"))
        .await
        .unwrap();
    drop(a);
    let c = p.open(&path).await.unwrap();
    assert_eq!(c.read_at(8 << 20, 10).await.unwrap(), "tail");
    assert_eq!(c.read_at(0, 8 << 20).await.unwrap(), big);

    // A different provider (a restart) keeps existing contents too.
    drop(c);
    let fresh = provider();
    let r = fresh.open(&path).await.unwrap();
    assert_eq!(r.read_at((8 << 20) - 2, 6).await.unwrap(), "ZZtail");
}

#[tokio::test]
async fn preallocate_reserves_and_sizes_exactly() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("f");
    let s = provider().open(&path).await.unwrap();
    s.write_at(0, Bytes::from_static(b"keep")).await.unwrap();
    s.preallocate(64 << 20).await.unwrap();
    s.flush().await.unwrap();
    let meta = std::fs::metadata(&path).unwrap();
    assert_eq!(meta.len(), 64 << 20);
    #[cfg(target_os = "linux")]
    {
        // Really reserved, not a sparse file.
        use std::os::unix::fs::MetadataExt;
        assert!(
            meta.blocks() * 512 >= 64 << 20,
            "only {} blocks",
            meta.blocks()
        );
    }
    assert_eq!(s.read_at(0, 6).await.unwrap(), &b"keep\0\0"[..]);
    // A file left larger by an earlier attempt is cut to the known size.
    s.preallocate(10).await.unwrap();
    s.flush().await.unwrap();
    assert_eq!(std::fs::metadata(&path).unwrap().len(), 10);
}

#[tokio::test]
async fn remove_deletes_and_ignores_missing() {
    let d = tempfile::tempdir().unwrap();
    let p = provider();
    let path = d.path().join("f");
    p.remove(&path).await.unwrap();
    let s = p.open(&path).await.unwrap();
    s.write_at(0, Bytes::from_static(b"x")).await.unwrap();
    s.flush().await.unwrap();
    drop(s);
    p.remove(&path).await.unwrap();
    assert!(!path.exists());
}

/// `/dev/full` fails every write with ENOSPC: the error must reach the
/// engine as `StorageFull`, from the next call after the write was
/// buffered, and `flush` must report it.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn disk_full_surfaces_as_storage_full() {
    let p = provider();
    let s = p.open(Path::new("/dev/full")).await.unwrap();
    s.write_at(0, Bytes::from_static(b"data")).await.unwrap();
    let e = s.flush().await.unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::StorageFull, "{e}");
    // Reported once; the next flush epoch starts clean (fdatasync on a
    // character device may still fail, but not with the old error).
    if let Err(e) = s.flush().await {
        assert_ne!(e.kind(), io::ErrorKind::StorageFull);
    }
    s.write_at(0, Bytes::from_static(b"more")).await.unwrap();
    // Either the next write or the flush sees it, never success.
    let next = s.write_at(4, Bytes::from_static(b"x")).await;
    let flushed = s.flush().await;
    let e = next
        .err()
        .or(flushed.err())
        .expect("disk full was swallowed");
    assert_eq!(e.kind(), io::ErrorKind::StorageFull);
}

/// Random concurrent writes, network-sized chunks, overlapping rewrites of
/// identical bytes and a small buffer: the result must match the in-memory
/// reference byte for byte, before and after `finish`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn matches_the_reference_under_concurrent_random_writes() {
    const SIZE: u64 = 24 << 20;
    let d = tempfile::tempdir().unwrap();
    let content: Bytes = {
        let mut r = Rng(0x2545_f491_4f6c_dd1d);
        (0..SIZE)
            .map(|_| r.next(256) as u8)
            .collect::<Vec<_>>()
            .into()
    };
    let p = FileStorageProvider::new(StorageOptions {
        buffer_bytes: 256 << 10,
    });
    let s = p.open(&d.path().join("f.vayu-part")).await.unwrap();
    let mem = MemStorageProvider::default();
    let m = mem.open(Path::new("/m")).await.unwrap();
    s.preallocate(SIZE).await.unwrap();
    m.preallocate(SIZE).await.unwrap();

    // 16 segments, each written front to back in random 1–64 KiB chunks,
    // like connections streaming; every segment also rewrites a random
    // earlier piece (a hedged duplicate).
    let segs = 16;
    let seg_len = SIZE / segs;
    let mut tasks = Vec::new();
    for i in 0..segs {
        let (s, m, content) = (s.clone(), m.clone(), content.clone());
        tasks.push(tokio::spawn(async move {
            let mut r = Rng(i + 1);
            let (start, end) = (i * seg_len, (i + 1) * seg_len);
            let mut at = start;
            while at < end {
                let n = (1 + r.next(64 << 10)).min(end - at);
                let chunk = content.slice(at as usize..(at + n) as usize);
                s.write_at(at, chunk.clone()).await.unwrap();
                m.write_at(at, chunk).await.unwrap();
                if r.next(50) == 0 {
                    let back = start + r.next(at - start + 1);
                    let len = r.next(100_000).min(end - back);
                    let dup = content.slice(back as usize..(back + len) as usize);
                    s.write_at(back, dup.clone()).await.unwrap();
                    m.write_at(back, dup).await.unwrap();
                }
                at += n;
            }
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }
    let got = s.read_at(0, SIZE as usize).await.unwrap();
    assert_eq!(got, m.read_at(0, SIZE as usize).await.unwrap());
    assert_eq!(got, content);
    s.flush().await.unwrap();
    let fin = d.path().join("f");
    s.finish(&fin).await.unwrap();
    assert!(std::fs::read(&fin).unwrap() == content);
}

#[tokio::test]
async fn works_without_a_tokio_runtime() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("f");
    std::thread::spawn(move || {
        futures::executor::block_on(async {
            let p = provider();
            let s = p.open(&path).await.unwrap();
            s.write_at(0, Bytes::from_static(b"abc")).await.unwrap();
            s.flush().await.unwrap();
            assert_eq!(s.read_at(0, 3).await.unwrap(), "abc");
            s.finish(&path.with_extension("done")).await.unwrap();
        })
    })
    .join()
    .unwrap();
}
