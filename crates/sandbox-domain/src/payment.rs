//! Capacity (D-014): the committed sums of a payment and its in-flight reservations.
//!
//! Every capture, void and refund first *reserves* its amount (in the same transaction that
//! records the operation), then calls the simulated network outside any transaction, then
//! *completes* the reservation: a success moves it into the committed sum, a decline or a
//! fenced "never executed" releases it. The database's `CHECK` constraints repeat
//! [`PaymentSums::check`], so neither a bug here nor a concurrent request can overrun.

use serde::Serialize;

use crate::money::Amount;
use crate::operation::OperationState;

/// Committed sums and reservations of one payment, in minor units of its currency.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct PaymentSums {
    /// Amount of the successful authorization (0 until it succeeds).
    pub authorized: i64,
    /// Captured so far.
    pub captured: i64,
    /// Captures in flight.
    pub capture_reserved: i64,
    /// Released by a successful void.
    pub voided: i64,
    /// A void in flight.
    pub void_reserved: i64,
    /// Refunded so far.
    pub refunded: i64,
    /// Refunds in flight.
    pub refund_reserved: i64,
}

/// Which capacity an operation consumes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reservation {
    /// Draws on the authorized, uncaptured remainder.
    Capture,
    /// Releases the whole uncaptured remainder.
    Void,
    /// Draws on the captured, unrefunded amount.
    Refund,
}

/// A named refusal from the capacity rules.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CapacityError {
    /// The request exceeds what is left; concurrent requests that would overrun fail here.
    #[error("requested {requested} exceeds the remaining capacity of {available}")]
    InsufficientCapacity {
        /// Requested minor units.
        requested: i64,
        /// Minor units still available.
        available: i64,
    },
    /// The payment was never authorized, or is still authorizing.
    #[error("the payment is not authorized")]
    NotAuthorized,
    /// A void needs every in-flight capture resolved first, so the remainder it releases is
    /// exact.
    #[error("a capture is still in flight; void after it resolves")]
    CaptureInFlight,
    /// Nothing is left to release.
    #[error("nothing is left to void")]
    NothingToVoid,
    /// Completing more than was reserved: a caller bug, never a client error.
    #[error("completion exceeds the reservation")]
    NotReserved,
    /// A sum left `i64`.
    #[error("payment sums overflow")]
    Overflow,
}

/// A violated invariant (a defect, reported by reconciliation).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("payment sums violate an invariant: {0}")]
pub struct InvariantViolation(pub &'static str);

impl PaymentSums {
    /// Sums right after a successful authorization of `amount`.
    pub const fn authorized(amount: Amount) -> Self {
        Self {
            authorized: amount.minor(),
            captured: 0,
            capture_reserved: 0,
            voided: 0,
            void_reserved: 0,
            refunded: 0,
            refund_reserved: 0,
        }
    }

    /// What a capture or void may still draw on.
    pub fn capturable(&self) -> i64 {
        self.authorized
            .saturating_sub(self.captured)
            .saturating_sub(self.capture_reserved)
            .saturating_sub(self.voided)
            .saturating_sub(self.void_reserved)
    }

    /// What a refund may still draw on.
    pub fn refundable(&self) -> i64 {
        self.captured
            .saturating_sub(self.refunded)
            .saturating_sub(self.refund_reserved)
    }

    /// The amount a void would release now.
    pub fn void_amount(&self) -> Result<Amount, CapacityError> {
        if self.capture_reserved > 0 {
            return Err(CapacityError::CaptureInFlight);
        }
        Amount::new(self.capturable()).map_err(|_| CapacityError::NothingToVoid)
    }

    /// Reserves `amount` for an operation, or refuses with a named error.
    pub fn reserve(&self, reservation: Reservation, amount: Amount) -> Result<Self, CapacityError> {
        if self.authorized == 0 {
            return Err(CapacityError::NotAuthorized);
        }
        let requested = amount.minor();
        let mut next = *self;
        match reservation {
            Reservation::Capture => {
                let available = self.capturable();
                if requested > available {
                    return Err(CapacityError::InsufficientCapacity {
                        requested,
                        available,
                    });
                }
                next.capture_reserved = add(self.capture_reserved, requested)?;
            }
            Reservation::Void => {
                let exact = self.void_amount()?;
                if exact != amount {
                    return Err(CapacityError::InsufficientCapacity {
                        requested,
                        available: exact.minor(),
                    });
                }
                next.void_reserved = add(self.void_reserved, requested)?;
            }
            Reservation::Refund => {
                let available = self.refundable();
                if requested > available {
                    return Err(CapacityError::InsufficientCapacity {
                        requested,
                        available,
                    });
                }
                next.refund_reserved = add(self.refund_reserved, requested)?;
            }
        }
        Ok(next)
    }

    /// Completes a reservation: `succeeded` moves it into the committed sum, otherwise it is
    /// released.
    pub fn complete(
        &self,
        reservation: Reservation,
        amount: Amount,
        succeeded: bool,
    ) -> Result<Self, CapacityError> {
        let minor = amount.minor();
        let mut next = *self;
        let (reserved, committed) = match reservation {
            Reservation::Capture => (&mut next.capture_reserved, &mut next.captured),
            Reservation::Void => (&mut next.void_reserved, &mut next.voided),
            Reservation::Refund => (&mut next.refund_reserved, &mut next.refunded),
        };
        if *reserved < minor {
            return Err(CapacityError::NotReserved);
        }
        *reserved = reserved.checked_sub(minor).ok_or(CapacityError::Overflow)?;
        if succeeded {
            *committed = add(*committed, minor)?;
        }
        Ok(next)
    }

    /// The invariants the database also enforces with `CHECK` constraints.
    pub fn check(&self) -> Result<(), InvariantViolation> {
        let fields = [
            self.authorized,
            self.captured,
            self.capture_reserved,
            self.voided,
            self.void_reserved,
            self.refunded,
            self.refund_reserved,
        ];
        if fields.iter().any(|v| *v < 0) {
            return Err(InvariantViolation("a sum is negative"));
        }
        let drawn = [
            self.captured,
            self.capture_reserved,
            self.voided,
            self.void_reserved,
        ]
        .iter()
        .try_fold(0_i64, |acc, v| acc.checked_add(*v))
        .ok_or(InvariantViolation("sums overflow"))?;
        if drawn > self.authorized {
            return Err(InvariantViolation(
                "captured + voided + reservations exceed authorized",
            ));
        }
        let refund_drawn = self
            .refunded
            .checked_add(self.refund_reserved)
            .ok_or(InvariantViolation("sums overflow"))?;
        if refund_drawn > self.captured {
            return Err(InvariantViolation(
                "refunded + reservations exceed captured",
            ));
        }
        Ok(())
    }
}

fn add(a: i64, b: i64) -> Result<i64, CapacityError> {
    a.checked_add(b).ok_or(CapacityError::Overflow)
}

/// The payment's status as shown to clients, derived from the authorization's state and the
/// sums, never stored independently of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PaymentStatus {
    /// The authorization is pending or uncertain.
    Authorizing,
    /// The network declined the authorization.
    Declined,
    /// The authorization never executed (fenced readback).
    Failed,
    /// Authorized; nothing captured yet.
    Authorized,
    /// Some captured; more may be.
    PartiallyCaptured,
    /// Nothing more can be captured, and something was.
    Captured,
    /// Released without any capture.
    Voided,
    /// Some captured money refunded.
    PartiallyRefunded,
    /// Every captured unit refunded and nothing left to capture.
    Refunded,
}

impl PaymentStatus {
    /// Stable code.
    pub const fn code(self) -> &'static str {
        match self {
            Self::Authorizing => "authorizing",
            Self::Declined => "declined",
            Self::Failed => "failed",
            Self::Authorized => "authorized",
            Self::PartiallyCaptured => "partially_captured",
            Self::Captured => "captured",
            Self::Voided => "voided",
            Self::PartiallyRefunded => "partially_refunded",
            Self::Refunded => "refunded",
        }
    }

    /// Derives the status.
    pub fn derive(authorization: OperationState, sums: &PaymentSums) -> Self {
        match authorization {
            OperationState::Pending | OperationState::Uncertain => return Self::Authorizing,
            OperationState::Declined => return Self::Declined,
            OperationState::NotExecuted => return Self::Failed,
            OperationState::Succeeded => {}
        }
        let open = sums.capturable() > 0 || sums.capture_reserved > 0 || sums.void_reserved > 0;
        if sums.refunded > 0 {
            if sums.refunded == sums.captured && !open {
                Self::Refunded
            } else {
                Self::PartiallyRefunded
            }
        } else if sums.captured == 0 {
            if sums.voided > 0 && !open {
                Self::Voided
            } else {
                Self::Authorized
            }
        } else if open {
            Self::PartiallyCaptured
        } else {
            Self::Captured
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use proptest::prelude::*;

    use super::*;
    use crate::account::{AccountKind, CHART};
    use crate::currency::Currency;
    use crate::journal::Line;
    use crate::recipe::Recipe;

    fn amt(minor: i64) -> Amount {
        Amount::new(minor).unwrap()
    }

    #[test]
    fn sixty_four_refunds_of_ten_against_three_hundred_admit_exactly_thirty() {
        let mut sums = PaymentSums::authorized(amt(300));
        sums = sums.reserve(Reservation::Capture, amt(300)).unwrap();
        sums = sums.complete(Reservation::Capture, amt(300), true).unwrap();
        let mut admitted = 0;
        let mut refused = 0;
        for _ in 0..64 {
            match sums.reserve(Reservation::Refund, amt(10)) {
                Ok(next) => {
                    sums = next;
                    admitted += 1;
                }
                Err(CapacityError::InsufficientCapacity { .. }) => refused += 1,
                Err(other) => panic!("unexpected {other:?}"),
            }
        }
        assert_eq!((admitted, refused), (30, 34));
        sums.check().unwrap();
    }

    #[test]
    fn a_void_waits_for_in_flight_captures_and_then_closes_the_hold() {
        let sums = PaymentSums::authorized(amt(100))
            .reserve(Reservation::Capture, amt(40))
            .unwrap();
        assert_eq!(sums.void_amount(), Err(CapacityError::CaptureInFlight));
        let sums = sums.complete(Reservation::Capture, amt(40), true).unwrap();
        let remainder = sums.void_amount().unwrap();
        assert_eq!(remainder.minor(), 60);
        let sums = sums.reserve(Reservation::Void, remainder).unwrap();
        assert_eq!(
            sums.reserve(Reservation::Capture, amt(1)),
            Err(CapacityError::InsufficientCapacity {
                requested: 1,
                available: 0
            })
        );
        let sums = sums.complete(Reservation::Void, remainder, true).unwrap();
        assert_eq!(
            PaymentStatus::derive(OperationState::Succeeded, &sums),
            PaymentStatus::Captured
        );
        assert_eq!(sums.void_amount(), Err(CapacityError::NothingToVoid));
    }

    #[test]
    fn a_refund_cannot_draw_on_a_capture_still_in_flight() {
        let sums = PaymentSums::authorized(amt(100))
            .reserve(Reservation::Capture, amt(100))
            .unwrap();
        assert_eq!(
            sums.reserve(Reservation::Refund, amt(1)),
            Err(CapacityError::InsufficientCapacity {
                requested: 1,
                available: 0
            })
        );
    }

    #[test]
    fn nothing_can_be_reserved_before_authorization() {
        assert_eq!(
            PaymentSums::default().reserve(Reservation::Capture, amt(1)),
            Err(CapacityError::NotAuthorized)
        );
    }

    #[test]
    fn statuses_follow_the_sums() {
        let s = |authorized, captured, voided, refunded| PaymentSums {
            authorized,
            captured,
            voided,
            refunded,
            ..PaymentSums::default()
        };
        let ok = OperationState::Succeeded;
        assert_eq!(
            PaymentStatus::derive(OperationState::Uncertain, &s(0, 0, 0, 0)),
            PaymentStatus::Authorizing
        );
        assert_eq!(
            PaymentStatus::derive(OperationState::Declined, &s(0, 0, 0, 0)),
            PaymentStatus::Declined
        );
        assert_eq!(
            PaymentStatus::derive(OperationState::NotExecuted, &s(0, 0, 0, 0)),
            PaymentStatus::Failed
        );
        assert_eq!(
            PaymentStatus::derive(ok, &s(100, 0, 0, 0)),
            PaymentStatus::Authorized
        );
        assert_eq!(
            PaymentStatus::derive(ok, &s(100, 40, 0, 0)),
            PaymentStatus::PartiallyCaptured
        );
        assert_eq!(
            PaymentStatus::derive(ok, &s(100, 100, 0, 0)),
            PaymentStatus::Captured
        );
        assert_eq!(
            PaymentStatus::derive(ok, &s(100, 40, 60, 0)),
            PaymentStatus::Captured
        );
        assert_eq!(
            PaymentStatus::derive(ok, &s(100, 0, 100, 0)),
            PaymentStatus::Voided
        );
        assert_eq!(
            PaymentStatus::derive(ok, &s(100, 100, 0, 30)),
            PaymentStatus::PartiallyRefunded
        );
        assert_eq!(
            PaymentStatus::derive(ok, &s(100, 100, 0, 100)),
            PaymentStatus::Refunded
        );
    }

    /// One step of a random payment history.
    #[derive(Debug, Clone)]
    enum Step {
        Capture(i64, Outcome),
        Void(Outcome),
        Refund(i64, Outcome),
    }

    #[derive(Debug, Clone, Copy)]
    enum Outcome {
        Approved,
        Released,
        /// Left in flight until the end of the history, then released.
        Hanging,
    }

    fn arb_outcome() -> impl Strategy<Value = Outcome> {
        prop_oneof![
            6 => Just(Outcome::Approved),
            2 => Just(Outcome::Released),
            1 => Just(Outcome::Hanging),
        ]
    }

    fn arb_step() -> impl Strategy<Value = Step> {
        prop_oneof![
            4 => (1i64..400, arb_outcome()).prop_map(|(a, o)| Step::Capture(a, o)),
            1 => arb_outcome().prop_map(Step::Void),
            4 => (1i64..400, arb_outcome()).prop_map(|(a, o)| Step::Refund(a, o)),
        ]
    }

    fn post(
        ledger: &mut BTreeMap<AccountKind, i64>,
        recipe: Recipe,
        currency: Currency,
        minor: i64,
    ) {
        let entry = recipe.post(currency, amt(minor)).unwrap();
        assert_eq!(entry.lines().iter().map(Line::signed_minor).sum::<i64>(), 0);
        for line in entry.lines() {
            *ledger.entry(line.account).or_default() += line.signed_minor();
        }
    }

    proptest! {
        /// The core accounting property: whatever sequence of captures, voids and refunds is
        /// attempted, and whatever the network answers, (1) the sums never break their
        /// invariants, (2) the ledger sums to zero, (3) every account balance equals the sum
        /// its journal lines imply from the payment's committed sums, and (4) no presented
        /// balance goes negative.
        #[test]
        fn journal_and_capacity_stay_consistent(
            authorized in 1i64..2_000,
            currency in (0usize..4).prop_map(|i| Currency::ALL[i]),
            steps in prop::collection::vec(arb_step(), 0..40),
        ) {
            let mut ledger: BTreeMap<AccountKind, i64> = BTreeMap::new();
            let mut sums = PaymentSums::authorized(amt(authorized));
            post(&mut ledger, Recipe::Authorize, currency, authorized);
            let mut hanging: Vec<(Reservation, Amount)> = Vec::new();

            for step in steps {
                let (reservation, amount, outcome) = match step {
                    Step::Capture(a, o) => (Reservation::Capture, amt(a), o),
                    Step::Refund(a, o) => (Reservation::Refund, amt(a), o),
                    Step::Void(o) => match sums.void_amount() {
                        Ok(remainder) => (Reservation::Void, remainder, o),
                        Err(_) => continue,
                    },
                };
                let Ok(next) = sums.reserve(reservation, amount) else { continue };
                sums = next;
                prop_assert!(sums.check().is_ok());
                match outcome {
                    Outcome::Hanging => hanging.push((reservation, amount)),
                    Outcome::Released => sums = sums.complete(reservation, amount, false).unwrap(),
                    Outcome::Approved => {
                        sums = sums.complete(reservation, amount, true).unwrap();
                        let recipe = match reservation {
                            Reservation::Capture => Recipe::Capture,
                            Reservation::Void => Recipe::Void,
                            Reservation::Refund => Recipe::Refund,
                        };
                        post(&mut ledger, recipe, currency, amount.minor());
                    }
                }
                prop_assert!(sums.check().is_ok());
            }
            for (reservation, amount) in hanging {
                sums = sums.complete(reservation, amount, false).unwrap();
            }
            prop_assert!(sums.check().is_ok());
            prop_assert_eq!(ledger.values().sum::<i64>(), 0);
            let bal = |k: AccountKind| k.presented(ledger.get(&k).copied().unwrap_or(0));
            prop_assert_eq!(bal(AccountKind::PayerHeld), sums.authorized - sums.captured - sums.voided);
            prop_assert_eq!(bal(AccountKind::MerchantPending), sums.captured - sums.refunded);
            prop_assert_eq!(bal(AccountKind::PayerFunding), sums.authorized - sums.voided - sums.refunded);
            for kind in CHART {
                prop_assert!(bal(kind) >= 0, "{} went negative", kind);
            }
        }
    }
}
