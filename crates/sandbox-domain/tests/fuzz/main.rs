//! Never-panics fuzzing for every parser/decoder that takes outside input (the standing
//! security rule): each is run over its seed corpus in `corpus/<name>/` and then a
//! proptest smoke run (CI sets `PROPTEST_CASES`). A panic is a red-first bug — proptest records
//! the shrinking input under `tests/fuzz/proptest-regressions/`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test harness: a panic is the failure report"
)]

use std::path::PathBuf;

use proptest::prelude::*;
use sandbox_domain::access::Credential;
use sandbox_domain::card_guard;
use sandbox_domain::currency::Currency;
use sandbox_domain::identity::IdempotencyKey;
use sandbox_domain::keys::Scope;
use sandbox_domain::letters;
use sandbox_domain::money::Amount;
use sandbox_domain::project::ProjectSlug;
use sandbox_domain::scenario::Scenario;
use sandbox_domain::webhook;

/// Every file under `corpus/<name>/`.
fn corpus(name: &str) -> Vec<Vec<u8>> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fuzz/corpus")
        .join(name);
    let mut files: Vec<Vec<u8>> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("corpus dir {}: {e}", dir.display()))
        .map(|e| std::fs::read(e.unwrap().path()).unwrap())
        .collect();
    files.sort();
    assert!(!files.is_empty(), "corpus/{name} has no seeds");
    files
}

/// Runs `parse` over every seed: the corpus itself must never panic.
fn seeded(name: &str, mut parse: impl FnMut(&[u8])) {
    for bytes in corpus(name) {
        parse(&bytes);
    }
}

fn text(bytes: &[u8]) -> &str {
    std::str::from_utf8(bytes).unwrap_or("")
}

#[test]
fn idempotency_key_parse_never_panics() {
    let parse = |b: &[u8]| {
        let _ = IdempotencyKey::parse(text(b));
    };
    seeded("idempotency_key", parse);
    proptest!(|(input in any::<Vec<u8>>())| parse(&input));
}

#[test]
fn card_guard_never_panics() {
    let parse = |b: &[u8]| {
        let _ = card_guard::contains_pan_like(text(b));
        let _ = card_guard::is_card_field_name(text(b));
        let _ = card_guard::luhn_valid(b);
    };
    for name in ["pan_like", "luhn", "idempotency_key"] {
        seeded(name, parse);
    }
    proptest!(|(input in any::<Vec<u8>>())| parse(&input));
}

#[test]
fn webhook_target_parse_never_panics() {
    let parse = |b: &[u8]| {
        let _ = webhook::parse_target(text(b));
    };
    seeded("webhook_url", parse);
    proptest!(|(input in any::<Vec<u8>>())| parse(&input));
}

#[test]
fn webhook_signature_verify_never_panics() {
    let parse = |b: &[u8]| {
        let _ = webhook::verify(&["whsec_sim_test"], text(b), "evt_1", b"{}", 1_732_000_000);
    };
    seeded("signature", parse);
    proptest!(|(input in any::<Vec<u8>>(),
               body in any::<Vec<u8>>(),
               now in any::<i64>())| {
        let _ = webhook::verify(&["whsec_sim_test"], text(&input), "evt_1", &body, now);
        parse(&input);
    });
}

#[test]
fn project_slug_parse_never_panics() {
    let parse = |b: &[u8]| {
        let _ = ProjectSlug::parse(text(b));
    };
    seeded("slug", parse);
    proptest!(|(input in any::<Vec<u8>>())| parse(&input));
}

#[test]
fn currency_code_never_panics() {
    let parse = |b: &[u8]| {
        let _ = Currency::from_code(text(b));
    };
    seeded("currency", parse);
    proptest!(|(input in any::<Vec<u8>>())| parse(&input));
}

#[test]
fn scenario_token_never_panics() {
    let parse = |b: &[u8]| {
        let _ = Scenario::from_token(text(b));
    };
    seeded("token", parse);
    proptest!(|(input in any::<Vec<u8>>())| parse(&input));
}

#[test]
fn scope_code_never_panics() {
    let parse = |b: &[u8]| {
        let _ = Scope::from_code(text(b));
    };
    seeded("scope", parse);
    proptest!(|(input in any::<Vec<u8>>())| parse(&input));
}

#[test]
fn credential_new_never_panics() {
    let parse = |b: &[u8]| {
        let _ = Credential::new(text(b));
    };
    seeded("credential", parse);
    proptest!(|(input in any::<Vec<u8>>())| parse(&input));
}

#[test]
fn money_decimal_parse_never_panics() {
    let usd = Currency::from_code("USD").unwrap();
    let parse = |b: &[u8]| {
        let _ = Amount::parse_decimal(text(b), usd);
    };
    seeded("decimal", parse);
    proptest!(|(input in any::<Vec<u8>>())| parse(&input));
}

#[test]
fn letters_decode_never_panics() {
    let parse = |b: &[u8]| {
        let _ = letters::decode(text(b));
    };
    seeded("letters", parse);
    proptest!(|(input in any::<Vec<u8>>())| parse(&input));
}

#[test]
fn amount_json_decode_never_panics() {
    let parse = |b: &[u8]| {
        let _ = serde_json::from_slice::<Amount>(b);
    };
    seeded("amount_json", parse);
    proptest!(|(input in any::<Vec<u8>>())| parse(&input));
}
