//! Raw-card detection (D-019). The primary defence is that no UI has a card field and the
//! API accepts only `tok_sim_*` tokens; this guard is the second line: a request that names
//! a card field or carries a card-number-like digit run is refused before parsing goes
//! further, and the value is never echoed, logged or stored.
//!
//! "Card-number-like" (review finding 6; D-043): a run of 13 or more digits — ASCII or
//! full-width, joined across separators of at most two non-alphanumeric characters — that
//! has a Luhn-valid window of 13–19 digits. Letters, longer gaps and canonical UUIDs end a
//! run. The windowed check refuses some long digit strings that are not card numbers; in a
//! sandbox that never needs them, that is the right side to err on.

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

/// Most separator characters allowed between two digit groups of one number.
const MAX_SEPARATOR: usize = 2;

/// The decimal value of an ASCII or full-width digit.
fn digit_of(c: char) -> Option<u8> {
    if c.is_ascii_digit() {
        return u8::try_from(u32::from(c).wrapping_sub(u32::from('0'))).ok();
    }
    if ('\u{ff10}'..='\u{ff19}').contains(&c) {
        return u8::try_from(u32::from(c).wrapping_sub(0xff10)).ok();
    }
    None
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

/// A canonical hyphenated UUID starts here (identifiers are never card numbers).
fn starts_uuid(chars: &[char]) -> bool {
    if chars.len() < 36 {
        return false;
    }
    chars[..36].iter().enumerate().all(|(n, c)| match n {
        8 | 13 | 18 | 23 => *c == '-',
        _ => c.is_ascii_hexdigit(),
    }) && chars.get(36).is_none_or(|c| !c.is_ascii_hexdigit())
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

    #[test]
    fn ordinary_text_and_identifiers_are_not_card_like() {
        let clean = [
            "tok_sim_approve",
            "order-1234",
            "018f2f0e-3c1a-7cc0-9e1e-3f1f6b7f9a01",
            "12345678-1234-1234-1234-123456789012",
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
        /// Every all-digit UUID passes: digit-only groups are at most 12 long and the
        /// 8-4-4-4-12 grouping is not a card grouping.
        #[test]
        fn uuids_never_look_like_cards(id in any::<u128>()) {
            let text = uuid::Uuid::from_u128(id).to_string();
            prop_assert!(!contains_pan_like(&text));
        }

        #[test]
        fn digit_uuids_never_look_like_cards(groups in ("[0-9]{8}", "[0-9]{4}", "[0-9]{4}", "[0-9]{4}", "[0-9]{12}")) {
            let text = format!("{}-{}-{}-{}-{}", groups.0, groups.1, groups.2, groups.3, groups.4);
            prop_assert!(!contains_pan_like(&text));
        }
    }
}
