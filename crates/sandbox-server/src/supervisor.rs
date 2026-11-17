//! Background tasks under supervision (D-051, review finding 19).
//!
//! The reconciler and the deliverer used to be plain `tokio::spawn`: a panic ended them
//! silently, nothing restarted them, and `/readyz` stayed green while uncertain operations
//! and webhooks waited forever. Now each runs under [`supervise`]:
//!
//! - a panic, or a return while the server is not stopping, is logged at `ERROR` with the
//!   task's name and restarted after a backoff (1 s doubling to 30 s; reset after a minute
//!   of healthy running);
//! - [`Tasks`] records each task's state, restart count and last heartbeat; a task beats
//!   after each pass ([`Tasks::beat`]);
//! - readiness is red while a task is down (between a failure and its restart) or when its
//!   last heartbeat is older than its staleness bound (a task stuck rather than dead).
//!
//! On shutdown a task is never restarted.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde::Serialize;
use tokio::sync::watch;
use tokio::time::Instant;

/// First restart delay.
pub const FIRST_BACKOFF: Duration = Duration::from_secs(1);
/// Longest restart delay.
pub const MAX_BACKOFF: Duration = Duration::from_secs(30);
/// A run this long resets the backoff.
pub const HEALTHY_RUN: Duration = Duration::from_secs(60);

#[derive(Debug, Clone)]
struct Health {
    running: bool,
    restarts: u64,
    last_failure: Option<&'static str>,
    last_beat: Instant,
    stale_after: Duration,
}

/// One task's state, as readiness reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskStatus {
    /// The task.
    pub name: &'static str,
    /// True while an instance is running.
    pub running: bool,
    /// Restarts since the server started.
    pub restarts: u64,
    /// `panicked` or `returned`, for the last failure.
    pub last_failure: Option<&'static str>,
    /// Milliseconds since the last heartbeat.
    pub since_beat_ms: u64,
    /// True when the heartbeat is older than the task's bound.
    pub stale: bool,
}

/// The registry of supervised tasks (shared by the supervisor and readiness).
#[derive(Debug, Default)]
pub struct Tasks(Mutex<BTreeMap<&'static str, Health>>);

impl Tasks {
    /// Registers `name`; readiness turns red if it misses heartbeats for `stale_after`.
    pub fn register(&self, name: &'static str, stale_after: Duration) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(
                name,
                Health {
                    running: false,
                    restarts: 0,
                    last_failure: None,
                    last_beat: Instant::now(),
                    stale_after,
                },
            );
    }

    /// Records a completed pass of `name`.
    pub fn beat(&self, name: &'static str) {
        if let Some(health) = self
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_mut(name)
        {
            health.last_beat = Instant::now();
        }
    }

    fn update(&self, name: &'static str, change: impl FnOnce(&mut Health)) {
        if let Some(health) = self
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_mut(name)
        {
            change(health);
        }
    }

    /// Every task's state.
    pub fn statuses(&self) -> Vec<TaskStatus> {
        let now = Instant::now();
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|(name, h)| {
                let since = now.saturating_duration_since(h.last_beat);
                TaskStatus {
                    name,
                    running: h.running,
                    restarts: h.restarts,
                    last_failure: h.last_failure,
                    since_beat_ms: u64::try_from(since.as_millis()).unwrap_or(u64::MAX),
                    stale: since > h.stale_after,
                }
            })
            .collect()
    }

    /// The first task that makes the server not ready (down, or stale), if any.
    pub fn unhealthy(&self) -> Option<TaskStatus> {
        self.statuses()
            .into_iter()
            .find(|status| !status.running || status.stale)
    }
}

/// Runs `make()` as `name` until `shutdown` turns true, restarting it after a panic or an
/// unexpected return. The returned future ends only on shutdown.
pub async fn supervise<F, Fut>(
    tasks: Arc<Tasks>,
    name: &'static str,
    mut shutdown: watch::Receiver<bool>,
    make: F,
) where
    F: Fn() -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let mut backoff = FIRST_BACKOFF;
    loop {
        tasks.update(name, |h| {
            h.running = true;
            h.last_beat = Instant::now();
        });
        let started = Instant::now();
        let outcome = tokio::spawn(make()).await;
        if *shutdown.borrow() {
            tasks.update(name, |h| h.running = false);
            return;
        }
        let failure = match outcome {
            Ok(()) => "returned",
            Err(error) if error.is_panic() => "panicked",
            Err(_) => "cancelled",
        };
        tracing::error!(
            task = name,
            failure,
            restart_in_ms = u64::try_from(backoff.as_millis()).unwrap_or(u64::MAX),
            "background task stopped; restarting"
        );
        tasks.update(name, |h| {
            h.running = false;
            h.restarts = h.restarts.saturating_add(1);
            h.last_failure = Some(failure);
        });
        if started.elapsed() >= HEALTHY_RUN {
            backoff = FIRST_BACKOFF;
        }
        tokio::select! {
            () = tokio::time::sleep(backoff) => {}
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return;
                }
            }
        }
        backoff = backoff.saturating_mul(2).min(MAX_BACKOFF);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    #[tokio::test(start_paused = true)]
    async fn a_panicking_task_is_restarted_with_backoff_and_reported() {
        let tasks = Arc::new(Tasks::default());
        tasks.register("worker", Duration::from_secs(3_600));
        let (stop, shutdown) = watch::channel(false);
        let runs = Arc::new(AtomicU32::new(0));
        let counted = runs.clone();
        let supervisor = tokio::spawn(supervise(tasks.clone(), "worker", shutdown, move || {
            let runs = counted.clone();
            async move {
                // Panics on its first two runs, then runs until shutdown.
                assert!(runs.fetch_add(1, Ordering::SeqCst) >= 2, "injected");
                std::future::pending::<()>().await;
            }
        }));
        tokio::time::sleep(Duration::from_millis(100)).await;
        let down = tasks.unhealthy().unwrap();
        assert_eq!(
            (down.running, down.restarts, down.last_failure),
            (false, 1, Some("panicked"))
        );
        tokio::time::sleep(FIRST_BACKOFF).await;
        // Restarted, panicked again, now waiting twice as long.
        assert_eq!(tasks.statuses()[0].restarts, 2);
        tokio::time::sleep(FIRST_BACKOFF).await;
        assert!(tasks.unhealthy().is_some(), "still in its 2 s backoff");
        tokio::time::sleep(FIRST_BACKOFF + Duration::from_millis(100)).await;
        assert_eq!(runs.load(Ordering::SeqCst), 3);
        assert!(tasks.unhealthy().is_none(), "{:?}", tasks.statuses());
        stop.send(true).unwrap();
        supervisor.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn a_task_that_stops_beating_makes_the_server_not_ready() {
        let tasks = Tasks::default();
        tasks.register("sweeper", Duration::from_secs(10));
        tasks.update("sweeper", |h| h.running = true);
        tasks.beat("sweeper");
        assert!(tasks.unhealthy().is_none());
        tokio::time::sleep(Duration::from_secs(11)).await;
        assert!(tasks.unhealthy().is_some_and(|s| s.stale));
        tasks.beat("sweeper");
        assert!(tasks.unhealthy().is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn nothing_is_restarted_on_shutdown() {
        let tasks = Arc::new(Tasks::default());
        tasks.register("worker", Duration::from_secs(60));
        let (stop, shutdown) = watch::channel(false);
        let runs = Arc::new(AtomicU32::new(0));
        let (counted, stopping) = (runs.clone(), shutdown.clone());
        let supervisor = tokio::spawn(supervise(tasks.clone(), "worker", shutdown, move || {
            let (runs, mut stopping) = (counted.clone(), stopping.clone());
            async move {
                runs.fetch_add(1, Ordering::SeqCst);
                let _ = stopping.changed().await;
            }
        }));
        tokio::time::sleep(Duration::from_millis(10)).await;
        stop.send(true).unwrap();
        supervisor.await.unwrap();
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        assert!(!tasks.statuses()[0].running);
    }
}
