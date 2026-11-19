//! v0.2 "when the downloads are finished": the action of a queue (quit, sleep, hibernate, shut
//! down) after the engine's cancellable 60-second countdown (`eid_engine::scheduler`).
//!
//! Safety rules: the action is ONE-SHOT: every queue's "when finished" is reset to nothing and
//! saved BEFORE the action runs (and when the countdown is cancelled), so a later session can
//! never shut the computer down by surprise. The countdown shows in the window with a Cancel
//! button and is aborted by the engine as soon as new work appears.

use crate::core::power_name;
use eid_engine::queue::PowerAction;
use eid_engine::scheduler::{PowerControl, QueueEvent, Scheduler};
use serde::Serialize;
use std::sync::{Arc, Mutex, OnceLock};
use tauri::AppHandle;

/// What the window shows (part of every `list` answer).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct PowerView {
    /// `(seconds left, action)` while a countdown runs.
    pub countdown: Option<(u64, &'static str)>,
    /// The last outcome, for a message: `cancelled`, `aborted`, `done:<action>` or
    /// `failed:<action>` (cleared by the window when shown).
    pub last: Option<String>,
}

impl PowerView {
    /// Follow the engine's events (pure, unit tested).
    pub fn apply(&mut self, ev: &QueueEvent) {
        match ev {
            QueueEvent::CountdownTick {
                remaining_secs,
                action,
            } => self.countdown = Some((*remaining_secs, power_name(*action))),
            QueueEvent::CountdownCancelled => {
                self.countdown = None;
                self.last = Some("cancelled".into());
            }
            QueueEvent::CountdownAborted => {
                self.countdown = None;
                self.last = Some("aborted".into());
            }
            QueueEvent::PowerExecuted { action } => {
                self.countdown = None;
                self.last = Some(format!("done:{}", power_name(*action)));
            }
            QueueEvent::PowerFailed { action, .. } => {
                self.countdown = None;
                self.last = Some(format!("failed:{}", power_name(*action)));
            }
            _ => {}
        }
    }
}

/// Set every queue's "when finished" back to nothing (one-shot rule).
pub fn reset_all(qs: &mut eid_engine::queue::QueueSet) {
    for q in &mut qs.queues {
        q.after_done = PowerAction::None;
    }
}

/// One-shot rule: reset every queue's "when finished" and wait until it is SAVED. Called on a
/// blocking thread of the runtime (the scheduler runs power actions with `spawn_blocking`).
/// Tauri-free, so it is unit tested without a window (ci-windows run 44573561513: a test that
/// touched the app handle linked the window libraries and the test program could not start).
pub fn reset_and_save(s: &Scheduler) -> Result<(), String> {
    s.edit(|qs| {
        reset_all(qs);
        Ok(())
    })
    .map_err(|e| e.to_string())?;
    tokio::runtime::Handle::current()
        .block_on(s.flush())
        .map_err(|e| e.to_string())
}

/// The app's [`PowerControl`]: knows the app (for "quit") and the scheduler (to reset and save
/// the one-shot choice first). Both are attached once they exist.
#[derive(Default)]
pub struct GuiPower {
    pub app: OnceLock<AppHandle>,
    pub sched: OnceLock<Scheduler>,
}

impl PowerControl for GuiPower {
    fn execute(&self, action: PowerAction) -> Result<(), String> {
        // one-shot: reset and SAVE before anything irreversible happens
        if let Some(s) = self.sched.get() {
            reset_and_save(s)?;
        }
        let io = |r: std::io::Result<()>| r.map_err(|e| e.to_string());
        match action {
            PowerAction::None => Ok(()),
            PowerAction::Exit => {
                let app = self.app.get().ok_or("the window is not ready")?;
                app.exit(0);
                Ok(())
            }
            PowerAction::Sleep => io(eid_platform::power::sleep()),
            PowerAction::Hibernate => io(eid_platform::power::hibernate()),
            PowerAction::Shutdown => io(eid_platform::power::shutdown()),
        }
    }
}

/// The scheduler's event sink: keeps `view` current and resets the one-shot choice when the
/// user cancels the countdown.
pub fn sink(
    view: Arc<Mutex<PowerView>>,
    sched: Arc<OnceLock<Scheduler>>,
) -> Arc<dyn Fn(QueueEvent) + Send + Sync> {
    Arc::new(move |ev: QueueEvent| {
        view.lock().unwrap_or_else(|p| p.into_inner()).apply(&ev);
        if matches!(ev, QueueEvent::CountdownCancelled) {
            if let Some(s) = sched.get() {
                let s = s.clone();
                // not from inside the scheduler's own call: a separate task
                tauri::async_runtime::spawn(async move {
                    let _ = s.edit(|qs| {
                        reset_all(qs);
                        Ok(())
                    });
                });
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_follows_the_countdown_and_its_outcome() {
        let mut v = PowerView::default();
        v.apply(&QueueEvent::CountdownTick {
            remaining_secs: 60,
            action: PowerAction::Shutdown,
        });
        assert_eq!(v.countdown, Some((60, "shutdown")));
        v.apply(&QueueEvent::CountdownTick {
            remaining_secs: 59,
            action: PowerAction::Shutdown,
        });
        assert_eq!(v.countdown, Some((59, "shutdown")));
        v.apply(&QueueEvent::CountdownCancelled);
        assert_eq!((v.countdown, v.last.as_deref()), (None, Some("cancelled")));
        v.apply(&QueueEvent::PowerFailed {
            action: PowerAction::Hibernate,
            message: "off".into(),
        });
        assert_eq!(v.last.as_deref(), Some("failed:hibernate"));
        v.apply(&QueueEvent::CountdownAborted);
        assert_eq!(v.last.as_deref(), Some("aborted"));
        let mut qs = eid_engine::queue::QueueSet::default();
        let mut q = eid_engine::queue::Queue::new("main", "/d");
        q.after_done = PowerAction::Shutdown;
        qs.queues.push(q);
        reset_all(&mut qs);
        assert_eq!(qs.queues[0].after_done, PowerAction::None);
    }

    /// The action resets and SAVES every queue's choice first, from the scheduler's blocking
    /// thread (as `run_countdown` calls it), before doing anything. "None" does nothing else.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_choice_is_reset_and_saved_before_the_action_runs() {
        use eid_engine::scheduler::{
            load_queues, save_queues, NoPower, SchedulerConfig, SystemClock,
        };
        let d = tempfile::tempdir().unwrap();
        let qpath = d.path().join("queues.json");
        let mut qs = eid_engine::queue::QueueSet::default();
        let mut q = eid_engine::queue::Queue::new("main", d.path().to_str().unwrap());
        q.after_done = PowerAction::Shutdown;
        qs.queues.push(q);
        save_queues(&qpath, &qs).unwrap();
        struct Never;
        impl eid_engine::scheduler::JobRunner for Never {
            fn run(
                &self,
                _: eid_engine::scheduler::RunJob,
                _: Arc<dyn Fn(String) + Send + Sync>,
                _: eid_engine::CancellationToken,
            ) -> futures_util::future::BoxFuture<'static, eid_engine::scheduler::RunResult>
            {
                Box::pin(async { eid_engine::scheduler::RunResult::Cancelled })
            }
        }
        let sched = Scheduler::open(
            SchedulerConfig::new(&qpath),
            Arc::new(Never),
            Arc::new(SystemClock),
            Arc::new(NoPower),
            Arc::new(|_| {}),
        )
        .unwrap();
        let s2 = sched.clone();
        tokio::task::spawn_blocking(move || reset_and_save(&s2))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            load_queues(&qpath).unwrap().queues[0].after_done,
            PowerAction::None,
            "the reset was not saved"
        );
    }
}
