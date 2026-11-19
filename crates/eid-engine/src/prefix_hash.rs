//! Incremental SHA-256 of a download's completed prefix, computed *during* the transfer.
//!
//! Finalization always needs the digest of the whole file (finalization record, review B2).
//! Re-reading the finished file for it is the largest part of finalization on a big file
//! (measured: docs/BENCHMARKS.md). Instead, a background task reads the bytes below the table's contiguous completed frontier
//! (positional reads through the SAME verified handle the engine writes through) and feeds
//! them to the digest, so finalization only hashes whatever tail is left.
//!
//! Correctness rests on the reservation protocol (ADR-0009): a byte below `done` is written
//! and never written again in this generation. The task is joined before any truncation
//! (restart) or finalization, and its digest is thrown away on any error, after which
//! finalization simply hashes the whole file as before.

use crate::table::Table;
use eid_platform::fs as pfs;
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// How much one blocking slice hashes at most (bounds how long a stop request waits).
const SLICE: u64 = 32 * 1024 * 1024;
/// Do not wake the blocking pool for less than this, unless the file is complete.
const MIN_STEP: u64 = 4 * 1024 * 1024;
const POLL: Duration = Duration::from_millis(100);

/// SHA-256 over the bytes `[0, len)` of a file.
pub struct PrefixHash {
    len: u64,
    h: Sha256,
}

impl Default for PrefixHash {
    fn default() -> Self {
        Self::new()
    }
}

impl PrefixHash {
    pub fn new() -> Self {
        PrefixHash {
            len: 0,
            h: Sha256::new(),
        }
    }

    /// Number of leading bytes already in the digest.
    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Feed bytes that are known to be the file's bytes at `[len, len + data.len())`.
    pub fn update(&mut self, data: &[u8]) {
        self.h.update(data);
        self.len = self.len.saturating_add(data.len() as u64);
    }

    /// Extend the digest with the file's bytes `[len, upto)`, read positionally through
    /// `file`. An early end of file is an error (the digest is then unusable).
    pub fn extend_from(&mut self, file: &File, upto: u64, buf: &mut [u8]) -> io::Result<()> {
        if buf.is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty buffer"));
        }
        while self.len < upto {
            let want = (upto - self.len).min(buf.len() as u64) as usize;
            let n = pfs::read_at(file, &mut buf[..want], self.len)?;
            if n == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "file ended before the hashed prefix",
                ));
            }
            self.update(&buf[..n]);
        }
        Ok(())
    }

    /// Lower-case hex digest of `[0, len)`.
    pub fn finish_hex(self) -> String {
        crate::state::hex(&self.h.finalize())
    }
}

/// Background hasher of the completed prefix. `stop` + [`HashTask::join`] before any truncation
/// or finalization; [`HashTask::join`] returns `None` if hashing failed (then hash from zero).
pub struct HashTask {
    stop: CancellationToken,
    hashed: Arc<AtomicU64>,
    handle: tokio::task::JoinHandle<Option<PrefixHash>>,
}

impl HashTask {
    pub fn start(file: Arc<File>, table: Arc<Mutex<Table>>) -> HashTask {
        let stop = CancellationToken::new();
        let s2 = stop.clone();
        let hashed = Arc::new(AtomicU64::new(0));
        let h2 = hashed.clone();
        let handle = tokio::spawn(async move {
            let mut ph = PrefixHash::new();
            let mut buf = vec![0u8; 1 << 20];
            loop {
                if s2.is_cancelled() {
                    return Some(ph);
                }
                let (frontier, total) = {
                    let t = table.lock().unwrap();
                    (t.contiguous_done(), t.total)
                };
                let have = ph.len();
                let ready = frontier > have && (frontier - have >= MIN_STEP || frontier == total);
                if !ready {
                    tokio::select! {
                        _ = s2.cancelled() => return Some(ph),
                        _ = tokio::time::sleep(POLL) => continue,
                    }
                }
                let upto = frontier.min(have.saturating_add(SLICE));
                let f = file.clone();
                // Awaited to completion even when a stop arrives: a slice is bounded (SLICE) and
                // only reads, so nothing is left half-done on disk.
                let r = tokio::task::spawn_blocking(move || {
                    let r = ph.extend_from(&f, upto, &mut buf);
                    (ph, buf, r)
                })
                .await;
                match r {
                    Ok((p, b, Ok(()))) => {
                        h2.store(p.len(), Ordering::Relaxed);
                        ph = p;
                        buf = b;
                    }
                    _ => return None,
                }
            }
        });
        HashTask {
            stop,
            hashed,
            handle,
        }
    }

    /// Bytes hashed so far (progress evidence; the digest itself is only returned by `join`).
    pub fn hashed(&self) -> u64 {
        self.hashed.load(Ordering::Relaxed)
    }

    /// Stop and wait for the in-flight slice. Never aborts the blocking read.
    pub async fn join(mut self) -> Option<PrefixHash> {
        self.stop.cancel();
        (&mut self.handle).await.ok().flatten()
    }
}

impl Drop for HashTask {
    /// A task that is dropped without `join` (its download future was dropped) must not keep
    /// polling the table and holding the file handle for ever.
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn data(n: usize) -> Vec<u8> {
        (0..n)
            .map(|i| (i.wrapping_mul(31) ^ (i >> 7)) as u8)
            .collect()
    }

    fn sha(b: &[u8]) -> String {
        crate::state::hex(&Sha256::digest(b))
    }

    #[test]
    fn extending_in_odd_steps_equals_one_digest_and_a_short_file_is_an_error() {
        let d = data(3_000_017);
        let mut f = tempfile::tempfile().unwrap();
        f.write_all(&d).unwrap();
        let mut ph = PrefixHash::new();
        let mut buf = vec![0u8; 4097];
        for upto in [0, 1, 4096, 4097, 1_000_000, 1_000_000, 3_000_017] {
            ph.extend_from(&f, upto, &mut buf).unwrap();
            assert_eq!(ph.len(), upto);
        }
        assert_eq!(ph.finish_hex(), sha(&d));
        // the digest never runs past the end of the file
        let mut ph = PrefixHash::new();
        let e = ph.extend_from(&f, 3_000_018, &mut buf).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::UnexpectedEof);
        // update() and extend_from() describe the same prefix
        let mut a = PrefixHash::new();
        a.update(&d[..10]);
        a.extend_from(&f, 20, &mut buf).unwrap();
        assert_eq!(a.finish_hex(), sha(&d[..20]));
        assert!(PrefixHash::new().is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_task_hashes_only_the_completed_prefix_and_stops_on_request() {
        let total = 20 * 1024 * 1024u64;
        let d = data(total as usize);
        let mut f = tempfile::tempfile().unwrap();
        f.write_all(&d).unwrap();
        let file = Arc::new(f);
        let table = Arc::new(Mutex::new(Table::new(total)));
        let seg = table.lock().unwrap().take_pending().unwrap();
        let task = HashTask::start(file.clone(), table.clone());
        // complete 9 MiB: the task must reach exactly that frontier and no further
        let nine = 9 * 1024 * 1024;
        {
            let mut t = table.lock().unwrap();
            let (o, n) = t.reserve(seg, total);
            assert_eq!((o, n), (0, total));
            t.complete(seg, 0, nine);
        }
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while task.hashed() < nine {
            assert!(
                tokio::time::Instant::now() < deadline,
                "task never hashed the prefix"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        // nothing beyond the frontier: give it a few polls to (wrongly) run ahead
        tokio::time::sleep(POLL * 3).await;
        assert_eq!(task.hashed(), nine);
        let ph = task.join().await.unwrap();
        assert_eq!(ph.len(), nine, "hashed exactly the completed prefix");
        assert_eq!(ph.finish_hex(), sha(&d[..nine as usize]));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_read_error_discards_the_digest() {
        // the table claims more completed bytes than the file has: hashing must give up (None),
        // never return a digest of fewer bytes than it claims
        let mut f = tempfile::tempfile().unwrap();
        f.write_all(&data(1000)).unwrap();
        let table = Arc::new(Mutex::new(Table::new(8 * 1024 * 1024)));
        {
            let mut t = table.lock().unwrap();
            let s = t.take_pending().unwrap();
            let (o, n) = t.reserve(s, u64::MAX);
            t.complete(s, o, n);
        }
        let task = HashTask::start(Arc::new(f), table);
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(task.join().await.is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_dropped_task_stops_and_releases_the_file() {
        let file = Arc::new(tempfile::tempfile().unwrap());
        let table = Arc::new(Mutex::new(Table::new(8 * 1024 * 1024)));
        let task = HashTask::start(file.clone(), table);
        assert_eq!(Arc::strong_count(&file), 2);
        drop(task);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while Arc::strong_count(&file) > 1 {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the detached hasher kept the file handle"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}
