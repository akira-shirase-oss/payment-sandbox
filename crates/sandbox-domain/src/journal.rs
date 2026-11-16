//! Journal entries: the only authority over money (D-003). An entry is built only through
//! [`JournalEntry::new`], which refuses anything that does not balance in one currency, so
//! an unbalanced entry is unrepresentable in the rest of the code base. The database checks
//! the same rule again at insert and at commit.

use serde::Serialize;

use crate::account::{AccountKind, Side};
use crate::currency::Currency;
use crate::money::Amount;
use crate::recipe::Recipe;

/// Most lines one entry may have. Every recipe uses two; the cap bounds the work of the
/// commit-time balance check.
pub const MAX_LINES: usize = 8;

/// One line of an entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Line {
    /// The account the line posts to.
    pub account: AccountKind,
    /// Debit or credit.
    pub side: Side,
    /// The line's currency; every line of an entry must share it.
    pub currency: Currency,
    /// Positive amount in minor units.
    pub amount: Amount,
}

impl Line {
    /// The stored signed amount: debit positive, credit negative.
    pub const fn signed_minor(&self) -> i64 {
        match self.side {
            Side::Debit => self.amount.minor(),
            Side::Credit => self.amount.minor().saturating_neg(),
        }
    }
}

/// Why a set of lines is not a valid entry. Each is a named refusal.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PostingError {
    /// Fewer than two lines.
    #[error("a journal entry needs at least two lines")]
    TooFewLines,
    /// More than [`MAX_LINES`] lines.
    #[error("a journal entry may have at most 8 lines")]
    TooManyLines,
    /// Lines in more than one currency (D-010: no FX).
    #[error("a journal entry must be in a single currency")]
    MixedCurrency,
    /// Debits and credits differ.
    #[error("debits {debits} and credits {credits} do not balance")]
    Unbalanced {
        /// Total debits in minor units.
        debits: i64,
        /// Total credits in minor units.
        credits: i64,
    },
    /// A sum left `i64`.
    #[error("journal entry totals overflow")]
    Overflow,
}

/// A validated, balanced, single-currency journal entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct JournalEntry {
    recipe: Recipe,
    currency: Currency,
    lines: Vec<Line>,
}

impl JournalEntry {
    /// Validates `lines` into an entry.
    pub fn new(recipe: Recipe, lines: Vec<Line>) -> Result<Self, PostingError> {
        if lines.len() < 2 {
            return Err(PostingError::TooFewLines);
        }
        if lines.len() > MAX_LINES {
            return Err(PostingError::TooManyLines);
        }
        let currency = lines[0].currency;
        if lines.iter().any(|line| line.currency != currency) {
            return Err(PostingError::MixedCurrency);
        }
        let (mut debits, mut credits) = (0_i64, 0_i64);
        for line in &lines {
            let total = match line.side {
                Side::Debit => &mut debits,
                Side::Credit => &mut credits,
            };
            *total = total
                .checked_add(line.amount.minor())
                .ok_or(PostingError::Overflow)?;
        }
        if debits != credits {
            return Err(PostingError::Unbalanced { debits, credits });
        }
        Ok(Self {
            recipe,
            currency,
            lines,
        })
    }

    /// The recipe that produced the entry.
    pub const fn recipe(&self) -> Recipe {
        self.recipe
    }

    /// The entry's single currency.
    pub const fn currency(&self) -> Currency {
        self.currency
    }

    /// The lines, in order.
    pub fn lines(&self) -> &[Line] {
        &self.lines
    }

    /// Total of the debit side (equal to the credit side).
    pub fn total(&self) -> i64 {
        self.lines
            .iter()
            .filter(|line| line.side == Side::Debit)
            .map(|line| line.amount.minor())
            .fold(0_i64, i64::saturating_add)
    }

    /// The correcting entry: the same lines with sides swapped. Posted entries are never
    /// edited; this is the only way to undo one (D-003).
    #[must_use]
    pub fn reversal(&self) -> Self {
        let lines = self
            .lines
            .iter()
            .map(|line| Line {
                side: match line.side {
                    Side::Debit => Side::Credit,
                    Side::Credit => Side::Debit,
                },
                ..*line
            })
            .collect();
        Self {
            recipe: Recipe::Reversal,
            currency: self.currency,
            lines,
        }
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;
    use crate::account::CHART;

    fn line(account: AccountKind, side: Side, currency: Currency, minor: i64) -> Line {
        Line {
            account,
            side,
            currency,
            amount: Amount::new(minor).unwrap(),
        }
    }

    #[test]
    fn a_balanced_pair_is_accepted() {
        let entry = JournalEntry::new(
            Recipe::Authorize,
            vec![
                line(AccountKind::PayerFunding, Side::Debit, Currency::Usd, 500),
                line(AccountKind::PayerHeld, Side::Credit, Currency::Usd, 500),
            ],
        )
        .unwrap();
        assert_eq!(entry.total(), 500);
        assert_eq!(entry.lines().iter().map(Line::signed_minor).sum::<i64>(), 0);
    }

    #[test]
    fn invalid_postings_are_named_refusals() {
        let usd = Currency::Usd;
        assert_eq!(
            JournalEntry::new(
                Recipe::Authorize,
                vec![line(AccountKind::PayerHeld, Side::Debit, usd, 1)]
            ),
            Err(PostingError::TooFewLines)
        );
        assert_eq!(
            JournalEntry::new(
                Recipe::Authorize,
                vec![
                    line(AccountKind::PayerFunding, Side::Debit, usd, 500),
                    line(AccountKind::PayerHeld, Side::Credit, usd, 499),
                ]
            ),
            Err(PostingError::Unbalanced {
                debits: 500,
                credits: 499
            })
        );
        assert_eq!(
            JournalEntry::new(
                Recipe::Authorize,
                vec![
                    line(AccountKind::PayerFunding, Side::Debit, usd, 500),
                    line(AccountKind::PayerHeld, Side::Credit, Currency::Eur, 500),
                ]
            ),
            Err(PostingError::MixedCurrency)
        );
        let nine = (0..9)
            .map(|i| {
                let side = if i % 2 == 0 {
                    Side::Debit
                } else {
                    Side::Credit
                };
                line(AccountKind::PayerHeld, side, usd, 1)
            })
            .collect();
        assert_eq!(
            JournalEntry::new(Recipe::Reversal, nine),
            Err(PostingError::TooManyLines)
        );
    }

    #[test]
    fn the_amount_cap_keeps_the_largest_entry_inside_i64() {
        let max = crate::money::MAX_AMOUNT_MINOR;
        // Eight lines at the cap total 8 × 10^12, far below i64::MAX: the checked sums can
        // only fail if the cap is ever raised carelessly, and then they refuse, not wrap.
        let lines: Vec<Line> = (0..8)
            .map(|i| {
                let side = if i < 4 { Side::Debit } else { Side::Credit };
                line(AccountKind::PayerHeld, side, Currency::Jpy, max)
            })
            .collect();
        assert!(JournalEntry::new(Recipe::Reversal, lines).is_ok());
    }

    fn arb_currency() -> impl Strategy<Value = Currency> {
        (0usize..4).prop_map(|i| Currency::ALL[i])
    }

    fn arb_account() -> impl Strategy<Value = AccountKind> {
        (0usize..CHART.len()).prop_map(|i| CHART[i])
    }

    proptest! {
        /// Whatever lines are offered, an accepted entry's signed lines sum to exactly zero.
        #[test]
        fn every_accepted_entry_sums_to_zero(
            raw in prop::collection::vec((arb_account(), any::<bool>(), 1i64..=1_000_000), 0..10),
            currency in arb_currency(),
        ) {
            let lines: Vec<Line> = raw
                .into_iter()
                .map(|(account, debit, minor)| {
                    line(account, if debit { Side::Debit } else { Side::Credit }, currency, minor)
                })
                .collect();
            let signed: i64 = lines.iter().map(Line::signed_minor).sum();
            match JournalEntry::new(Recipe::Reversal, lines.clone()) {
                Ok(entry) => prop_assert_eq!(entry.lines().iter().map(Line::signed_minor).sum::<i64>(), 0),
                Err(PostingError::Unbalanced { .. }) => prop_assert_ne!(signed, 0),
                Err(PostingError::TooFewLines) => prop_assert!(lines.len() < 2),
                Err(PostingError::TooManyLines) => prop_assert!(lines.len() > MAX_LINES),
                Err(other) => prop_assert!(false, "unexpected refusal {other:?}"),
            }
        }

        /// A reversal undoes its entry exactly: together they leave every account unchanged.
        #[test]
        fn an_entry_and_its_reversal_cancel(minor in 1i64..=1_000_000_000, currency in arb_currency()) {
            let entry = JournalEntry::new(
                Recipe::Capture,
                vec![
                    line(AccountKind::PayerHeld, Side::Debit, currency, minor),
                    line(AccountKind::MerchantPending, Side::Credit, currency, minor),
                ],
            ).unwrap();
            let reversal = entry.reversal();
            for account in CHART {
                let net: i64 = entry.lines().iter().chain(reversal.lines())
                    .filter(|l| l.account == account)
                    .map(Line::signed_minor)
                    .sum();
                prop_assert_eq!(net, 0);
            }
        }
    }
}
