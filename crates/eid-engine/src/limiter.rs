//! Hierarchical rate limiter (global -> job). Virtual-time scheduling: each acquire books the
//! next free slot, so the long-run rate is exact and tests can use a paused tokio clock.

use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::time::Instant;

pub struct Limiter {
    inner: Mutex<Inner>,
    parent: Option<Arc<Limiter>>,
}

struct Inner {
    /// bytes per second; 0 = unlimited
    rate: u64,
    /// earliest instant the next byte may be sent
    next: Instant,
}

impl Limiter {
    pub fn new(rate_bps: u64) -> Arc<Limiter> {
        Arc::new(Limiter {
            inner: Mutex::new(Inner {
                rate: rate_bps,
                next: Instant::now(),
            }),
            parent: None,
        })
    }

    /// A child limiter: every acquire must satisfy both this limiter and its ancestors.
    pub fn child(parent: &Arc<Limiter>, rate_bps: u64) -> Arc<Limiter> {
        Arc::new(Limiter {
            inner: Mutex::new(Inner {
                rate: rate_bps,
                next: Instant::now(),
            }),
            parent: Some(parent.clone()),
        })
    }

    pub fn set_rate(&self, rate_bps: u64) {
        self.inner.lock().unwrap().rate = rate_bps;
    }

    /// Book `n` bytes; returns the instant at which sending may start.
    fn reserve(&self, n: u64, now: Instant) -> Instant {
        let mut g = self.inner.lock().unwrap();
        if g.rate == 0 {
            return now;
        }
        // allow a small burst (100 ms worth) so bursts of tiny reads are not over-penalised
        let burst = Duration::from_millis(100);
        let earliest = now.checked_sub(burst).unwrap_or(now);
        let start = g.next.max(earliest);
        let cost = Duration::from_secs_f64(n as f64 / g.rate as f64);
        g.next = start + cost;
        start.max(now)
    }

    /// Wait until `n` bytes may be consumed.
    pub async fn acquire(&self, n: usize) {
        let now = Instant::now();
        let mut at = self.reserve(n as u64, now);
        let mut p = self.parent.as_ref();
        while let Some(l) = p {
            at = at.max(l.reserve(n as u64, now));
            p = l.parent.as_ref();
        }
        if at > now {
            tokio::time::sleep_until(at).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn drain(l: &Limiter, total: usize, chunk: usize) -> Duration {
        let t0 = Instant::now();
        let mut left = total;
        while left > 0 {
            let n = left.min(chunk);
            l.acquire(n).await;
            left -= n;
        }
        // the last booking is paid for by waiting until its own slot has passed
        Instant::now() - t0
    }

    #[tokio::test(start_paused = true)]
    async fn limiter_rate_within_5_percent_virtual_time() {
        let l = Limiter::new(1_000_000);
        let took = drain(&l, 6_000_000, 64 * 1024).await;
        let rate = 6_000_000.0 / took.as_secs_f64();
        assert!(
            (rate - 1_000_000.0).abs() / 1_000_000.0 < 0.05,
            "rate {rate}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn limiter_job_under_global_both_enforced() {
        // global 1 MB/s shared by two jobs each allowed 1 MB/s -> aggregate stays <= 1 MB/s
        let g = Limiter::new(1_000_000);
        let a = Limiter::child(&g, 1_000_000);
        let b = Limiter::child(&g, 1_000_000);
        let t0 = Instant::now();
        let (x, y) = tokio::join!(
            drain(&a, 3_000_000, 32 * 1024),
            drain(&b, 3_000_000, 32 * 1024)
        );
        let total = (Instant::now() - t0).as_secs_f64();
        let agg = 6_000_000.0 / total;
        assert!(
            agg <= 1_050_000.0,
            "aggregate {agg} exceeds global limit (x={x:?} y={y:?})"
        );
        assert!(agg >= 0.90 * 1_000_000.0, "aggregate {agg} far below limit");
    }

    #[tokio::test(start_paused = true)]
    async fn limiter_per_job_limit_tighter_than_global() {
        let g = Limiter::new(10_000_000);
        let j = Limiter::child(&g, 500_000);
        let took = drain(&j, 3_000_000, 50_000).await;
        let rate = 3_000_000.0 / took.as_secs_f64();
        assert!((rate - 500_000.0).abs() / 500_000.0 < 0.05, "rate {rate}");
    }

    #[tokio::test(start_paused = true)]
    async fn unlimited_does_not_wait() {
        let l = Limiter::new(0);
        let t0 = Instant::now();
        l.acquire(1 << 30).await;
        assert_eq!(Instant::now(), t0);
    }
}
