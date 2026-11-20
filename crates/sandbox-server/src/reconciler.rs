//! The reconciler (ARCHITECTURE "Recovery / reconcile"; D-004, D-017).
//!
//! At startup and then periodically, it finds operations still `pending` or `uncertain`
//! (older than a grace period, so a live request is not raced), asks the network for its
//! record **with a fence**, and applies the answer exactly once:
//!
//! - executed → the recorded approval or decline is applied, as if the reply had arrived;
//! - never executed → the identity is tombstoned at the network, so a delayed request can
//!   never execute later, and the operation becomes `not_executed`, releasing any
//!   reservation.
//!
//! Nothing is ever re-sent blindly. Each sweep is bounded (`PS_RECONCILE_BATCH`) and fair
//! (D-048, review finding 11): projects are taken in turn, so one project's backlog cannot
//! starve another's recovery; readbacks run concurrently, at most
//! `PS_RECONCILE_CONCURRENCY` at once; a check that fails (network unreachable, an unusable
//! answer) postpones that operation with exponential backoff instead of heading every
//! sweep; and an operation open longer than `PS_RECONCILE_ALERT_AGE_MS` is reported at
//! WARN on every sweep. A full batch is followed at once by the next sweep. The loop stops
//! on shutdown.

use std::sync::Arc;

use sandbox_db::ProjectScope;
use sandbox_db::payments::{self, NetworkResult, OpenOperation};
use sandbox_domain::NetworkOutcome;
use sandbox_domain::network::{Outcome, ReadbackStatus};
use serde::Serialize;
use sqlx::PgPool;
use tokio::sync::{Semaphore, watch};
use tokio::task::JoinSet;
use uuid::Uuid;

use crate::error::ApiError;
use crate::lifecycle;
use crate::state::AppState;

/// What one sweep did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct SweepReport {
    /// Open operations examined.
    pub examined: usize,
    /// Resolved from the network's record (approved or declined).
    pub resolved_from_record: usize,
    /// Fenced: proved never executed.
    pub fenced: usize,
    /// Left open (network unreachable, or recording failed); checked again after a backoff.
    pub left_open: usize,
    /// Age of the oldest open operation of any project, in milliseconds, if one is open.
    pub oldest_open_ms: Option<u64>,
}

/// One bounded, fair pass over the open operations.
pub async fn sweep(state: &AppState) -> Result<SweepReport, ApiError> {
    crate::faults::reconciler_panic();
    let tuning = state.config.reconcile;
    let open = payments::open_operations(
        &state.write_db,
        state.config.reconcile_grace,
        state.config.reconcile_batch,
        tuning.per_project,
    )
    .await?;
    let mut report = SweepReport {
        examined: open.len(),
        ..SweepReport::default()
    };
    let permits = Arc::new(Semaphore::new(tuning.concurrency));
    let mut checks = JoinSet::new();
    for op in open {
        let Ok(permit) = permits.clone().acquire_owned().await else {
            break;
        };
        let state = state.clone();
        checks.spawn(async move {
            let outcome = resolve(&state, op.scope, op.payment_id, op.id).await;
            drop(permit);
            (op, outcome)
        });
    }
    while let Some(joined) = checks.join_next().await {
        let (op, outcome) = match joined {
            Ok(done) => done,
            Err(error) => {
                tracing::error!(%error, "a readback task failed; its operation stays open");
                report.left_open += 1;
                continue;
            }
        };
        match outcome {
            Ok(Some(NetworkOutcome::NotExecuted)) => report.fenced += 1,
            Ok(Some(_)) => report.resolved_from_record += 1,
            Ok(None) => {
                report.left_open += 1;
                postpone(state, &op).await;
            }
            Err(error) => {
                tracing::warn!(operation = %op.id, %error, "recording a readback failed; the operation stays open");
                report.left_open += 1;
                postpone(state, &op).await;
            }
        }
    }
    let oldest = payments::oldest_open_age(&state.write_db).await?;
    report.oldest_open_ms = oldest.map(|age| u64::try_from(age.as_millis()).unwrap_or(u64::MAX));
    if let Some(age) = oldest
        && age > tuning.alert_age
    {
        tracing::warn!(
            oldest_open_ms = report.oldest_open_ms,
            "an operation has been open longer than PS_RECONCILE_ALERT_AGE_MS"
        );
    }
    if report.examined > 0 {
        tracing::info!(?report, "reconciliation sweep");
    }
    Ok(report)
}

/// A failed check waits before the next: one interval, doubling, at most the maximum.
async fn postpone(state: &AppState, op: &OpenOperation) {
    if let Err(error) = payments::postpone_check(
        &state.write_db,
        op.scope,
        op.id,
        state.config.reconcile_interval,
        state.config.reconcile.backoff_max,
    )
    .await
    {
        tracing::warn!(operation = %op.id, %error, "postponing a failed check failed; it is due again at once");
    }
}

/// Resolves one open operation by fenced readback. `None` when the network could not be
/// asked.
async fn resolve(
    state: &AppState,
    scope: ProjectScope,
    payment_id: Uuid,
    operation_id: Uuid,
) -> Result<Option<NetworkOutcome>, ApiError> {
    let readback = match state.network.readback(operation_id, true).await {
        Ok(readback) => readback,
        Err(error) => {
            tracing::warn!(operation = %operation_id, %error, "readback failed; leaving the operation open");
            return Ok(None);
        }
    };
    let expected = expected_fingerprint(state, scope, payment_id, operation_id).await?;
    let result = match (readback.status, readback.result) {
        // The network's record is of another request (the key was reused with other
        // fields, e.g. after a restore): declined as a conflict, never applied.
        (ReadbackStatus::Executed, Some(reply))
            if Some(&reply.fingerprint) != expected.as_ref() =>
        {
            NetworkResult {
                outcome: NetworkOutcome::Declined,
                decline_code: Some(lifecycle::IDENTITY_CONFLICT.to_owned()),
                network_ref: None,
            }
        }
        (ReadbackStatus::Executed, Some(reply)) => NetworkResult {
            outcome: match reply.outcome {
                Outcome::Approved => NetworkOutcome::Approved,
                Outcome::Declined => NetworkOutcome::Declined,
            },
            decline_code: reply.decline_code,
            network_ref: Some(reply.network_ref),
        },
        (ReadbackStatus::NotExecuted, _) => NetworkResult {
            outcome: NetworkOutcome::NotExecuted,
            decline_code: None,
            network_ref: None,
        },
        (status, _) => {
            tracing::error!(operation = %operation_id, ?status, "fenced readback returned an unusable answer");
            return Ok(None);
        }
    };
    let outcome = result.outcome;
    lifecycle::apply_with_retry(state, scope, payment_id, operation_id, &result).await?;
    tracing::info!(operation = %operation_id, ?outcome, "resolved by readback");
    Ok(Some(outcome))
}

/// The fingerprint of the request the server would send for this operation.
async fn expected_fingerprint(
    state: &AppState,
    scope: ProjectScope,
    payment_id: Uuid,
    operation_id: Uuid,
) -> Result<Option<String>, ApiError> {
    let operation = payments::get_operation(&state.write_db, scope, operation_id).await?;
    let payment = payments::get_payment(&state.write_db, scope, payment_id).await?;
    Ok(operation
        .zip(payment)
        .map(|(o, p)| lifecycle::network_request(scope, &o, &p).fingerprint()))
}

/// The reconciler's name under supervision (D-051).
pub const TASK: &str = "reconciler";

/// Runs a sweep at once and then every `interval`, until `shutdown` turns true. Each sweep
/// is a heartbeat for readiness.
pub async fn run(state: AppState, mut shutdown: watch::Receiver<bool>) {
    let interval = state.config.reconcile_interval;
    let full = usize::try_from(state.config.reconcile_batch).unwrap_or(usize::MAX);
    loop {
        let swept = sweep(&state).await;
        state.tasks.beat(TASK);
        match swept {
            // A full batch: more may be due; the next bounded sweep starts at once.
            Ok(report) if report.examined >= full && !*shutdown.borrow() => {
                tokio::task::yield_now().await;
                continue;
            }
            Ok(_) => {}
            Err(error) => tracing::warn!(%error, "reconciliation sweep failed"),
        }
        tokio::select! {
            () = tokio::time::sleep(interval) => {}
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return;
                }
            }
        }
    }
}

/// One field on which the server and the network disagree about an operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Disagreement {
    /// The operation.
    pub id: Uuid,
    /// `kind`, `amount_minor` or `outcome`.
    pub field: &'static str,
    /// What the server recorded.
    pub server: String,
    /// What the network recorded.
    pub network: String,
}

/// Agreement between the server's records and the network's, for one project, both ways.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Agreement {
    /// True only if the network's listing was read to its end.
    pub complete: bool,
    /// Network operations read.
    pub network_operations_scanned: usize,
    /// On the network, unknown to the server (what a restore of an older backup leaves).
    pub unknown_to_server: Vec<Uuid>,
    /// Answered on the server (succeeded or declined), unknown to the network.
    pub unknown_to_network: Vec<Uuid>,
    /// Known to both, recorded differently.
    pub disagreements: Vec<Disagreement>,
}

impl Agreement {
    /// True when the scan completed and found nothing.
    pub fn is_clean(&self) -> bool {
        self.complete
            && self.unknown_to_server.is_empty()
            && self.unknown_to_network.is_empty()
            && self.disagreements.is_empty()
    }
}

/// Most entries kept in each list of an [`Agreement`].
const LIST_BOUND: usize = 1_000;

fn push_bounded<T>(list: &mut Vec<T>, item: T) {
    if list.len() < LIST_BOUND {
        list.push(item);
    }
}

/// Compares `scope`'s project with the network, both ways and field by field (review
/// finding 4): every network operation of the project must be recorded by the server with
/// the same kind, amount and outcome, and every operation the server says the network
/// answered must be on the network. Reads at most `PS_RECONCILE_NETWORK_MAX_PAGES` pages of
/// 1,000; a scan that runs out reports `complete: false` and is never clean. Disagreements
/// are reported, never applied.
pub async fn network_agreement(
    state: &AppState,
    db: &PgPool,
    scope: ProjectScope,
) -> Result<Agreement, ApiError> {
    let mut agreement = Agreement::default();
    let mut on_network = std::collections::HashSet::new();
    let mut after = 0;
    let mut pages = 0;
    loop {
        if pages == state.config.reconcile_network_max_pages {
            break;
        }
        pages += 1;
        let page = state
            .network
            .list(scope.project_id, after, 1_000)
            .await
            .map_err(|e| {
                ApiError::Unavailable(anyhow::anyhow!("listing the network's operations: {e}"))
            })?;
        agreement.network_operations_scanned += page.operations.len();
        let ids: Vec<Uuid> = page.operations.iter().map(|o| o.id).collect();
        let recorded = payments::recorded_operations(db, scope.project_id, &ids).await?;
        let recorded: std::collections::HashMap<Uuid, payments::RecordedOperation> =
            recorded.into_iter().map(|r| (r.id, r)).collect();
        for op in &page.operations {
            on_network.insert(op.id);
            let Some(server) = recorded.get(&op.id) else {
                push_bounded(&mut agreement.unknown_to_server, op.id);
                continue;
            };
            let network_outcome = match op.outcome {
                Outcome::Approved => "succeeded",
                Outcome::Declined => "declined",
            };
            let mut differ = |field, server: String, network: String| {
                if server != network {
                    push_bounded(
                        &mut agreement.disagreements,
                        Disagreement {
                            id: op.id,
                            field,
                            server,
                            network,
                        },
                    );
                }
            };
            differ("kind", server.kind.clone(), op.kind.code().to_owned());
            differ(
                "amount_minor",
                server.amount_minor.to_string(),
                op.amount_minor.to_string(),
            );
            let server_outcome =
                if server.decline_code.as_deref() == Some(lifecycle::IDENTITY_CONFLICT) {
                    "conflict".to_owned()
                } else {
                    server.state.clone()
                };
            differ("outcome", server_outcome, network_outcome.to_owned());
        }
        let Some(next) = page.next_after else {
            agreement.complete = true;
            break;
        };
        after = next;
    }
    if agreement.complete {
        let mut cursor = None;
        loop {
            let ids =
                payments::answered_operation_ids(&state.write_db, scope.project_id, cursor, 10_000)
                    .await?;
            let Some(last) = ids.last().copied() else {
                break;
            };
            for id in ids {
                if !on_network.contains(&id) {
                    push_bounded(&mut agreement.unknown_to_network, id);
                }
            }
            cursor = Some(last);
        }
    }
    Ok(agreement)
}

/// A shutdown signal for [`run`].
pub fn shutdown_channel() -> (watch::Sender<bool>, watch::Receiver<bool>) {
    watch::channel(false)
}
