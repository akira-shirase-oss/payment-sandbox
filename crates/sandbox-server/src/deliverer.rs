//! The webhook deliverer (ARCHITECTURE "Webhook"; D-006, D-020, D-023, D-032).
//!
//! It claims due deliveries under a lease, signs each event, sends it to a loopback target
//! only (checked again after DNS resolution, and connecting to the resolved address), with
//! a 5 s deadline and no redirects, then records the attempt — but only while it still holds
//! the delivery's current lease token. Failures back off exponentially with full jitter to
//! at most 10 minutes; the 12th failure is `dead`. Delivery is at-least-once: a receiver
//! must deduplicate by event id (the reference store in `fixtures/dev-store` does).
//!
//! The clock is injected, so drill (b)'s wall-clock half can jump it.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use sandbox_db::webhooks::{self, Attempt, Claimed};
use sandbox_domain::webhook::{
    self, ATTEMPT_DEADLINE, EVENT_ID_HEADER, EventPayload, MAX_ATTEMPTS, SIGNATURE_HEADER,
};
use serde::Serialize;
use sqlx::PgPool;
use time::OffsetDateTime;
use tokio::sync::{Semaphore, watch};
use tokio::task::JoinSet;

use crate::secrets::SecretBox;
use crate::state::AppState;

/// Time as the deliverer sees it.
pub trait Clock: Send + Sync + 'static {
    /// Now.
    fn now(&self) -> OffsetDateTime;
}

/// The wall clock.
#[derive(Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }
}

/// The wall clock shifted by an adjustable offset (tests and drills).
#[derive(Debug, Default)]
pub struct ShiftedClock {
    offset_ms: AtomicI64,
}

impl ShiftedClock {
    /// Moves this clock by `ms` (either direction).
    pub fn jump(&self, ms: i64) {
        self.offset_ms.fetch_add(ms, Ordering::SeqCst);
    }
}

impl Clock for ShiftedClock {
    fn now(&self) -> OffsetDateTime {
        OffsetDateTime::now_utc()
            + time::Duration::milliseconds(self.offset_ms.load(Ordering::SeqCst))
    }
}

/// What one pass did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct DeliveryReport {
    /// Deliveries claimed.
    pub claimed: usize,
    /// Accepted by the receiver.
    pub delivered: usize,
    /// Failed; scheduled again.
    pub retried: usize,
    /// Failed for the last time.
    pub dead: usize,
    /// Results discarded because another worker holds a newer lease.
    pub stale: usize,
}

/// Most deliveries claimed per pass.
pub const BATCH: i64 = 32;
/// Most attempts in flight at once.
pub const CONCURRENCY: usize = 16;
/// Lease length: well beyond one attempt's deadline.
pub const LEASE: Duration = Duration::from_secs(30);

/// The deliverer.
#[derive(Clone)]
pub struct Deliverer {
    db: PgPool,
    secrets: Arc<SecretBox>,
    clock: Arc<dyn Clock>,
    http: reqwest::Client,
    permits: Arc<Semaphore>,
    poll: Duration,
}

impl std::fmt::Debug for Deliverer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Deliverer")
    }
}

impl Deliverer {
    /// Builds a deliverer, or `None` when webhooks are off (no `PS_SECRET_KEY`).
    pub fn new(state: &AppState, clock: Arc<dyn Clock>) -> Option<Self> {
        let secrets = state.secrets.clone()?;
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(1))
            .timeout(ATTEMPT_DEADLINE)
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .pool_max_idle_per_host(32)
            .build()
            .ok()?;
        Some(Self {
            db: state.db.clone(),
            secrets,
            clock,
            http,
            permits: Arc::new(Semaphore::new(CONCURRENCY)),
            poll: state.config.delivery_poll,
        })
    }

    /// Claims and attempts one batch of due deliveries.
    pub async fn run_once(&self) -> Result<DeliveryReport, sandbox_db::DbError> {
        let claimed = webhooks::claim(&self.db, self.clock.now(), LEASE, BATCH).await?;
        let mut report = DeliveryReport {
            claimed: claimed.len(),
            ..DeliveryReport::default()
        };
        let mut tasks = JoinSet::new();
        for delivery in claimed {
            let this = self.clone();
            let Ok(permit) = self.permits.clone().acquire_owned().await else {
                break;
            };
            tasks.spawn(async move {
                let attempt = this.attempt(&delivery).await;
                let recorded =
                    webhooks::complete(&this.db, &delivery, &attempt, this.clock.now()).await;
                drop(permit);
                (attempt, recorded)
            });
        }
        while let Some(joined) = tasks.join_next().await {
            match joined {
                Ok((_, Ok(false))) => report.stale += 1,
                Ok((Attempt::Delivered { .. }, Ok(true))) => report.delivered += 1,
                Ok((Attempt::Retry { .. }, Ok(true))) => report.retried += 1,
                Ok((Attempt::Dead { .. }, Ok(true))) => report.dead += 1,
                Ok((_, Err(error))) => {
                    tracing::warn!(%error, "recording a delivery attempt failed; its lease will expire");
                }
                Err(error) => tracing::error!(%error, "a delivery task failed"),
            }
        }
        Ok(report)
    }

    /// Delivers until `shutdown` turns true.
    pub async fn run(self, mut shutdown: watch::Receiver<bool>) {
        loop {
            let busy = match self.run_once().await {
                Ok(report) => report.claimed > 0,
                Err(error) => {
                    tracing::warn!(%error, "claiming deliveries failed");
                    false
                }
            };
            if busy {
                continue;
            }
            tokio::select! {
                () = tokio::time::sleep(self.poll) => {}
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        return;
                    }
                }
            }
        }
    }

    fn failure(&self, delivery: &Claimed, status: Option<u16>, error: &str) -> Attempt {
        let attempt = delivery.attempts.saturating_add(1);
        if attempt >= MAX_ATTEMPTS {
            return Attempt::Dead {
                status,
                error: error.to_owned(),
            };
        }
        let wait = crate::lifecycle::full_jitter(webhook::retry_ceiling(attempt));
        let next_at = self.clock.now()
            + time::Duration::milliseconds(i64::try_from(wait.as_millis()).unwrap_or(i64::MAX));
        Attempt::Retry {
            next_at,
            status,
            error: error.to_owned(),
        }
    }

    /// Checks the URL, resolves it, and refuses any resolution outside loopback. DNS
    /// rebinding cannot help: the request goes to the address checked here, not to a
    /// second lookup.
    async fn resolve(&self, delivery: &Claimed) -> Result<(SocketAddr, webhook::Target), Attempt> {
        let target = webhook::parse_target(&delivery.url).map_err(|e| Attempt::Dead {
            status: None,
            error: e.to_string(),
        })?;
        let host = target
            .host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .to_owned();
        let resolved = tokio::time::timeout(
            Duration::from_secs(1),
            tokio::net::lookup_host((host.as_str(), target.port)),
        )
        .await;
        let addresses: Vec<SocketAddr> = match resolved {
            Ok(Ok(addresses)) => addresses.collect(),
            _ => return Err(self.failure(delivery, None, "could not resolve the webhook host")),
        };
        let Some(address) = addresses.first().copied() else {
            return Err(self.failure(delivery, None, "the webhook host resolved to nothing"));
        };
        if addresses.iter().any(|a| !webhook::ip_allowed(a.ip())) {
            return Err(Attempt::Dead {
                status: None,
                error: "the webhook host resolves outside loopback".to_owned(),
            });
        }
        Ok((address, target))
    }

    async fn attempt(&self, delivery: &Claimed) -> Attempt {
        let Ok(secret) =
            self.secrets
                .open(&delivery.secret, delivery.project_id, delivery.endpoint_id)
        else {
            return self.failure(
                delivery,
                None,
                "signing secret unreadable with this server key",
            );
        };
        let previous = delivery.previous.as_ref().and_then(|p| {
            self.secrets
                .open(p, delivery.project_id, delivery.endpoint_id)
                .ok()
        });
        let (address, target) = match self.resolve(delivery).await {
            Ok(resolved) => resolved,
            Err(attempt) => return attempt,
        };
        let payload = EventPayload {
            id: delivery.event_id,
            kind: delivery.kind.clone(),
            project_id: delivery.project_id,
            payment_id: delivery.payment_id,
            operation_id: delivery.operation_id,
            sequence: delivery.sequence,
            created_at: delivery.created_at,
            simulated: true,
        };
        let Ok(body) = serde_json::to_vec(&payload) else {
            return Attempt::Dead {
                status: None,
                error: "event could not be encoded".to_owned(),
            };
        };
        let event_id = delivery.event_id.to_string();
        let mut secrets: Vec<&str> = vec![secret.as_str()];
        if let Some(previous) = &previous {
            secrets.push(previous.as_str());
        }
        let signature = webhook::signature_header(
            &secrets,
            &event_id,
            self.clock.now().unix_timestamp(),
            &body,
        );
        let sent = self
            .http
            .post(format!("http://{address}{}", target.path))
            .header("host", format!("{}:{}", target.host, target.port))
            .header("content-type", "application/json")
            .header("user-agent", "payment-sandbox-webhooks/0.1 (simulated)")
            .header("sandbox-simulated", "true")
            .header(EVENT_ID_HEADER, &event_id)
            .header(SIGNATURE_HEADER, signature)
            .body(body)
            .send()
            .await;
        match sent {
            Ok(response) if response.status().is_success() => Attempt::Delivered {
                status: response.status().as_u16(),
            },
            Ok(response) => {
                let status = response.status().as_u16();
                self.failure(delivery, Some(status), "the receiver did not answer 2xx")
            }
            Err(e) if e.is_timeout() => {
                self.failure(delivery, None, "the receiver exceeded the 5 s deadline")
            }
            Err(_) => self.failure(delivery, None, "could not reach the receiver"),
        }
    }
}
