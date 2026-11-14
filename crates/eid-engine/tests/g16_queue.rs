//! P2: the queue driver on a VIRTUAL clock (tokio `start_paused`) with a fake job runner. No
//! network, no files except `queues.json` in a temp dir. The real engine + real processes are
//! covered by `g17_queue_engine.rs` and the CLI kill test.
use eid_engine::queue::{PowerAction, Queue, QueueSet, Status, Window};
use eid_engine::scheduler::{
    load_queues, Clock, JobRunner, PowerControl, QueueEvent, RunJob, RunResult, Scheduler,
    SchedulerConfig, StopReason, SystemClock,
};
use eid_engine::CancellationToken;
use eid_platform::time::{from_unix, LocalNow};
use futures_util::future::BoxFuture;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::time::Instant;

/// Monday 2026-11-09 00:00:00 UTC.
const MON: i64 = 1_794_182_400;

/// Virtual wall clock: `base + virtual seconds elapsed`.
struct FakeClock {
    base: i64,
    t0: Instant,
}
impl FakeClock {
    fn new(base: i64) -> Arc<Self> {
        Arc::new(FakeClock {
            base,
            t0: Instant::now(),
        })
    }
    fn secs(&self) -> i64 {
        self.base + self.t0.elapsed().as_secs() as i64
    }
}
impl Clock for FakeClock {
    fn now(&self) -> LocalNow {
        from_unix(self.secs(), 0)
    }
}

#[derive(Default)]
struct Stats {
    cur: AtomicUsize,
    max: AtomicUsize,
    /// (item url, virtual unix at start)
    starts: Mutex<Vec<(String, i64)>>,
    auth_seen: Mutex<Vec<Option<(String, String)>>>,
}

type Script = dyn Fn(&str, usize) -> (Duration, RunResult) + Send + Sync;

/// Runs "downloads" that only sleep. `script(url, attempt_index)` gives duration and result.
struct FakeRunner {
    clock: Arc<FakeClock>,
    stats: Arc<Stats>,
    attempts: Mutex<HashMap<String, usize>>,
    script: Box<Script>,
}
impl JobRunner for FakeRunner {
    fn run(
        &self,
        job: RunJob,
        started: Arc<dyn Fn(String) + Send + Sync>,
        cancel: CancellationToken,
    ) -> BoxFuture<'static, RunResult> {
        let n = {
            let mut a = self.attempts.lock().unwrap();
            let e = a.entry(job.url.clone()).or_insert(0);
            *e += 1;
            *e - 1
        };
        let (dur, res) = (self.script)(&job.url, n);
        let (stats, clock) = (self.stats.clone(), self.clock.clone());
        Box::pin(async move {
            let cur = stats.cur.fetch_add(1, Ordering::SeqCst) + 1;
            stats.max.fetch_max(cur, Ordering::SeqCst);
            stats
                .starts
                .lock()
                .unwrap()
                .push((job.url.clone(), clock.secs()));
            stats.auth_seen.lock().unwrap().push(job.auth.clone());
            started(format!("f{}.bin", n));
            let out = tokio::select! {
                _ = tokio::time::sleep(dur) => res,
                _ = cancel.cancelled() => RunResult::Cancelled,
            };
            stats.cur.fetch_sub(1, Ordering::SeqCst);
            out
        })
    }
}

#[derive(Default)]
struct MockPower {
    calls: Mutex<Vec<PowerAction>>,
    fail: bool,
}
impl PowerControl for MockPower {
    fn execute(&self, a: PowerAction) -> Result<(), String> {
        self.calls.lock().unwrap().push(a);
        if self.fail {
            Err("denied".into())
        } else {
            Ok(())
        }
    }
}

struct Rig {
    sched: Scheduler,
    clock: Arc<FakeClock>,
    stats: Arc<Stats>,
    events: Arc<Mutex<Vec<QueueEvent>>>,
    power: Arc<MockPower>,
    path: PathBuf,
    _dir: tempfile::TempDir,
}

fn rig(
    base: i64,
    script: Box<Script>,
    power: MockPower,
    prepare: impl FnOnce(&mut QueueSet),
) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("queues.json");
    let mut qs = QueueSet::default();
    prepare(&mut qs);
    eid_engine::scheduler::save_queues(&path, &qs).unwrap();
    rig_at(dir, path, base, script, power)
}

fn rig_at(
    dir: tempfile::TempDir,
    path: PathBuf,
    base: i64,
    script: Box<Script>,
    power: MockPower,
) -> Rig {
    let clock = FakeClock::new(base);
    let stats = Arc::new(Stats::default());
    let events: Arc<Mutex<Vec<QueueEvent>>> = Arc::default();
    let power = Arc::new(power);
    let runner = Arc::new(FakeRunner {
        clock: clock.clone(),
        stats: stats.clone(),
        attempts: Mutex::default(),
        script,
    });
    let ev = events.clone();
    let sched = Scheduler::open(
        SchedulerConfig::new(&path),
        runner,
        clock.clone(),
        power.clone(),
        Arc::new(move |e| ev.lock().unwrap().push(e)),
    )
    .unwrap();
    Rig {
        sched,
        clock,
        stats,
        events,
        power,
        path,
        _dir: dir,
    }
}

fn done(_: &str, _: usize) -> (Duration, RunResult) {
    (
        Duration::from_secs(3),
        RunResult::Done { path: "/x".into() },
    )
}

fn queue(qs: &mut QueueSet, max_parallel: u32, n: usize) {
    let mut q = Queue::new("main", "/dl");
    q.max_parallel = max_parallel;
    qs.queues.push(q);
    for i in 0..n {
        qs.add_item("main", &format!("http://h/{i}.bin"), None, None, None)
            .unwrap();
    }
}

#[tokio::test(start_paused = true)]
async fn ten_downloads_with_a_limit_of_two_never_run_more_than_two() {
    let r = rig(
        MON + 12 * 3600,
        Box::new(done),
        MockPower::default(),
        |qs| queue(qs, 2, 10),
    );
    r.sched.run(CancellationToken::new(), true).await.unwrap();
    assert_eq!(r.stats.max.load(Ordering::SeqCst), 2, "peak concurrency");
    assert_eq!(r.stats.starts.lock().unwrap().len(), 10);
    let qs = load_queues(&r.path).unwrap();
    assert!(qs.queues[0].items.iter().all(|i| i.status == Status::Done));
    // 10 jobs x 3 s on 2 slots = 15 virtual seconds (and not 30: both slots were used)
    let elapsed = r.clock.secs() - (MON + 12 * 3600);
    assert!((15..=16).contains(&elapsed), "{elapsed}");
    // limit 1 and limit 5 as well
    for (limit, expect_peak) in [(1u32, 1usize), (5, 5)] {
        let r = rig(MON, Box::new(done), MockPower::default(), |qs| {
            queue(qs, limit, 10)
        });
        r.sched.run(CancellationToken::new(), true).await.unwrap();
        assert_eq!(
            r.stats.max.load(Ordering::SeqCst),
            expect_peak,
            "limit {limit}"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn a_job_scheduled_in_five_seconds_starts_within_a_second_of_that() {
    let t0 = MON + 12 * 3600;
    let r = rig(t0, Box::new(done), MockPower::default(), |qs| {
        queue(qs, 2, 0);
        qs.add_item("main", "http://h/later.bin", None, None, Some(t0 + 5))
            .unwrap();
        qs.add_item("main", "http://h/now.bin", None, None, None)
            .unwrap();
    });
    r.sched.run(CancellationToken::new(), true).await.unwrap();
    let starts = r.stats.starts.lock().unwrap().clone();
    let now_start = starts.iter().find(|s| s.0.ends_with("now.bin")).unwrap().1;
    let later_start = starts
        .iter()
        .find(|s| s.0.ends_with("later.bin"))
        .unwrap()
        .1;
    assert_eq!(now_start, t0, "an unscheduled item starts immediately");
    assert!(
        (t0 + 5..t0 + 6).contains(&later_start),
        "scheduled item started at +{}s",
        later_start - t0
    );
}

#[tokio::test(start_paused = true)]
async fn a_closing_window_stops_running_jobs_and_the_next_opening_restarts_them() {
    // window 09:00-17:00 every day; we start at 16:59:30 with a 120 s job
    let t0 = MON + 16 * 3600 + 59 * 60 + 30;
    let script: Box<Script> = Box::new(|_, attempt| {
        if attempt == 0 {
            (
                Duration::from_secs(120),
                RunResult::Done { path: "/x".into() },
            )
        } else {
            (
                Duration::from_secs(10),
                RunResult::Done { path: "/x".into() },
            )
        }
    });
    let r = rig(t0, script, MockPower::default(), |qs| {
        queue(qs, 1, 1);
        qs.queues[0].window = Some(Window {
            start_min: 9 * 60,
            end_min: 17 * 60,
            weekdays: Window::ALL_DAYS,
        });
    });
    let cancel = CancellationToken::new();
    let (s2, c2) = (r.sched.clone(), cancel.clone());
    let h = tokio::spawn(async move { s2.run(c2, true).await });
    // run past the next opening (09:00 next day) plus the retry job
    tokio::time::sleep(Duration::from_secs(18 * 3600)).await;
    h.await.unwrap().unwrap();
    let starts = r.stats.starts.lock().unwrap().clone();
    assert_eq!(starts.len(), 2, "{starts:?}");
    assert_eq!(starts[0].1, t0);
    let next_open = MON + 86_400 + 9 * 3600;
    assert!(
        (next_open..next_open + 2).contains(&starts[1].1),
        "restart at {}",
        starts[1].1 - MON
    );
    let ev = r.events.lock().unwrap().clone();
    assert!(ev.iter().any(|e| matches!(
        e,
        QueueEvent::ItemStopped {
            reason: StopReason::WindowClosed,
            ..
        }
    )));
    assert_eq!(
        load_queues(&r.path).unwrap().queues[0].items[0].status,
        Status::Done
    );
}

#[tokio::test(start_paused = true)]
async fn failures_are_requeued_at_the_end_with_growing_delay_and_then_given_up() {
    let t0 = MON + 10 * 3600;
    // a.bin fails (retryable) 5 times; b.bin ok; c.bin non-retryable
    let script: Box<Script> = Box::new(|url, _| {
        if url.ends_with("a.bin") {
            (
                Duration::from_secs(1),
                RunResult::Failed {
                    retryable: true,
                    message: "boom".into(),
                },
            )
        } else if url.ends_with("c.bin") {
            (
                Duration::from_secs(1),
                RunResult::Failed {
                    retryable: false,
                    message: "HTTP 404".into(),
                },
            )
        } else {
            (
                Duration::from_secs(1),
                RunResult::Done { path: "/x".into() },
            )
        }
    });
    let r = rig(t0, script, MockPower::default(), |qs| {
        queue(qs, 1, 0);
        qs.queues[0].retries.max = 2;
        qs.queues[0].retries.delay_secs = 30;
        for n in ["a.bin", "b.bin", "c.bin"] {
            qs.add_item("main", &format!("http://h/{n}"), None, None, None)
                .unwrap();
        }
    });
    r.sched.run(CancellationToken::new(), true).await.unwrap();
    let starts = r.stats.starts.lock().unwrap().clone();
    let a: Vec<i64> = starts
        .iter()
        .filter(|s| s.0.ends_with("a.bin"))
        .map(|s| s.1 - t0)
        .collect();
    assert_eq!(a.len(), 3, "first try + 2 retries: {a:?}");
    // 1st failure at ~1s -> wait 30 s; 2nd failure -> wait 60 s (linear growth)
    assert!((31..=33).contains(&a[1]), "{a:?}");
    assert!((a[2] - a[1] - 1) >= 59 && (a[2] - a[1] - 1) <= 62, "{a:?}");
    // b ran BEFORE a's first retry (the failure went to the end of the queue)
    let b = starts.iter().find(|s| s.0.ends_with("b.bin")).unwrap().1 - t0;
    assert!(b < a[1], "b at {b}, a's retry at {}", a[1]);
    assert_eq!(
        starts.iter().filter(|s| s.0.ends_with("c.bin")).count(),
        1,
        "non-retryable: tried once"
    );
    let qs = load_queues(&r.path).unwrap();
    let st = |n: &str| {
        qs.queues[0]
            .items
            .iter()
            .find(|i| i.url.ends_with(n))
            .unwrap()
    };
    assert_eq!(st("a.bin").status, Status::Failed);
    assert_eq!(st("a.bin").attempts, 3);
    assert_eq!(st("a.bin").last_error.as_deref(), Some("boom"));
    assert_eq!(st("b.bin").status, Status::Done);
    assert_eq!(st("c.bin").status, Status::Failed);
    // the failed ones sit at the end, in failure order
    assert_eq!(qs.queues[0].items[0].url, "http://h/b.bin");
}

#[tokio::test(start_paused = true)]
async fn a_panicking_job_fails_alone_and_the_queue_continues() {
    struct Panicky(Arc<Stats>);
    impl JobRunner for Panicky {
        fn run(
            &self,
            job: RunJob,
            _s: Arc<dyn Fn(String) + Send + Sync>,
            _c: CancellationToken,
        ) -> BoxFuture<'static, RunResult> {
            let stats = self.0.clone();
            Box::pin(async move {
                stats.starts.lock().unwrap().push((job.url.clone(), 0));
                if job.url.ends_with("0.bin") {
                    panic!("bug in the runner");
                }
                RunResult::Done { path: "/x".into() }
            })
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("queues.json");
    let mut qs = QueueSet::default();
    queue(&mut qs, 1, 3);
    eid_engine::scheduler::save_queues(&path, &qs).unwrap();
    let stats = Arc::new(Stats::default());
    let sched = Scheduler::open(
        SchedulerConfig::new(&path),
        Arc::new(Panicky(stats.clone())),
        Arc::new(SystemClock),
        Arc::new(MockPower::default()),
        Arc::new(|_| {}),
    )
    .unwrap();
    sched.run(CancellationToken::new(), true).await.unwrap();
    let qs = load_queues(&path).unwrap();
    let s: Vec<_> = qs.queues[0]
        .items
        .iter()
        .map(|i| (i.url.clone(), i.status))
        .collect();
    assert_eq!(s[2].1, Status::Failed, "{s:?}"); // the panicker went to the end, failed for good
    assert_eq!(s.iter().filter(|x| x.1 == Status::Done).count(), 2, "{s:?}");
    assert_eq!(
        stats.starts.lock().unwrap().len(),
        3,
        "no retry for a panic"
    );
}

#[tokio::test(start_paused = true)]
async fn credentials_stay_in_memory_and_reach_the_runner() {
    let r = rig(MON, Box::new(done), MockPower::default(), |qs| {
        queue(qs, 1, 0)
    });
    r.sched
        .add_item(
            "main",
            "http://bob:S3CRET-pw@h/secret.bin?token=Q",
            None,
            None,
            None,
        )
        .unwrap();
    r.sched.run(CancellationToken::new(), true).await.unwrap();
    assert_eq!(
        r.stats.auth_seen.lock().unwrap()[0],
        Some(("bob".to_string(), "S3CRET-pw".to_string()))
    );
    let on_disk = std::fs::read_to_string(&r.path).unwrap();
    assert!(
        !on_disk.contains("S3CRET-pw") && !on_disk.contains("bob"),
        "{on_disk}"
    );
    assert!(on_disk.contains("http://h/secret.bin?token=Q"));
    let dbg = format!("{:?}{:?}", r.sched.snapshot(), r.events.lock().unwrap());
    assert!(!dbg.contains("S3CRET-pw"));
}

#[tokio::test(start_paused = true)]
async fn a_restart_demotes_running_items_and_never_reruns_done_ones() {
    let t0 = MON;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("queues.json");
    let mut qs = QueueSet::default();
    queue(&mut qs, 2, 3);
    qs.queues[0].items[0].status = Status::Done;
    qs.queues[0].items[1].status = Status::Running; // the previous process died mid-job
    qs.queues[0].items[1].file_name = Some("known.bin".into());
    eid_engine::scheduler::save_queues(&path, &qs).unwrap();
    let r = rig_at(dir, path, t0, Box::new(done), MockPower::default());
    r.sched.run(CancellationToken::new(), true).await.unwrap();
    let urls: Vec<String> = r
        .stats
        .starts
        .lock()
        .unwrap()
        .iter()
        .map(|s| s.0.clone())
        .collect();
    assert_eq!(urls.len(), 2, "{urls:?}");
    assert!(
        !urls.contains(&"http://h/0.bin".to_string()),
        "a Done item was started again"
    );
    assert!(
        urls.contains(&"http://h/1.bin".to_string())
            && urls.contains(&"http://h/2.bin".to_string())
    );
}

#[tokio::test(start_paused = true)]
async fn pausing_a_running_item_stops_it_and_resume_continues() {
    let r = rig(
        MON,
        Box::new(|_, _| {
            (
                Duration::from_secs(100),
                RunResult::Done { path: "/x".into() },
            )
        }),
        MockPower::default(),
        |qs| queue(qs, 1, 1),
    );
    let (s2, cancel) = (r.sched.clone(), CancellationToken::new());
    let c2 = cancel.clone();
    let h = tokio::spawn(async move { s2.run(c2, false).await });
    tokio::time::sleep(Duration::from_secs(5)).await;
    r.sched.pause_item(0).unwrap();
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert_eq!(r.sched.snapshot().queues[0].items[0].status, Status::Paused);
    assert_eq!(
        r.stats.starts.lock().unwrap().len(),
        1,
        "a paused item is not restarted"
    );
    r.sched.resume_item(0).unwrap();
    tokio::time::sleep(Duration::from_secs(150)).await;
    assert_eq!(r.sched.snapshot().queues[0].items[0].status, Status::Done);
    assert_eq!(r.stats.starts.lock().unwrap().len(), 2);
    cancel.cancel();
    h.await.unwrap().unwrap();
}

// ---------------------------------------------------------------- power countdown

fn with_power(action: PowerAction) -> impl FnOnce(&mut QueueSet) {
    move |qs| {
        queue(qs, 1, 2);
        qs.queues[0].after_done = action;
    }
}

fn ticks(r: &Rig) -> Vec<u64> {
    r.events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|e| match e {
            QueueEvent::CountdownTick { remaining_secs, .. } => Some(*remaining_secs),
            _ => None,
        })
        .collect()
}

#[tokio::test(start_paused = true)]
async fn shutdown_runs_once_after_the_full_countdown_when_nothing_cancels_it() {
    let r = rig(
        MON,
        Box::new(done),
        MockPower::default(),
        with_power(PowerAction::Shutdown),
    );
    let cancel = CancellationToken::new();
    let (s2, c2) = (r.sched.clone(), cancel.clone());
    let h = tokio::spawn(async move { s2.run(c2, true).await });
    h.await.unwrap().unwrap(); // exit_when_idle waits for the countdown too
    assert_eq!(*r.power.calls.lock().unwrap(), vec![PowerAction::Shutdown]);
    let t = ticks(&r);
    assert_eq!(
        (t.first(), t.last(), t.len()),
        (Some(&60), Some(&1), 60),
        "60 one-second ticks"
    );
    // the action did not run early: 2 jobs x 3 s sequential + 60 s countdown
    let elapsed = r.clock.secs() - MON;
    assert!((66..=68).contains(&elapsed), "{elapsed}");
    assert!(r
        .events
        .lock()
        .unwrap()
        .contains(&QueueEvent::PowerExecuted {
            action: PowerAction::Shutdown
        }));
}

#[tokio::test(start_paused = true)]
async fn the_countdown_can_be_cancelled_and_then_nothing_is_executed() {
    let r = rig(
        MON,
        Box::new(done),
        MockPower::default(),
        with_power(PowerAction::Sleep),
    );
    let cancel = CancellationToken::new();
    let (s2, c2) = (r.sched.clone(), cancel.clone());
    let h = tokio::spawn(async move { s2.run(c2, false).await });
    tokio::time::sleep(Duration::from_secs(6 + 20)).await; // jobs done, 20 s into the countdown
    assert!(r.sched.countdown_active());
    assert!(r.sched.cancel_countdown());
    tokio::time::sleep(Duration::from_secs(200)).await;
    assert!(
        r.power.calls.lock().unwrap().is_empty(),
        "the action ran after the user cancelled"
    );
    assert!(!r.sched.countdown_active());
    assert!(r
        .events
        .lock()
        .unwrap()
        .contains(&QueueEvent::CountdownCancelled));
    assert!(!r.sched.cancel_countdown(), "nothing left to cancel");
    // a cancelled countdown does not re-arm itself while nothing changes
    assert!(
        ticks(&r).len() < 40,
        "the countdown kept ticking after it was cancelled"
    );
    cancel.cancel();
    h.await.unwrap().unwrap();
}

#[tokio::test(start_paused = true)]
async fn new_work_during_the_countdown_aborts_it() {
    let r = rig(
        MON,
        Box::new(done),
        MockPower::default(),
        with_power(PowerAction::Shutdown),
    );
    let cancel = CancellationToken::new();
    let (s2, c2) = (r.sched.clone(), cancel.clone());
    let h = tokio::spawn(async move { s2.run(c2, false).await });
    tokio::time::sleep(Duration::from_secs(6 + 10)).await;
    assert!(r.sched.countdown_active());
    r.sched
        .add_item("main", "http://h/late.bin", None, None, None)
        .unwrap();
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(r
        .events
        .lock()
        .unwrap()
        .contains(&QueueEvent::CountdownAborted));
    assert!(r.power.calls.lock().unwrap().is_empty());
    // the new job finishes and a NEW countdown runs to completion
    tokio::time::sleep(Duration::from_secs(80)).await;
    assert_eq!(*r.power.calls.lock().unwrap(), vec![PowerAction::Shutdown]);
    cancel.cancel();
    h.await.unwrap().unwrap();
}

#[tokio::test(start_paused = true)]
async fn no_countdown_without_an_action_for_pending_retries_or_for_a_queue_loaded_finished() {
    // action None
    let r = rig(
        MON,
        Box::new(done),
        MockPower::default(),
        with_power(PowerAction::None),
    );
    r.sched.run(CancellationToken::new(), true).await.unwrap();
    assert!(ticks(&r).is_empty());
    // a failing retry keeps the queue unfinished: no countdown until it has given up
    let script: Box<Script> = Box::new(|_, _| {
        (
            Duration::from_secs(1),
            RunResult::Failed {
                retryable: true,
                message: "x".into(),
            },
        )
    });
    let r = rig(MON, script, MockPower::default(), |qs| {
        with_power(PowerAction::Shutdown)(qs);
        qs.queues[0].retries.max = 1;
        qs.queues[0].retries.delay_secs = 100;
    });
    let cancel = CancellationToken::new();
    let (s2, c2) = (r.sched.clone(), cancel.clone());
    let h = tokio::spawn(async move { s2.run(c2, false).await });
    tokio::time::sleep(Duration::from_secs(50)).await;
    assert!(
        ticks(&r).is_empty(),
        "still retrying: the queue is not finished"
    );
    cancel.cancel();
    h.await.unwrap().unwrap();
    // loaded already finished: nothing ran in this process => no shutdown
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("queues.json");
    let mut qs = QueueSet::default();
    with_power(PowerAction::Shutdown)(&mut qs);
    for i in &mut qs.queues[0].items {
        i.status = Status::Done;
    }
    eid_engine::scheduler::save_queues(&path, &qs).unwrap();
    let r = rig_at(dir, path, MON, Box::new(done), MockPower::default());
    let cancel = CancellationToken::new();
    let (s2, c2) = (r.sched.clone(), cancel.clone());
    let h = tokio::spawn(async move { s2.run(c2, false).await });
    tokio::time::sleep(Duration::from_secs(300)).await;
    assert!(ticks(&r).is_empty() && r.power.calls.lock().unwrap().is_empty());
    cancel.cancel();
    h.await.unwrap().unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_failing_power_action_is_reported_not_swallowed() {
    let r = rig(
        MON,
        Box::new(done),
        MockPower {
            fail: true,
            ..MockPower::default()
        },
        with_power(PowerAction::Hibernate),
    );
    r.sched.run(CancellationToken::new(), true).await.unwrap();
    let ev = r.events.lock().unwrap().clone();
    assert!(ev.iter().any(|e| matches!(e, QueueEvent::PowerFailed { action: PowerAction::Hibernate, message } if message == "denied")), "{ev:?}");
}
