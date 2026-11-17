//! Raw-card detection (D-019). The primary defence is that no UI has a card field and the
//! API accepts only `tok_sim_*` tokens; this guard is the second line: a request that names
//! a card field or carries a card-number-like digit run is refused before parsing goes
//! further, and the value is never echoed, logged or stored.
//!
//! "Card-number-like" (review finding 6, N7; D-043, D-059): a run of 13 or more digits —
//! any Unicode decimal digit (ASCII, full-width, Arabic-Indic, Devanagari, mathematical
//! digits and the other `Nd` scripts) or a superscript, subscript or circled digit, joined
//! across separators of at most three non-alphanumeric characters — that has a Luhn-valid
//! window of 13–19 digits. Letters, longer gaps and canonical UUIDs that contain a letter
//! end a run (an all-digit UUID is digits like any other). The windowed check refuses some
//! long digit strings that are not card numbers; in a sandbox that never needs them, that
//! is the right side to err on.

/// Field names that only card data would use. Compared case-insensitively after removing
/// `_` and `-`.
const CARD_FIELDS: [&str; 14] = [
    "card",
    "cardnumber",
    "number",
    "pan",
    "cvc",
    "cvv",
    "cvc2",
    "cvv2",
    "securitycode",
    "expmonth",
    "expyear",
    "expiry",
    "expiration",
    "track2",
];

/// True when `name` is a card-data field name.
pub fn is_card_field_name(name: &str) -> bool {
    let normalized: String = name
        .chars()
        .filter(|c| *c != '_' && *c != '-')
        .flat_map(char::to_lowercase)
        .collect();
    CARD_FIELDS.contains(&normalized.as_str()) || normalized.contains("cardnumber")
}

/// The Luhn (mod 10) check over ASCII digits.
pub fn luhn_valid(digits: &[u8]) -> bool {
    let mut sum = 0_u32;
    for (i, digit) in digits.iter().rev().enumerate() {
        let mut d = u32::from(digit.wrapping_sub(b'0'));
        if i % 2 == 1 {
            d = d.wrapping_mul(2);
            if d > 9 {
                d = d.wrapping_sub(9);
            }
        }
        sum = sum.wrapping_add(d);
    }
    !digits.is_empty() && sum.is_multiple_of(10)
}

/// Most separator characters allowed between two digit groups of one number
/// (`4242 - 4242` has three).
const MAX_SEPARATOR: usize = 3;

/// The zero of every block of ten Unicode decimal digits (`Nd`) outside ASCII: a digit's
/// value is its distance from its block's zero.
const DIGIT_ZEROS: [u32; 67] = [
    0x0660, 0x06F0, 0x07C0, 0x0966, 0x09E6, 0x0A66, 0x0AE6, 0x0B66, 0x0BE6, 0x0C66, 0x0CE6, 0x0D66,
    0x0DE6, 0x0E50, 0x0ED0, 0x0F20, 0x1040, 0x1090, 0x17E0, 0x1810, 0x1946, 0x19D0, 0x1A80, 0x1A90,
    0x1B50, 0x1BB0, 0x1C40, 0x1C50, 0xA620, 0xA8D0, 0xA900, 0xA9D0, 0xA9F0, 0xAA50, 0xABF0, 0xFF10,
    0x104A0, 0x10D30, 0x11066, 0x110F0, 0x11136, 0x111D0, 0x112F0, 0x11450, 0x114D0, 0x11650,
    0x116C0, 0x11730, 0x118E0, 0x11950, 0x11C50, 0x11D50, 0x11DA0, 0x11F50, 0x16A60, 0x16AC0,
    0x16B50, 0x1D7CE, 0x1D7D8, 0x1D7E2, 0x1D7EC, 0x1D7F6, 0x1E140, 0x1E2F0, 0x1E4F0, 0x1E950,
    0x1FBF0,
];

/// The decimal value of a digit in any of the forms above.
fn digit_of(c: char) -> Option<u8> {
    let code = u32::from(c);
    if c.is_ascii_digit() {
        return u8::try_from(code.wrapping_sub(u32::from('0'))).ok();
    }
    if code < 0x80 {
        return None;
    }
    let value = match code {
        // Superscripts, subscripts, circled digits (compatibility forms).
        0x00B9 => Some(1),
        0x00B2 => Some(2),
        0x00B3 => Some(3),
        0x2070 | 0x2080 | 0x24EA => Some(0),
        0x2074..=0x2079 => Some(code.wrapping_sub(0x2070)),
        0x2081..=0x2089 => Some(code.wrapping_sub(0x2080)),
        0x2460..=0x2468 => Some(code.wrapping_sub(0x245F)),
        _ => DIGIT_ZEROS
            .iter()
            .find(|&&zero| (zero..zero.saturating_add(10)).contains(&code))
            .map(|&zero| code.wrapping_sub(zero)),
    };
    value.and_then(|v| u8::try_from(v).ok())
}

/// True when `text` carries a card-number-like digit run (review finding 6).
///
/// Digits — ASCII or full-width — are joined across short runs (at most two characters)
/// of anything that is neither a letter nor a digit: spaces, NBSP, `.`, `/`, `_`, `-`,
/// doubled separators. A letter, a longer gap, or a canonical UUID ends a run. Every 13–19
/// digit window of a run is checked with Luhn, so a number embedded in a longer run is
/// found too.
pub fn contains_pan_like(text: &str) -> bool {
    let chars: Vec<char> = text.chars().collect();
    let mut digits: Vec<u8> = Vec::new();
    let mut gap = 0_usize;
    let mut i = 0;
    while i < chars.len() {
        if starts_uuid(&chars[i..]) {
            if run_has_pan(&digits) {
                return true;
            }
            digits.clear();
            gap = 0;
            i = i.saturating_add(36);
            continue;
        }
        let c = chars[i];
        if let Some(d) = digit_of(c) {
            digits.push(d.saturating_add(b'0'));
            gap = 0;
        } else if c.is_alphanumeric() {
            if run_has_pan(&digits) {
                return true;
            }
            digits.clear();
        } else {
            gap = gap.saturating_add(1);
            if gap > MAX_SEPARATOR {
                if run_has_pan(&digits) {
                    return true;
                }
                digits.clear();
            }
        }
        i = i.saturating_add(1);
    }
    run_has_pan(&digits)
}

/// A canonical hyphenated UUID with at least one letter starts here (identifiers are never
/// card numbers; an all-digit "UUID" is digits like any other, review N7).
fn starts_uuid(chars: &[char]) -> bool {
    if chars.len() < 36 {
        return false;
    }
    chars[..36].iter().enumerate().all(|(n, c)| match n {
        8 | 13 | 18 | 23 => *c == '-',
        _ => c.is_ascii_hexdigit(),
    }) && chars[..36].iter().any(char::is_ascii_alphabetic)
        && chars.get(36).is_none_or(|c| !c.is_ascii_hexdigit())
}

fn run_has_pan(digits: &[u8]) -> bool {
    if digits.len() < 13 {
        return false;
    }
    (13..=19).any(|len| digits.windows(len).any(luhn_valid))
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    /// Well-known public test numbers (network test cards, never real accounts).
    const TEST_PANS: [&str; 5] = [
        "4242424242424242",
        "4000056655665556",
        "5555555555554444",
        "378282246310005",
        "6011111111111117",
    ];

    #[test]
    fn known_test_numbers_are_detected_in_every_common_format() {
        for pan in TEST_PANS {
            assert!(contains_pan_like(pan), "{pan}");
            assert!(contains_pan_like(&format!("note: {pan} thanks")), "{pan}");
        }
        assert!(contains_pan_like("4242 4242 4242 4242"));
        assert!(contains_pan_like("4242-4242-4242-4242"));
        assert!(contains_pan_like("3782 822463 10005"));
        for shape in [
            "4242.4242.4242.4242",
            "4242 4242  4242 4242",
            "4242\u{a0}4242\u{a0}4242\u{a0}4242",
            "\u{ff14}\u{ff12}\u{ff14}\u{ff12}\u{ff14}\u{ff12}\u{ff14}\u{ff12}\u{ff14}\u{ff12}\u{ff14}\u{ff12}\u{ff14}\u{ff12}\u{ff14}\u{ff12}",
            "4 2 4 2 4 2 4 2 4 2 4 2 4 2 4 2",
            "4242/4242/4242/4242",
            "4242_4242_4242_4242",
            "37828224631000 5",
            "ref 99 4242424242424242 99",
        ] {
            assert!(contains_pan_like(shape), "{shape:?}");
        }
    }

    /// Review N7: shapes the guard of D-043 missed.
    #[test]
    fn wide_gaps_other_digit_forms_and_all_digit_uuids_are_detected() {
        for shape in [
            "4242 - 4242 - 4242 - 4242",
            "\u{1d7d2}\u{1d7d0}\u{1d7d2}\u{1d7d0}\u{1d7d2}\u{1d7d0}\u{1d7d2}\u{1d7d0}\u{1d7d2}\u{1d7d0}\u{1d7d2}\u{1d7d0}\u{1d7d2}\u{1d7d0}\u{1d7d2}\u{1d7d0}",
            "\u{2074}\u{b2}\u{2074}\u{b2}\u{2074}\u{b2}\u{2074}\u{b2}\u{2074}\u{b2}\u{2074}\u{b2}\u{2074}\u{b2}\u{2074}\u{b2}",
            "\u{664}\u{662}\u{664}\u{662}\u{664}\u{662}\u{664}\u{662}\u{664}\u{662}\u{664}\u{662}\u{664}\u{662}\u{664}\u{662}",
            "42424242-4242-4242-4242-424242424242",
        ] {
            assert!(contains_pan_like(shape), "{shape:?}");
        }
    }

    #[test]
    fn ordinary_text_and_identifiers_are_not_card_like() {
        let clean = [
            "tok_sim_approve",
            "order-1234",
            "018f2f0e-3c1a-7cc0-9e1e-3f1f6b7f9a01",
            // 12 digits: shorter than any card number. (A 16-digit string that fails Luhn
            // as a whole is still refused when a 13-15 digit window inside it passes: the
            // windowed check trades those false positives for catching embedded numbers.)
            "424242424241",
            "order 1234 of 2026-11-16 at 12:30",
            "1234 5678",
            "2026-11-16",
            "",
        ];
        for text in clean {
            assert!(!contains_pan_like(text), "{text:?}");
        }
    }

    #[test]
    fn every_digit_block_is_ten_numeric_characters() {
        for zero in DIGIT_ZEROS {
            for n in 0..10 {
                let c = char::from_u32(zero + n).unwrap();
                assert!(c.is_numeric(), "U+{:04X}", zero + n);
                assert_eq!(
                    digit_of(c),
                    Some(u8::try_from(n).unwrap()),
                    "U+{:04X}",
                    zero + n
                );
            }
        }
        assert_eq!(digit_of('\u{b9}'), Some(1));
        assert_eq!(digit_of('\u{2468}'), Some(9));
        assert_eq!(digit_of('a'), None);
        assert_eq!(digit_of('\u{e9}'), None);
    }

    #[test]
    fn card_field_names_are_recognised_in_any_casing() {
        for name in [
            "number",
            "card_number",
            "cardNumber",
            "CVC",
            "cvv2",
            "exp_month",
            "card",
            "PAN",
        ] {
            assert!(is_card_field_name(name), "{name}");
        }
        for name in ["amount", "currency", "token", "description", "payment_id"] {
            assert!(!is_card_field_name(name), "{name}");
        }
    }

    #[test]
    fn luhn_matches_the_reference_examples() {
        assert!(luhn_valid(b"79927398713"));
        assert!(!luhn_valid(b"79927398710"));
        assert!(!luhn_valid(b""));
    }

    proptest! {
        /// Every UUID with a letter passes, whatever its digits. (One made only of digits is
        /// treated as digits, review N7; a random UUID is that with probability ~(10/16)^32.)
        #[test]
        fn uuids_never_look_like_cards(id in any::<u128>()) {
            let text = uuid::Uuid::from_u128(id).to_string();
            prop_assume!(text.bytes().any(|b| b.is_ascii_alphabetic()));
            prop_assert!(!contains_pan_like(&text));
        }
    }
}
