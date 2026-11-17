//! The payment lifecycle: identity first, the network call outside any transaction, the
//! answer applied exactly once (ARCHITECTURE "Paths"; D-004, D-014, D-015).
//!
//! A repeated request with the same key and body *re-drives* an operation that is still
//! open: it calls the network again under the **same** identity, which the network answers
//! from its record. That is reconciliation by readback, not a blind replay: the network can
//! never execute one identity twice.

use sandbox_db::ProjectScope;
use sandbox_db::payments::{self, Begin, BeginError, NetworkResult, Operation, Payment};
use sandbox_domain::identity::{self, IdempotencyKey, OperationRequest};
use sandbox_domain::network::ExecuteRequest;
use sandbox_domain::{Amount, CapacityError, Currency, NetworkOutcome, OperationKind};
use uuid::Uuid;

use crate::error::ApiError;
use crate::network::Answer;
use crate::state::AppState;

/// What a lifecycle call produced.
#[derive(Debug, Clone)]
pub struct Outcome {
    /// The operation, as recorded.
    pub operation: Operation,
    /// Its payment, as recorded.
    pub payment: Payment,
    /// True when the key had been seen before.
    pub replayed: bool,
}

/// A validated authorization request.
#[derive(Debug, Clone)]
pub struct AuthorizeInput {
    /// Amount.
    pub amount: Amount,
    /// Currency.
    pub currency: Currency,
    /// Scenario token (validated).
    pub token: String,
    /// Description.
    pub description: Option<String>,
}

/// Places a hold.
pub async fn authorize(
    state: &AppState,
    scope: ProjectScope,
    key: &IdempotencyKey,
    input: &AuthorizeInput,
) -> Result<Outcome, ApiError> {
    let operation_id =
        identity::operation_id(scope.project_id, scope.epoch, OperationKind::Authorize, key);
    let payment_id = identity::payment_id(operation_id);
    let fingerprint = OperationRequest {
        kind: OperationKind::Authorize,
        payment: None,
        amount: Some(input.amount),
        currency: Some(input.currency),
        token: Some(&input.token),
        description: input.description.as_deref(),
    }
    .fingerprint();
    let begun = payments::begin_authorize(
        &state.db,
        scope,
        &payments::NewAuthorize {
            operation_id,
            payment_id,
            idempotency_key: key.as_str(),
            fingerprint,
            amount: input.amount,
            currency: input.currency,
            token: &input.token,
            description: input.description.as_deref(),
        },
    )
    .await
    .map_err(begin_error)?;
    finish(state, scope, begun).await
}

/// Captures, voids or refunds. `amount` is `None` for a void.
pub async fn child(
    state: &AppState,
    scope: ProjectScope,
    key: &IdempotencyKey,
    kind: OperationKind,
    payment_id: Uuid,
    amount: Option<Amount>,
) -> Result<Outcome, ApiError> {
    let operation_id = identity::operation_id(scope.project_id, scope.epoch, kind, key);
    let fingerprint = OperationRequest {
        kind,
        payment: Some(payment_id),
        amount,
        currency: None,
        token: None,
        description: None,
    }
    .fingerprint();
    let begun = {
        // Queue behind this payment's other requests before taking a connection (D-033);
        // the turn ends with this block, before the network is called.
        let _turn = state
            .payment_locks
            .lock(payment_id, state.config.payment_queue_wait)
            .await
            .map_err(|_| ApiError::PaymentBusy)?;
        payments::begin_child(
            &state.db,
            scope,
            &payments::NewChild {
                operation_id,
                payment_id,
                kind,
                idempotency_key: key.as_str(),
                fingerprint,
                amount,
            },
        )
        .await
        .map_err(begin_error)?
    };
    finish(state, scope, begun).await
}

async fn finish(state: &AppState, scope: ProjectScope, begun: Begin) -> Result<Outcome, ApiError> {
    match begun {
        Begin::Started(operation, payment) => {
            drive(state, scope, &operation, &payment, false).await
        }
        Begin::Existing(operation, payment) if operation.state.is_terminal() => Ok(Outcome {
            operation,
            payment,
            replayed: true,
        }),
        Begin::Existing(operation, payment) => {
            drive(state, scope, &operation, &payment, true).await
        }
    }
}

/// Calls the network for an open operation (no transaction is open here) and applies the
/// answer.
pub async fn drive(
    state: &AppState,
    scope: ProjectScope,
    operation: &Operation,
    payment: &Payment,
    replayed: bool,
) -> Result<Outcome, ApiError> {
    let request = network_request(scope, operation, payment);
    crate::faults::point(crate::faults::Point::BeforeNetwork, operation.kind);
    let answer = state.network.execute(&request).await;
    crate::faults::point(crate::faults::Point::AfterNetwork, operation.kind);
    let conflict = answer == Answer::Conflict;
    let result = network_result(&answer);
    if let Answer::Unknown(reason) = answer {
        tracing::warn!(operation = %operation.id, kind = %operation.kind, reason, "network answer unknown; operation stays uncertain");
    }
    let (operation, payment) = apply_with_retry(state, scope, payment.id, operation.id, &result)
        .await?
        .into_parts();
    if conflict {
        // The key was used with other fields (here: the server's record of it was lost to
        // a restore). Recorded as declined, reservation released, and never reinterpreted.
        return Err(ApiError::IdempotencyKeyConflict);
    }
    Ok(Outcome {
        operation,
        payment,
        replayed,
    })
}

/// Attempts at recording one answer; with jittered backoff from 10 ms they span at most
/// about 600 ms.
const APPLY_ATTEMPTS: u32 = 6;

/// Records the network's answer, retrying transient database failures (a lock or statement
/// deadline, a busy pool) with exponential backoff and full jitter. Applying is idempotent,
/// so a retry can never double-post; giving up leaves the operation open (its reservation
/// held) for a client retry or the reconciler, never half-applied.
pub(crate) async fn apply_with_retry(
    state: &AppState,
    scope: ProjectScope,
    payment_id: Uuid,
    operation_id: Uuid,
    result: &NetworkResult,
) -> Result<payments::Applied, ApiError> {
    let mut ceiling = std::time::Duration::from_millis(10);
    for attempt in 1..=APPLY_ATTEMPTS {
        let applied = {
            let _turn = state
                .payment_locks
                .lock(payment_id, state.config.payment_queue_wait)
                .await
                .map_err(|_| ApiError::PaymentBusy)?;
            payments::apply(&state.db, scope, payment_id, operation_id, result).await
        };
        match applied {
            Ok(applied) => return Ok(applied),
            Err(error) if error.is_transient() && attempt < APPLY_ATTEMPTS => {
                tracing::warn!(operation = %operation_id, attempt, %error, "recording the network answer failed; retrying");
                tokio::time::sleep(full_jitter(ceiling)).await;
                ceiling = ceiling.saturating_mul(2);
            }
            Err(error) => return Err(error.into()),
        }
    }
    Err(ApiError::Internal(anyhow::anyhow!(
        "apply retry loop ended without a result"
    )))
}

/// A uniformly random duration in `[0, ceiling]` (full jitter).
pub(crate) fn full_jitter(ceiling: std::time::Duration) -> std::time::Duration {
    let mut bytes = [0_u8; 8];
    if getrandom::fill(&mut bytes).is_err() {
        return ceiling;
    }
    let max = u64::try_from(ceiling.as_micros())
        .unwrap_or(u64::MAX)
        .max(1);
    std::time::Duration::from_micros(u64::from_le_bytes(bytes) % max)
}

/// The request the network gets for `operation`; its fingerprint is what any answer or
/// readback must match.
pub fn network_request(
    scope: ProjectScope,
    operation: &Operation,
    payment: &Payment,
) -> ExecuteRequest {
    ExecuteRequest {
        id: operation.id,
        project_id: scope.project_id,
        kind: operation.kind,
        parent_id: (operation.kind != OperationKind::Authorize)
            .then_some(payment.authorize_operation_id),
        amount_minor: operation.amount_minor,
        currency: payment.currency,
        token: (operation.kind == OperationKind::Authorize).then(|| payment.token.clone()),
    }
}

/// Decline code recorded when the network holds another request under the identity.
pub const IDENTITY_CONFLICT: &str = sandbox_domain::checkout::NETWORK_IDENTITY_CONFLICT;

/// Translates a network answer into what is recorded.
pub fn network_result(answer: &Answer) -> NetworkResult {
    match answer {
        Answer::Approved(reply) => NetworkResult {
            outcome: NetworkOutcome::Approved,
            decline_code: None,
            network_ref: Some(reply.network_ref.clone()),
        },
        Answer::Declined(reply) => NetworkResult {
            outcome: NetworkOutcome::Declined,
            decline_code: reply.decline_code.clone(),
            network_ref: Some(reply.network_ref.clone()),
        },
        Answer::Unknown(_) => NetworkResult {
            outcome: NetworkOutcome::Unknown,
            decline_code: None,
            network_ref: None,
        },
        Answer::Conflict => NetworkResult {
            outcome: NetworkOutcome::Declined,
            decline_code: Some(IDENTITY_CONFLICT.to_owned()),
            network_ref: None,
        },
        Answer::Fenced => NetworkResult {
            outcome: NetworkOutcome::NotExecuted,
            decline_code: None,
            network_ref: None,
        },
    }
}

fn begin_error(error: BeginError) -> ApiError {
    match error {
        BeginError::KeyConflict => ApiError::IdempotencyKeyConflict,
        BeginError::PaymentNotFound => ApiError::NotFound("payment"),
        BeginError::Capacity(CapacityError::InsufficientCapacity {
            requested,
            available,
        }) => ApiError::InsufficientCapacity(format!(
            "requested {requested} minor units, but only {available} remain; the excess is refused"
        )),
        BeginError::Capacity(CapacityError::NotAuthorized) => {
            ApiError::InvalidState("the payment is not authorized".to_owned())
        }
        BeginError::Capacity(CapacityError::CaptureInFlight) => ApiError::InvalidState(
            "a capture is still in flight; void after it resolves".to_owned(),
        ),
        BeginError::Capacity(CapacityError::NothingToVoid) => {
            ApiError::InvalidState("nothing is left to void".to_owned())
        }
        BeginError::Capacity(other) => {
            ApiError::Internal(anyhow::anyhow!("capacity defect: {other}"))
        }
        BeginError::Db(db) => db.into(),
    }
}
