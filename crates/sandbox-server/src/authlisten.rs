//! The `auth_epoch` listener (D-102's addendum): the key-identity cache is exact inside
//! the process that changed a key; across processes the database's own triggers NOTIFY
//! `k<project_id>:<key_id>` on revoke and `p<project_id>` on a dataset epoch opening,
//! so a revocation made by ANY writer — a second server, the CLI, a psql — drops the
//! entry here on the same commit. A dropped connection may have missed events: the whole
//! cache is flushed before listening again. The 30 s TTL is now only the backstop for a
//! notification lost while no listener was attached.
//!
//! The connection is the listener's own, held for its life outside the pools — one more
//! connection over the `PS_DB_MAX_CONNECTIONS` budget, deliberate and fixed.

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

/// Applies one notification: `k<pid>:<kid>` drops a key, `p<pid>` drops a project's
/// epoch-tagged identities. Any role may NOTIFY this channel, so an empty or
/// unparseable payload flushes the cache — safer to over-invalidate than to hold a
/// stale key (`strip_prefix`: `split_at(1)` panicked on an empty or multibyte payload,
/// and the restart that followed it never flushed — S2-1).
fn apply(cache: &KeyCache, payload: &str) {
    if let Some(rest) = payload.strip_prefix('k') {
        if let Some((_, id)) = rest.split_once(':')
            && let Ok(key_id) = Uuid::parse_str(id)
        {
            cache.invalidate_key(key_id);
            return;
        }
    } else if let Some(rest) = payload.strip_prefix('p')
        && let Ok(project_id) = Uuid::parse_str(rest)
    {
        cache.invalidate_project(project_id);
        return;
    }
    cache.clear();
}

/// The supervised loop: connect, `LISTEN`, apply notifications until shutdown.
/// `recv()` reconnects inside sqlx and silently drops the gap's notifications, so the
/// loop drives `try_recv()` — `Ok(None)` means the connection was lost and what it
/// missed is gone; the cache flushes on the gap AND again after the next `LISTEN`
/// lands, so the whole un-listened window is covered. A (re)start after a panic
/// flushes the same way: whatever notified while the task was down cannot be replayed.
pub async fn run(state: AppState, mut stop: watch::Receiver<bool>) {
    state.key_cache.clear();
    loop {
        let url = state.config.database_url.expose().to_owned();
        let connected = async {
            let mut listener = PgListener::connect(&url).await?;
            listener.listen(CHANNEL).await?;
            Ok::<_, sqlx::Error>(listener)
        };
        let Ok(mut listener) = connected.await else {
            state.key_cache.clear();
            if tokio::time::timeout(RECONNECT, stop.changed())
                .await
                .is_ok()
            {
                return;
            }
            continue;
        };
        // Attached: flush after LISTEN lands, not before it — entries resolved in the
        // un-listened window could carry a revocation that landed there.
        state.key_cache.clear();
        state.tasks.beat(TASK);
        loop {
            tokio::select! {
                _ = stop.changed() => return,
                received = listener.try_recv() => {
                    if let Ok(Some(note)) = received {
                        apply(&state.key_cache, note.payload());
                    } else {
                        // Connection lost (Ok(None)) or a driver error: the gap's
                        // notifications are gone — flush and re-attach.
                        state.key_cache.clear();
                        break;
                    }
                },
                () = tokio::time::sleep(BEAT) => state.tasks.beat(TASK),
            }
        }
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
    /// (S2-1), and the supervisor's restart never flushed what the panic left.
    #[test]
    fn a_malformed_notification_flushes_the_cache_without_panicking() {
        let cache = KeyCache::default();
        let (project, key) = (Uuid::now_v7(), Uuid::now_v7());
        let put = |hash: u8| {
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
            put(1);
            apply(&cache, payload);
            assert!(
                cache.get(&KeyHash([1; 32])).is_none(),
                "{payload:?} left a cached entry"
            );
        }
        // The well-formed forms still apply precisely.
        put(2);
        apply(&cache, &format!("k{project}:{key}"));
        assert!(cache.get(&KeyHash([2; 32])).is_none());
        put(3);
        apply(&cache, &format!("p{project}"));
        assert!(cache.get(&KeyHash([3; 32])).is_none());
    }
}
