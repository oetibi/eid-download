//! Test-only fault injection. Compiled to no-ops unless feature `test-faults` is on.
//!
//! * `EID_FAULT=<name>` + optional `EID_FAULT_AFTER=<n>`: abort the process the n-th time
//!   fault point `<name>` is reached (simulates power loss / kill -9 at that exact point).
//! * [`set_jitter`]: random yields/sleeps at race-prone points to shake out interleavings.

#[cfg(feature = "test-faults")]
mod imp {
    use std::sync::atomic::{AtomicU64, Ordering};
    static HITS: AtomicU64 = AtomicU64::new(0);
    static JITTER: AtomicU64 = AtomicU64::new(0);

    pub fn hit(name: &str) {
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
    pub fn set_jitter(seed: u64) {
        JITTER.store(seed, Ordering::SeqCst);
    }
    pub fn jitter() {
        let s = JITTER.load(Ordering::Relaxed);
        if s != 0 {
            match fastrand::u8(..) % 8 {
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
    #[inline(always)]
    pub fn set_panic_point(_name: &str, _after: u64) {}
    #[inline(always)]
    pub fn set_jitter(_seed: u64) {}
    #[inline(always)]
    pub fn jitter() {}
}

pub use imp::{hit, jitter, set_jitter, set_panic_point};
