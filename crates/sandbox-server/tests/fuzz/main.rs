//! Never-panics fuzzing for the server's own outside-input decoder: `CursorKey::open`
//! turns a client-supplied `?cursor=` value back into fields (D-056, D-105). It must
//! answer `Err(CursorError)` — never panic — on anything a client sends. The seed corpus
//! is generated in-process (a real sealed cursor, then every mutation class), then a
//! proptest smoke run; a panic is a red-first bug.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test harness: a panic is the failure report"
)]

use proptest::prelude::*;
use sandbox_db::reconcile::ExplainCursor;
use sandbox_server::cursor::CursorKey;

const CONTEXT: &str = "explain:test";

fn sealed() -> (CursorKey, String) {
    let key = CursorKey::new(None, std::time::Duration::from_secs(3_600));
    let cursor = ExplainCursor {
        generation: "1".to_owned(),
        as_of: "1:2:1".to_owned(),
        after_seq: 0,
        running: 0,
    };
    let sealed = key.seal(&cursor, CONTEXT).unwrap();
    (key, sealed)
}

/// The seed corpus: one authentic cursor and every mutation class a client can send.
#[test]
fn cursor_open_survives_its_seed_corpus() {
    let (key, good) = sealed();
    let mut cases = vec![
        good.clone(),
        good[..good.len() / 2].to_owned(), // truncated
        format!("{good}{good}"),           // doubled, over the field bound
        good.replace('a', "b"),            // altered bytes (forgery)
        "z".repeat(good.len()),            // outside the a–p alphabet
        "a".to_string(),                   // too short to hold a tag
        "a".repeat(70_000),                // over MAX_CURSOR_CHARS
        String::new(),                     // empty
        "💳".repeat(200),                  // non-ASCII
    ];
    // Every single-character deletion of the real cursor.
    for i in 0..good.len() {
        cases.push(format!("{}{}", &good[..i], &good[i + 1..]));
    }
    for case in cases {
        let _ = key.open(&case, CONTEXT);
    }
    // The unmutated corpus entry is the control: it must still open.
    assert!(key.open(&good, CONTEXT).is_ok());
}

proptest! {
    #[test]
    fn cursor_open_never_panics(input in any::<Vec<u8>>()) {
        let (key, _) = sealed();
        let text = String::from_utf8_lossy(&input);
        let _ = key.open(&text, CONTEXT);
    }

    #[test]
    fn cursor_open_never_panics_on_alphabet(input in proptest::collection::vec(b'a'..=b'p', 0..2048)) {
        let (key, _) = sealed();
        let text = String::from_utf8(input).unwrap();
        let _ = key.open(&text, CONTEXT);
    }
}
