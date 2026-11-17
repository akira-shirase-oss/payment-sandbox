//! Checked integer money. An [`Amount`] is a positive count of minor units; there is no
//! float anywhere, and every sum is checked.

use std::fmt::Write as _;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::currency::Currency;

/// Largest amount of one operation, in minor units (D-010). At this cap no single entry
/// can overflow `i64` (≈ 9.2 × 10^18); an account sum of about 9.2 million maximum-size
/// postings could, and fails closed (the database's cast errors) rather than wrapping.
pub const MAX_AMOUNT_MINOR: i64 = 1_000_000_000_000;

/// A positive amount in minor units, `1..=MAX_AMOUNT_MINOR`. Its currency travels beside it
/// (on the line, entry or payment), so the scale is always explicit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Amount(i64);

/// Why an amount was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AmountError {
    /// Zero or negative.
    #[error("amount must be at least 1 minor unit")]
    NotPositive,
    /// Above [`MAX_AMOUNT_MINOR`].
    #[error("amount exceeds the sandbox maximum of 1000000000000 minor units")]
    TooLarge,
    /// A decimal string that is not a plain non-negative number.
    #[error("amount must be a plain decimal number such as 12.34")]
    Malformed,
    /// More fractional digits than the currency has.
    #[error("too many decimal places for this currency")]
    TooPrecise,
}

impl Amount {
    /// Validates a minor-unit count.
    pub const fn new(minor: i64) -> Result<Self, AmountError> {
        if minor < 1 {
            Err(AmountError::NotPositive)
        } else if minor > MAX_AMOUNT_MINOR {
            Err(AmountError::TooLarge)
        } else {
            Ok(Self(minor))
        }
    }

    /// The minor-unit count.
    pub const fn minor(self) -> i64 {
        self.0
    }

    /// Parses a major-unit decimal string (`"12.34"` USD → 1234 minor units). Refuses signs,
    /// exponents, separators, and more decimal places than `currency` has.
    pub fn parse_decimal(text: &str, currency: Currency) -> Result<Self, AmountError> {
        let (whole, fraction) = match text.split_once('.') {
            Some((whole, fraction)) => (whole, fraction),
            None => (text, ""),
        };
        let all_digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
        if whole.is_empty() || !all_digits(whole) || !all_digits(fraction) {
            return Err(AmountError::Malformed);
        }
        if text.contains('.') && fraction.is_empty() {
            return Err(AmountError::Malformed);
        }
        let exponent = currency.exponent();
        let fraction_len = u32::try_from(fraction.len()).map_err(|_| AmountError::TooPrecise)?;
        if fraction_len > exponent {
            return Err(AmountError::TooPrecise);
        }
        // Digit strings longer than this cannot be ≤ MAX_AMOUNT_MINOR; refusing them early
        // keeps the arithmetic below inside i64.
        if whole.len() > 13 {
            return Err(AmountError::TooLarge);
        }
        let scale = 10_i64.pow(exponent);
        let whole: i64 = whole.parse().map_err(|_| AmountError::Malformed)?;
        let padding = 10_i64.pow(exponent.saturating_sub(fraction_len));
        let fraction: i64 = if fraction.is_empty() {
            0
        } else {
            fraction.parse().map_err(|_| AmountError::Malformed)?
        };
        let minor = whole
            .checked_mul(scale)
            .and_then(|w| fraction.checked_mul(padding).and_then(|f| w.checked_add(f)))
            .ok_or(AmountError::TooLarge)?;
        Self::new(minor)
    }

    /// Formats as a major-unit decimal string with exactly the currency's places.
    pub fn to_decimal(self, currency: Currency) -> String {
        format_minor(self.0, currency)
    }
}

/// Formats any signed minor-unit count with the currency's decimal places.
pub fn format_minor(minor: i64, currency: Currency) -> String {
    let exponent = currency.exponent();
    let digits = minor.unsigned_abs().to_string();
    let mut out = String::new();
    if minor < 0 {
        out.push('-');
    }
    let places = usize::try_from(exponent).unwrap_or(0);
    if places == 0 {
        out.push_str(&digits);
        return out;
    }
    let padded = format!("{digits:0>width$}", width = places.saturating_add(1));
    let split = padded.len().saturating_sub(places);
    let _ = write!(out, "{}.{}", &padded[..split], &padded[split..]);
    out
}

impl Serialize for Amount {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_i64(self.0)
    }
}

impl<'de> Deserialize<'de> for Amount {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let minor = i64::deserialize(deserializer)?;
        Self::new(minor).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn amounts_outside_one_to_max_are_refused() {
        assert_eq!(Amount::new(0), Err(AmountError::NotPositive));
        assert_eq!(Amount::new(-5), Err(AmountError::NotPositive));
        assert_eq!(
            Amount::new(MAX_AMOUNT_MINOR + 1),
            Err(AmountError::TooLarge)
        );
        assert_eq!(
            Amount::new(MAX_AMOUNT_MINOR).map(Amount::minor),
            Ok(MAX_AMOUNT_MINOR)
        );
    }

    #[test]
    fn decimal_parsing_respects_each_currency_scale() {
        let cases = [
            ("12.34", Currency::Usd, Ok(1234)),
            ("12.3", Currency::Eur, Ok(1230)),
            ("12", Currency::Usd, Ok(1200)),
            ("500", Currency::Jpy, Ok(500)),
            ("1.5", Currency::Jpy, Err(AmountError::TooPrecise)),
            ("1.234", Currency::Kwd, Ok(1234)),
            ("1.2345", Currency::Kwd, Err(AmountError::TooPrecise)),
            ("0.001", Currency::Kwd, Ok(1)),
            ("0.00", Currency::Usd, Err(AmountError::NotPositive)),
            ("-1", Currency::Usd, Err(AmountError::Malformed)),
            ("+1", Currency::Usd, Err(AmountError::Malformed)),
            ("1e3", Currency::Usd, Err(AmountError::Malformed)),
            ("1,000", Currency::Usd, Err(AmountError::Malformed)),
            ("1.", Currency::Usd, Err(AmountError::Malformed)),
            (".5", Currency::Usd, Err(AmountError::Malformed)),
            ("", Currency::Usd, Err(AmountError::Malformed)),
            ("10000000000.01", Currency::Usd, Err(AmountError::TooLarge)),
            (
                "99999999999999999999",
                Currency::Jpy,
                Err(AmountError::TooLarge),
            ),
        ];
        for (text, currency, expected) in cases {
            assert_eq!(
                Amount::parse_decimal(text, currency).map(Amount::minor),
                expected,
                "{text:?} {currency}"
            );
        }
    }

    #[test]
    fn formatting_pads_to_the_currency_scale() {
        assert_eq!(format_minor(1234, Currency::Usd), "12.34");
        assert_eq!(format_minor(5, Currency::Usd), "0.05");
        assert_eq!(format_minor(-5, Currency::Eur), "-0.05");
        assert_eq!(format_minor(500, Currency::Jpy), "500");
        assert_eq!(format_minor(1, Currency::Kwd), "0.001");
        assert_eq!(format_minor(0, Currency::Kwd), "0.000");
    }

    #[test]
    fn serde_refuses_invalid_amounts() {
        assert!(serde_json::from_str::<Amount>("0").is_err());
        assert!(serde_json::from_str::<Amount>("1.5").is_err());
        assert!(serde_json::from_str::<Amount>("1000000000001").is_err());
        assert_eq!(
            serde_json::from_str::<Amount>("42").map(Amount::minor).ok(),
            Some(42)
        );
    }

    proptest! {
        #[test]
        fn format_then_parse_is_the_identity(minor in 1..=MAX_AMOUNT_MINOR, idx in 0usize..4) {
            let currency = Currency::ALL[idx];
            let amount = Amount::new(minor).unwrap();
            let text = amount.to_decimal(currency);
            prop_assert_eq!(Amount::parse_decimal(&text, currency), Ok(amount));
        }
    }
}
