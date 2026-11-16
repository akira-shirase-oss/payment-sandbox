//! The fixed currency set (D-010): two-, zero- and three-decimal scales are all present, so
//! code that assumes "cents" fails a test instead of a customer.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A supported sandbox currency.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Currency {
    /// US dollar, 2 decimal places.
    Usd,
    /// Euro, 2 decimal places.
    Eur,
    /// Japanese yen, 0 decimal places.
    Jpy,
    /// Kuwaiti dinar, 3 decimal places.
    Kwd,
}

/// A currency code outside the supported set.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unsupported currency; the sandbox supports USD, EUR, JPY and KWD")]
pub struct UnsupportedCurrency;

impl Currency {
    /// Every supported currency, in a stable order.
    pub const ALL: [Self; 4] = [Self::Usd, Self::Eur, Self::Jpy, Self::Kwd];

    /// ISO 4217 code.
    pub const fn code(self) -> &'static str {
        match self {
            Self::Usd => "USD",
            Self::Eur => "EUR",
            Self::Jpy => "JPY",
            Self::Kwd => "KWD",
        }
    }

    /// Number of decimal places of the minor unit (ISO 4217 exponent).
    pub const fn exponent(self) -> u32 {
        match self {
            Self::Usd | Self::Eur => 2,
            Self::Jpy => 0,
            Self::Kwd => 3,
        }
    }

    /// Parses an upper-case ISO code. Lower case is refused so the wire format has one
    /// spelling.
    pub fn from_code(code: &str) -> Result<Self, UnsupportedCurrency> {
        Self::ALL
            .into_iter()
            .find(|currency| currency.code() == code)
            .ok_or(UnsupportedCurrency)
    }
}

impl fmt::Display for Currency {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

impl FromStr for Currency {
    type Err = UnsupportedCurrency;

    fn from_str(code: &str) -> Result<Self, Self::Err> {
        Self::from_code(code)
    }
}

impl Serialize for Currency {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.code())
    }
}

impl<'de> Deserialize<'de> for Currency {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let code = <&str>::deserialize(deserializer)?;
        Self::from_code(code).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_currency_round_trips_through_its_code() {
        for currency in Currency::ALL {
            assert_eq!(Currency::from_code(currency.code()), Ok(currency));
        }
    }

    #[test]
    fn the_set_covers_zero_two_and_three_decimal_scales() {
        let exponents: Vec<u32> = Currency::ALL.iter().map(|c| c.exponent()).collect();
        assert_eq!(exponents, [2, 2, 0, 3]);
    }

    #[test]
    fn unknown_and_lower_case_codes_are_refused() {
        for code in ["usd", "GBP", "", "US", "USDX"] {
            assert_eq!(
                Currency::from_code(code),
                Err(UnsupportedCurrency),
                "{code:?}"
            );
        }
    }

    #[test]
    fn serde_uses_the_iso_code() {
        let json = serde_json::to_string(&Currency::Kwd).unwrap();
        assert_eq!(json, "\"KWD\"");
        let back: Currency = serde_json::from_str("\"JPY\"").unwrap();
        assert_eq!(back, Currency::Jpy);
        assert!(serde_json::from_str::<Currency>("\"XAU\"").is_err());
    }
}
