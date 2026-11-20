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
/// epoch-tagged identities. An unparseable payload flushes the cache — safer to
/// over-invalidate than to hold a stale key.
fn apply(state: &AppState, payload: &str) {
    let (kind, rest) = payload.split_at(1);
    if kind == "k" {
        if let Some((_, id)) = rest.split_once(':')
            && let Ok(key_id) = Uuid::parse_str(id)
        {
            state.key_cache.invalidate_key(key_id);
            return;
        }
    } else if kind == "p"
        && let Ok(project_id) = Uuid::parse_str(rest)
    {
        state.key_cache.invalidate_project(project_id);
        return;
    }
    // An unparseable payload flushes the cache — safer to over-invalidate than to hold
    // a stale key.
    state.key_cache.clear();
}

/// The supervised loop: connect, `LISTEN`, apply notifications until shutdown. On any
/// error — connect, listen, recv — the cache flushes (events may have been missed) and
/// the loop tries again after [`RECONNECT`]; the supervisor restarts a panic.
pub async fn run(state: AppState, mut stop: watch::Receiver<bool>) {
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
        loop {
            tokio::select! {
                _ = stop.changed() => return,
                received = listener.recv() => if let Ok(note) = received {
                    apply(&state, note.payload());
                } else {
                    // A lost connection may have lost notifications with it.
                    state.key_cache.clear();
                    break;
                },
                () = tokio::time::sleep(BEAT) => state.tasks.beat(TASK),
            }
        }
    }
}
