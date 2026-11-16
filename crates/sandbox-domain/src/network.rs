//! The wire protocol between the server and the simulated network (`sim/v1`). Pure types,
//! so the server never links the simulator's store.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::currency::Currency;
use crate::operation::OperationKind;

/// Protocol version, sent as the `Sandbox-Network-Protocol` header.
pub const PROTOCOL_VERSION: &str = "sim/v1";

/// Execute an operation under its identity. Idempotent: the same id returns the recorded
/// result; the same id with different fields is a conflict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecuteRequest {
    /// The operation identity (derived from the client's key, D-015).
    pub id: Uuid,
    /// The project it belongs to.
    pub project_id: Uuid,
    /// What to do.
    pub kind: OperationKind,
    /// The authorization it acts on (capture, void, refund).
    pub parent_id: Option<Uuid>,
    /// Minor units.
    pub amount_minor: i64,
    /// Currency.
    pub currency: Currency,
    /// Scenario token (authorize only).
    pub token: Option<String>,
}

impl ExecuteRequest {
    /// SHA-256 over every field, length-prefixed.
    pub fn fingerprint(&self) -> String {
        let mut hasher = Sha256::new();
        let mut field = |text: &str| {
            hasher.update((text.len() as u64).to_be_bytes());
            hasher.update(text.as_bytes());
        };
        field(PROTOCOL_VERSION);
        field(&self.id.to_string());
        field(&self.project_id.to_string());
        field(self.kind.code());
        field(&self.parent_id.map(|p| p.to_string()).unwrap_or_default());
        field(&self.amount_minor.to_string());
        field(self.currency.code());
        field(self.token.as_deref().unwrap_or(""));
        hex::encode(hasher.finalize())
    }
}

/// The network's answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// Approved and recorded.
    Approved,
    /// Declined and recorded.
    Declined,
}

/// A recorded result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecuteReply {
    /// The operation identity.
    pub id: Uuid,
    /// Approved or declined.
    pub outcome: Outcome,
    /// Why it was declined.
    pub decline_code: Option<String>,
    /// The network's reference (deterministic per id, so scenario runs reproduce).
    pub network_ref: String,
    /// The network's virtual clock when it executed.
    pub virtual_time: i64,
    /// True when this answer replays an earlier execution.
    pub replayed: bool,
    /// Always true.
    pub simulated: bool,
}

/// Readback status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadbackStatus {
    /// Executed; `result` is the recorded answer.
    Executed,
    /// Never executed, and fenced: a later arrival under this id is refused (D-017).
    NotExecuted,
    /// Not executed yet and not fenced (a plain, non-fencing readback).
    Unknown,
}

/// A readback answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Readback {
    /// The identity asked about.
    pub id: Uuid,
    /// What the network knows.
    pub status: ReadbackStatus,
    /// The recorded answer, when executed.
    pub result: Option<ExecuteReply>,
}

/// One recorded operation, as listed for reverse reconciliation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListedOperation {
    /// Allocation order in the network's store.
    pub seq: i64,
    /// Identity.
    pub id: Uuid,
    /// Project.
    pub project_id: Uuid,
    /// Kind.
    pub kind: OperationKind,
    /// Parent authorization.
    pub parent_id: Option<Uuid>,
    /// Minor units.
    pub amount_minor: i64,
    /// Currency.
    pub currency: Currency,
    /// Result.
    pub outcome: Outcome,
    /// Virtual time of execution.
    pub virtual_time: i64,
}

/// A page of listed operations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationPage {
    /// Operations with `seq` greater than the request's `after`.
    pub operations: Vec<ListedOperation>,
    /// Pass as `after` for the next page; `None` at the end.
    pub next_after: Option<i64>,
}

/// The network reference for an id: deterministic, so the same scenario run twice yields
/// the same references.
pub fn network_ref(id: Uuid) -> String {
    let digest = Sha256::digest(format!("network-ref/v1/{id}").as_bytes());
    format!("simnet_{}", &hex::encode(digest)[..20])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> ExecuteRequest {
        ExecuteRequest {
            id: Uuid::from_u128(1),
            project_id: Uuid::from_u128(2),
            kind: OperationKind::Authorize,
            parent_id: None,
            amount_minor: 500,
            currency: Currency::Usd,
            token: Some("tok_sim_approve".to_owned()),
        }
    }

    #[test]
    fn fingerprints_bind_every_field() {
        let base = request();
        let mut other = request();
        other.amount_minor = 501;
        assert_ne!(base.fingerprint(), other.fingerprint());
        let mut other = request();
        other.token = None;
        assert_ne!(base.fingerprint(), other.fingerprint());
        assert_eq!(base.fingerprint(), request().fingerprint());
    }

    #[test]
    fn network_refs_are_deterministic_and_distinct() {
        assert_eq!(
            network_ref(Uuid::from_u128(1)),
            network_ref(Uuid::from_u128(1))
        );
        assert_ne!(
            network_ref(Uuid::from_u128(1)),
            network_ref(Uuid::from_u128(2))
        );
        assert!(network_ref(Uuid::from_u128(1)).starts_with("simnet_"));
    }

    #[test]
    fn wire_format_is_stable_json() {
        let json = serde_json::to_value(request()).unwrap();
        assert_eq!(json["kind"], "authorize");
        assert_eq!(json["currency"], "USD");
        let back: ExecuteRequest = serde_json::from_value(json).unwrap();
        assert_eq!(back, request());
    }
}
