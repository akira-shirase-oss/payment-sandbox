//! Idempotency keys, derived identity (D-015) and request fingerprints (D-016).
//!
//! Operation ids are UUIDv5 over (project, dataset epoch, kind, client key), so the same
//! request always maps to the same identity, even after the server's database was
//! restored to a backup that predates it. The simulator recognises the identity and
//! answers from its store instead of charging twice.

use std::fmt;

use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::card_guard;
use crate::currency::Currency;
use crate::money::Amount;
use crate::operation::OperationKind;
use crate::recipe::Recipe;

/// Namespace for every identity this crate derives. Never change it: doing so would give
/// every retried request a new identity.
pub const NAMESPACE: Uuid = Uuid::from_u128(0x6c2e_8f0a_9d3b_4c71_a5e2_1b7f_3d94_0e58);

/// Longest accepted idempotency key, in bytes.
pub const MAX_KEY_LEN: usize = 255;

/// A validated `Idempotency-Key`: 1–255 visible ASCII characters, never card-like.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct IdempotencyKey(String);

/// Why a key was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KeyError {
    /// Empty or longer than 255 bytes.
    #[error("Idempotency-Key must be 1 to 255 characters")]
    Length,
    /// Contains a space, a control character or non-ASCII.
    #[error("Idempotency-Key may contain only visible ASCII characters")]
    Charset,
    /// Looks like a card number (D-019). The value is not echoed.
    #[error("raw card data is never accepted")]
    CardLike,
}

impl IdempotencyKey {
    /// Validates a client-supplied key.
    pub fn parse(raw: &str) -> Result<Self, KeyError> {
        if raw.is_empty() || raw.len() > MAX_KEY_LEN {
            return Err(KeyError::Length);
        }
        if !raw.bytes().all(|b| (0x21..=0x7e).contains(&b)) {
            return Err(KeyError::Charset);
        }
        if card_guard::contains_pan_like(raw) {
            return Err(KeyError::CardLike);
        }
        Ok(Self(raw.to_owned()))
    }

    /// The key text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for IdempotencyKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The identity of an operation, derived from the client's key.
pub fn operation_id(project: Uuid, epoch: i32, kind: OperationKind, key: &IdempotencyKey) -> Uuid {
    let name = format!("operation/v1/{project}/{epoch}/{kind}/{key}");
    Uuid::new_v5(&NAMESPACE, name.as_bytes())
}

/// The payment an authorize operation creates.
pub fn payment_id(authorize_operation: Uuid) -> Uuid {
    Uuid::new_v5(
        &NAMESPACE,
        format!("payment/v1/{authorize_operation}").as_bytes(),
    )
}

/// The journal entry a recipe posts for an operation; unique per (operation, recipe), so
/// an operation's success can be posted at most once.
pub fn entry_id(operation: Uuid, recipe: Recipe) -> Uuid {
    Uuid::new_v5(
        &NAMESPACE,
        format!("entry/v1/{operation}/{recipe}").as_bytes(),
    )
}

/// The canonical fields an idempotency key binds to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OperationRequest<'a> {
    /// The operation kind.
    pub kind: OperationKind,
    /// The parent payment (capture, void, refund).
    pub payment: Option<Uuid>,
    /// The amount, when the request names one.
    pub amount: Option<Amount>,
    /// The currency (authorize).
    pub currency: Option<Currency>,
    /// The synthetic token (authorize).
    pub token: Option<&'a str>,
    /// Free-text description (authorize).
    pub description: Option<&'a str>,
}

impl OperationRequest<'_> {
    /// SHA-256 over a length-prefixed encoding of every field, so no two different requests
    /// share a fingerprint by concatenation accident.
    pub fn fingerprint(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();
        let mut field = |value: Option<&str>| match value {
            Some(text) => {
                hasher.update(b"s");
                hasher.update((text.len() as u64).to_be_bytes());
                hasher.update(text.as_bytes());
            }
            None => hasher.update(b"n"),
        };
        let payment = self.payment.map(|id| id.to_string());
        let amount = self.amount.map(|a| a.minor().to_string());
        field(Some("fingerprint/v1"));
        field(Some(self.kind.code()));
        field(payment.as_deref());
        field(amount.as_deref());
        field(self.currency.map(Currency::code));
        field(self.token);
        field(self.description);
        hasher.finalize().into()
    }

    /// The fingerprint as lowercase hex.
    pub fn fingerprint_hex(&self) -> String {
        hex::encode(self.fingerprint())
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn key(text: &str) -> IdempotencyKey {
        IdempotencyKey::parse(text).unwrap()
    }

    #[test]
    fn keys_are_bounded_visible_ascii() {
        assert_eq!(IdempotencyKey::parse(""), Err(KeyError::Length));
        assert_eq!(
            IdempotencyKey::parse(&"k".repeat(256)),
            Err(KeyError::Length)
        );
        assert!(IdempotencyKey::parse(&"k".repeat(255)).is_ok());
        assert_eq!(IdempotencyKey::parse("has space"), Err(KeyError::Charset));
        assert_eq!(IdempotencyKey::parse("tab\there"), Err(KeyError::Charset));
        assert_eq!(IdempotencyKey::parse("caf\u{e9}"), Err(KeyError::Charset));
        assert_eq!(
            IdempotencyKey::parse("order-4242424242424242"),
            Err(KeyError::CardLike)
        );
        assert!(IdempotencyKey::parse("018f2f0e-3c1a-7cc0-9e1e-3f1f6b7f9a01").is_ok());
    }

    #[test]
    fn identity_is_a_pure_function_of_project_epoch_kind_and_key() {
        let project = Uuid::from_u128(1);
        let a = operation_id(project, 1, OperationKind::Authorize, &key("order-1"));
        assert_eq!(
            a,
            operation_id(project, 1, OperationKind::Authorize, &key("order-1"))
        );
        assert_ne!(
            a,
            operation_id(project, 1, OperationKind::Authorize, &key("order-2"))
        );
        assert_ne!(
            a,
            operation_id(project, 1, OperationKind::Refund, &key("order-1"))
        );
        assert_ne!(
            a,
            operation_id(project, 2, OperationKind::Authorize, &key("order-1"))
        );
        assert_ne!(
            a,
            operation_id(
                Uuid::from_u128(2),
                1,
                OperationKind::Authorize,
                &key("order-1")
            )
        );
        assert_eq!(a.get_version_num(), 5);
        assert_eq!(payment_id(a), payment_id(a));
        assert_ne!(entry_id(a, Recipe::Authorize), entry_id(a, Recipe::Capture));
    }

    #[test]
    fn the_fingerprint_binds_every_field() {
        let base = OperationRequest {
            kind: OperationKind::Authorize,
            payment: None,
            amount: Amount::new(500).ok(),
            currency: Some(Currency::Usd),
            token: Some("tok_sim_approve"),
            description: Some("order 1"),
        };
        let variants = [
            OperationRequest {
                amount: Amount::new(501).ok(),
                ..base
            },
            OperationRequest {
                currency: Some(Currency::Eur),
                ..base
            },
            OperationRequest {
                token: Some("tok_sim_decline"),
                ..base
            },
            OperationRequest {
                description: None,
                ..base
            },
            OperationRequest {
                description: Some("order 2"),
                ..base
            },
            OperationRequest {
                kind: OperationKind::Capture,
                ..base
            },
            OperationRequest {
                payment: Some(Uuid::from_u128(9)),
                ..base
            },
        ];
        for variant in variants {
            assert_ne!(base.fingerprint(), variant.fingerprint(), "{variant:?}");
        }
        assert_eq!(base.fingerprint_hex(), base.fingerprint_hex());
        // Moving text between fields must not collide.
        let a = OperationRequest {
            token: Some("ab"),
            description: Some("c"),
            ..base
        };
        let b = OperationRequest {
            token: Some("a"),
            description: Some("bc"),
            ..base
        };
        assert_ne!(a.fingerprint(), b.fingerprint());
    }

    proptest! {
        #[test]
        fn distinct_keys_give_distinct_operations(a in "[a-z0-9-]{1,40}", b in "[a-z0-9-]{1,40}") {
            prop_assume!(a != b);
            let project = Uuid::from_u128(7);
            prop_assert_ne!(
                operation_id(project, 1, OperationKind::Capture, &key(&a)),
                operation_id(project, 1, OperationKind::Capture, &key(&b))
            );
        }
    }
}
