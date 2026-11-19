//! The queue driver: starts what `QueueSet::plan` says, stops what a closed window forbids,
//! re-queues failures, persists every transition to `queues.json`, and runs the cancellable
//! "after the queue" power countdown. Time comes from a [`Clock`] and tokio's timers, so the tests
//! run on a virtual clock; the real downloads come from a [`JobRunner`] (the engine in
//! production, a fake in the timing tests).

use crate::error::{Error, Result};
use crate::queue::{Item, PowerAction, QueueSet, Status, QUEUES_VERSION};
use crate::store;
use eid_platform::time::LocalNow;
use futures_util::future::{BoxFuture, FutureExt};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

// ------------------------------------------------------------------ seams

pub trait Clock: Send + Sync {
    fn now(&self) -> LocalNow;
}

pub struct SystemClock;
impl Clock for SystemClock {
    fn now(&self) -> LocalNow {
        eid_platform::time::local_now()
    }
}

/// Suspend / shut down the machine. The real implementation lives in `eid-platform` (P3); the
/// scheduler only ever talks to this trait, so the countdown logic is tested with a mock.
pub trait PowerControl: Send + Sync {
    fn execute(&self, action: PowerAction) -> std::result::Result<(), String>;
}

/// Refuses everything (used where no platform implementation exists yet).
pub struct NoPower;
impl PowerControl for NoPower {
    fn execute(&self, action: PowerAction) -> std::result::Result<(), String> {
        Err(format!(
            "power action {action:?} is not available in this build"
        ))
    }
}

#[derive(Clone)]
pub struct RunJob {
    /// The queue item this job runs (lets a progress sink tell items apart).
    pub item: u64,
    /// Credential-free URL.
    pub url: String,
    /// In memory only; never written anywhere by the scheduler.
    pub auth: Option<(String, String)>,
    /// The queue's download directory (chosen by the user).
    pub base_dir: PathBuf,
    /// A validated single plain component below `base_dir` (from an import, a category...).
    pub subdir: Option<String>,
    /// Last known/requested file name (names the engine sidecars).
    pub file_name: Option<String>,
    /// v0.2: this download's own limit in bytes/s (under the global limiter), `None` = none.
    pub limit_bps: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunResult {
    Done { path: PathBuf },
    Cancelled,
    Failed { retryable: bool, message: String },
}

pub trait JobRunner: Send + Sync + 'static {
    /// Run (or resume) one job. `started(name)` reports the file name as soon as it is known so
    /// it can be persisted before the transfer is far along.
    fn run(
        &self,
        job: RunJob,
        started: Arc<dyn Fn(String) + Send + Sync>,
        cancel: CancellationToken,
    ) -> BoxFuture<'static, RunResult>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReason {
    WindowClosed,
    UserPause,
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueueEvent {
    ItemStarted {
        queue: String,
        item: u64,
    },
    ItemDone {
        queue: String,
        item: u64,
    },
    ItemRetry {
        queue: String,
        item: u64,
        attempt: u32,
        delay_secs: u64,
    },
    ItemFailed {
        queue: String,
        item: u64,
        message: String,
    },
    ItemStopped {
        queue: String,
        item: u64,
        reason: StopReason,
    },
    QueueFinished {
        queue: String,
    },
    CountdownTick {
        remaining_secs: u64,
        action: PowerAction,
    },
    CountdownCancelled,
    /// New work appeared (or the queue stopped being finished) before the action ran.
    CountdownAborted,
    PowerExecuted {
        action: PowerAction,
    },
    PowerFailed {
        action: PowerAction,
        message: String,
    },
}

pub type QueueSink = Arc<dyn Fn(QueueEvent) + Send + Sync>;

#[derive(Clone)]
pub struct SchedulerConfig {
    pub path: PathBuf,
    /// Seconds of the cancellable countdown before a power action (the design says 60).
    pub countdown_secs: u64,
    /// Longest sleep between two looks at the clock (window edges may be missed by DST jumps).
    pub max_idle: Duration,
}

impl SchedulerConfig {
    pub fn new(path: &Path) -> Self {
        SchedulerConfig {
            path: path.to_path_buf(),
            countdown_secs: 60,
            max_idle: Duration::from_secs(30),
        }
    }
}

// ------------------------------------------------------------------ files

/// Load `queues.json` (a missing file is an empty set). The result is validated.
pub fn load_queues(path: &Path) -> Result<QueueSet> {
    let v = store::load::<QueueSet>(path, QUEUES_VERSION, &|q| q.validate())?;
    Ok(v.unwrap_or_default())
}

pub fn save_queues(path: &Path, qs: &QueueSet) -> Result<()> {
    qs.validate().map_err(Error::InvalidInput)?;
    store::save(path, QUEUES_VERSION, qs)
}

// ------------------------------------------------------------------ scheduler

struct Running {
    cancel: CancellationToken,
    reason: Option<StopReason>,
}

struct Inner {
    cfg: SchedulerConfig,
    qs: Mutex<QueueSet>,
    runner: Arc<dyn JobRunner>,
    clock: Arc<dyn Clock>,
    power: Arc<dyn PowerControl>,
    sink: QueueSink,
    notify: Notify,
    dirty: AtomicBool,
    running: Mutex<HashMap<u64, Running>>,
    auth: Mutex<HashMap<u64, (String, String)>>,
    /// Queues that did work since they last finished: only those may arm a power countdown, so a
    /// finished queue loaded from disk never shuts the computer down by itself.
    armed: Mutex<HashSet<String>>,
    countdown: Mutex<Option<CancellationToken>>,
    persist_lock: tokio::sync::Mutex<()>,
}

#[derive(Clone)]
pub struct Scheduler {
    inner: Arc<Inner>,
}

impl Scheduler {
    /// Open (or create) the queue file. Items that were `Running` when the previous process
    /// died are demoted to `Pending`; their engine sidecars resume them.
    pub fn open(
        cfg: SchedulerConfig,
        runner: Arc<dyn JobRunner>,
        clock: Arc<dyn Clock>,
        power: Arc<dyn PowerControl>,
        sink: QueueSink,
    ) -> Result<Scheduler> {
        let mut qs = load_queues(&cfg.path)?;
        let demoted = qs.demote_running();
        let s = Scheduler {
            inner: Arc::new(Inner {
                cfg,
                qs: Mutex::new(qs),
                runner,
                clock,
                power,
                sink,
                notify: Notify::new(),
                dirty: AtomicBool::new(demoted > 0),
                running: Mutex::new(HashMap::new()),
                auth: Mutex::new(HashMap::new()),
                armed: Mutex::new(HashSet::new()),
                countdown: Mutex::new(None),
                persist_lock: tokio::sync::Mutex::new(()),
            }),
        };
        Ok(s)
    }

    pub fn snapshot(&self) -> QueueSet {
        self.inner.qs.lock().unwrap().clone()
    }

    /// Apply a validated mutation to the set (queues, items, settings). The change is persisted
    /// by the loop (or by [`flush`](Self::flush)) and the loop is woken.
    pub fn edit<T>(
        &self,
        f: impl FnOnce(&mut QueueSet) -> std::result::Result<T, String>,
    ) -> Result<T> {
        let mut g = self.inner.qs.lock().unwrap();
        let mut copy = g.clone();
        // A panic inside the caller's closure must not poison the queue lock (it would turn every
        // later request into a panic): catch it here, while the guard is still held and intact.
        // `copy` is thrown away on a panic, so the real queues are untouched.
        let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&mut copy)))
            .map_err(|_| Error::InvalidInput("internal error while editing the queues".into()))?
            .map_err(Error::InvalidInput)?;
        copy.validate().map_err(Error::InvalidInput)?;
        // queues that gained pending work are armed again
        let mut armed = self.inner.armed.lock().unwrap();
        for q in &copy.queues {
            if q.items
                .iter()
                .any(|i| matches!(i.status, Status::Pending | Status::Running))
            {
                armed.insert(q.name.clone());
            }
        }
        *g = copy;
        drop(g);
        drop(armed);
        self.inner.dirty.store(true, Ordering::SeqCst);
        self.inner.notify.notify_one();
        Ok(out)
    }

    /// Add one item. `auth` is kept in memory only.
    pub fn add_item(
        &self,
        queue: &str,
        url: &str,
        subdir: Option<&str>,
        file_name: Option<&str>,
        not_before: Option<i64>,
    ) -> Result<u64> {
        let (clean, creds) = crate::urlsafe::parse_and_strip(url).map_err(Error::InvalidInput)?;
        let id =
            self.edit(|qs| qs.add_item(queue, clean.as_str(), subdir, file_name, not_before))?;
        if let Some(c) = creds {
            self.inner.auth.lock().unwrap().insert(id, c);
        }
        Ok(id)
    }

    pub fn pause_item(&self, id: u64) -> Result<()> {
        if let Some(r) = self.inner.running.lock().unwrap().get_mut(&id) {
            r.reason = Some(StopReason::UserPause);
            r.cancel.cancel();
        }
        self.edit(|qs| {
            for q in &mut qs.queues {
                if let Some(i) = q.items.iter_mut().find(|i| i.id == id) {
                    if i.status == Status::Pending {
                        i.status = Status::Paused;
                    }
                    return Ok(());
                }
            }
            Err("no such item".into())
        })
    }

    pub fn resume_item(&self, id: u64) -> Result<()> {
        self.edit(|qs| {
            for q in &mut qs.queues {
                if let Some(i) = q.items.iter_mut().find(|i| i.id == id) {
                    if i.status == Status::Paused || i.status == Status::Failed {
                        i.status = Status::Pending;
                        i.attempts = 0;
                        i.not_before = None;
                    }
                    return Ok(());
                }
            }
            Err("no such item".into())
        })
    }

    /// Cancel a running power countdown. `true` if one was running.
    pub fn cancel_countdown(&self) -> bool {
        match self.inner.countdown.lock().unwrap().take() {
            Some(t) => {
                t.cancel();
                true
            }
            None => false,
        }
    }

    pub fn countdown_active(&self) -> bool {
        self.inner.countdown.lock().unwrap().is_some()
    }

    /// Persist now (tests and the CLI "add" path use this; the loop does it by itself).
    pub async fn flush(&self) -> Result<()> {
        self.inner.persist().await
    }

    /// Run until `cancel` fires (or, with `exit_when_idle`, until nothing is left to do).
    /// Running jobs are cancelled on exit and stay `Pending`, resumable from their sidecars.
    pub async fn run(&self, cancel: CancellationToken, exit_when_idle: bool) -> Result<()> {
        let inner = &self.inner;
        let mut set: JoinSet<(u64, RunResult)> = JoinSet::new();
        loop {
            let now = inner.clock.now();
            let plan = inner.qs.lock().unwrap().plan(&now);
            for s in &plan.stop {
                inner.request_stop(s.item, StopReason::WindowClosed);
            }
            for s in &plan.start {
                inner.start_job(s.queue, s.item, &mut set);
            }
            if inner.dirty.swap(false, Ordering::SeqCst) {
                inner.persist().await?;
            }
            inner.check_finished(self);
            if exit_when_idle
                && set.is_empty()
                && !inner.qs.lock().unwrap().has_pending_work()
                && !self.countdown_active()
            {
                inner.persist().await?;
                return Ok(());
            }
            let idle = match plan.wake_at {
                Some(t) => Duration::from_secs(u64::try_from((t - now.unix).max(0)).unwrap_or(0)),
                None => inner.cfg.max_idle,
            }
            .min(inner.cfg.max_idle);
            tokio::select! {
                _ = cancel.cancelled() => {
                    inner.stop_all(StopReason::Shutdown);
                    while let Some(r) = set.join_next().await {
                        inner.job_ended(r);
                    }
                    inner.persist().await?;
                    if let Some(t) = inner.countdown.lock().unwrap().take() { t.cancel(); }
                    return Ok(());
                }
                _ = inner.notify.notified() => {}
                Some(r) = set.join_next(), if !set.is_empty() => inner.job_ended(r),
                _ = tokio::time::sleep(idle) => {}
            }
        }
    }
}

impl Inner {
    async fn persist(&self) -> Result<()> {
        let _g = self.persist_lock.lock().await;
        let snapshot = self.qs.lock().unwrap().clone();
        let path = self.cfg.path.clone();
        tokio::task::spawn_blocking(move || save_queues(&path, &snapshot))
            .await
            .map_err(|e| Error::Network(format!("persist task failed: {e}")))?
    }

    fn emit(&self, e: QueueEvent) {
        (self.sink)(e)
    }

    fn request_stop(&self, item: u64, reason: StopReason) {
        if let Some(r) = self.running.lock().unwrap().get_mut(&item) {
            if r.reason.is_none() {
                r.reason = Some(reason);
            }
            r.cancel.cancel();
        }
    }

    fn stop_all(&self, reason: StopReason) {
        for r in self.running.lock().unwrap().values_mut() {
            if r.reason.is_none() {
                r.reason = Some(reason.clone());
            }
            r.cancel.cancel();
        }
    }

    fn start_job(
        self: &Arc<Self>,
        queue_idx: usize,
        item_id: u64,
        set: &mut JoinSet<(u64, RunResult)>,
    ) {
        // re-validate under the lock: only a still-pending item of a still-existing queue starts
        let (job, qname) = {
            let mut qs = self.qs.lock().unwrap();
            let Some(q) = qs.queues.get_mut(queue_idx) else {
                return;
            };
            if q.running() >= q.max_parallel as usize {
                return;
            }
            let qname = q.name.clone();
            let base_dir = PathBuf::from(&q.dir);
            let Some(it) = q
                .items
                .iter_mut()
                .find(|i| i.id == item_id && i.status == Status::Pending)
            else {
                return;
            };
            it.status = Status::Running;
            (
                RunJob {
                    item: item_id,
                    url: it.url.clone(),
                    auth: self.auth.lock().unwrap().get(&item_id).cloned(),
                    base_dir,
                    subdir: it.subdir.clone(),
                    file_name: it.file_name.clone(),
                    limit_bps: it.limit_kib.map(|k| k.saturating_mul(1024)),
                },
                qname,
            )
        };
        self.armed.lock().unwrap().insert(qname.clone());
        self.dirty.store(true, Ordering::SeqCst);
        let cancel = CancellationToken::new();
        self.running.lock().unwrap().insert(
            item_id,
            Running {
                cancel: cancel.clone(),
                reason: None,
            },
        );
        self.emit(QueueEvent::ItemStarted {
            queue: qname,
            item: item_id,
        });
        let me = self.clone();
        let started: Arc<dyn Fn(String) + Send + Sync> = Arc::new(move |name: String| {
            let mut changed = false;
            {
                let mut qs = me.qs.lock().unwrap();
                for q in &mut qs.queues {
                    if let Some(i) = q.items.iter_mut().find(|i| i.id == item_id) {
                        if i.file_name.as_deref() != Some(name.as_str())
                            && crate::sanitize::is_safe_stored_name(&name)
                        {
                            i.file_name = Some(name.clone());
                            changed = true;
                        }
                    }
                }
            }
            if changed {
                me.dirty.store(true, Ordering::SeqCst);
                me.notify.notify_one();
            }
        });
        let fut = self.runner.run(job, started, cancel);
        set.spawn(async move {
            let r = std::panic::AssertUnwindSafe(fut)
                .catch_unwind()
                .await
                .unwrap_or(RunResult::Failed {
                    retryable: false,
                    message: "internal error: the download task panicked".into(),
                });
            (item_id, r)
        });
    }

    fn job_ended(&self, joined: std::result::Result<(u64, RunResult), tokio::task::JoinError>) {
        let Ok((item_id, result)) = joined else {
            return; // a join error means the task was aborted: nothing to record
        };
        let reason = self
            .running
            .lock()
            .unwrap()
            .remove(&item_id)
            .and_then(|r| r.reason);
        let now = self.clock.now();
        let mut events = vec![];
        {
            let mut qs = self.qs.lock().unwrap();
            for q in &mut qs.queues {
                let Some(pos) = q.items.iter().position(|i| i.id == item_id) else {
                    continue;
                };
                let policy = q.retries;
                let qname = q.name.clone();
                match result.clone() {
                    RunResult::Done { .. } => {
                        let it = &mut q.items[pos];
                        it.status = Status::Done;
                        it.last_error = None;
                        events.push(QueueEvent::ItemDone {
                            queue: qname,
                            item: item_id,
                        });
                    }
                    RunResult::Cancelled => {
                        let it = &mut q.items[pos];
                        let r = reason.clone().unwrap_or(StopReason::Shutdown);
                        it.status = if r == StopReason::UserPause {
                            Status::Paused
                        } else {
                            Status::Pending
                        };
                        events.push(QueueEvent::ItemStopped {
                            queue: qname,
                            item: item_id,
                            reason: r,
                        });
                    }
                    RunResult::Failed { retryable, message } => {
                        let mut it: Item = q.items.remove(pos);
                        it.attempts += 1;
                        it.last_error = Some(message.chars().take(1000).collect());
                        if retryable && it.attempts <= policy.max {
                            let delay = policy.delay_secs.saturating_mul(u64::from(it.attempts));
                            it.status = Status::Pending;
                            it.not_before = Some(
                                now.unix
                                    .saturating_add(i64::try_from(delay).unwrap_or(i64::MAX)),
                            );
                            events.push(QueueEvent::ItemRetry {
                                queue: qname,
                                item: item_id,
                                attempt: it.attempts,
                                delay_secs: delay,
                            });
                        } else {
                            it.status = Status::Failed;
                            events.push(QueueEvent::ItemFailed {
                                queue: qname,
                                item: item_id,
                                message: message.chars().take(200).collect(),
                            });
                        }
                        q.items.push(it); // a failed job goes to the END of its queue
                    }
                }
                break;
            }
        }
        self.dirty.store(true, Ordering::SeqCst);
        for e in events {
            self.emit(e);
        }
        self.notify.notify_one();
    }

    /// Arm the countdown when an engaged queue just finished.
    fn check_finished(self: &Arc<Self>, sched: &Scheduler) {
        if self.countdown.lock().unwrap().is_some() {
            return;
        }
        let finished: Vec<(String, PowerAction)> = {
            let qs = self.qs.lock().unwrap();
            let armed = self.armed.lock().unwrap();
            qs.queues
                .iter()
                .filter(|q| q.is_finished() && q.running() == 0 && armed.contains(&q.name))
                .map(|q| (q.name.clone(), q.after_done))
                .collect()
        };
        for (name, action) in finished {
            self.armed.lock().unwrap().remove(&name);
            self.emit(QueueEvent::QueueFinished { queue: name });
            if action != PowerAction::None {
                let token = CancellationToken::new();
                *self.countdown.lock().unwrap() = Some(token.clone());
                let me = self.clone();
                let sched = sched.clone();
                tokio::spawn(async move { me.run_countdown(sched, action, token).await });
                break; // one countdown at a time
            }
        }
    }

    async fn run_countdown(
        self: Arc<Self>,
        sched: Scheduler,
        action: PowerAction,
        token: CancellationToken,
    ) {
        let total = self.cfg.countdown_secs;
        let clear = |me: &Inner| *me.countdown.lock().unwrap() = None;
        for remaining in (1..=total).rev() {
            self.emit(QueueEvent::CountdownTick {
                remaining_secs: remaining,
                action,
            });
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(1)) => {}
                _ = token.cancelled() => {
                    clear(&self);
                    self.emit(QueueEvent::CountdownCancelled);
                    sched.inner.notify.notify_one();
                    return;
                }
            }
            if self.qs.lock().unwrap().has_pending_work() {
                clear(&self);
                self.emit(QueueEvent::CountdownAborted);
                sched.inner.notify.notify_one();
                return;
            }
        }
        // last look before the irreversible step
        if token.is_cancelled() {
            clear(&self);
            self.emit(QueueEvent::CountdownCancelled);
            return;
        }
        if self.qs.lock().unwrap().has_pending_work() {
            clear(&self);
            self.emit(QueueEvent::CountdownAborted);
            return;
        }
        let power = self.power.clone();
        let r = tokio::task::spawn_blocking(move || power.execute(action))
            .await
            .unwrap_or_else(|e| Err(format!("power task failed: {e}")));
        clear(&self);
        match r {
            Ok(()) => self.emit(QueueEvent::PowerExecuted { action }),
            Err(message) => self.emit(QueueEvent::PowerFailed { action, message }),
        }
        sched.inner.notify.notify_one();
    }
}
