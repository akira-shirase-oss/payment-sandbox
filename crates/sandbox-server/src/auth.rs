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
            let resolved = sandbox_db::keys::resolve(pool, &hash)
                .await?
                .ok_or(ApiError::Unauthorized)?;
            state.key_cache.put(hash, resolved.clone());
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
    map: Mutex<HashMap<KeyHash, CacheEntry>>,
    hits: AtomicU64,
    misses: AtomicU64,
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

    /// A cached identity, fresh by [`Self::TTL`]; `None` on miss or expiry.
    pub fn get(&self, hash: &KeyHash) -> Option<KeyIdentity> {
        let entry = self
            .map
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(hash)
            .filter(|e| e.at.elapsed() < Self::TTL)
            .map(|e| e.identity.clone());
        match entry {
            Some(_) => self.hits.fetch_add(1, Ordering::Relaxed),
            None => self.misses.fetch_add(1, Ordering::Relaxed),
        };
        entry
    }

    /// Remembers a resolved identity, evicting the oldest entry when full.
    pub fn put(&self, hash: KeyHash, identity: KeyIdentity) {
        let mut map = self
            .map
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if map.len() >= Self::CAP
            && let Some(oldest) = map
                .iter()
                .max_by_key(|(_, e)| e.at.elapsed())
                .map(|(k, _)| *k)
        {
            map.remove(&oldest);
        }
        map.insert(
            hash,
            CacheEntry {
                identity,
                at: Instant::now(),
            },
        );
    }

    /// Drops every entry of a key — call after a successful revoke.
    pub fn invalidate_key(&self, key_id: Uuid) {
        self.map
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|_, e| e.identity.key_id != key_id);
    }

    /// Drops every entry of a project — call after a dataset reset opened a new epoch.
    pub fn invalidate_project(&self, project_id: Uuid) {
        self.map
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|_, e| e.identity.scope.project_id != project_id);
    }

    /// Drops everything — called when the `auth_epoch` listener reconnects after a gap,
    /// since notifications it missed cannot be replayed (D-102's addendum).
    pub fn clear(&self) {
        self.map
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }

    /// `(hits, misses)` — observability for the cache that serves the hot read path.
    pub fn stats(&self) -> (u64, u64) {
        (
            self.hits.load(Ordering::Relaxed),
            self.misses.load(Ordering::Relaxed),
        )
    }
}

impl Default for KeyCache {
    fn default() -> Self {
        Self {
            map: Mutex::new(HashMap::new()),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
        }
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
        assert_eq!(cache.stats(), (1, 1));
    }

    /// A revoked key's entries drop at once — the next request resolves afresh and a
    /// revoked identity is never served from the cache.
    #[test]
    fn a_revocation_is_effective_immediately() {
        let cache = KeyCache::default();
        let (project, key, other) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        cache.put(KeyHash([1; 32]), identity(project, key));
        cache.put(KeyHash([2; 32]), identity(project, other));
        cache.invalidate_key(key);
        assert!(cache.get(&KeyHash([1; 32])).is_none());
        assert!(cache.get(&KeyHash([2; 32])).is_some());
        cache.invalidate_project(project);
        assert!(cache.get(&KeyHash([2; 32])).is_none());
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
            .map
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len();
        assert!(len <= KeyCache::CAP);
    }
}
