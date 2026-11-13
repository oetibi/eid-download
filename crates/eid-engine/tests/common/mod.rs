#![allow(dead_code)]
use eid_engine::{CancellationToken, Config, Event, EventSink, JobSpec, Outcome};
use eid_testserver::{Behavior, Content, TestServer};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub fn fast_cfg(connections: usize) -> Config {
    Config {
        connections,
        min_split: 64 * 1024,
        ramp_interval: Duration::from_millis(5),
        checkpoint_interval: Duration::from_millis(40),
        stall_timeout: Duration::from_secs(5),
        max_attempts: 6,
        backoff_base: Duration::from_millis(5),
        backoff_cap: Duration::from_millis(40),
        retry_after_cap: Duration::from_secs(5),
        write_chunk: 32 * 1024,
        use_system_proxy: false,
        compute_sha256: true,
        ..Config::default()
    }
}

#[derive(Clone, Default)]
pub struct Log {
    pub events: Arc<Mutex<Vec<Event>>>,
    live: Arc<std::sync::atomic::AtomicUsize>,
    peak_live: Arc<std::sync::atomic::AtomicUsize>,
}
impl Log {
    /// Records events and, at emit time, the number of live responses (opened, not yet dropped).
    pub fn sink(&self) -> EventSink {
        use std::sync::atomic::Ordering::SeqCst;
        let l = self.events.clone();
        let (live, peak) = (self.live.clone(), self.peak_live.clone());
        Arc::new(move |e| {
            match &e {
                Event::ResponseOpened { .. } => {
                    let n = live.fetch_add(1, SeqCst) + 1;
                    peak.fetch_max(n, SeqCst);
                }
                Event::ResponseClosed { .. } => {
                    live.fetch_sub(1, SeqCst);
                }
                _ => {}
            }
            l.lock().unwrap().push(e)
        })
    }
    /// Highest number of simultaneously live responses as seen by the ENGINE.
    pub fn peak_live_responses(&self) -> usize {
        self.peak_live.load(std::sync::atomic::Ordering::SeqCst)
    }
    pub fn live_responses(&self) -> usize {
        self.live.load(std::sync::atomic::Ordering::SeqCst)
    }
    pub fn count(&self, f: impl Fn(&Event) -> bool) -> usize {
        self.events.lock().unwrap().iter().filter(|e| f(e)).count()
    }
}

pub async fn serve(path: &str, content: Content, b: Behavior) -> (TestServer, String) {
    let s = TestServer::start().await;
    s.add(path, content, b);
    let url = format!("{}{}", s.base_url(), path);
    (s, url)
}

pub async fn get(url: &str, dir: &Path, cfg: Config) -> eid_engine::Result<(Outcome, Log)> {
    let log = Log::default();
    let o = eid_engine::download(
        JobSpec {
            url: url.to_string(),
            dir: dir.to_path_buf(),
            file_name: None,
        },
        cfg,
        log.sink(),
        CancellationToken::new(),
    )
    .await?;
    Ok((o, log))
}

pub fn sha_of_file(p: &Path) -> String {
    use sha2::{Digest, Sha256};
    let b = std::fs::read(p).unwrap();
    eid_engine::state::hex(&Sha256::digest(b))
}

/// No leftovers other than the expected final file(s).
pub fn only_files(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

/// The kernel socket table (netlink SOCK_DIAG) exists on Linux only. Where it does not
/// (Windows), tests using [`SocketWatch`] print "NOT TESTED" and the required-test list marks
/// them `unix:` so `check_required.py` reports them as NOT TESTED, never as passed. Where it
/// does, every failure mode below PANICS: there is no silent-skip path on Linux.
pub fn socket_watch_supported() -> bool {
    !eid_platform::IS_WINDOWS
}

/// One kernel query: the number of client sockets to `port`, or None if the query failed.
pub type SockQuery = Box<dyn FnMut(u16) -> Option<usize> + Send>;

#[derive(Default, Clone, Copy)]
struct WatchStats {
    samples: u64,
    failed: u64,
    max_gap: Duration,
    broken: bool,
}

/// Kernel-level, client-side socket counter (ADR-0011). A sampler thread queries the kernel
/// socket table (netlink SOCK_DIAG; 50 us sleep between queries, measured mean period ~0.9 ms,
/// printed on every use) and counts the sockets OWNED BY THIS PROCESS (inode in /proc/self/fd,
/// any TCP state) whose REMOTE port is the test server's port: the client's sockets, active
/// responses plus idle pooled ones. It is a sampler: it can miss short peaks, and a non-atomic
/// dump can in principle mis-count (ADR-0011 §1).
pub struct SocketWatch {
    stop: Arc<std::sync::atomic::AtomicBool>,
    timeline: Arc<Mutex<Vec<(u64, usize)>>>,
    stats: Arc<Mutex<WatchStats>>,
    started: std::time::Instant,
    thread: Option<std::thread::JoinHandle<()>>,
}

/// Result of a validated [`SocketWatch`]: `timeline` holds (time_us, sockets) at every CHANGE.
pub struct SocketReport {
    pub peak: usize,
    pub timeline: Vec<(u64, usize)>,
    pub samples: u64,
    pub failed: u64,
    pub elapsed: Duration,
    pub max_gap: Duration,
}
impl Drop for SocketWatch {
    fn drop(&mut self) {
        // never leak a sampler thread (e.g. a watch dropped without finish)
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}
impl SocketReport {
    /// True when the count went down at least once strictly before the last sample that was
    /// still above zero, i.e. sockets were closed while the download was still running.
    pub fn decreased_mid_download(&self) -> bool {
        let t: Vec<usize> = self.timeline.iter().map(|x| x.1).collect();
        let last_busy = t.iter().rposition(|&n| n > 0).unwrap_or(0);
        (1..=last_busy).any(|i| t[i] < t[i - 1] && t[i] > 0)
    }
    /// The line printed next to every PEAK line: the real sampling period.
    pub fn describe(&self) -> String {
        format!(
            "SAMPLER samples={} failed={} mean_interval_us={} max_interval_us={}",
            self.samples,
            self.failed,
            (self.elapsed.as_micros() as u64)
                .checked_div(self.samples)
                .unwrap_or(0),
            self.max_gap.as_micros()
        )
    }
}

/// Validation applied to every finished watch: it must have sampled, must not have given up,
/// and failed queries must be a small fraction (<= 10%). Pure, so it is unit-tested directly.
pub fn validate_watch(samples: u64, failed: u64, broken: bool) -> Result<(), String> {
    if broken {
        return Err("sampler gave up after 500 consecutive failed kernel queries".into());
    }
    if samples == 0 {
        return Err("no kernel sample was ever taken (samples == 0)".into());
    }
    if failed * 10 > samples + failed {
        return Err(format!(
            "too many failed kernel queries: {failed} failed vs {samples} ok"
        ));
    }
    Ok(())
}

impl SocketWatch {
    /// `None` only where the platform has no kernel socket table (see
    /// [`socket_watch_supported`]). On Linux this PANICS if the table cannot be opened.
    pub fn start(base_url: &str) -> Option<Self> {
        Self::start_with_open(base_url, eid_platform::net::SockDiag::open)
    }

    /// Like [`start`](Self::start) with an injectable `open` (used to prove the failure path).
    pub fn start_with_open(
        base_url: &str,
        open: impl FnOnce() -> Option<eid_platform::net::SockDiag>,
    ) -> Option<Self> {
        if !socket_watch_supported() {
            return None;
        }
        let mut diag =
            open().expect("kernel socket table (netlink SOCK_DIAG) must be available on Linux");
        let query: SockQuery = Box::new(move |port| {
            let r = diag.client_sockets_to_port(port);
            if r.is_none() {
                // stale dump data may remain queued: start from a fresh socket
                if let Some(d) = eid_platform::net::SockDiag::open() {
                    diag = d;
                }
            }
            r
        });
        Some(Self::start_with(base_url, query))
    }

    /// Core: sample with an arbitrary query function (failure injection in tests).
    pub fn start_with(base_url: &str, mut query: SockQuery) -> Self {
        use std::sync::atomic::Ordering::SeqCst;
        let port: u16 = base_url
            .rsplit(':')
            .next()
            .and_then(|p| p.trim_end_matches('/').parse().ok())
            .unwrap_or_else(|| panic!("cannot read the server port from {base_url:?}"));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let timeline: Arc<Mutex<Vec<(u64, usize)>>> = Arc::default();
        let stats: Arc<Mutex<WatchStats>> = Arc::default();
        let (s2, t2, st2) = (stop.clone(), timeline.clone(), stats.clone());
        let started = std::time::Instant::now();
        let thread = std::thread::spawn(move || {
            let mut last = usize::MAX;
            let mut last_ok = std::time::Instant::now();
            let mut consecutive_failed = 0u32;
            while !s2.load(SeqCst) {
                // A failed query is NOT a "0 sockets" sample: count it, back off, and give up
                // (=> the test fails in `finish`) after 500 in a row.
                let Some(n) = query(port) else {
                    let mut st = st2.lock().unwrap();
                    st.failed += 1;
                    consecutive_failed += 1;
                    if consecutive_failed >= 500 {
                        st.broken = true;
                        return;
                    }
                    drop(st);
                    std::thread::sleep(Duration::from_millis(1));
                    continue;
                };
                consecutive_failed = 0;
                {
                    let mut st = st2.lock().unwrap();
                    st.samples += 1;
                    st.max_gap = st.max_gap.max(last_ok.elapsed());
                }
                last_ok = std::time::Instant::now();
                if n != last {
                    t2.lock().unwrap().push((eid_testserver::now_us(), n));
                    last = n;
                }
                std::thread::sleep(Duration::from_micros(50));
            }
        });
        // Readiness barrier (replaces a fixed sleep that bet on thread scheduling): return only
        // after the sampler took its first successful sample (or gave up, which `finish`
        // reports). If neither happens within 5 s the TEST FAILS here.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            {
                let st = stats.lock().unwrap();
                if st.samples > 0 || st.broken {
                    break;
                }
            }
            assert!(
                std::time::Instant::now() < deadline,
                "socket sampler took no sample within 5 s (thread never scheduled?)"
            );
            std::thread::sleep(Duration::from_micros(200));
        }
        Self {
            stop,
            timeline,
            stats,
            started,
            thread: Some(thread),
        }
    }

    /// Stops sampling; Err if the watch was not trustworthy (see `validate_watch`).
    pub fn finish_checked(mut self) -> Result<SocketReport, String> {
        use std::sync::atomic::Ordering::SeqCst;
        self.stop.store(true, SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        let st = *self.stats.lock().unwrap();
        validate_watch(st.samples, st.failed, st.broken)?;
        let timeline = self.timeline.lock().unwrap().clone();
        let peak = timeline.iter().map(|x| x.1).max().unwrap_or(0);
        Ok(SocketReport {
            peak,
            timeline,
            samples: st.samples,
            failed: st.failed,
            elapsed: self.started.elapsed(),
            max_gap: st.max_gap,
        })
    }

    /// Like `finish_checked` but PANICS (fails the test) on an untrustworthy watch.
    pub fn finish(self) -> SocketReport {
        self.finish_checked()
            .unwrap_or_else(|e| panic!("socket watch invalid: {e}"))
    }
}
