//! Sandbox-key authentication. Every `/v1` request (other than project bootstrap) carries
//! `Authorization: Bearer sk_sim_…`; the key's SHA-256 resolves to its identity through a
//! bounded cache (D-102): a revocation in this process removes the entry at once, so a
//! revoked key still fails on its next request, and a 30-second TTL is the backstop for
//! any revocation this process did not make.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use axum::{
    extract::FromRequestParts,
    http::{header, request::Parts},
};
use sandbox_db::ProjectScope;
use sandbox_db::keys::KeyIdentity;
use sandbox_domain::keys::{self, KeyHash, Scope};
use uuid::Uuid;

use crate::error::ApiError;
use crate::state::AppState;

/// An authenticated request: the key's project (and epoch) and scopes.
#[derive(Debug, Clone)]
pub struct Auth(pub KeyIdentity);

/// An identity a front door has already authenticated (the dashboard's session, D-062),
/// placed in the request's extensions by that front door's middleware. Nothing outside the
/// server can put one there; the API itself never does, so `/v1` always needs a key.
#[derive(Debug, Clone)]
pub struct Delegated(pub KeyIdentity);

impl Auth {
    /// The project scope every query of this request must use.
    pub const fn scope(&self) -> ProjectScope {
        self.0.scope
    }

    /// The key's id (the actor in audit events).
    pub const fn key_id(&self) -> Uuid {
        self.0.key_id
    }

    /// Refuses unless the key holds `required` (or a scope implying it).
    pub fn require(&self, required: Scope) -> Result<(), ApiError> {
        if keys::permits(&self.0.scopes, required) {
            Ok(())
        } else {
            Err(ApiError::Forbidden(required.code()))
        }
    }
}

/// The bearer token of a request, if well formed.
pub fn bearer(parts: &Parts) -> Option<&str> {
    parts
        .headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
}

impl FromRequestParts<AppState> for Auth {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        if let Some(Delegated(identity)) = parts.extensions.get::<Delegated>() {
            return Ok(Self(identity.clone()));
        }
        let token = bearer(parts).ok_or(ApiError::Unauthorized)?;
        let hash = keys::hash_presented(token).map_err(|_| ApiError::Unauthorized)?;
        // The classifier already chose this request's pool (D-101): a read's lookup
        // pays from the read pool, so a write flood that holds every write connection
        // cannot make a read fail at its credential check. `db_for` is the fallback
        // for a request that never passed `classify_pool`.
        let pool = parts
            .extensions
            .get::<crate::middleware::DbPool>()
            .map_or_else(|| state.db_for(&parts.method), |db| &db.0);
        // The classifier already chose this request's pool (D-101) — a read's lookup pays
        // from the read pool — and the bounded cache (D-102) answers a seen key without
        // the round trip.
        let identity = if let Some(identity) = state.key_cache.get(&hash) {
            identity
        } else {
            // The window is read BEFORE the resolve: an invalidation of this project
            // (or a clear) that lands while the SELECT runs must invalidate the `put`
            // too, or the just-revoked identity is cached again for up to the TTL (S1-1).
            let began = state.key_cache.began();
            let resolved = sandbox_db::keys::resolve(pool, &hash)
                .await?
                .ok_or(ApiError::Unauthorized)?;
            state.key_cache.put_if_fresh(hash, resolved.clone(), began);
            resolved
        };
        Ok(Self(identity))
    }
}

/// True when the request presents the configured bootstrap token (compared as SHA-256
/// digests in constant time).
pub fn is_bootstrap(parts: &Parts, state: &AppState) -> bool {
    let (Some(expected), Some(presented)) = (&state.config.bootstrap_token, bearer(parts)) else {
        return false;
    };
    keys::constant_time_eq(&keys::digest(expected.expose()), &keys::digest(presented))
}

/// Bounded key-identity cache (D-102): `resolve` cost a round trip per request, and at
/// m5's offered rate that alone was ~1,200 queries a second — the plans are
/// sub-millisecond (`explain_plans.rs`), so round trips, not rows, are the capacity.
/// Entries live at most [`TTL`], and every in-process mutation removes exactly what it
/// changed (`revoke` → [`Self::invalidate_key`], dataset reset →
/// [`Self::invalidate_project`]), so the TTL only ever covers a change made outside this
/// process. The map is bounded — keys are few; a full cache evicts its oldest entry.
#[derive(Debug)]
pub struct KeyCache {
    inner: Mutex<Inner>,
    hits: AtomicU64,
    misses: AtomicU64,
    /// Malformed-notification clears the ~1/s bound suppressed (N6).
    suppressed_clears: AtomicU64,
    /// `auth_epoch` listener pair connects that failed or outlived their bound —
    /// the N-2 signal a deployment sized at +1 connection would otherwise hide.
    connect_failures: AtomicU64,
    /// How long a per-project marker lives: the longest a `began`-to-`put` window
    /// can run — the request deadline — floored at the cache TTL, so a marker
    /// cannot be pruned while a resolve it guards is still legitimately in flight
    /// (round-3 A: pruning at the TTL is unsafe past a 30 s request timeout).
    marker_window: Duration,
}

#[derive(Debug, Default)]
struct Inner {
    map: HashMap<KeyHash, CacheEntry>,
    /// When the cache last cleared wholesale (listener gap or restart).
    cleared_all: Option<Instant>,
    /// When each project's invalidation last landed: a resolve must compare only its
    /// own project's window, so one tenant's revokes never cancel another tenant's
    /// in-flight `put`s — the global generation did, multiplying misses for all (N6).
    invalidated: HashMap<Uuid, Instant>,
    /// The malformed-notification rate limiter's last permitted clear.
    last_malformed: Option<Instant>,
}

#[derive(Debug)]
struct CacheEntry {
    identity: KeyIdentity,
    at: Instant,
}

impl KeyCache {
    /// Most keys remembered at once — far above any project's key count.
    const CAP: usize = 1_024;
    /// The out-of-band backstop: an entry can never outlive this.
    const TTL: Duration = Duration::from_secs(30);
    /// Smallest gap between malformed-payload clears (N6).
    const MALFORMED_CLEAR: Duration = Duration::from_secs(1);

    /// A cached identity, fresh by [`Self::TTL`]; `None` on miss or expiry.
    pub fn get(&self, hash: &KeyHash) -> Option<KeyIdentity> {
        let entry = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .map
            .get(hash)
            .filter(|e| e.at.elapsed() < Self::TTL)
            .map(|e| e.identity.clone());
        match entry {
            Some(_) => self.hits.fetch_add(1, Ordering::Relaxed),
            None => self.misses.fetch_add(1, Ordering::Relaxed),
        };
        entry
    }

    /// The resolve's window: read before `resolve`, passed to [`Self::put_if_fresh`].
    /// A timestamp, not a counter — the project is not known until the resolve returns.
    pub fn began(&self) -> Instant {
        Instant::now()
    }

    /// Inserts under the held lock, evicting the oldest entry when full.
    fn insert_locked(inner: &mut Inner, hash: KeyHash, identity: KeyIdentity) {
        if inner.map.len() >= Self::CAP
            && let Some(oldest) = inner
                .map
                .iter()
                .max_by_key(|(_, e)| e.at.elapsed())
                .map(|(k, _)| *k)
        {
            inner.map.remove(&oldest);
        }
        inner.map.insert(
            hash,
            CacheEntry {
                identity,
                at: Instant::now(),
            },
        );
    }

    /// Remembers a resolved identity. Tests seed the cache directly; request code must
    /// use [`Self::put_if_fresh`].
    #[cfg(test)]
    pub fn put(&self, hash: KeyHash, identity: KeyIdentity) {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Self::insert_locked(&mut inner, hash, identity);
    }

    /// Caches `identity` only if no clear and no invalidation of ITS project has landed
    /// since `began` — the compare happens under the same lock every invalidator marks
    /// under, so a resolve that raced a revoke or a reset can never resurrect the old
    /// entry (S1-1), while another project's invalidation costs this resolve nothing.
    /// A resolve that has already outlived `marker_window` is refused outright: any
    /// marker its `began` needed may already be pruned, so the stale check below
    /// could silently pass on a guard that is gone (N-3 — report and checkout
    /// resolves can run past the request deadline the window is sized from).
    pub fn put_if_fresh(&self, hash: KeyHash, identity: KeyIdentity, began: Instant) {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if began.elapsed() >= self.marker_window {
            self.misses.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let stale = inner.cleared_all.is_some_and(|at| at >= began)
            || inner
                .invalidated
                .get(&identity.scope.project_id)
                .is_some_and(|at| *at >= began);
        if stale {
            self.misses.fetch_add(1, Ordering::Relaxed);
            return;
        }
        Self::insert_locked(&mut inner, hash, identity);
    }

    /// Marks `project`'s window and drops its entries — shared by every invalidation.
    /// The marker lets an in-flight resolve of this project (and only it) discard its
    /// answer instead of resurrecting a revoked identity; markers live for the
    /// longest possible resolve (the request deadline, floored at the TTL), and an
    /// overflowing map degrades to the global clear window — forgetting markers
    /// outright would let a flood of forged `p<uuid>`s or real resets erase a
    /// victim's window and resurrect its revoked key (round-3 A).
    fn invalidate_locked(inner: &mut Inner, marker_window: Duration, project_id: Uuid) {
        let now = Instant::now();
        inner
            .invalidated
            .retain(|_, at| now.duration_since(*at) < marker_window);
        if inner.invalidated.len() >= KeyCache::CAP {
            inner.cleared_all = Some(now);
            inner.invalidated.clear();
        }
        inner.invalidated.insert(project_id, now);
    }

    /// Drops every entry of a key — call after a successful revoke.
    pub fn invalidate_key(&self, project_id: Uuid, key_id: Uuid) {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Self::invalidate_locked(&mut inner, self.marker_window, project_id);
        inner.map.retain(|_, e| e.identity.key_id != key_id);
    }

    /// Drops every entry of a project — call after a dataset reset opened a new epoch.
    pub fn invalidate_project(&self, project_id: Uuid) {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Self::invalidate_locked(&mut inner, self.marker_window, project_id);
        inner
            .map
            .retain(|_, e| e.identity.scope.project_id != project_id);
    }

    /// Drops everything — called when the `auth_epoch` listener reconnects after a gap,
    /// since notifications it missed cannot be replayed (D-102's addendum).
    pub fn clear(&self) {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.cleared_all = Some(Instant::now());
        inner.invalidated.clear();
        inner.map.clear();
    }

    /// A malformed payload clears like a lost connection — but any database role may
    /// `NOTIFY` garbage, so the clear is bounded to roughly one a second (N6): a
    /// spammer gets a suppressed count, not a full-cache rebuild per message. The
    /// suppressed window cannot hide a real revocation for long — a malformed payload
    /// carries no key to keep, and the TTL is the designed backstop for the rest.
    pub fn malformed_clear(&self) {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = Instant::now();
        if inner
            .last_malformed
            .is_some_and(|at| now.duration_since(at) < Self::MALFORMED_CLEAR)
        {
            self.suppressed_clears.fetch_add(1, Ordering::Relaxed);
            return;
        }
        inner.last_malformed = Some(now);
        inner.cleared_all = Some(now);
        inner.invalidated.clear();
        inner.map.clear();
    }

    /// The listener's connect pair failed or outlived its bound — counted so a
    /// deployment missing the +2 connection budget shows up as a number, not
    /// just a cache that keeps clearing (N-2). Returns the running total.
    pub fn note_connect_failure(&self) -> u64 {
        self.connect_failures.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// `(hits, misses, suppressed malformed clears, listener connect failures)` —
    /// observability for the cache that serves the hot read path.
    pub fn stats(&self) -> (u64, u64, u64, u64) {
        (
            self.hits.load(Ordering::Relaxed),
            self.misses.load(Ordering::Relaxed),
            self.suppressed_clears.load(Ordering::Relaxed),
            self.connect_failures.load(Ordering::Relaxed),
        )
    }
}

impl KeyCache {
    /// `resolve_bound` is the longest a `began` → [`Self::put_if_fresh`] window can
    /// run — the request deadline the resolve lives inside; markers live at least
    /// that long (round-3 A).
    pub fn new(resolve_bound: Duration) -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            suppressed_clears: AtomicU64::new(0),
            connect_failures: AtomicU64::new(0),
            marker_window: Self::TTL.max(resolve_bound),
        }
    }
}

impl Default for KeyCache {
    fn default() -> Self {
        Self::new(Duration::ZERO)
    }
}

/// Generates a new key from OS randomness.
pub fn generate_key() -> Result<String, ApiError> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("OS randomness unavailable: {e}")))?;
    Ok(keys::key_from_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sandbox_db::ProjectScope;

    fn identity(project: Uuid, key: Uuid) -> KeyIdentity {
        KeyIdentity {
            scope: ProjectScope {
                project_id: project,
                epoch: 1,
            },
            key_id: key,
            scopes: vec![Scope::Read],
        }
    }

    /// What was resolved once is answered without the round trip, counted as a hit.
    #[test]
    fn a_seen_key_is_a_hit_not_a_query() {
        let cache = KeyCache::default();
        let hash = KeyHash([7; 32]);
        assert!(cache.get(&hash).is_none());
        cache.put(hash, identity(Uuid::now_v7(), Uuid::now_v7()));
        assert!(cache.get(&hash).is_some());
        assert_eq!(cache.stats(), (1, 1, 0, 0));
    }

    /// A revoked key's entries drop at once — the next request resolves afresh and a
    /// revoked identity is never served from the cache.
    #[test]
    fn a_revocation_is_effective_immediately() {
        let cache = KeyCache::default();
        let (project, key, other) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        cache.put(KeyHash([1; 32]), identity(project, key));
        cache.put(KeyHash([2; 32]), identity(project, other));
        cache.invalidate_key(project, key);
        assert!(cache.get(&KeyHash([1; 32])).is_none());
        assert!(cache.get(&KeyHash([2; 32])).is_some());
        cache.invalidate_project(project);
        assert!(cache.get(&KeyHash([2; 32])).is_none());
    }

    /// The S1-1 race: a resolve that started before an invalidation commits must never
    /// be cached after it — before the window check, the stale `put` resurrected a
    /// revoked identity for up to the TTL.
    #[test]
    fn a_resolution_older_than_an_invalidation_is_never_cached() {
        let cache = KeyCache::default();
        let hash = KeyHash([3; 32]);
        let (project, key) = (Uuid::now_v7(), Uuid::now_v7());
        // A request misses, opens its window, then its SELECT runs long...
        let seen_at_miss = cache.began();
        // ...the revoke commits and invalidates while the SELECT was in flight...
        cache.invalidate_key(project, key);
        // ...and its answer arrives now. It must not be cached.
        cache.put_if_fresh(hash, identity(project, key), seen_at_miss);
        assert!(
            cache.get(&hash).is_none(),
            "a resolve older than a revocation must never be served from the cache"
        );
        // A resolve that began after the invalidation still caches.
        cache.put_if_fresh(hash, identity(project, key), cache.began());
        assert!(cache.get(&hash).is_some());
    }

    /// N6: the windows are per project — another tenant's revocation cancels none of
    /// this project's in-flight resolves, so one tenant cannot multiply misses for all.
    #[test]
    fn an_invalidation_in_one_project_cancels_no_others() {
        let cache = KeyCache::default();
        let hash = KeyHash([5; 32]);
        let (mine, theirs, key) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        let began = cache.began();
        cache.invalidate_project(theirs);
        // My resolve began before THEIR invalidation — and still caches.
        cache.put_if_fresh(hash, identity(mine, key), began);
        assert!(
            cache.get(&hash).is_some(),
            "another project's invalidation must not cancel my put"
        );
        // A wholesale clear still cancels it — the global window remains for gaps.
        let again = cache.began();
        cache.clear();
        cache.put_if_fresh(hash, identity(mine, key), again);
        assert!(
            cache.get(&hash).is_none(),
            "a clear must cancel every in-flight resolve"
        );
    }

    /// N6: a flood of malformed payloads clears at the bound, once — the rest are
    /// counted, not repeated full-cache rebuilds.
    #[test]
    fn malformed_clears_are_rate_limited_and_counted() {
        let cache = KeyCache::default();
        let put = |hash: u8| {
            cache.put(
                KeyHash([hash; 32]),
                identity(Uuid::now_v7(), Uuid::now_v7()),
            );
        };
        put(1);
        cache.malformed_clear();
        assert!(cache.get(&KeyHash([1; 32])).is_none());
        // Inside the bound: the second malformed clear is suppressed — entries put
        // between them survive, and the suppression is counted.
        put(2);
        cache.malformed_clear();
        cache.malformed_clear();
        assert!(cache.get(&KeyHash([2; 32])).is_some());
        assert_eq!(cache.stats().2, 2);
    }

    /// N-3: a resolve that has already outlived the marker window must be
    /// refused, not compared — any invalidation marker its `began` needed may
    /// already be pruned, so the stale check would silently pass on a guard
    /// that is gone. Report and checkout resolves can legitimately run past
    /// the request deadline the window is sized from.
    #[test]
    fn a_resolve_that_outlived_its_marker_window_cannot_put() {
        let cache = KeyCache::default();
        let hash = KeyHash([9; 32]);
        let (project, key) = (Uuid::now_v7(), Uuid::now_v7());
        // The window floor is the 30 s TTL — a `began` older than it must be
        // refused even though no invalidation ever landed inside it.
        let began = Instant::now()
            .checked_sub(KeyCache::TTL + Duration::from_secs(1))
            .expect("the test clock cannot reach past the marker window");
        cache.put_if_fresh(hash, identity(project, key), began);
        assert!(
            cache.get(&hash).is_none(),
            "a put past its marker window must be refused"
        );
        // A resolve inside the window still caches — the bound is the window's
        // edge, not a blanket refusal of aged instants.
        cache.put_if_fresh(hash, identity(project, key), cache.began());
        assert!(cache.get(&hash).is_some());
    }

    /// Round-3 A: the marker map is hard-capped — but overflowing it without the
    /// global window forgets every invalidation. A role `NOTIFY`ing `p<random
    /// uuid>`s (or enough real resets inside the window) could then resurrect a
    /// revoked key in a racing resolve: the overflow must behave like a gap.
    #[test]
    fn an_invalidation_flood_cannot_erase_the_victims_window() {
        let cache = KeyCache::default();
        let hash = KeyHash([9; 32]);
        let (victim, key) = (Uuid::now_v7(), Uuid::now_v7());
        // The victim's resolve opens its window, the revoke lands and marks it…
        let began = cache.began();
        cache.invalidate_project(victim);
        // …then a flood of other projects' invalidations overflows the map.
        for _ in 0..KeyCache::CAP {
            cache.invalidate_project(Uuid::now_v7());
        }
        // The victim's stale answer must still be refused.
        cache.put_if_fresh(hash, identity(victim, key), began);
        assert!(
            cache.get(&hash).is_none(),
            "an overflowing invalidation map must behave like a gap, not forget"
        );
    }

    /// Round-3 A: markers prune at the longest possible resolve, not the cache TTL —
    /// a request may legitimately take the whole request deadline (60 s here), so a
    /// revocation 31 s old still guards a resolve that began 40 s ago.
    #[test]
    fn a_marker_lives_for_the_longest_resolve_not_the_ttl() {
        let cache = KeyCache::new(Duration::from_secs(60));
        let hash = KeyHash([11; 32]);
        let (victim, key) = (Uuid::now_v7(), Uuid::now_v7());
        // The resolve began 40 s ago; the revoke landed 9 s into it — past the
        // 30 s TTL, inside the resolve window.
        let began = Instant::now()
            .checked_sub(Duration::from_secs(40))
            .expect("forty seconds ago is a representable instant");
        {
            let mut inner = cache
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            inner.invalidated.insert(
                victim,
                Instant::now()
                    .checked_sub(Duration::from_secs(31))
                    .expect("thirty-one seconds ago is a representable instant"),
            );
        }
        // A later invalidation prunes by the window — the marker must survive.
        cache.invalidate_project(Uuid::now_v7());
        cache.put_if_fresh(hash, identity(victim, key), began);
        assert!(
            cache.get(&hash).is_none(),
            "a marker older than the TTL but inside the resolve window was pruned"
        );
    }

    /// The map never exceeds its bound: at capacity the oldest entry goes, never a grow.
    #[test]
    fn the_cache_is_bounded() {
        let cache = KeyCache::default();
        for i in 0..=KeyCache::CAP {
            let mut raw = [0_u8; 32];
            raw[..8].copy_from_slice(&(i as u64).to_be_bytes());
            cache.put(KeyHash(raw), identity(Uuid::now_v7(), Uuid::now_v7()));
        }
        let len = cache
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .map
            .len();
        assert!(len <= KeyCache::CAP);
    }
}
