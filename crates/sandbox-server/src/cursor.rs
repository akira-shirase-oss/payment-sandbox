//! Explanation cursors, sealed by the server (D-056, review N3; encrypted and leased,
//! D-105).
//!
//! A cursor's fields used to come back from the client as plain query parameters: `as_of`
//! went straight into a `::pg_snapshot` cast (a card number there became a 500 whose message
//! reached the ERROR log and the database's log) and `running` was whatever the client said.
//! Then came a signed plaintext, whose readable fields were their own leak: the generation
//! names the cluster's system identifier and the snapshot carries its global transaction
//! ids — a write-rate side channel every tenant's cursor handed to every other tenant.
//! Now the wire form is ChaCha20-Poly1305 ciphertext under a random nonce: the context a
//! cursor was issued for is the AEAD's associated data, so it does not decrypt for anything
//! else; the payload carries an `issued_at`, and [`CursorKey::open`] refuses a cursor older
//! than its lease with [`CursorError::Expired`]. Everything is written with the letters
//! `a`–`p` (one per half-byte), so a cursor has no digits for the card guard to mistake.
//!
//! The key is derived from `PS_SECRET_KEY` when it is set, so cursors outlive a restart;
//! otherwise it is random per process. There is no constant key: without a configured
//! secret or OS randomness the process refuses to start, because a predictable key would
//! make every cursor forgeable (S3).

use chacha20poly1305::aead::{Aead, Payload};
use chacha20poly1305::{ChaCha20Poly1305, KeyInit as _, Nonce};
use hmac::{Hmac, KeyInit, Mac as _};
use sandbox_db::reconcile::ExplainCursor;
use sha2::Sha256;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::secrets::SealError;

type HmacSha256 = Hmac<Sha256>;

/// Longest cursor accepted (a snapshot's in-progress list is bounded the same way).
pub const MAX_CURSOR_CHARS: usize = 2 * (16_384 + TAG_BYTES + NONCE_BYTES);
const NONCE_BYTES: usize = 12;
const TAG_BYTES: usize = 16;

/// The key cursors are sealed with.
pub struct CursorKey {
    cipher: ChaCha20Poly1305,
    /// The opaque-label key: same secret as `cipher`, different domain.
    labeler: HmacSha256,
    /// How long a sealed cursor stays good.
    ttl: Duration,
}

impl std::fmt::Debug for CursorKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CursorKey(<redacted>)")
    }
}

/// What opening a cursor can fail as.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CursorError {
    /// Not issued by this server, malformed, or issued for another context.
    #[error("this cursor was not issued by this server; start the explanation again")]
    Invalid,
    /// Older than its lease; the snapshot it described is not leased forever.
    #[error("this cursor has expired; start the explanation again")]
    Expired,
}

/// No operating-system randomness and no configured server key at startup: startup
/// refuses rather than mint a constant key that would make every cursor forgeable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error(
    "the operating system gave no randomness for the cursor key and PS_SECRET_KEY is unset; refusing to start under a constant key"
)]
pub struct NoEntropy;

impl CursorKey {
    /// Derived from the server key (64 hex characters) when there is one, else random.
    ///
    /// # Panics
    ///
    /// Refuses to start without OS randomness when `PS_SECRET_KEY` is unset: a constant
    /// cursor key would make every cursor forgeable.
    #[expect(
        clippy::expect_used,
        reason = "invariant: a sandbox without OS randomness must not start; a constant cursor key is forgeable"
    )]
    pub fn new(secret_key_hex: Option<&str>, ttl: Duration) -> Self {
        Self::with_randomness(secret_key_hex, ttl, |key| {
            getrandom::fill(key).map_err(|_| NoEntropy)
        })
        .expect("invariant: refusing to start without OS randomness")
    }

    /// `new` with the entropy source passed in, so the refusal is testable.
    fn with_randomness(
        secret_key_hex: Option<&str>,
        ttl: Duration,
        fill: impl FnOnce(&mut [u8; 32]) -> Result<(), NoEntropy>,
    ) -> Result<Self, NoEntropy> {
        let key = if let Some(bytes) = secret_key_hex.and_then(|hex| hex::decode(hex).ok())
            && let Ok(mut mac) = <HmacSha256 as KeyInit>::new_from_slice(&bytes)
        {
            mac.update(b"payment-sandbox explain cursor v2");
            mac.finalize().into_bytes().into()
        } else {
            let mut key = [0_u8; 32];
            fill(&mut key)?;
            key
        };
        #[expect(
            clippy::expect_used,
            reason = "invariant: the derived and generated keys are 32 bytes"
        )]
        let cipher = ChaCha20Poly1305::new_from_slice(&key).expect("invariant: a 32-byte AEAD key");
        #[expect(
            clippy::expect_used,
            reason = "invariant: HMAC accepts a key of any length"
        )]
        let mut labeler =
            <HmacSha256 as KeyInit>::new_from_slice(&key).expect("invariant: any key length");
        labeler.update(b"payment-sandbox explain label");
        Ok(Self {
            cipher,
            labeler,
            ttl,
        })
    }

    /// An opaque label for a server-side value the explain response names (the restore
    /// generation, the snapshot): tenants can tell two labels apart but cannot read the
    /// cluster's identifier or its transaction ids out of one.
    pub fn label(&self, value: &str) -> String {
        let mut mac = self.labeler.clone();
        mac.update(value.as_bytes());
        let out = mac.finalize().into_bytes();
        sandbox_domain::letters::encode(&out[..TAG_BYTES])
    }

    /// Seals `cursor` for `context` — the context is the AEAD's associated data, so the
    /// sealed value does not decrypt for anything else (D-056's binding, now enforced by
    /// the authentication tag itself).
    pub fn seal(&self, cursor: &ExplainCursor, context: &str) -> Result<String, SealError> {
        self.seal_at(cursor, context, unix_secs().ok_or(SealError)?)
    }

    /// `seal` with an explicit issue time — tests lease a cursor into the past.
    fn seal_at(
        &self,
        cursor: &ExplainCursor,
        context: &str,
        issued_at: u64,
    ) -> Result<String, SealError> {
        let payload = format!(
            "{}|{}|{}|{}|{issued_at}",
            cursor.generation, cursor.as_of, cursor.after_seq, cursor.running
        );
        let mut nonce = [0_u8; NONCE_BYTES];
        getrandom::fill(&mut nonce).map_err(|_| SealError)?;
        let ciphertext = self
            .cipher
            .encrypt(
                &Nonce::from(nonce),
                Payload {
                    msg: payload.as_bytes(),
                    aad: context.as_bytes(),
                },
            )
            .map_err(|_| SealError)?;
        let mut sealed = nonce.to_vec();
        sealed.extend_from_slice(&ciphertext);
        Ok(sandbox_domain::letters::encode(&sealed))
    }

    /// Opens a sealed cursor issued for `context`: shape, decode, AEAD-open, fields, then
    /// the lease — all before anything reaches the database.
    pub fn open(&self, sealed: &str, context: &str) -> Result<ExplainCursor, CursorError> {
        if sealed.len() > MAX_CURSOR_CHARS
            || sealed.len() < 2 * (NONCE_BYTES + TAG_BYTES + 1)
            || !sealed.len().is_multiple_of(2)
        {
            return Err(CursorError::Invalid);
        }
        let bytes = sandbox_domain::letters::decode(sealed).ok_or(CursorError::Invalid)?;
        let (nonce, ciphertext) = bytes.split_at(NONCE_BYTES);
        let nonce: [u8; NONCE_BYTES] = nonce.try_into().map_err(|_| CursorError::Invalid)?;
        let plain = self
            .cipher
            .decrypt(
                &Nonce::from(nonce),
                Payload {
                    msg: ciphertext,
                    aad: context.as_bytes(),
                },
            )
            .map_err(|_| CursorError::Invalid)?;
        let text = std::str::from_utf8(&plain).map_err(|_| CursorError::Invalid)?;
        let (cursor, issued_at) = parse(text).ok_or(CursorError::Invalid)?;
        // The lease, failing closed: a clock that cannot be read, or one behind the
        // issue time, means the cursor cannot be trusted as current.
        let age = unix_secs().and_then(|now| now.checked_sub(issued_at));
        if age.is_none_or(|age| age > self.ttl.as_secs()) {
            return Err(CursorError::Expired);
        }
        Ok(cursor)
    }
}

/// Seconds since the epoch; `None` on a clock that says it is before 1970.
fn unix_secs() -> Option<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

/// What a cursor explains: one account of one project's dataset epoch, in one currency.
pub fn context(project: uuid::Uuid, epoch: i32, currency: &str, account: &str) -> String {
    format!("{project}/{epoch}/{currency}/{account}")
}

/// `generation|as_of|after_seq|running|issued_at`, each in its exact grammar.
fn parse(text: &str) -> Option<(ExplainCursor, u64)> {
    let mut fields = text.split('|');
    let (generation, as_of, after_seq, running, issued_at) = (
        fields.next()?,
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
    let issued_at: u64 = issued_at.parse().ok()?;
    (generation_ok && after_seq >= 0 && valid_snapshot(as_of)).then(|| {
        (
            ExplainCursor {
                generation: generation.to_owned(),
                as_of: as_of.to_owned(),
                after_seq,
                running,
            },
            issued_at,
        )
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
    const TTL: Duration = Duration::from_secs(3_600);

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
        let key = CursorKey::new(None, TTL);
        let sealed = key.seal(&cursor(), HERE).unwrap();
        assert!(!sealed.bytes().any(|b| b.is_ascii_digit()), "{sealed}");
        assert_eq!(key.open(&sealed, HERE), Ok(cursor()));
        // Another process's key: the ciphertext does not open and nothing is readable.
        assert_eq!(
            CursorKey::new(None, TTL).open(&sealed, HERE),
            Err(CursorError::Invalid)
        );
        // An edited letter anywhere — nonce head, ciphertext body, tag tail — is an
        // authentication failure, the property the seal exists for.
        for i in [0, sealed.len() / 2, sealed.len() - 1] {
            let mut forged = sealed.clone();
            let c = forged.as_bytes()[i];
            forged.replace_range(i..=i, if c == b'a' { "b" } else { "a" });
            assert_eq!(
                key.open(&forged, HERE),
                Err(CursorError::Invalid),
                "edit at {i}"
            );
        }
        // The server key makes cursors outlive a restart.
        let hex = "00".repeat(32);
        let a = CursorKey::new(Some(&hex), TTL);
        assert_eq!(
            CursorKey::new(Some(&hex), TTL).open(&a.seal(&cursor(), HERE).unwrap(), HERE),
            Ok(cursor())
        );
    }

    #[test]
    fn the_sealed_form_reveals_nothing() {
        let key = CursorKey::new(None, TTL);
        let sealed = key.seal(&cursor(), HERE).unwrap();
        let raw = sandbox_domain::letters::decode(&sealed).expect("letters");
        // The system identifier, the snapshot's global xids and the explain context must
        // not be readable: they name the cluster and leak its write rate to any tenant
        // holding a cursor.
        for needle in [
            "7708887061324505132".as_bytes(),
            "276382".as_bytes(),
            HERE.as_bytes(),
        ] {
            assert!(
                !raw.windows(needle.len()).any(|w| w == needle),
                "the sealed form carries {:?} in the clear",
                String::from_utf8_lossy(needle)
            );
        }
        // And labels hide the same values while staying comparable: two pages of one
        // explanation label alike, another value labels differently.
        let labelled = key.label(&cursor().generation);
        assert_eq!(labelled, key.label(&cursor().generation));
        assert_ne!(labelled, key.label("other"));
        assert!(!labelled.bytes().any(|b| b.is_ascii_digit()), "{labelled}");
    }

    #[test]
    fn expired_cursors_are_refused_by_name() {
        let key = CursorKey::new(None, TTL);
        let now = unix_secs().expect("clock");
        let old = key
            .seal_at(&cursor(), HERE, now - TTL.as_secs() - 1)
            .unwrap();
        assert_eq!(key.open(&old, HERE), Err(CursorError::Expired));
        // A future-dated cursor fails closed too: a clock that moved backwards cannot
        // certify the cursor is still young.
        let future = key.seal_at(&cursor(), HERE, now + 60).unwrap();
        assert_eq!(key.open(&future, HERE), Err(CursorError::Expired));
        // Inside the lease, the same fields open.
        let fresh = key.seal_at(&cursor(), HERE, now).unwrap();
        assert_eq!(key.open(&fresh, HERE), Ok(cursor()));
    }

    #[test]
    fn without_os_randomness_there_is_no_key() {
        assert!(
            matches!(
                CursorKey::with_randomness(None, TTL, |_| Err(NoEntropy)),
                Err(NoEntropy)
            ),
            "a constant-key path must not exist"
        );
        // A configured server key needs no OS randomness.
        assert!(
            CursorKey::with_randomness(Some(&"00".repeat(32)), TTL, |_| Err(NoEntropy)).is_ok()
        );
        // And working randomness feeds the key.
        assert!(
            CursorKey::with_randomness(None, TTL, |key| {
                key.fill(7);
                Ok(())
            })
            .is_ok()
        );
    }

    #[test]
    fn a_cursor_opens_only_for_what_it_explains() {
        let key = CursorKey::new(None, TTL);
        let sealed = key.seal(&cursor(), HERE).unwrap();
        let id = uuid::Uuid::from_u128(7);
        for elsewhere in [
            context(id, 1, "USD", "payer_held"),
            "018f2f0e-3c1a-7cc0-9e1e-3f1f6b7f9a01/2/USD/payer_held".to_owned(),
            "018f2f0e-3c1a-7cc0-9e1e-3f1f6b7f9a01/1/EUR/payer_held".to_owned(),
            "018f2f0e-3c1a-7cc0-9e1e-3f1f6b7f9a01/1/USD/payer_funding".to_owned(),
        ] {
            assert_eq!(
                key.open(&sealed, &elsewhere),
                Err(CursorError::Invalid),
                "{elsewhere}"
            );
        }
    }

    #[test]
    fn malformed_cursors_and_fields_are_refused() {
        let key = CursorKey::new(None, TTL);
        for bad in [
            "",
            "abc",
            "zz",
            &"a".repeat(MAX_CURSOR_CHARS + 2),
            "4242424242424242",
        ] {
            assert_eq!(key.open(bad, HERE), Err(CursorError::Invalid), "{bad:?}");
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
                key.open(&key.seal(&c, HERE).unwrap(), HERE),
                Err(CursorError::Invalid),
                "{as_of}"
            );
        }
        assert!(valid_snapshot("10:10:"));
        assert!(valid_snapshot("10:20:11,15"));
    }
}
