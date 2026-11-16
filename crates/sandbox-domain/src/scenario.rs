//! Synthetic payment tokens. A payer never types card data (D-019): checkout offers these
//! scenarios, each maps to one `tok_sim_*` token, and the simulator decides the network's
//! answer from the token alone, deterministically.

use serde::Serialize;

/// The simulated network's behaviour for an authorization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Scenario {
    /// Approve every operation.
    Approve,
    /// Decline the authorization: insufficient funds.
    DeclineInsufficientFunds,
    /// Decline the authorization: do not honor.
    DeclineDoNotHonor,
    /// Decline the authorization: expired payment method.
    DeclineExpired,
    /// Approve and record, then lose the reply: the server sees no answer and must keep
    /// the operation `uncertain` until readback.
    LostReply,
    /// Answer `503` without executing: the server cannot tell, so the operation is
    /// `uncertain` until a fenced readback proves it never executed.
    Unavailable,
}

/// A decline reason the network reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeclineCode {
    /// Not enough synthetic funds.
    InsufficientFunds,
    /// Generic decline.
    DoNotHonor,
    /// The synthetic payment method is expired.
    ExpiredPaymentMethod,
}

impl DeclineCode {
    /// Stable code.
    pub const fn code(self) -> &'static str {
        match self {
            Self::InsufficientFunds => "insufficient_funds",
            Self::DoNotHonor => "do_not_honor",
            Self::ExpiredPaymentMethod => "expired_payment_method",
        }
    }

    /// Parses a code.
    pub fn from_code(code: &str) -> Option<Self> {
        [
            Self::InsufficientFunds,
            Self::DoNotHonor,
            Self::ExpiredPaymentMethod,
        ]
        .into_iter()
        .find(|c| c.code() == code)
    }
}

/// Why a token was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TokenError {
    /// Not of the form `tok_sim_<scenario>`.
    #[error("payment tokens look like tok_sim_approve; raw card data is never accepted")]
    Malformed,
    /// Well formed, but no scenario has that name.
    #[error("unknown sandbox scenario token")]
    UnknownScenario,
}

impl Scenario {
    /// Every scenario, in the order checkout lists them.
    pub const ALL: [Self; 6] = [
        Self::Approve,
        Self::DeclineInsufficientFunds,
        Self::DeclineDoNotHonor,
        Self::DeclineExpired,
        Self::LostReply,
        Self::Unavailable,
    ];

    /// The token that selects this scenario.
    pub const fn token(self) -> &'static str {
        match self {
            Self::Approve => "tok_sim_approve",
            Self::DeclineInsufficientFunds => "tok_sim_decline_insufficient_funds",
            Self::DeclineDoNotHonor => "tok_sim_decline_do_not_honor",
            Self::DeclineExpired => "tok_sim_decline_expired",
            Self::LostReply => "tok_sim_lost_reply",
            Self::Unavailable => "tok_sim_unavailable",
        }
    }

    /// A short label for checkout and the dashboard.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Approve => "Approve",
            Self::DeclineInsufficientFunds => "Decline: insufficient funds",
            Self::DeclineDoNotHonor => "Decline: do not honor",
            Self::DeclineExpired => "Decline: expired payment method",
            Self::LostReply => "Uncertain: the network's reply is lost",
            Self::Unavailable => "Uncertain: the network is unavailable",
        }
    }

    /// The decline this scenario produces for an authorization, if any.
    pub const fn decline(self) -> Option<DeclineCode> {
        match self {
            Self::DeclineInsufficientFunds => Some(DeclineCode::InsufficientFunds),
            Self::DeclineDoNotHonor => Some(DeclineCode::DoNotHonor),
            Self::DeclineExpired => Some(DeclineCode::ExpiredPaymentMethod),
            Self::Approve | Self::LostReply | Self::Unavailable => None,
        }
    }

    /// Parses and validates a token.
    pub fn from_token(token: &str) -> Result<Self, TokenError> {
        let Some(name) = token.strip_prefix("tok_sim_") else {
            return Err(TokenError::Malformed);
        };
        let shape_ok = !name.is_empty()
            && token.len() <= 64
            && name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
        if !shape_ok {
            return Err(TokenError::Malformed);
        }
        Self::ALL
            .into_iter()
            .find(|scenario| scenario.token() == token)
            .ok_or(TokenError::UnknownScenario)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_scenario_round_trips_through_its_token() {
        for scenario in Scenario::ALL {
            assert_eq!(Scenario::from_token(scenario.token()), Ok(scenario));
        }
    }

    #[test]
    fn anything_but_a_known_sim_token_is_refused() {
        assert_eq!(
            Scenario::from_token("4242424242424242"),
            Err(TokenError::Malformed)
        );
        assert_eq!(Scenario::from_token("tok_visa"), Err(TokenError::Malformed));
        assert_eq!(Scenario::from_token("tok_sim_"), Err(TokenError::Malformed));
        assert_eq!(
            Scenario::from_token("tok_sim_APPROVE"),
            Err(TokenError::Malformed)
        );
        assert_eq!(
            Scenario::from_token("tok_sim_teleport"),
            Err(TokenError::UnknownScenario)
        );
    }

    #[test]
    fn only_decline_scenarios_decline() {
        let declining: Vec<_> = Scenario::ALL
            .iter()
            .filter(|s| s.decline().is_some())
            .collect();
        assert_eq!(declining.len(), 3);
        for code in [
            DeclineCode::InsufficientFunds,
            DeclineCode::DoNotHonor,
            DeclineCode::ExpiredPaymentMethod,
        ] {
            assert_eq!(DeclineCode::from_code(code.code()), Some(code));
        }
    }
}
