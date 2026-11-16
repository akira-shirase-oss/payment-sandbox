//! The chart of accounts (D-012). Every project has one of each kind per currency.
//!
//! Sign convention: a journal line stores a *signed* minor-unit amount, debit positive and
//! credit negative, so an entry balances exactly when its lines sum to zero and an
//! account's signed balance is the plain sum of its lines. [`AccountKind::presented`] turns
//! a signed balance into the positive number people expect on the account's normal side.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Debit or credit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    /// Increases debit-normal accounts; stored as a positive signed amount.
    Debit,
    /// Increases credit-normal accounts; stored as a negative signed amount.
    Credit,
}

/// One account in the chart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountKind {
    /// The synthetic source of payer money. Debit-normal: its balance is the payer money
    /// currently out of the source (held, captured and not refunded).
    PayerFunding,
    /// Authorized holds not yet captured or released. Credit-normal.
    PayerHeld,
    /// Captured money owed to the merchant, not yet settled. Credit-normal.
    MerchantPending,
    /// Settled merchant money (billing milestone). Credit-normal.
    MerchantAvailable,
    /// Simulated fees earned (billing milestone). Credit-normal.
    Fees,
    /// Refunds sent to the network and not yet confirmed (billing milestone). Credit-normal.
    RefundsClearing,
    /// Receivable from the simulated network (billing milestone). Debit-normal.
    NetworkSettlement,
}

/// The whole chart, in a stable order.
pub const CHART: [AccountKind; 7] = [
    AccountKind::PayerFunding,
    AccountKind::PayerHeld,
    AccountKind::MerchantPending,
    AccountKind::MerchantAvailable,
    AccountKind::Fees,
    AccountKind::RefundsClearing,
    AccountKind::NetworkSettlement,
];

/// An account code outside the chart.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown account")]
pub struct UnknownAccount;

impl AccountKind {
    /// Stable code used in the database and on the wire.
    pub const fn code(self) -> &'static str {
        match self {
            Self::PayerFunding => "payer_funding",
            Self::PayerHeld => "payer_held",
            Self::MerchantPending => "merchant_pending",
            Self::MerchantAvailable => "merchant_available",
            Self::Fees => "fees",
            Self::RefundsClearing => "refunds_clearing",
            Self::NetworkSettlement => "network_settlement",
        }
    }

    /// Parses a code from [`AccountKind::code`].
    pub fn from_code(code: &str) -> Result<Self, UnknownAccount> {
        CHART
            .into_iter()
            .find(|kind| kind.code() == code)
            .ok_or(UnknownAccount)
    }

    /// The side on which this account's balance normally grows.
    pub const fn normal_side(self) -> Side {
        match self {
            Self::PayerFunding | Self::NetworkSettlement => Side::Debit,
            Self::PayerHeld
            | Self::MerchantPending
            | Self::MerchantAvailable
            | Self::Fees
            | Self::RefundsClearing => Side::Credit,
        }
    }

    /// The signed balance as seen on the account's normal side (non-negative when the
    /// ledger is healthy).
    pub const fn presented(self, signed_balance: i64) -> i64 {
        match self.normal_side() {
            Side::Debit => signed_balance,
            Side::Credit => signed_balance.saturating_neg(),
        }
    }
}

impl fmt::Display for AccountKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_code_round_trips_and_codes_are_unique() {
        let mut codes: Vec<&str> = CHART.iter().map(|k| k.code()).collect();
        for kind in CHART {
            assert_eq!(AccountKind::from_code(kind.code()), Ok(kind));
        }
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), CHART.len());
        assert_eq!(AccountKind::from_code("cash"), Err(UnknownAccount));
    }

    #[test]
    fn presented_balances_flip_only_credit_normal_accounts() {
        assert_eq!(AccountKind::PayerFunding.presented(100), 100);
        assert_eq!(AccountKind::PayerHeld.presented(-100), 100);
        assert_eq!(AccountKind::MerchantPending.presented(-7), 7);
    }
}
