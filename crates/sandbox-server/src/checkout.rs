//! Checkout sessions on the server (D-060): driving an attempt through the payment
//! lifecycle, and the views the payer's page and the merchant's API read.
//!
//! Driving is always safe to repeat. The authorize and capture keys are derived from the
//! session and the attempt, so a retry, a double click or a page that lost its tab state
//! re-drives the same identities, which the network answers from its record (D-015): the
//! worst a repetition can do is ask again.

use sandbox_db::checkout::{Attempt, Session};
use sandbox_db::payments::{Operation, Payment};
use sandbox_domain::checkout::{self, AttemptFacts, CaptureMethod, Derived};
use sandbox_domain::identity::IdempotencyKey;
use sandbox_domain::money::format_minor;
use sandbox_domain::scenario::Scenario;
use sandbox_domain::{Amount, OperationKind, OperationState};
use serde_json::{Value, json};

use crate::error::ApiError;
use crate::lifecycle::{self, AuthorizeInput};
use crate::state::AppState;

fn key(text: &str) -> Result<IdempotencyKey, ApiError> {
    IdempotencyKey::parse(text)
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("derived checkout key refused: {e}")))
}

/// The capture key prefix of attempt `number` (every try's key starts with it).
fn capture_prefix(session: &Session, number: i32) -> String {
    let mut prefix = checkout::capture_key(session.id, number, 0);
    prefix.truncate(prefix.len().saturating_sub(1));
    prefix
}

/// What is recorded about an attempt: its authorize operation, its checkout captures
/// (oldest first) and its payment.
struct Recorded {
    authorize: Option<Operation>,
    captures: Vec<Operation>,
    payment: Option<Payment>,
}

async fn recorded(
    state: &AppState,
    session: &Session,
    attempt: &Attempt,
) -> Result<Recorded, ApiError> {
    let scope = session.scope();
    let authorize =
        sandbox_db::payments::get_operation(&state.db, scope, attempt.authorize_operation_id)
            .await?;
    let Some(authorize) = authorize else {
        return Ok(Recorded {
            authorize: None,
            captures: Vec::new(),
            payment: None,
        });
    };
    let payment = sandbox_db::payments::get_payment(&state.db, scope, attempt.payment_id).await?;
    let prefix = capture_prefix(session, attempt.number);
    let captures =
        sandbox_db::payments::operations_for_payment(&state.db, scope, attempt.payment_id)
            .await?
            .into_iter()
            .filter(|o| o.kind == OperationKind::Capture && o.idempotency_key.starts_with(&prefix))
            .collect();
    Ok(Recorded {
        authorize: Some(authorize),
        captures,
        payment,
    })
}

fn facts(recorded: &Recorded) -> AttemptFacts {
    AttemptFacts {
        authorize: recorded.authorize.as_ref().map(|o| o.state),
        capture: recorded.captures.last().map(|o| o.state),
        capture_tries: i32::try_from(recorded.captures.len()).unwrap_or(i32::MAX),
        captured_minor: recorded.payment.as_ref().map_or(0, |p| p.sums.captured),
    }
}

/// Drives `attempt`: authorizes (or re-drives the authorization under its identity), and,
/// with automatic capture, captures the whole amount once the authorization succeeded. Only
/// failures the payer should retry (the server or the database was busy or down) are
/// errors; a refusal of the capture (the merchant moved money meanwhile) leaves the view to
/// tell what is true.
pub async fn drive(state: &AppState, session: &Session, attempt: &Attempt) -> Result<(), ApiError> {
    let scope = session.scope();
    let amount = Amount::new(session.amount_minor)
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("stored checkout amount: {e}")))?;
    let input = AuthorizeInput {
        amount,
        currency: session.currency,
        token: attempt.token.clone(),
        description: session.description.clone(),
    };
    let authorize_key = key(&checkout::attempt_key(session.id, attempt.number))?;
    let authorized = match lifecycle::authorize(state, scope, &authorize_key, &input).await {
        Ok(outcome) => outcome,
        // The network holds another request under this identity (after a restore): it was
        // recorded as declined, and the view says so.
        Err(ApiError::IdempotencyKeyConflict) => return Ok(()),
        Err(error) => return Err(error),
    };
    if authorized.operation.state != OperationState::Succeeded
        || session.capture_method == CaptureMethod::Manual
        || authorized.payment.sums.captured >= session.amount_minor
    {
        return Ok(());
    }
    let recorded = recorded(state, session, attempt).await?;
    let tries = i32::try_from(recorded.captures.len()).unwrap_or(i32::MAX);
    let try_number = match recorded.captures.last().map(|o| o.state) {
        None => 1,
        // Still open: re-drive the same try.
        Some(OperationState::Pending | OperationState::Uncertain) => tries,
        Some(OperationState::NotExecuted) if tries < checkout::MAX_CAPTURE_TRIES => {
            tries.saturating_add(1)
        }
        Some(_) => return Ok(()),
    };
    let capture_key = key(&checkout::capture_key(
        session.id,
        attempt.number,
        try_number,
    ))?;
    match lifecycle::child(
        state,
        scope,
        &capture_key,
        OperationKind::Capture,
        attempt.payment_id,
        Some(amount),
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(
            error @ (ApiError::InsufficientCapacity(_)
            | ApiError::InvalidState(_)
            | ApiError::IdempotencyKeyConflict),
        ) => {
            tracing::info!(session = %session.id, code = ?error.code(), "checkout capture refused; the hold stands");
            Ok(())
        }
        Err(error) => Err(error),
    }
}

fn scenario_kind(scenario: Scenario) -> &'static str {
    match scenario {
        Scenario::Approve => "approve",
        Scenario::LostReply | Scenario::Unavailable => "uncertain",
        Scenario::DeclineInsufficientFunds
        | Scenario::DeclineDoNotHonor
        | Scenario::DeclineExpired => "decline",
    }
}

/// The scenarios a payer may pick, in checkout order.
pub fn scenarios() -> Value {
    Value::Array(
        Scenario::ALL
            .iter()
            .map(|s| json!({"token": s.token(), "label": s.label(), "kind": scenario_kind(*s)}))
            .collect(),
    )
}

/// What the payer's page shows. No secret, key or internal id beyond the session's own.
pub async fn payer_view(state: &AppState, session: &Session) -> Result<Value, ApiError> {
    let latest = sandbox_db::checkout::attempts(&state.db, session)
        .await?
        .pop();
    let recorded = match &latest {
        Some(attempt) => Some(recorded(state, session, attempt).await?),
        None => None,
    };
    let derived = checkout::derive(
        session.capture_method,
        session.amount_minor,
        recorded.as_ref().map(facts).as_ref(),
        session.can_start_new(),
    );
    let attempt = latest
        .as_ref()
        .zip(recorded.as_ref())
        .map(|(attempt, rec)| {
            json!({
                "number": attempt.number,
                "scenario": attempt.token,
                "authorize": rec.authorize.as_ref().map(|o| o.state),
                "decline_code": rec.authorize.as_ref().and_then(|o| o.decline_code.clone()),
                "capture": rec.captures.last().map(|o| o.state),
            })
        });
    let payment = recorded.as_ref().and_then(|r| r.payment.as_ref());
    Ok(json!({
        "session": session_json(session, derived),
        "attempt": attempt,
        "captured": payment.map_or(0, |p| p.sums.captured),
        "refunded": payment.map_or(0, |p| p.sums.refunded),
        "scenarios": scenarios(),
        "simulated": true,
    }))
}

/// What the merchant's API shows: the payer's view plus every attempt and its payment.
pub async fn merchant_view(state: &AppState, session: &Session) -> Result<Value, ApiError> {
    let attempts = sandbox_db::checkout::attempts(&state.db, session).await?;
    let mut view = payer_view(state, session).await?;
    view["attempts"] = json!(
        attempts
            .iter()
            .map(|a| json!({
                "number": a.number,
                "scenario": a.token,
                "payment_id": a.payment_id,
                "authorize_operation_id": a.authorize_operation_id,
                "created_at": a.created_at.unix_timestamp(),
            }))
            .collect::<Vec<_>>()
    );
    Ok(view)
}

fn session_json(session: &Session, derived: Derived) -> Value {
    json!({
        "id": session.id,
        "merchant": session.merchant_name,
        "amount": session.amount_minor,
        "currency": session.currency,
        "exponent": session.currency.exponent(),
        "amount_display": format_minor(session.amount_minor, session.currency),
        "description": session.description,
        "capture_method": session.capture_method,
        "embed_origins": session.embed_origins,
        "return_url": session.return_url,
        "expires_at": session.expires_at.unix_timestamp(),
        "expired": session.expired,
        "attempts_left": checkout::MAX_ATTEMPTS.saturating_sub(session.attempts).max(0),
        "state": derived.state,
        "next_action": derived.next_action,
        "simulated": true,
    })
}
