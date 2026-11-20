//! Per-pool saturation metrics (D-101, Colophon's D-225 rule): each pool gets an
//! in-use occupancy picture — sampled by a supervised task, since sqlx exposes size
//! and idle counts but no acquire-wait hook — and an acquire histogram recorded by
//! [`timed_acquire`] at explicit acquisitions. Implicit acquires inside
//! `query.execute(&pool)` still surface as the sampled occupancy plus the per-op 503s
//! the load gate already counts.
//!
//! The sampler runs under [`supervise`](crate::supervisor::supervise) like every other
//! background task, so a panic restarts it and readiness notices a missed beat.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde::Serialize;
use sqlx::{PgPool, Postgres, pool::PoolConnection};
use tokio::sync::watch;

use sandbox_db::DbError;

use crate::state::AppState;

/// The supervised task's name, for readiness.
pub const TASK: &str = "pool_metrics";
/// Occupancy sampling cadence: fine enough to see a pool fill, coarse enough to cost
/// nothing (two atomic reads a tick).
const SAMPLE: Duration = Duration::from_millis(500);

/// One pool's counters. `in_use_*` are written by the sampler; `acquire_*` by
/// [`timed_acquire`].
#[derive(Debug, Default)]
pub struct PoolStat {
    /// Sampler ticks so far.
    samples: AtomicU64,
    /// Sum of in-use samples (mean = `in_use_sum / samples`).
    in_use_sum: AtomicU64,
    /// Highest in-use count ever sampled.
    in_use_max: AtomicU64,
    /// In-use histogram: `0, 1, 2, 3–4, 5–8, 9–16, 17–32, 33+` connections.
    in_use_hist: [AtomicU64; 8],
    /// Explicit acquisitions timed by [`timed_acquire`].
    acquires: AtomicU64,
    /// Of those, the ones that hit the pool's acquire deadline (`PoolTimedOut`).
    acquire_timeouts: AtomicU64,
    /// Acquire-wait histogram in microseconds: `<100, <1_000, <4_000, <16_000,
    /// <64_000, <256_000, <1_000_000, ≥1_000_000`.
    wait_hist: [AtomicU64; 8],
}

/// The in-use bucket boundaries (a pool is never bigger than 32 connections).
const IN_USE_BUCKETS: [u64; 7] = [0, 1, 2, 4, 8, 16, 32];
/// The acquire-wait bucket boundaries in microseconds.
const WAIT_US_BUCKETS: [u64; 7] = [100, 1_000, 4_000, 16_000, 64_000, 256_000, 1_000_000];

fn bucket(boundaries: &[u64; 7], value: u64) -> usize {
    boundaries
        .iter()
        .position(|&bound| value <= bound)
        .unwrap_or(boundaries.len())
}

/// A point-in-time copy of one pool's counters, for tests and the status line.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct PoolStatSnapshot {
    /// Sampler ticks.
    pub samples: u64,
    /// Mean in-use connections ×100 (integer; `/100` for the mean).
    pub in_use_sum: u64,
    /// Highest in-use count sampled.
    pub in_use_max: u64,
    /// In-use histogram: `0, 1, 2, 3–4, 5–8, 9–16, 17–32, 33+`.
    pub in_use_hist: [u64; 8],
    /// Timed acquisitions.
    pub acquires: u64,
    /// Acquires that hit the deadline.
    pub acquire_timeouts: u64,
    /// Acquire-wait histogram: `<100µs, <1ms, <4ms, <16ms, <64ms, <256ms, <1s, ≥1s`.
    pub wait_hist: [u64; 8],
}

impl PoolStat {
    /// Records one occupancy sample (`size − idle`).
    fn observe_in_use(&self, in_use: u64) {
        self.samples.fetch_add(1, Ordering::Relaxed);
        self.in_use_sum.fetch_add(in_use, Ordering::Relaxed);
        self.in_use_max.fetch_max(in_use, Ordering::Relaxed);
        self.in_use_hist[bucket(&IN_USE_BUCKETS, in_use)].fetch_add(1, Ordering::Relaxed);
    }

    /// Records one timed acquisition: its wait and whether it ran out of time.
    fn observe_acquire(&self, waited: Duration, timed_out: bool) {
        self.acquires.fetch_add(1, Ordering::Relaxed);
        if timed_out {
            self.acquire_timeouts.fetch_add(1, Ordering::Relaxed);
        }
        let micros = u64::try_from(waited.as_micros()).unwrap_or(u64::MAX);
        self.wait_hist[bucket(&WAIT_US_BUCKETS, micros)].fetch_add(1, Ordering::Relaxed);
    }

    /// Copies the counters out.
    pub fn snapshot(&self) -> PoolStatSnapshot {
        let mut snapshot = PoolStatSnapshot {
            samples: self.samples.load(Ordering::Relaxed),
            in_use_sum: self.in_use_sum.load(Ordering::Relaxed),
            in_use_max: self.in_use_max.load(Ordering::Relaxed),
            acquires: self.acquires.load(Ordering::Relaxed),
            acquire_timeouts: self.acquire_timeouts.load(Ordering::Relaxed),
            ..PoolStatSnapshot::default()
        };
        for (target, source) in snapshot
            .in_use_hist
            .iter_mut()
            .zip(&self.in_use_hist)
            .chain(snapshot.wait_hist.iter_mut().zip(&self.wait_hist))
        {
            *target = source.load(Ordering::Relaxed);
        }
        snapshot
    }
}

/// The two pools' counters.
#[derive(Debug, Default)]
pub struct PoolStats {
    /// The read pool's.
    pub read: PoolStat,
    /// The write pool's.
    pub write: PoolStat,
}

impl PoolStats {
    /// One sampling pass over both pools.
    fn observe(&self, read: &PgPool, write: &PgPool) {
        let in_use = |pool: &PgPool| {
            pool.size()
                .saturating_sub(u32::try_from(pool.num_idle()).unwrap_or(u32::MAX))
        };
        self.read.observe_in_use(u64::from(in_use(read)));
        self.write.observe_in_use(u64::from(in_use(write)));
    }

    /// Both pools, copied out.
    pub fn snapshot(&self) -> (PoolStatSnapshot, PoolStatSnapshot) {
        (self.read.snapshot(), self.write.snapshot())
    }
}

/// Acquires a pooled connection, recording the wait and a deadline hit into `stat`
/// (D-101): call sites that take a connection explicitly — a transaction begin, a
/// held connection in a test — pay the histogram's cost, which is two atomics.
pub async fn timed_acquire(
    pool: &PgPool,
    stat: &PoolStat,
) -> Result<PoolConnection<Postgres>, DbError> {
    let started = Instant::now();
    let conn = pool.acquire().await;
    let waited = started.elapsed();
    stat.observe_acquire(waited, matches!(conn, Err(sqlx::Error::PoolTimedOut)));
    conn.map_err(DbError::from)
}

/// The sampler loop (D-101): ticks [`PoolStats::observe`] until shutdown. Cheap by
/// construction — two atomic reads per pool per tick, nothing awaited but the timer.
pub async fn run(state: AppState, mut stop: watch::Receiver<bool>) {
    let mut tick = tokio::time::interval(SAMPLE);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = tick.tick() => {
                state.pool_stats.observe(&state.read_db, &state.write_db);
                state.tasks.beat(TASK);
            }
            _ = stop.changed() => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The buckets land where their names say: a wait at a boundary goes in the
    /// boundary's own bucket, never the next one.
    #[test]
    fn buckets_edge_at_their_own_boundaries() {
        assert_eq!(bucket(&WAIT_US_BUCKETS, 0), 0);
        assert_eq!(bucket(&WAIT_US_BUCKETS, 100), 0);
        assert_eq!(bucket(&WAIT_US_BUCKETS, 101), 1);
        assert_eq!(bucket(&WAIT_US_BUCKETS, 1_000_000), 6);
        assert_eq!(bucket(&WAIT_US_BUCKETS, u64::MAX), 7);
        assert_eq!(bucket(&IN_USE_BUCKETS, 32), 6);
        assert_eq!(bucket(&IN_USE_BUCKETS, 33), 7);
    }

    /// A timeout still counts as an acquire — the wait histogram tells slow from
    /// failed because only the flag differs.
    #[test]
    fn the_stat_counts_acquires_waits_and_timeouts() {
        let stat = PoolStat::default();
        stat.observe_acquire(Duration::from_millis(2), false);
        stat.observe_acquire(Duration::from_secs(2), true);
        stat.observe_in_use(5);
        let snapshot = stat.snapshot();
        assert_eq!(snapshot.acquires, 2);
        assert_eq!(snapshot.acquire_timeouts, 1);
        assert_eq!(snapshot.wait_hist[2], 1); // 2ms < 4ms
        assert_eq!(snapshot.wait_hist[7], 1); // 2s ≥ 1s
        assert_eq!(snapshot.in_use_max, 5);
        assert_eq!(snapshot.in_use_hist[bucket(&IN_USE_BUCKETS, 5)], 1);
    }
}
