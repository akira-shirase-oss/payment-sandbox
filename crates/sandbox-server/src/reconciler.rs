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
//! Nothing is ever re-sent blindly. A network error leaves the operation for the next sweep.
//! Each sweep is bounded (`PS_RECONCILE_BATCH`), and the loop stops on shutdown.

use sandbox_db::ProjectScope;
use sandbox_db::payments::{self, NetworkResult};
use sandbox_domain::NetworkOutcome;
use sandbox_domain::network::{Outcome, ReadbackStatus};
use serde::Serialize;
use tokio::sync::watch;
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
    /// Left open (network unreachable, or recording failed); retried next sweep.
    pub left_open: usize,
}

/// One bounded pass over the open operations.
pub async fn sweep(state: &AppState) -> Result<SweepReport, ApiError> {
    let open = payments::open_operations(
        &state.db,
        state.config.reconcile_grace,
        state.config.reconcile_batch,
    )
    .await?;
    let mut report = SweepReport {
        examined: open.len(),
        ..SweepReport::default()
    };
    for op in open {
        match resolve(state, op.scope, op.payment_id, op.id).await {
            Ok(Some(NetworkOutcome::NotExecuted)) => report.fenced += 1,
            Ok(Some(_)) => report.resolved_from_record += 1,
            Ok(None) | Err(_) => report.left_open += 1,
        }
    }
    if report.examined > 0 {
        tracing::info!(?report, "reconciliation sweep");
    }
    Ok(report)
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
    let operation = payments::get_operation(&state.db, scope, operation_id).await?;
    let payment = payments::get_payment(&state.db, scope, payment_id).await?;
    Ok(operation
        .zip(payment)
        .map(|(o, p)| lifecycle::network_request(scope, &o, &p).fingerprint()))
}

/// Runs a sweep at once and then every `interval`, until `shutdown` turns true.
pub async fn run(state: AppState, mut shutdown: watch::Receiver<bool>) {
    let interval = state.config.reconcile_interval;
    loop {
        if let Err(error) = sweep(&state).await {
            tracing::warn!(%error, "reconciliation sweep failed");
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

/// Network operations of `scope`'s project that the server has no record of — what a
/// restore of the server's database to an older backup leaves behind. Reported, never
/// applied blindly: a client retry under the same key re-derives the same identity and
/// applies the network's record once. Pages through at most `max_pages` × 1,000.
pub async fn unknown_to_server(
    state: &AppState,
    scope: ProjectScope,
    max_pages: usize,
) -> Result<Vec<Uuid>, ApiError> {
    let mut unknown = Vec::new();
    let mut after = 0;
    for _ in 0..max_pages {
        let page = state.network.list(after, 1_000).await.map_err(|e| {
            ApiError::Unavailable(anyhow::anyhow!("listing the network's operations: {e}"))
        })?;
        let ids: Vec<Uuid> = page
            .operations
            .iter()
            .filter(|o| o.project_id == scope.project_id)
            .map(|o| o.id)
            .collect();
        if !ids.is_empty() {
            unknown.extend(payments::unknown_operations(&state.db, scope.project_id, &ids).await?);
        }
        match page.next_after {
            Some(next) => after = next,
            None => break,
        }
        if unknown.len() >= 1_000 {
            break;
        }
    }
    Ok(unknown)
}

/// A shutdown signal for [`run`].
pub fn shutdown_channel() -> (watch::Sender<bool>, watch::Receiver<bool>) {
    watch::channel(false)
}
