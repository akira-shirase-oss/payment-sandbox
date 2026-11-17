//! Bounded report work (D-047, review finding 9).
//!
//! Reports (the reconciliation, explain-balance pages) scan a whole epoch, and with
//! `network=true` page through the network's listing too, so they are admitted by budget
//! rather than first come, first served:
//!
//! - **per project, a token bucket** (`PS_REPORT_BURST` units, refilled at
//!   `PS_REPORT_REFILL_PER_MINUTE`): an explain page costs [`EXPLAIN_PAGE`], a
//!   reconciliation [`RECONCILIATION`], one with network agreement [`NETWORK_AGREEMENT`].
//!   Past it: `429 rate_limited` with `Retry-After`.
//! - **per project, at most `PS_REPORT_PER_PROJECT` reports at once**, and **at most
//!   `PS_REPORT_CONCURRENCY` across the server**, so one project's scans can neither hold
//!   every database connection nor queue behind another's.
//!
//! The project map is bounded ([`MAX_PROJECTS`]): idle, refilled buckets are evicted first,
//! since a fresh bucket starts full and evicting one loses nothing.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::Instant;
use uuid::Uuid;

use crate::error::ApiError;

/// Cost of one explain page.
pub const EXPLAIN_PAGE: u64 = 1;
/// Cost of one reconciliation report.
pub const RECONCILIATION: u64 = 10;
/// Cost of one reconciliation report with network agreement (`network=true`).
pub const NETWORK_AGREEMENT: u64 = 50;
/// Most projects tracked at once.
pub const MAX_PROJECTS: usize = 65_536;

const MILLI: u64 = 1_000;

#[derive(Debug, Clone, Copy)]
struct Bucket {
    /// Thousandths of a unit, so slow refills accrue exactly.
    milli: u64,
    at: Instant,
    in_flight: u32,
}

/// The server's report admission.
#[derive(Debug)]
pub struct ReportGate {
    global: Arc<Semaphore>,
    projects: Mutex<HashMap<Uuid, Bucket>>,
    burst_milli: u64,
    /// Thousandths of a unit per second.
    refill_milli_per_sec: u64,
    per_project: u32,
}

/// A report admitted; dropping it releases its places (not its cost).
#[derive(Debug)]
pub struct ReportPermit {
    gate: Arc<ReportGate>,
    project: Uuid,
    _global: OwnedSemaphorePermit,
}

impl Drop for ReportPermit {
    fn drop(&mut self) {
        let mut projects = self
            .gate
            .projects
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(bucket) = projects.get_mut(&self.project) {
            bucket.in_flight = bucket.in_flight.saturating_sub(1);
        }
    }
}

impl ReportGate {
    /// A gate with `burst` units per project, refilled at `per_minute`, at most
    /// `per_project` reports at once per project and `concurrency` across the server.
    pub fn new(burst: u64, per_minute: u64, per_project: u32, concurrency: usize) -> Self {
        Self {
            global: Arc::new(Semaphore::new(concurrency.max(1))),
            projects: Mutex::new(HashMap::new()),
            burst_milli: burst.saturating_mul(MILLI),
            refill_milli_per_sec: per_minute.saturating_mul(MILLI) / 60,
            per_project: per_project.max(1),
        }
    }

    /// Admits one report of `cost` for `project`, or says when to retry.
    pub fn admit(self: &Arc<Self>, project: Uuid, cost: u64) -> Result<ReportPermit, ApiError> {
        self.admit_at(project, cost, Instant::now())
    }

    fn admit_at(
        self: &Arc<Self>,
        project: Uuid,
        cost: u64,
        now: Instant,
    ) -> Result<ReportPermit, ApiError> {
        let cost_milli = cost.saturating_mul(MILLI);
        let mut projects = self.projects.lock().unwrap_or_else(PoisonError::into_inner);
        if !projects.contains_key(&project) && projects.len() >= MAX_PROJECTS {
            let (burst, rate) = (self.burst_milli, self.refill_milli_per_sec);
            projects.retain(|_, b| b.in_flight > 0 || refilled(b, now, rate, burst) < burst);
            if projects.len() >= MAX_PROJECTS {
                tracing::warn!("report gate: every tracked project is active; refusing");
                return Err(ApiError::RateLimited { retry_after: 1 });
            }
        }
        let bucket = projects.entry(project).or_insert(Bucket {
            milli: self.burst_milli,
            at: now,
            in_flight: 0,
        });
        bucket.milli = refilled(bucket, now, self.refill_milli_per_sec, self.burst_milli);
        bucket.at = now;
        if bucket.milli < cost_milli {
            let missing = cost_milli.saturating_sub(bucket.milli);
            let seconds = missing.div_ceil(self.refill_milli_per_sec.max(1)).max(1);
            return Err(ApiError::RateLimited {
                retry_after: seconds,
            });
        }
        if bucket.in_flight >= self.per_project {
            return Err(ApiError::RateLimited { retry_after: 1 });
        }
        // The server-wide place last: a refusal above must not hold one.
        let Ok(global) = self.global.clone().try_acquire_owned() else {
            return Err(ApiError::Overloaded);
        };
        bucket.milli = bucket.milli.saturating_sub(cost_milli);
        bucket.in_flight = bucket.in_flight.saturating_add(1);
        Ok(ReportPermit {
            gate: self.clone(),
            project,
            _global: global,
        })
    }
}

fn refilled(bucket: &Bucket, now: Instant, rate: u64, burst: u64) -> u64 {
    let elapsed = now.saturating_duration_since(bucket.at);
    let earned = u64::try_from(
        elapsed
            .as_millis()
            .saturating_mul(u128::from(rate))
            .checked_div(1_000)
            .unwrap_or(0),
    )
    .unwrap_or(u64::MAX);
    bucket.milli.saturating_add(earned).min(burst)
}

/// A report's cost: the reconciliation, with or without network agreement.
pub const fn reconciliation_cost(network: bool) -> u64 {
    if network {
        NETWORK_AGREEMENT
    } else {
        RECONCILIATION
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn retry_after(result: &Result<ReportPermit, ApiError>) -> Option<u64> {
        match result {
            Err(ApiError::RateLimited { retry_after }) => Some(*retry_after),
            _ => None,
        }
    }

    #[test]
    fn a_bucket_spends_refills_and_names_its_wait() {
        let gate = Arc::new(ReportGate::new(50, 600, 8, 8));
        let (p, t0) = (Uuid::from_u128(1), Instant::now());
        for _ in 0..5 {
            drop(gate.admit_at(p, RECONCILIATION, t0).unwrap());
        }
        assert_eq!(retry_after(&gate.admit_at(p, RECONCILIATION, t0)), Some(1));
        assert_eq!(
            retry_after(&gate.admit_at(p, NETWORK_AGREEMENT, t0)),
            Some(5)
        );
        // 10 units per second: one reconciliation a second later.
        let t1 = t0 + Duration::from_secs(1);
        drop(gate.admit_at(p, RECONCILIATION, t1).unwrap());
        assert!(gate.admit_at(p, EXPLAIN_PAGE, t1).is_err());
        // Another project is untouched, and a bucket never refills past its burst.
        drop(
            gate.admit_at(Uuid::from_u128(2), NETWORK_AGREEMENT, t0)
                .unwrap(),
        );
        let later = t0 + Duration::from_secs(3_600);
        drop(gate.admit_at(p, NETWORK_AGREEMENT, later).unwrap());
        assert!(gate.admit_at(p, RECONCILIATION, later).is_err());
    }

    #[test]
    fn concurrency_is_bounded_per_project_and_across_the_server() {
        let gate = Arc::new(ReportGate::new(1_000, 60_000, 2, 3));
        let t = Instant::now();
        let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let a1 = gate.admit_at(a, EXPLAIN_PAGE, t).unwrap();
        let a2 = gate.admit_at(a, EXPLAIN_PAGE, t).unwrap();
        assert_eq!(retry_after(&gate.admit_at(a, EXPLAIN_PAGE, t)), Some(1));
        let b1 = gate.admit_at(b, EXPLAIN_PAGE, t).unwrap();
        assert!(matches!(
            gate.admit_at(Uuid::from_u128(3), EXPLAIN_PAGE, t),
            Err(ApiError::Overloaded)
        ));
        drop(a1);
        let a3 = gate.admit_at(a, EXPLAIN_PAGE, t).unwrap();
        drop((a2, a3, b1));
        assert_eq!(
            gate.projects
                .lock()
                .unwrap()
                .values()
                .map(|b| b.in_flight)
                .sum::<u32>(),
            0
        );
    }

    #[test]
    fn the_project_map_is_bounded_and_evicts_idle_full_buckets() {
        let gate = Arc::new(ReportGate::new(10, 600, 1, 1));
        let t = Instant::now();
        for n in 0..MAX_PROJECTS {
            let key = Uuid::from_u128(u128::try_from(n).unwrap());
            gate.projects.lock().unwrap().insert(
                key,
                Bucket {
                    milli: 10 * MILLI,
                    at: t,
                    in_flight: 0,
                },
            );
        }
        drop(
            gate.admit_at(Uuid::from_u128(u128::MAX), EXPLAIN_PAGE, t)
                .unwrap(),
        );
        assert!(gate.projects.lock().unwrap().len() < MAX_PROJECTS);
    }
}
