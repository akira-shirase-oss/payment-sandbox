//! Payments: authorize, capture, void, refund, and reads. Money-moving requests need an
//! `Idempotency-Key` and the `payments:write` scope; bodies pass the card guard first.

use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use sandbox_db::payments::{Operation, Payment};
use sandbox_domain::keys::Scope;
use sandbox_domain::money::format_minor;
use sandbox_domain::scenario::Scenario;
use sandbox_domain::{Amount, Currency, OperationKind, OperationState};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::auth::Auth;
use crate::error::ApiError;
use crate::extract::{IdempotencyKeyHeader, PathId, SandboxJson};
use crate::lifecycle::{self, AuthorizeInput, Outcome};
use crate::state::AppState;

/// `POST /v1/payments` body.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreatePayment {
    amount: i64,
    currency: String,
    token: String,
    description: Option<String>,
}

/// `POST /v1/payments/{id}/captures|refunds` body.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AmountBody {
    amount: i64,
}

/// `POST /v1/payments/{id}/void` body (empty object).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Empty {}

fn amount(minor: i64) -> Result<Amount, ApiError> {
    Amount::new(minor).map_err(|e| ApiError::InvalidBody(format!("amount: {e}")))
}

/// Places a hold: the payment and its authorize operation.
pub async fn create(
    State(state): State<AppState>,
    auth: Auth,
    IdempotencyKeyHeader(key): IdempotencyKeyHeader,
    SandboxJson(body): SandboxJson<CreatePayment>,
) -> Result<Response, ApiError> {
    auth.require(Scope::PaymentsWrite)?;
    let currency =
        Currency::from_code(&body.currency).map_err(|e| ApiError::InvalidBody(e.to_string()))?;
    Scenario::from_token(&body.token).map_err(|e| ApiError::InvalidPaymentToken(e.to_string()))?;
    if let Some(description) = &body.description
        && (description.chars().count() > 200 || description.chars().any(char::is_control))
    {
        return Err(ApiError::InvalidBody(
            "description must be at most 200 characters without control characters".to_owned(),
        ));
    }
    let input = AuthorizeInput {
        amount: amount(body.amount)?,
        currency,
        token: body.token,
        description: body.description,
    };
    let outcome = lifecycle::authorize(&state, auth.scope(), &key, &input).await?;
    Ok(respond(&outcome))
}

/// Captures part or all of the hold.
pub async fn capture(
    State(state): State<AppState>,
    auth: Auth,
    PathId(payment_id): PathId,
    IdempotencyKeyHeader(key): IdempotencyKeyHeader,
    SandboxJson(body): SandboxJson<AmountBody>,
) -> Result<Response, ApiError> {
    auth.require(Scope::PaymentsWrite)?;
    let outcome = lifecycle::child(
        &state,
        auth.scope(),
        &key,
        OperationKind::Capture,
        payment_id,
        Some(amount(body.amount)?),
    )
    .await?;
    Ok(respond(&outcome))
}

/// Releases the uncaptured remainder.
pub async fn void(
    State(state): State<AppState>,
    auth: Auth,
    PathId(payment_id): PathId,
    IdempotencyKeyHeader(key): IdempotencyKeyHeader,
    SandboxJson(Empty {}): SandboxJson<Empty>,
) -> Result<Response, ApiError> {
    auth.require(Scope::PaymentsWrite)?;
    let outcome = lifecycle::child(
        &state,
        auth.scope(),
        &key,
        OperationKind::Void,
        payment_id,
        None,
    )
    .await?;
    Ok(respond(&outcome))
}

/// Refunds captured money.
pub async fn refund(
    State(state): State<AppState>,
    auth: Auth,
    PathId(payment_id): PathId,
    IdempotencyKeyHeader(key): IdempotencyKeyHeader,
    SandboxJson(body): SandboxJson<AmountBody>,
) -> Result<Response, ApiError> {
    auth.require(Scope::PaymentsWrite)?;
    let outcome = lifecycle::child(
        &state,
        auth.scope(),
        &key,
        OperationKind::Refund,
        payment_id,
        Some(amount(body.amount)?),
    )
    .await?;
    Ok(respond(&outcome))
}

/// A payment with its operations.
pub async fn get(
    State(state): State<AppState>,
    auth: Auth,
    PathId(payment_id): PathId,
) -> Result<Response, ApiError> {
    auth.require(Scope::Read)?;
    let payment = sandbox_db::payments::get_payment(&state.db, auth.scope(), payment_id)
        .await?
        .ok_or(ApiError::NotFound("payment"))?;
    let operations =
        sandbox_db::payments::operations_for_payment(&state.db, auth.scope(), payment_id).await?;
    Ok(Json(json!({
        "payment": payment_json(&payment),
        "operations": operations.iter().map(operation_json).collect::<Vec<_>>(),
        "simulated": true,
    }))
    .into_response())
}

/// A payment's journal entries.
pub async fn journal(
    State(state): State<AppState>,
    auth: Auth,
    PathId(payment_id): PathId,
) -> Result<Response, ApiError> {
    auth.require(Scope::Read)?;
    if sandbox_db::payments::get_payment(&state.db, auth.scope(), payment_id)
        .await?
        .is_none()
    {
        return Err(ApiError::NotFound("payment"));
    }
    let entries =
        sandbox_db::journal::entries_for_payment(&state.db, auth.scope(), payment_id).await?;
    Ok(Json(json!({"entries": entries, "simulated": true})).into_response())
}

/// `GET /v1/payments?limit=` query.
#[derive(Debug, Deserialize)]
pub struct ListQuery {
    limit: Option<i64>,
}

/// The most recent payments.
pub async fn list(
    State(state): State<AppState>,
    auth: Auth,
    Query(q): Query<ListQuery>,
) -> Result<Response, ApiError> {
    auth.require(Scope::Read)?;
    let payments =
        sandbox_db::payments::recent_payments(&state.db, auth.scope(), q.limit.unwrap_or(50))
            .await?;
    Ok(Json(json!({
        "payments": payments.iter().map(payment_json).collect::<Vec<_>>(),
        "simulated": true,
    }))
    .into_response())
}

/// Every account balance, summed from the journal (D-028).
pub async fn balances(State(state): State<AppState>, auth: Auth) -> Result<Response, ApiError> {
    auth.require(Scope::Read)?;
    let balances = sandbox_db::journal::balances(&state.db, auth.scope()).await?;
    let rows: Vec<Value> = balances
        .iter()
        .map(|b| {
            let presented = b.account.presented(b.signed_minor);
            json!({
                "currency": b.currency,
                "account": b.account,
                "normal_side": b.account.normal_side(),
                "balance_minor": presented,
                "balance": format_minor(presented, b.currency),
                "signed_minor": b.signed_minor,
            })
        })
        .collect();
    Ok(Json(json!({"balances": rows, "source": "journal", "simulated": true})).into_response())
}

/// `GET /v1/reconciliation?network=true` query.
#[derive(Debug, Deserialize)]
pub struct ReconcileQuery {
    #[serde(default)]
    network: bool,
}

/// The reconciliation report for the caller's project. With `network=true` it also lists
/// the network's operations the server has no record of (reverse reconciliation, D-015).
pub async fn reconciliation(
    State(state): State<AppState>,
    auth: Auth,
    Query(q): Query<ReconcileQuery>,
) -> Result<Response, ApiError> {
    auth.require(Scope::Read)?;
    let report = sandbox_db::reconcile::run(&state.db, auth.scope()).await?;
    let mut clean = report.is_clean();
    let mut body = json!({"report": report, "simulated": true});
    if q.network {
        let agreement = crate::reconciler::network_agreement(&state, auth.scope()).await?;
        clean = clean && agreement.is_clean();
        body["network"] = json!(agreement);
    }
    body["clean"] = json!(clean);
    Ok(Json(body).into_response())
}

fn respond(outcome: &Outcome) -> Response {
    let status = if outcome.replayed {
        StatusCode::OK
    } else if matches!(
        outcome.operation.state,
        OperationState::Pending | OperationState::Uncertain
    ) {
        StatusCode::ACCEPTED
    } else {
        StatusCode::CREATED
    };
    (
        status,
        Json(json!({
            "payment": payment_json(&outcome.payment),
            "operation": operation_json(&outcome.operation),
            "replayed": outcome.replayed,
            "simulated": true,
        })),
    )
        .into_response()
}

/// The payment as the API shows it.
pub fn payment_json(p: &Payment) -> Value {
    json!({
        "id": p.id,
        "status": p.status,
        "amount": p.amount_minor,
        "currency": p.currency,
        "token": p.token,
        "description": p.description,
        "authorized": p.sums.authorized,
        "captured": p.sums.captured,
        "voided": p.sums.voided,
        "refunded": p.sums.refunded,
        "capturable": p.sums.capturable(),
        "refundable": p.sums.refundable(),
        "reserved": {
            "capture": p.sums.capture_reserved,
            "void": p.sums.void_reserved,
            "refund": p.sums.refund_reserved,
        },
        "decline_code": p.decline_code,
        "authorize_operation_id": p.authorize_operation_id,
        "created_at": p.created_at.unix_timestamp(),
        "simulated": true,
    })
}

/// The operation as the API shows it.
pub fn operation_json(o: &Operation) -> Value {
    json!({
        "id": o.id,
        "kind": o.kind,
        "state": o.state,
        "amount": o.amount_minor,
        "idempotency_key": o.idempotency_key,
        "decline_code": o.decline_code,
        "network_ref": o.network_ref,
        "attempts": o.attempts,
    })
}

/// `GET /v1/balances/{currency}/{account}/explain` query: a first page has none of the
/// cursor fields; later pages pass back `as_of`, `after_seq` and `running` from `next`.
#[derive(Debug, Deserialize)]
pub struct ExplainQuery {
    as_of: Option<String>,
    after_seq: Option<i64>,
    running: Option<i64>,
    limit: Option<i64>,
}

/// The postings that sum to one account's balance, page by page, with running totals:
/// the ship gate's "every synthetic balance can be explained from journal entries".
pub async fn explain(
    State(state): State<AppState>,
    auth: Auth,
    Path((currency, account)): Path<(String, String)>,
    Query(q): Query<ExplainQuery>,
) -> Result<Response, ApiError> {
    auth.require(Scope::Read)?;
    let currency = Currency::from_code(&currency).map_err(|_| ApiError::NotFound("currency"))?;
    let account = sandbox_domain::AccountKind::from_code(&account)
        .map_err(|_| ApiError::NotFound("account"))?;
    let cursor = match (q.as_of, q.after_seq, q.running) {
        (Some(as_of), Some(after_seq), Some(running))
            if as_of.len() <= 16_384
                && as_of
                    .bytes()
                    .all(|b| b.is_ascii_digit() || b == b':' || b == b',') =>
        {
            Some(sandbox_db::reconcile::ExplainCursor {
                as_of,
                after_seq,
                running,
            })
        }
        (None, None, None) => None,
        _ => {
            return Err(ApiError::BadRequest(
                "pass as_of, after_seq and running together, as returned in `next`".to_owned(),
            ));
        }
    };
    let explanation = sandbox_db::reconcile::explain(
        &state.db,
        auth.scope(),
        currency.code(),
        account.code(),
        cursor,
        q.limit.unwrap_or(100),
    )
    .await?;
    let presented = account.presented(explanation.balance_minor);
    Ok(Json(json!({
        "currency": currency,
        "account": account,
        "normal_side": account.normal_side(),
        "balance_minor": presented,
        "balance": format_minor(presented, currency),
        "explanation": explanation,
        "simulated": true,
    }))
    .into_response())
}
