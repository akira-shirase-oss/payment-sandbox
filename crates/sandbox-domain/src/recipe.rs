//! Posting recipes of the two-phase hold model (D-011).
//!
//! | Recipe | Debit | Credit |
//! |---|---|---|
//! | authorize | `payer_funding` | `payer_held` |
//! | capture | `payer_held` | `merchant_pending` |
//! | void (uncaptured remainder) | `payer_held` | `payer_funding` |
//! | refund | `merchant_pending` | `payer_funding` |
//!
//! Void is the reversing entry of the part of an authorization that was never captured.

use std::fmt;

use serde::Serialize;

use crate::account::{AccountKind, Side};
use crate::currency::Currency;
use crate::journal::{JournalEntry, Line, PostingError};
use crate::money::Amount;

/// What produced a journal entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Recipe {
    /// Place a hold.
    Authorize,
    /// Move held money to the merchant.
    Capture,
    /// Release the uncaptured remainder of a hold.
    Void,
    /// Return captured money to the payer.
    Refund,
    /// A correcting entry that mirrors an earlier one.
    Reversal,
}

/// A recipe code outside the set.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown recipe")]
pub struct UnknownRecipe;

impl Recipe {
    /// Every recipe.
    pub const ALL: [Self; 5] = [
        Self::Authorize,
        Self::Capture,
        Self::Void,
        Self::Refund,
        Self::Reversal,
    ];

    /// Stable code used in the database and on the wire.
    pub const fn code(self) -> &'static str {
        match self {
            Self::Authorize => "authorize",
            Self::Capture => "capture",
            Self::Void => "void",
            Self::Refund => "refund",
            Self::Reversal => "reversal",
        }
    }

    /// Parses a code from [`Recipe::code`].
    pub fn from_code(code: &str) -> Result<Self, UnknownRecipe> {
        Self::ALL
            .into_iter()
            .find(|recipe| recipe.code() == code)
            .ok_or(UnknownRecipe)
    }

    /// The (debit, credit) accounts of a two-leg recipe; `None` for a reversal, whose legs
    /// are those of the entry it mirrors.
    pub const fn legs(self) -> Option<(AccountKind, AccountKind)> {
        match self {
            Self::Authorize => Some((AccountKind::PayerFunding, AccountKind::PayerHeld)),
            Self::Capture => Some((AccountKind::PayerHeld, AccountKind::MerchantPending)),
            Self::Void => Some((AccountKind::PayerHeld, AccountKind::PayerFunding)),
            Self::Refund => Some((AccountKind::MerchantPending, AccountKind::PayerFunding)),
            Self::Reversal => None,
        }
    }

    /// Builds this recipe's balanced entry for `amount` in `currency`.
    pub fn post(self, currency: Currency, amount: Amount) -> Result<JournalEntry, PostingError> {
        let Some((debit, credit)) = self.legs() else {
            return Err(PostingError::TooFewLines);
        };
        JournalEntry::new(
            self,
            vec![
                Line {
                    account: debit,
                    side: Side::Debit,
                    currency,
                    amount,
                },
                Line {
                    account: credit,
                    side: Side::Credit,
                    currency,
                    amount,
                },
            ],
        )
    }
}

impl fmt::Display for Recipe {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;
    use crate::account::CHART;

    #[test]
    fn codes_round_trip() {
        for recipe in Recipe::ALL {
            assert_eq!(Recipe::from_code(recipe.code()), Ok(recipe));
        }
        assert_eq!(Recipe::from_code("transfer"), Err(UnknownRecipe));
    }

    #[test]
    fn a_reversal_has_no_fixed_legs() {
        assert_eq!(
            Recipe::Reversal.post(Currency::Usd, Amount::new(1).unwrap()),
            Err(PostingError::TooFewLines)
        );
    }

    #[test]
    fn every_recipe_posts_only_to_accounts_in_the_chart() {
        for recipe in Recipe::ALL {
            if let Some((debit, credit)) = recipe.legs() {
                assert!(
                    CHART.contains(&debit) && CHART.contains(&credit),
                    "{recipe}"
                );
                assert_ne!(debit, credit, "{recipe} must move money between accounts");
            }
        }
    }

    proptest! {
        #[test]
        fn every_recipe_entry_balances(minor in 1i64..=crate::money::MAX_AMOUNT_MINOR, c in 0usize..4, r in 0usize..4) {
            let recipe = Recipe::ALL[r];
            let entry = recipe.post(Currency::ALL[c], Amount::new(minor).unwrap()).unwrap();
            prop_assert_eq!(entry.lines().iter().map(Line::signed_minor).sum::<i64>(), 0);
            prop_assert_eq!(entry.total(), minor);
            prop_assert_eq!(entry.recipe(), recipe);
        }
    }
}
