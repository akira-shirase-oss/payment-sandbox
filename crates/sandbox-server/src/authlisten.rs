//! The `auth_epoch` listener (D-102's addendum): the key-identity cache is exact inside
//! the process that changed a key; across processes the database's own triggers NOTIFY
//! `k<project_id>:<key_id>` on revoke and `p<project_id>` on a dataset epoch opening,
//! so a revocation made by ANY writer — a second server, the CLI, a psql — drops the
//! entry here on the same commit. A dropped connection may have missed events: the whole
//! cache is flushed before listening again. The 30 s TTL is now only the backstop for a
//! notification lost while no listener was attached.
//!
//! The connections are the listener's own, held for its life outside the pools — the
//! `LISTEN` socket and a second one for the liveness self-ping (round-3 B): two more
//! connections over the `PS_DB_MAX_CONNECTIONS` budget, deliberate and fixed.

use std::time::Duration;

use sqlx::postgres::PgListener;
use tokio::sync::watch;
use uuid::Uuid;

use crate::auth::KeyCache;
use crate::state::AppState;

/// The supervised task's name in readiness.
pub const TASK: &str = "auth_epoch_listen";
/// The channel the migrations' triggers notify on.
const CHANNEL: &str = "auth_epoch";
/// Between a lost connection and the next `LISTEN`.
const RECONNECT: Duration = Duration::from_millis(500);
/// The task's heartbeat when nothing notifies.
const BEAT: Duration = Duration::from_secs(15);
/// The whole connect pair — `LISTEN` socket and ping socket — must land inside
/// this bound. A peer that accepts then stalls holds an unbounded connect
/// forever otherwise (N-1): no beat, `/readyz` red for the process's life,
/// cross-process revocations left to the TTL, shutdown hung on the task. The
/// pool's acquire deadline does not cover the direct ping connect, and the
/// startup handshake itself has none.
const CONNECT_BOUND: Duration = Duration::from_secs(30);

/// The listener's connect URL with its own `application_name`: on a shared cluster a
/// query-text match cannot tell this backend from another process's listener (or from
/// the `pg_stat_activity` poller itself), so the name carries a per-state unique id
/// (N2). `application_name` is a startup packet field, not a credential.
fn listen_url(base: &str, name: &str) -> String {
    let sep = if base.contains('?') { '&' } else { '?' };
    format!("{base}{sep}application_name={name}")
}

/// Applies one notification: `s<nonce>` is the liveness self-ping (a no-op — its mere
/// arrival is the proof); `k<pid>:<kid>` drops a key, `p<pid>` drops a project's
/// epoch-tagged identities. Any role may NOTIFY this channel, so an empty or
/// unparseable payload flushes the cache — safer to over-invalidate than to hold a
/// stale key (`strip_prefix`: `split_at(1)` panicked on an empty or multibyte payload,
/// and the restart that followed it never flushed — S2-1), but throttled, since a
/// role spamming garbage should not get a full-cache rebuild per message (N6).
fn apply(cache: &KeyCache, payload: &str) {
    if let Some(rest) = payload.strip_prefix('s') {
        if Uuid::parse_str(rest).is_ok() {
            return;
        }
    } else if let Some(rest) = payload.strip_prefix('k') {
        if let Some((project, id)) = rest.split_once(':')
            && let (Ok(project_id), Ok(key_id)) = (Uuid::parse_str(project), Uuid::parse_str(id))
        {
            cache.invalidate_key(project_id, key_id);
            return;
        }
    } else if let Some(rest) = payload.strip_prefix('p')
        && let Ok(project_id) = Uuid::parse_str(rest)
    {
        cache.invalidate_project(project_id);
        return;
    }
    cache.malformed_clear();
}

/// The supervised loop: connect, `LISTEN`, apply notifications until shutdown.
/// `try_recv()` surfaces a lost connection (`eager_reconnect(false)` — sqlx's default
/// would re-attach inside the driver and silently drop the gap's notifications, N3);
/// `Ok(None)`/error means what it missed is gone, so the cache flushes on the gap AND
/// again after the next `LISTEN` lands. A (re)start after a panic flushes the same
/// way: whatever notified while the task was down cannot be replayed.
///
/// A half-open socket reports nothing at all — no error, no FIN — so each beat the
/// loop `pg_notify`s itself on its own second connection (N3): a delivered
/// notification is proof
/// the wire is live; one whole silent beat is proof it is not, and the connection is
/// rebuilt. The heartbeat lands in every select branch (N5): a notification flood
/// can starve the timer but never the readiness beat.
pub async fn run(state: AppState, stop: watch::Receiver<bool>) {
    run_with(state, stop, BEAT, CONNECT_BOUND).await;
}

/// `beat` is the liveness interval — `BEAT` in production, short in tests —
/// and `connect_bound` the deadline the whole connect pair must land inside
/// (`CONNECT_BOUND` in production; short in tests so a stalled attach is
/// observable).
#[doc(hidden)]
pub async fn run_with(
    state: AppState,
    mut stop: watch::Receiver<bool>,
    beat: Duration,
    connect_bound: Duration,
) {
    state.key_cache.clear();
    // N-6: consecutive attaches that died inside their first beat — a peer that
    // accepts then kills each socket. Every inner-loop break re-enters the
    // connect above; without the backoff below the loop re-dials as fast as
    // the kill lands. The counter resets once an attach survives a beat, so a
    // healthy re-attach never pays it.
    let mut dead_attaches = 0_u32;
    loop {
        if dead_attaches > 0
            && tokio::time::timeout(reattach_delay(dead_attaches), stop.changed())
                .await
                .is_ok()
        {
            return;
        }
        let url = listen_url(state.config.database_url.expose(), &state.listen_name);
        let ping_url = listen_url(
            state.config.database_url.expose(),
            &format!("{}-ping", state.listen_name),
        );
        let connected = async {
            let mut listener = PgListener::connect(&url).await?;
            // Surface a lost connection to this loop instead of re-attaching inside
            // sqlx with the gap's notifications silently dropped (N3).
            listener.eager_reconnect(false);
            listener.listen(CHANNEL).await?;
            // The liveness ping rides its own connection, never a pool: a pool
            // saturated past its acquire timeout would unsend every ping and tear
            // down a healthy listener — flushing the whole cache onto that same
            // saturated pool every two beats (round-3 B).
            let ping = <sqlx::PgConnection as sqlx::Connection>::connect(&ping_url).await?;
            Ok::<_, sqlx::Error>((listener, ping))
        };
        // The connect phase is bounded and watches `stop` (N-1): a peer that
        // accepts then stalls holds an unbounded `connect` forever — no beat,
        // `/readyz` red, shutdown hung. Dropping the future mid-connect drops
        // the half-open socket with it.
        let outcome = tokio::select! {
            done = tokio::time::timeout(connect_bound, connected) => done,
            _ = stop.changed() => return,
        };
        let (mut listener, mut ping) = match outcome {
            Ok(Ok(pair)) => pair,
            Ok(Err(error)) => {
                let failures = state.key_cache.note_connect_failure();
                tracing::warn!(%error, connect_failures = failures,
                    "auth listener pair could not connect");
                state.key_cache.clear();
                if tokio::time::timeout(RECONNECT, stop.changed())
                    .await
                    .is_ok()
                {
                    return;
                }
                continue;
            }
            Err(_elapsed) => {
                let failures = state.key_cache.note_connect_failure();
                tracing::warn!(bound = ?connect_bound, connect_failures = failures,
                    "auth listener pair connect outlived its bound");
                state.key_cache.clear();
                if tokio::time::timeout(RECONNECT, stop.changed())
                    .await
                    .is_ok()
                {
                    return;
                }
                continue;
            }
        };
        // Attached: flush after LISTEN lands, not before it — entries resolved in the
        // un-listened window could carry a revocation that landed there.
        state.key_cache.clear();
        state.tasks.beat(TASK);
        // Whether a whole beat passed with nothing received — including this loop's
        // own ping, which a live socket delivers well inside one.
        let mut silent = false;
        // Whether the attach survived at least one full beat-window event — a
        // delivered notification or the timer arm — before dying (N-6).
        let mut lived = false;
        loop {
            tokio::select! {
                _ = stop.changed() => return,
                received = listener.try_recv() => {
                    // Every branch beats (N5): under a notification flood the timer
                    // starves, the heartbeat must not.
                    state.tasks.beat(TASK);
                    if let Ok(Some(note)) = received {
                        silent = false;
                        lived = true;
                        apply(&state.key_cache, note.payload());
                    } else {
                        // Ok(None) or a driver error: the gap's notifications are
                        // gone — flush and re-attach.
                        state.key_cache.clear();
                        break;
                    }
                },
                () = tokio::time::sleep(beat) => {
                    state.tasks.beat(TASK);
                    // A whole beat elapsed with the socket still attached: it
                    // lived, however the loop exits from here.
                    lived = true;
                    if silent {
                        // A beat with the ping unanswered: the socket is half-dead
                        // (N3) — the TTL, not readiness green, must not hide this.
                        state.key_cache.clear();
                        break;
                    }
                    silent = true;
                    self_ping(&mut ping, beat).await;
                },
            }
        }
        dead_attaches = if lived {
            0
        } else {
            dead_attaches.saturating_add(1)
        };
    }
}

/// Re-attach backoff (N-6): doubling with jitter, so a peer that accepts then
/// kills every socket cannot spin the loop — and listeners sharing that peer's
/// fate do not re-attach in lockstep. `dead` is the run of attaches killed
/// inside their first beat; the bound caps at `CAP`, plus up to half again of
/// jitter.
fn reattach_delay(dead: u32) -> Duration {
    const BASE_MS: u64 = 50;
    const CAP_MS: u64 = 5_000;
    let step = BASE_MS.saturating_mul(1_u64 << dead.min(7)).min(CAP_MS);
    let mut salt = [0_u8; 8];
    let jitter = if getrandom::fill(&mut salt).is_ok() {
        u64::from_le_bytes(salt) % (step / 2 + 1)
    } else {
        0
    };
    Duration::from_millis(step + jitter)
}

/// One `pg_notify` on the listener's own second connection — bounded, so a wedged
/// wire cannot hold the liveness loop, and outside every pool, so a saturated pool
/// cannot unsend the ping and tear down a healthy listener (round-3 B). Failure
/// only means the ping never lands; the silence check on the next beat decides
/// what the connection is.
async fn self_ping(conn: &mut sqlx::PgConnection, beat: Duration) {
    let sent = sqlx::query("SELECT pg_notify($1, $2)")
        .bind(CHANNEL)
        .bind(format!("s{}", Uuid::now_v7()));
    let bound = beat.min(Duration::from_secs(5));
    if let Err(error) = tokio::time::timeout(bound, sent.execute(&mut *conn)).await {
        tracing::debug!(%error, "the listener self-ping could not be sent");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sandbox_db::ProjectScope;
    use sandbox_db::keys::KeyIdentity;
    use sandbox_domain::keys::{KeyHash, Scope};

    /// Any role may NOTIFY on the channel, so malformed payloads — empty, multibyte,
    /// wrong shape — must flush the cache safely: `split_at(1)` panicked on all three
    /// (S2-1), and the supervisor's restart never flushed what the panic left. Each
    /// case gets a fresh cache because the malformed clear is rate-limited (N6).
    #[test]
    fn a_malformed_notification_flushes_the_cache_without_panicking() {
        let (project, key) = (Uuid::now_v7(), Uuid::now_v7());
        let put = |cache: &KeyCache, hash: u8| {
            cache.put(
                KeyHash([hash; 32]),
                KeyIdentity {
                    scope: ProjectScope {
                        project_id: project,
                        epoch: 1,
                    },
                    key_id: key,
                    scopes: vec![Scope::Read],
                },
            );
        };
        for payload in ["", "k", "p", "x:not-a-uuid", "ké:v", "\u{1f600}"] {
            let cache = KeyCache::default();
            put(&cache, 1);
            apply(&cache, payload);
            assert!(
                cache.get(&KeyHash([1; 32])).is_none(),
                "{payload:?} left a cached entry"
            );
        }
        // The well-formed forms still apply precisely; the self-ping is a no-op.
        let cache = KeyCache::default();
        put(&cache, 2);
        apply(&cache, &format!("k{project}:{key}"));
        assert!(cache.get(&KeyHash([2; 32])).is_none());
        put(&cache, 3);
        apply(&cache, &format!("p{project}"));
        assert!(cache.get(&KeyHash([3; 32])).is_none());
        put(&cache, 4);
        apply(&cache, &format!("s{}", Uuid::now_v7()));
        assert!(
            cache.get(&KeyHash([4; 32])).is_some(),
            "a liveness self-ping must not clear the cache it is checking"
        );
    }

    /// N-6's backoff: each step lands inside its doubling band plus up to half
    /// again of jitter, and the run saturates at the cap — the wall-clock
    /// ceiling a kill-fast peer cannot shorten and a fast box cannot widen.
    #[test]
    fn reattach_delay_doubles_jitters_and_caps() {
        for dead in 1..=6_u32 {
            let step = Duration::from_millis(50 * (1_u64 << dead));
            for _ in 0..32 {
                let delay = reattach_delay(dead);
                assert!(delay >= step, "{delay:?} under its {step:?} step");
                assert!(
                    delay <= step + step / 2,
                    "{delay:?} over {step:?} plus half jitter"
                );
            }
        }
        for dead in 7..=64_u32 {
            let delay = reattach_delay(dead);
            assert!(
                (Duration::from_secs(5)..=Duration::from_millis(7_500)).contains(&delay),
                "dead={dead} escaped the cap: {delay:?}"
            );
        }
        // Jitter is real, not a constant: a small run must not collapse to the
        // bare step every time.
        assert!(
            (0..32).any(|_| reattach_delay(3) > Duration::from_millis(400)),
            "32 samples never once exceeded the bare step — the jitter is dead"
        );
    }
}
