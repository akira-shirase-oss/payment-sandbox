//! Explanation cursors, sealed by the server (D-056, review N3).
//!
//! A cursor's fields used to come back from the client as plain query parameters: `as_of`
//! went straight into a `::pg_snapshot` cast (a card number there became a 500 whose message
//! reached the ERROR log and the database's log) and `running` was whatever the client said.
//! Now the server issues one opaque value: the fields, then an HMAC-SHA256 tag (128 bits) over
//! them, both written with the letters `a`–`p` (one per half-byte), so a cursor has no digits
//! for the card guard to mistake and nothing a client can edit unnoticed. The fields include
//! what the cursor explains (project, epoch, currency and account: a second review check
//! found one project accepting another's cursor and its running total), and
//! [`CursorKey::open`] refuses a cursor issued for anything else. It checks shape, context,
//! tag and every field's grammar before anything reaches the database.
//!
//! The key is derived from `PS_SECRET_KEY` when it is set, so cursors outlive a restart;
//! otherwise it is random per process, and a cursor from an earlier process is refused as
//! invalid (start a new explanation).

use hmac::{Hmac, KeyInit, Mac as _};
use sandbox_db::reconcile::ExplainCursor;
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Longest cursor accepted (a snapshot's in-progress list is bounded the same way).
pub const MAX_CURSOR_CHARS: usize = 2 * (16_384 + 128) + TAG_CHARS;
const TAG_BYTES: usize = 16;
const TAG_CHARS: usize = 2 * TAG_BYTES;

/// The key cursors are sealed with.
pub struct CursorKey([u8; 32]);

impl std::fmt::Debug for CursorKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CursorKey(<redacted>)")
    }
}

/// A cursor that is not one this server issued, or is malformed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("this cursor was not issued by this server; start the explanation again")]
pub struct InvalidCursor;

/// What [`CursorKey::open`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Opened {
    /// Issued by this server, every field well formed.
    Authentic(ExplainCursor),
    /// Well formed, but the tag does not match: a forgery, or a cursor of another process
    /// or restore generation (the caller tells which, by its readable generation).
    Unverified {
        /// The restore generation the cursor names.
        generation: String,
    },
}

impl CursorKey {
    /// Derived from the server key (64 hex characters) when there is one, else random.
    pub fn new(secret_key_hex: Option<&str>) -> Self {
        if let Some(bytes) = secret_key_hex.and_then(|hex| hex::decode(hex).ok())
            && let Ok(mut mac) = <HmacSha256 as KeyInit>::new_from_slice(&bytes)
        {
            mac.update(b"payment-sandbox explain cursor v1");
            return Self(mac.finalize().into_bytes().into());
        }
        let mut key = [0_u8; 32];
        if getrandom::fill(&mut key).is_err() {
            // No OS randomness: cursors still work in this process; they are not secret
            // against a local process that can read memory anyway.
            key = *b"payment-sandbox-fallback-key-32b";
        }
        Self(key)
    }

    fn tag(&self, payload: &[u8]) -> [u8; TAG_BYTES] {
        #[expect(
            clippy::expect_used,
            reason = "invariant: HMAC accepts a key of any length"
        )]
        let mut mac =
            <HmacSha256 as KeyInit>::new_from_slice(&self.0).expect("invariant: any key length");
        mac.update(payload);
        let full = mac.finalize().into_bytes();
        let mut tag = [0_u8; TAG_BYTES];
        tag.copy_from_slice(&full[..TAG_BYTES]);
        tag
    }

    /// Seals `cursor` for `context` (what it explains; see [`context`]).
    pub fn seal(&self, cursor: &ExplainCursor, context: &str) -> String {
        let payload = format!(
            "{context}|{}|{}|{}|{}",
            cursor.generation, cursor.as_of, cursor.after_seq, cursor.running
        );
        let tag = self.tag(payload.as_bytes());
        let mut out = sandbox_domain::letters::encode(payload.as_bytes());
        out.push_str(&sandbox_domain::letters::encode(&tag));
        out
    }

    /// Opens a sealed cursor issued for `context`: shape, context, fields, then tag.
    pub fn open(&self, sealed: &str, context: &str) -> Result<Opened, InvalidCursor> {
        if sealed.len() > MAX_CURSOR_CHARS
            || sealed.len() < TAG_CHARS + 2
            || !sealed.len().is_multiple_of(2)
        {
            return Err(InvalidCursor);
        }
        let bytes = sandbox_domain::letters::decode(sealed).ok_or(InvalidCursor)?;
        let (payload, tag) = bytes.split_at(bytes.len() - TAG_BYTES);
        let text = std::str::from_utf8(payload).map_err(|_| InvalidCursor)?;
        let (sealed_for, fields) = text.split_once('|').ok_or(InvalidCursor)?;
        if sealed_for != context {
            return Err(InvalidCursor);
        }
        let cursor = parse(fields).ok_or(InvalidCursor)?;
        let expected = self.tag(payload);
        let same = expected
            .iter()
            .zip(tag)
            .fold(0_u8, |acc, (a, b)| acc | (a ^ b))
            == 0;
        if same {
            Ok(Opened::Authentic(cursor))
        } else {
            Ok(Opened::Unverified {
                generation: cursor.generation,
            })
        }
    }
}

/// What a cursor explains: one account of one project's dataset epoch, in one currency.
pub fn context(project: uuid::Uuid, epoch: i32, currency: &str, account: &str) -> String {
    format!("{project}/{epoch}/{currency}/{account}")
}

/// `generation|as_of|after_seq|running`, each in its exact grammar.
fn parse(text: &str) -> Option<ExplainCursor> {
    let mut fields = text.split('|');
    let (generation, as_of, after_seq, running) = (
        fields.next()?,
        fields.next()?,
        fields.next()?,
        fields.next()?,
    );
    if fields.next().is_some() {
        return None;
    }
    let generation_ok = !generation.is_empty()
        && generation.len() <= 64
        && generation.bytes().all(|b| b.is_ascii_digit() || b == b'.');
    let after_seq: i64 = after_seq.parse().ok()?;
    let running: i64 = running.parse().ok()?;
    (generation_ok && after_seq >= 0 && valid_snapshot(as_of)).then(|| ExplainCursor {
        generation: generation.to_owned(),
        as_of: as_of.to_owned(),
        after_seq,
        running,
    })
}

/// `xmin:xmax:xip,…` with `xmin ≤ xip < xmax`, ascending, at most 16 KiB.
fn valid_snapshot(text: &str) -> bool {
    if text.len() > 16_384 {
        return false;
    }
    let mut parts = text.split(':');
    let (Some(xmin), Some(xmax), Some(xip), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    let number = |s: &str| (!s.is_empty() && s.len() <= 20).then(|| s.parse::<u64>().ok())?;
    let (Some(xmin), Some(xmax)) = (number(xmin), number(xmax)) else {
        return false;
    };
    if xmin > xmax {
        return false;
    }
    if xip.is_empty() {
        return true;
    }
    let mut last = None;
    for id in xip.split(',') {
        let Some(id) = number(id) else {
            return false;
        };
        if id < xmin || id >= xmax || last.is_some_and(|l| id <= l) {
            return false;
        }
        last = Some(id);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    const HERE: &str = "018f2f0e-3c1a-7cc0-9e1e-3f1f6b7f9a01/1/USD/payer_held";

    fn cursor() -> ExplainCursor {
        ExplainCursor {
            generation: "7708887061324505132.1.16384".to_owned(),
            as_of: "276382:276390:276385,276386".to_owned(),
            after_seq: 42,
            running: -1_500,
        }
    }

    #[test]
    fn a_sealed_cursor_opens_and_any_edit_is_noticed() {
        let key = CursorKey::new(None);
        let sealed = key.seal(&cursor(), HERE);
        assert!(!sealed.bytes().any(|b| b.is_ascii_digit()), "{sealed}");
        assert_eq!(key.open(&sealed, HERE), Ok(Opened::Authentic(cursor())));
        // Another process's key: well formed, unverified, generation readable.
        assert_eq!(
            CursorKey::new(None).open(&sealed, HERE),
            Ok(Opened::Unverified {
                generation: cursor().generation
            })
        );
        // An edited running total.
        let mut forged = cursor();
        forged.running = 4_242_424_242_424_242;
        let forged = CursorKey::new(None).seal(&forged, HERE);
        assert!(matches!(
            key.open(&forged, HERE),
            Ok(Opened::Unverified { .. })
        ));
        // The server key makes cursors outlive a restart.
        let hex = "00".repeat(32);
        let a = CursorKey::new(Some(&hex));
        assert_eq!(
            CursorKey::new(Some(&hex)).open(&a.seal(&cursor(), HERE), HERE),
            Ok(Opened::Authentic(cursor()))
        );
    }

    #[test]
    fn a_cursor_opens_only_for_what_it_explains() {
        let key = CursorKey::new(None);
        let sealed = key.seal(&cursor(), HERE);
        let id = uuid::Uuid::from_u128(7);
        for elsewhere in [
            context(id, 1, "USD", "payer_held"),
            "018f2f0e-3c1a-7cc0-9e1e-3f1f6b7f9a01/2/USD/payer_held".to_owned(),
            "018f2f0e-3c1a-7cc0-9e1e-3f1f6b7f9a01/1/EUR/payer_held".to_owned(),
            "018f2f0e-3c1a-7cc0-9e1e-3f1f6b7f9a01/1/USD/payer_funding".to_owned(),
        ] {
            assert_eq!(
                key.open(&sealed, &elsewhere),
                Err(InvalidCursor),
                "{elsewhere}"
            );
        }
    }

    #[test]
    fn malformed_cursors_and_fields_are_refused() {
        let key = CursorKey::new(None);
        for bad in [
            "",
            "abc",
            "zz",
            &"a".repeat(MAX_CURSOR_CHARS + 2),
            "4242424242424242",
        ] {
            assert_eq!(key.open(bad, HERE), Err(InvalidCursor), "{bad:?}");
        }
        for as_of in [
            "4242424242424242",
            "5:4:",
            "1:9:9",
            "1:9:3,2",
            "1:9:x",
            "1:9",
            "1:9::",
        ] {
            let mut c = cursor();
            c.as_of = as_of.to_owned();
            assert_eq!(
                key.open(&key.seal(&c, HERE), HERE),
                Err(InvalidCursor),
                "{as_of}"
            );
        }
        assert!(valid_snapshot("10:10:"));
        assert!(valid_snapshot("10:20:11,15"));
    }
}
