//! The operation state machine. `uncertain` is a first-class state: a lost reply is
//! recorded as such and resolved only by readback under the same identity (D-004, D-017).
//!
//! ```text
//! pending ──approved──▶ succeeded
//!    │  └──declined───▶ declined
//!    │  └──not executed (fenced readback)──▶ not_executed
//!    └──unknown──▶ uncertain ──approved / declined / not executed──▶ (same terminals)
//!                     └──unknown──▶ uncertain
//! ```

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::payment::Reservation;
use crate::recipe::Recipe;

/// What an operation does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    /// Place a hold.
    Authorize,
    /// Capture part or all of a hold.
    Capture,
    /// Release the uncaptured remainder.
    Void,
    /// Return captured money.
    Refund,
}

impl OperationKind {
    /// Every kind.
    pub const ALL: [Self; 4] = [Self::Authorize, Self::Capture, Self::Void, Self::Refund];

    /// Stable code.
    pub const fn code(self) -> &'static str {
        match self {
            Self::Authorize => "authorize",
            Self::Capture => "capture",
            Self::Void => "void",
            Self::Refund => "refund",
        }
    }

    /// Parses a code.
    pub fn from_code(code: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.code() == code)
    }

    /// The recipe a success posts.
    pub const fn recipe(self) -> Recipe {
        match self {
            Self::Authorize => Recipe::Authorize,
            Self::Capture => Recipe::Capture,
            Self::Void => Recipe::Void,
            Self::Refund => Recipe::Refund,
        }
    }

    /// The capacity a capture, void or refund reserves; authorize reserves none.
    pub const fn reservation(self) -> Option<Reservation> {
        match self {
            Self::Authorize => None,
            Self::Capture => Some(Reservation::Capture),
            Self::Void => Some(Reservation::Void),
            Self::Refund => Some(Reservation::Refund),
        }
    }
}

impl fmt::Display for OperationKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

/// Where an operation stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationState {
    /// Identity committed; the network has not answered yet.
    Pending,
    /// The network's answer was lost; resolved only by readback.
    Uncertain,
    /// The network approved; postings are committed.
    Succeeded,
    /// The network declined; any reservation was released.
    Declined,
    /// Fenced readback proved the network never executed it; any reservation was released.
    NotExecuted,
}

/// What the network said, or failed to say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkOutcome {
    /// Approved and recorded by the network.
    Approved,
    /// Declined by the network.
    Declined,
    /// No usable answer (timeout, connection lost, 5xx): the operation may or may not have
    /// executed.
    Unknown,
    /// A fenced readback found no record and tombstoned the identity.
    NotExecuted,
}

/// An outcome arrived for an operation that is already resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("operation already resolved as {0:?}")]
pub struct AlreadyResolved(pub OperationState);

impl OperationState {
    /// Stable code.
    pub const fn code(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Uncertain => "uncertain",
            Self::Succeeded => "succeeded",
            Self::Declined => "declined",
            Self::NotExecuted => "not_executed",
        }
    }

    /// Parses a code.
    pub fn from_code(code: &str) -> Option<Self> {
        [
            Self::Pending,
            Self::Uncertain,
            Self::Succeeded,
            Self::Declined,
            Self::NotExecuted,
        ]
        .into_iter()
        .find(|state| state.code() == code)
    }

    /// True once nothing can change the operation.
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Declined | Self::NotExecuted)
    }

    /// Applies a network outcome. Terminal states refuse every outcome, so an answer can
    /// never be applied twice.
    pub const fn apply(self, outcome: NetworkOutcome) -> Result<Self, AlreadyResolved> {
        if self.is_terminal() {
            return Err(AlreadyResolved(self));
        }
        Ok(match outcome {
            NetworkOutcome::Approved => Self::Succeeded,
            NetworkOutcome::Declined => Self::Declined,
            NetworkOutcome::Unknown => Self::Uncertain,
            NetworkOutcome::NotExecuted => Self::NotExecuted,
        })
    }
}

impl fmt::Display for OperationState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OUTCOMES: [NetworkOutcome; 4] = [
        NetworkOutcome::Approved,
        NetworkOutcome::Declined,
        NetworkOutcome::Unknown,
        NetworkOutcome::NotExecuted,
    ];

    #[test]
    fn open_states_accept_every_outcome() {
        for from in [OperationState::Pending, OperationState::Uncertain] {
            assert_eq!(
                from.apply(NetworkOutcome::Approved),
                Ok(OperationState::Succeeded)
            );
            assert_eq!(
                from.apply(NetworkOutcome::Declined),
                Ok(OperationState::Declined)
            );
            assert_eq!(
                from.apply(NetworkOutcome::Unknown),
                Ok(OperationState::Uncertain)
            );
            assert_eq!(
                from.apply(NetworkOutcome::NotExecuted),
                Ok(OperationState::NotExecuted)
            );
        }
    }

    #[test]
    fn terminal_states_refuse_a_second_outcome() {
        for from in [
            OperationState::Succeeded,
            OperationState::Declined,
            OperationState::NotExecuted,
        ] {
            for outcome in OUTCOMES {
                assert_eq!(from.apply(outcome), Err(AlreadyResolved(from)));
            }
        }
    }

    #[test]
    fn codes_round_trip() {
        for kind in OperationKind::ALL {
            assert_eq!(OperationKind::from_code(kind.code()), Some(kind));
        }
        for state in [
            OperationState::Pending,
            OperationState::Uncertain,
            OperationState::Succeeded,
            OperationState::Declined,
            OperationState::NotExecuted,
        ] {
            assert_eq!(OperationState::from_code(state.code()), Some(state));
        }
    }
}
