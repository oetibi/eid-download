//! Test-only fault injection. Compiled to no-ops unless feature `test-faults` is on.
//!
//! * `EID_FAULT=<name>` + optional `EID_FAULT_AFTER=<n>`: abort the process the n-th time
//!   fault point `<name>` is reached (simulates power loss / kill -9 at that exact point).
//! * [`set_jitter`]: random yields/sleeps at race-prone points to shake out interleavings.

#[cfg(feature = "test-faults")]
mod imp {
    use std::sync::atomic::{AtomicU64, Ordering};
    static DRAIN_ITERATIONS: AtomicU64 = AtomicU64::new(0);
    pub fn coordinator_iteration(draining: bool) {
        if draining {
            DRAIN_ITERATIONS.fetch_add(1, Ordering::Relaxed);
        }
    }
    pub fn drain_iterations() -> u64 {
        DRAIN_ITERATIONS.load(Ordering::Relaxed)
    }
    static HITS: AtomicU64 = AtomicU64::new(0);
    static JITTER: AtomicU64 = AtomicU64::new(0);

    /// Test hook: the NEXT time fault point `name` is reached (on whatever thread, typically a
    /// blocking-pool thread) that thread signals `reached` and then blocks until `release` is
    /// signalled (30 s safety timeout). One-shot; process-global.
    pub fn set_pause_point(
        name: &str,
    ) -> (std::sync::mpsc::Receiver<()>, std::sync::mpsc::Sender<()>) {
        let (reached_tx, reached_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        *PAUSE_AT.lock().unwrap_or_else(|e| e.into_inner()) =
            Some((name.to_string(), reached_tx, release_rx));
        (reached_rx, release_tx)
    }
    #[allow(clippy::type_complexity)]
    static PAUSE_AT: std::sync::Mutex<
        Option<(
            String,
            std::sync::mpsc::Sender<()>,
            std::sync::mpsc::Receiver<()>,
        )>,
    > = std::sync::Mutex::new(None);

    pub fn hit(name: &str) {
        let pause = {
            let mut g = PAUSE_AT.lock().unwrap_or_else(|e| e.into_inner());
            if g.as_ref().is_some_and(|p| p.0 == name) {
                g.take()
            } else {
                None
            }
        };
        if let Some((_, reached, release)) = pause {
            let _ = reached.send(());
            let _ = release.recv_timeout(std::time::Duration::from_secs(30));
        }
        let fire = {
            let mut g = PANIC_AT.lock().unwrap_or_else(|e| e.into_inner());
            match g.as_ref() {
                Some((n, after))
                    if n == name && PANIC_HITS.fetch_add(1, Ordering::SeqCst) + 1 >= *after =>
                {
                    *g = None; // one-shot: later resumes in the same process run normally
                    true
                }
                _ => false,
            }
        };
        if fire {
            panic!("fault point {name} reached (simulated crash)");
        }
        if let Ok(want) = std::env::var("EID_FAULT") {
            if want == name {
                let after: u64 = std::env::var("EID_FAULT_AFTER")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(1);
                if HITS.fetch_add(1, Ordering::SeqCst) + 1 >= after {
                    std::process::abort();
                }
            }
        }
    }
    /// Test hook: PANIC (instead of abort) the `after`-th time fault point `name` is reached, so an
    /// in-process test can observe the on-disk state a crash at that point leaves behind.
    /// Process-global: use only from a test binary that runs a single such test.
    pub fn set_panic_point(name: &str, after: u64) {
        PANIC_HITS.store(0, Ordering::SeqCst);
        *PANIC_AT.lock().unwrap_or_else(|e| e.into_inner()) = Some((name.to_string(), after));
    }
    static PANIC_AT: std::sync::Mutex<Option<(String, u64)>> = std::sync::Mutex::new(None);
    static PANIC_HITS: AtomicU64 = AtomicU64::new(0);
    /// Test hook: after `skip` successful calls of [`fail`] with this `name`, the next `times`
    /// calls return a `PermissionDenied` error (an antivirus/indexer holding a file).
    /// Process-global.
    /// Arm ONE fail point (any previously armed points are disarmed).
    pub fn set_fail_point(name: &str, skip: u32, times: u32) {
        *FAIL_AT.lock().unwrap_or_else(|e| e.into_inner()) = vec![(name.to_string(), skip, times)];
    }
    /// v0.2: arm one more fail point next to those already armed (two failures in one run).
    pub fn add_fail_point(name: &str, skip: u32, times: u32) {
        FAIL_AT
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((name.to_string(), skip, times));
    }
    #[allow(clippy::type_complexity)]
    static FAIL_AT: std::sync::Mutex<Vec<(String, u32, u32)>> = std::sync::Mutex::new(Vec::new());
    /// An injectable I/O error at a named point (`Ok` unless a test armed it).
    pub fn fail(name: &str) -> std::io::Result<()> {
        let mut g = FAIL_AT.lock().unwrap_or_else(|e| e.into_inner());
        for (n, skip, times) in g.iter_mut() {
            if n != name {
                continue;
            }
            if *skip > 0 {
                *skip -= 1;
            } else if *times > 0 {
                *times -= 1;
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    format!("injected failure at {name}"),
                ));
            }
        }
        Ok(())
    }
    static JITTER_CALLS: AtomicU64 = AtomicU64::new(0);
    /// Arm jitter with `seed` (0 = off). The delay DECISIONS are a pure function of
    /// `(seed, call index)` (splitmix64), so a printed seed replays the same decision stream;
    /// which thread consumes which decision is up to the OS scheduler (honest limit: the
    /// interleaving itself is not bit-reproducible, a failing seed is replayed in a loop).
    pub fn set_jitter(seed: u64) {
        JITTER_CALLS.store(0, Ordering::SeqCst);
        JITTER.store(seed, Ordering::SeqCst);
    }
    pub fn jitter() {
        let s = JITTER.load(Ordering::Relaxed);
        if s != 0 {
            let n = JITTER_CALLS.fetch_add(1, Ordering::Relaxed);
            let mut z = s.wrapping_add(n.wrapping_mul(0x9E37_79B9_7F4A_7C15));
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^= z >> 31;
            match z % 8 {
                0 => std::thread::sleep(std::time::Duration::from_micros(50)),
                1 | 2 => std::thread::yield_now(),
                _ => {}
            }
        }
    }
}

#[cfg(not(feature = "test-faults"))]
mod imp {
    #[inline(always)]
    pub fn hit(_name: &str) {}
    pub fn coordinator_iteration(_draining: bool) {}
    pub fn drain_iterations() -> u64 {
        0
    }
    #[inline(always)]
    pub fn set_panic_point(_name: &str, _after: u64) {}
    #[inline(always)]
    pub fn set_fail_point(_name: &str, _skip: u32, _times: u32) {}
    #[inline(always)]
    pub fn add_fail_point(_name: &str, _skip: u32, _times: u32) {}
    #[inline(always)]
    pub fn fail(_name: &str) -> std::io::Result<()> {
        Ok(())
    }
    pub fn set_pause_point(
        _name: &str,
    ) -> (std::sync::mpsc::Receiver<()>, std::sync::mpsc::Sender<()>) {
        (std::sync::mpsc::channel().1, std::sync::mpsc::channel().0)
    }
    #[inline(always)]
    pub fn set_jitter(_seed: u64) {}
    #[inline(always)]
    pub fn jitter() {}
}

pub use imp::{
    add_fail_point, coordinator_iteration, drain_iterations, fail, hit, jitter, set_fail_point,
    set_jitter, set_panic_point, set_pause_point,
};
