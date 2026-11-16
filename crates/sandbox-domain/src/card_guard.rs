//! Raw-card detection (D-019). The primary defence is that no UI has a card field and the
//! API accepts only `tok_sim_*` tokens; this guard is the second line: a request that names
//! a card field or carries a card-number-like digit run is refused before parsing goes
//! further, and the value is never echoed, logged or stored.
//!
//! "Card-number-like" means 13–19 digits that pass the Luhn check, either contiguous or in
//! a printed card grouping (4-4-4-4[-3], 4-6-5, 4-6-4) separated by single spaces or
//! dashes. A UUID never matches: its longest digit-only group is 12 characters and its
//! 8-4-4-4-12 grouping is not a card grouping.

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

/// Card groupings, as digit counts per group.
const GROUPINGS: [&[usize]; 6] = [
    &[4, 4, 4, 4],
    &[4, 4, 4, 4, 3],
    &[4, 4, 4, 1],
    &[4, 6, 5],
    &[4, 6, 4],
    &[4, 4, 4, 2],
];

/// True when `text` contains a card-number-like digit run.
pub fn contains_pan_like(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if !bytes[i].is_ascii_digit() {
            i = i.saturating_add(1);
            continue;
        }
        // A maximal run of digit groups joined by single separators.
        let mut groups: Vec<usize> = Vec::new();
        let mut digits: Vec<u8> = Vec::new();
        let mut j = i;
        loop {
            let start = j;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                digits.push(bytes[j]);
                j = j.saturating_add(1);
            }
            groups.push(j.saturating_sub(start));
            let separated = j.saturating_add(1) < bytes.len()
                && matches!(bytes[j], b' ' | b'-')
                && bytes[j.saturating_add(1)].is_ascii_digit();
            if !separated {
                break;
            }
            j = j.saturating_add(1);
        }
        if run_is_pan_like(&groups, &digits) {
            return true;
        }
        i = j.max(i.saturating_add(1));
    }
    false
}

fn run_is_pan_like(groups: &[usize], digits: &[u8]) -> bool {
    if !(13..=19).contains(&digits.len()) || !luhn_valid(digits) {
        return false;
    }
    groups.len() == 1 || GROUPINGS.contains(&groups)
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
    }

    #[test]
    fn ordinary_text_and_identifiers_are_not_card_like() {
        let clean = [
            "tok_sim_approve",
            "order-1234",
            "018f2f0e-3c1a-7cc0-9e1e-3f1f6b7f9a01",
            "12345678-1234-1234-1234-123456789012",
            "4242424242424241",
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
