//! Checkout sessions in pure form (D-060): the rules that keep a payer's page honest and
//! keep one session from ever charging twice.
//!
//! A merchant fixes the amount, currency and description; the payer only picks a scenario
//! (D-019: there is no card field). Every press of "Pay" is an *attempt*, recorded before
//! anything reaches the network, whose authorize and capture keys are derived from the
//! session and the attempt number, so the operation identity (D-015) exists first. A new
//! attempt is admitted only while nothing of the session can still turn into money: no
//! attempt yet, or the latest one ended declined or never executed.
//!
//! What the payer sees is derived from the attempt's recorded operations and the payment's
//! journal-backed sums, never from a guess: `paid` only when the whole amount was captured,
//! `uncertain` while the network's answer is lost.

use serde::Serialize;
use uuid::Uuid;

use crate::OperationState;
use crate::letters;

/// Most attempts one session allows.
pub const MAX_ATTEMPTS: i32 = 10;
/// Most capture tries of one attempt (a try that never executed is retried under a new key).
pub const MAX_CAPTURE_TRIES: i32 = 3;
/// Most origins that may embed one session.
pub const MAX_EMBED_ORIGINS: usize = 4;
/// Prefix of a session's client secret.
pub const SECRET_PREFIX: &str = "cs_sim_";
/// Length of the letters after the prefix (32 random bytes).
pub const SECRET_LETTERS: usize = 64;
/// Shortest and longest session lifetime a merchant may ask for, and the default.
pub const MIN_LIFETIME_SECONDS: i64 = 60;
/// Longest lifetime.
pub const MAX_LIFETIME_SECONDS: i64 = 86_400;
/// Default lifetime.
pub const DEFAULT_LIFETIME_SECONDS: i64 = 3_600;

/// When the money is captured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureMethod {
    /// The checkout captures the whole amount once the authorization succeeds.
    Automatic,
    /// The checkout only authorizes; the merchant captures through the API.
    Manual,
}

impl CaptureMethod {
    /// Stable code.
    pub const fn code(self) -> &'static str {
        match self {
            Self::Automatic => "automatic",
            Self::Manual => "manual",
        }
    }

    /// Parses a code.
    pub fn from_code(code: &str) -> Option<Self> {
        [Self::Automatic, Self::Manual]
            .into_iter()
            .find(|m| m.code() == code)
    }
}

/// Where a session stands, as the payer is told.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckoutState {
    /// Nothing submitted yet.
    Open,
    /// Submitted; the network has not answered yet.
    Processing,
    /// The network's answer was lost; it is being confirmed by readback. Not paid, not
    /// failed: unknown until the network's record is read.
    Uncertain,
    /// The network declined; nothing was charged.
    Declined,
    /// Readback proved the network never executed it; nothing was charged.
    NotExecuted,
    /// A hold was placed and not (wholly) captured.
    Authorized,
    /// The whole amount was captured.
    Paid,
}

impl CheckoutState {
    /// Stable code.
    pub const fn code(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Processing => "processing",
            Self::Uncertain => "uncertain",
            Self::Declined => "declined",
            Self::NotExecuted => "not_executed",
            Self::Authorized => "authorized",
            Self::Paid => "paid",
        }
    }
}

/// What the payer's page should do next.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NextAction {
    /// Offer the scenario picker: a new attempt is admitted.
    Pay,
    /// Poll: the server (or its reconciler) is finishing the attempt.
    Wait,
    /// Ask the server to continue the attempt (`POST …/continue`): its authorization
    /// succeeded and its capture has not run, or a capture try never executed.
    Continue,
}

/// What is recorded about a session's latest attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttemptFacts {
    /// The authorize operation's state; `None` before its row exists.
    pub authorize: Option<OperationState>,
    /// The latest capture operation's state, if any capture was tried.
    pub capture: Option<OperationState>,
    /// Capture tries so far.
    pub capture_tries: i32,
    /// Captured minor units (the payment's journal-backed sum).
    pub captured_minor: i64,
}

/// The payer-facing state and next step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Derived {
    /// State.
    pub state: CheckoutState,
    /// Next step, if any.
    pub next_action: Option<NextAction>,
}

/// True when a new attempt may start after `latest` (the latest attempt's authorize state;
/// `None` = no attempt). An attempt without its operation row yet is still open.
pub fn admits_new_attempt(latest: Option<Option<OperationState>>) -> bool {
    match latest {
        None => true,
        Some(state) => matches!(
            state,
            Some(OperationState::Declined | OperationState::NotExecuted)
        ),
    }
}

/// Derives the payer-facing state. `can_start_new` says whether a new attempt would be
/// admitted by count and expiry (the state alone decides the rest).
pub fn derive(
    method: CaptureMethod,
    amount_minor: i64,
    attempt: Option<&AttemptFacts>,
    can_start_new: bool,
) -> Derived {
    let pay = can_start_new.then_some(NextAction::Pay);
    let Some(facts) = attempt else {
        return Derived {
            state: CheckoutState::Open,
            next_action: pay,
        };
    };
    let (state, next_action) = match facts.authorize {
        None | Some(OperationState::Pending) => (CheckoutState::Processing, Some(NextAction::Wait)),
        Some(OperationState::Uncertain) => (CheckoutState::Uncertain, Some(NextAction::Wait)),
        Some(OperationState::Declined) => (CheckoutState::Declined, pay),
        Some(OperationState::NotExecuted) => (CheckoutState::NotExecuted, pay),
        // Paid only from journal-backed captured money, whoever captured it.
        Some(OperationState::Succeeded) if facts.captured_minor >= amount_minor => {
            (CheckoutState::Paid, None)
        }
        Some(OperationState::Succeeded) => match (method, facts.capture) {
            (CaptureMethod::Automatic, None) => {
                (CheckoutState::Authorized, Some(NextAction::Continue))
            }
            (CaptureMethod::Automatic, Some(OperationState::Pending)) => {
                (CheckoutState::Processing, Some(NextAction::Wait))
            }
            (CaptureMethod::Automatic, Some(OperationState::Uncertain)) => {
                (CheckoutState::Uncertain, Some(NextAction::Wait))
            }
            (CaptureMethod::Automatic, Some(OperationState::NotExecuted))
                if facts.capture_tries < MAX_CAPTURE_TRIES =>
            {
                (CheckoutState::Authorized, Some(NextAction::Continue))
            }
            // Manual capture stops at the hold. Otherwise declined, exhausted, or succeeded
            // for less than the whole amount: the hold stands and the merchant decides
            // (capture or void through the API).
            (CaptureMethod::Manual, _) | (CaptureMethod::Automatic, Some(_)) => {
                (CheckoutState::Authorized, None)
            }
        },
    };
    Derived { state, next_action }
}

/// The authorize `Idempotency-Key` of a session's attempt: letters only, then the number, so
/// it can never look like card data and never collides with another session's.
pub fn attempt_key(session: Uuid, number: i32) -> String {
    format!("checkout.{}.{number}", letters::encode(session.as_bytes()))
}

/// The capture key of an attempt's `try_number`-th capture try.
pub fn capture_key(session: Uuid, number: i32, try_number: i32) -> String {
    format!(
        "checkout.{}.{number}.capture.{try_number}",
        letters::encode(session.as_bytes())
    )
}

/// A client secret from 32 random bytes: `cs_sim_` and 64 letters.
pub fn secret_from_bytes(random: [u8; 32]) -> String {
    format!("{SECRET_PREFIX}{}", letters::encode(&random))
}

/// True when `presented` has the exact shape of a client secret.
pub fn is_secret_shaped(presented: &str) -> bool {
    presented.strip_prefix(SECRET_PREFIX).is_some_and(|rest| {
        rest.len() == SECRET_LETTERS && rest.bytes().all(|b| (b'a'..=b'p').contains(&b))
    })
}

/// Why a merchant-supplied checkout field was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum FieldError {
    /// The description is too long or carries control, bidirectional or invisible
    /// characters.
    #[error(
        "description must be at most 200 characters without control, bidirectional or invisible characters"
    )]
    Description,
    /// An embed origin is not an exact loopback origin.
    #[error(
        "embed_origins are exact origins like http://127.0.0.1:5173 (loopback only in the local/CI mode, D-023)"
    )]
    EmbedOrigin,
    /// Too many embed origins.
    #[error("at most 4 embed_origins")]
    TooManyOrigins,
    /// The return URL is not an http(s) loopback URL.
    #[error(
        "return_url must be an http(s) URL on a loopback host (the local/CI mode, D-023), at most 2048 characters"
    )]
    ReturnUrl,
    /// The lifetime is out of range.
    #[error("expires_in must be between 60 and 86400 seconds")]
    Lifetime,
}

/// Bidirectional controls and invisible characters: a payer must see exactly what the
/// merchant wrote, and the amount next to it.
fn is_deceptive(c: char) -> bool {
    matches!(
        c,
        '\u{202A}'..='\u{202E}'
            | '\u{2066}'..='\u{2069}'
            | '\u{200B}'..='\u{200F}'
            | '\u{2060}'..='\u{2064}'
            | '\u{061C}'
            | '\u{FEFF}'
            | '\u{180E}'
            | '\u{00AD}'
    )
}

/// Validates a description shown to payers.
pub fn validate_description(text: &str) -> Result<(), FieldError> {
    if text.chars().count() > 200 || text.chars().any(|c| c.is_control() || is_deceptive(c)) {
        return Err(FieldError::Description);
    }
    Ok(())
}

/// The host part of a loopback authority, lowercased, if it is one.
fn loopback_host(host: &str) -> Option<&'static str> {
    match host.to_ascii_lowercase().as_str() {
        "127.0.0.1" => Some("127.0.0.1"),
        "localhost" => Some("localhost"),
        "[::1]" => Some("[::1]"),
        _ => None,
    }
}

/// Splits `scheme://authority...` for http(s) URLs of visible ASCII.
fn split_url(url: &str) -> Option<(&'static str, &str, &str)> {
    if url.is_empty()
        || url.len() > 2_048
        || !url.bytes().all(|b| (0x21..=0x7e).contains(&b))
        || url.contains('\\')
    {
        return None;
    }
    let (scheme, rest) = match url.strip_prefix("http://") {
        Some(rest) => ("http", rest),
        None => ("https", url.strip_prefix("https://")?),
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(end);
    (!authority.contains('@')).then_some((scheme, authority, tail))
}

/// The canonical form of an embed origin: `http(s)://<loopback host>:<port>`, nothing else.
pub fn canonical_origin(origin: &str) -> Result<String, FieldError> {
    let (scheme, authority, tail) = split_url(origin).ok_or(FieldError::EmbedOrigin)?;
    if !tail.is_empty() {
        return Err(FieldError::EmbedOrigin);
    }
    let (host, port) = authority.rsplit_once(':').ok_or(FieldError::EmbedOrigin)?;
    let host = loopback_host(host).ok_or(FieldError::EmbedOrigin)?;
    let port: u16 = port.parse().map_err(|_| FieldError::EmbedOrigin)?;
    if port == 0 {
        return Err(FieldError::EmbedOrigin);
    }
    Ok(format!("{scheme}://{host}:{port}"))
}

/// Validates and canonicalizes a session's embed origins.
pub fn embed_origins(origins: &[String]) -> Result<Vec<String>, FieldError> {
    if origins.len() > MAX_EMBED_ORIGINS {
        return Err(FieldError::TooManyOrigins);
    }
    let mut out = Vec::with_capacity(origins.len());
    for origin in origins {
        let canonical = canonical_origin(origin)?;
        if !out.contains(&canonical) {
            out.push(canonical);
        }
    }
    Ok(out)
}

/// Validates a return URL: http(s), a loopback host, an optional port, no credentials.
pub fn validate_return_url(url: &str) -> Result<(), FieldError> {
    let (_, authority, tail) = split_url(url).ok_or(FieldError::ReturnUrl)?;
    if tail.starts_with('#') {
        return Err(FieldError::ReturnUrl);
    }
    let host = match authority.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() && !port.contains(']') => {
            let port: u16 = port.parse().map_err(|_| FieldError::ReturnUrl)?;
            if port == 0 {
                return Err(FieldError::ReturnUrl);
            }
            host
        }
        _ => authority,
    };
    loopback_host(host).map(|_| ()).ok_or(FieldError::ReturnUrl)
}

/// Validates a requested lifetime.
pub fn validate_lifetime(seconds: i64) -> Result<i64, FieldError> {
    (MIN_LIFETIME_SECONDS..=MAX_LIFETIME_SECONDS)
        .contains(&seconds)
        .then_some(seconds)
        .ok_or(FieldError::Lifetime)
}

#[cfg(test)]
mod tests {
    use super::*;
    use OperationState as S;

    fn facts(authorize: Option<S>, capture: Option<S>, captured: i64) -> AttemptFacts {
        AttemptFacts {
            authorize,
            capture,
            capture_tries: i32::from(capture.is_some()),
            captured_minor: captured,
        }
    }

    #[test]
    fn paid_only_when_the_whole_amount_was_captured() {
        let auto = CaptureMethod::Automatic;
        let d = |f: AttemptFacts| derive(auto, 1_000, Some(&f), true);
        assert_eq!(
            d(facts(Some(S::Succeeded), Some(S::Succeeded), 1_000)).state,
            CheckoutState::Paid
        );
        // Every other combination is something else: an uncertain capture is never "paid".
        for (authorize, capture, captured) in [
            (None, None, 0),
            (Some(S::Pending), None, 0),
            (Some(S::Uncertain), None, 0),
            (Some(S::Declined), None, 0),
            (Some(S::NotExecuted), None, 0),
            (Some(S::Succeeded), None, 0),
            (Some(S::Succeeded), Some(S::Pending), 0),
            (Some(S::Succeeded), Some(S::Uncertain), 0),
            (Some(S::Succeeded), Some(S::Declined), 0),
            (Some(S::Succeeded), Some(S::NotExecuted), 0),
            (Some(S::Succeeded), Some(S::Succeeded), 999),
        ] {
            let derived = d(facts(authorize, capture, captured));
            assert_ne!(
                derived.state,
                CheckoutState::Paid,
                "{authorize:?} {capture:?} {captured}"
            );
        }
        assert_eq!(
            d(facts(Some(S::Succeeded), Some(S::Uncertain), 0)),
            Derived {
                state: CheckoutState::Uncertain,
                next_action: Some(NextAction::Wait)
            }
        );
    }

    #[test]
    fn each_authorization_state_has_its_own_payer_state_and_next_step() {
        let auto = CaptureMethod::Automatic;
        let cases = [
            (None, CheckoutState::Processing, Some(NextAction::Wait)),
            (
                Some(S::Pending),
                CheckoutState::Processing,
                Some(NextAction::Wait),
            ),
            (
                Some(S::Uncertain),
                CheckoutState::Uncertain,
                Some(NextAction::Wait),
            ),
            (
                Some(S::Declined),
                CheckoutState::Declined,
                Some(NextAction::Pay),
            ),
            (
                Some(S::NotExecuted),
                CheckoutState::NotExecuted,
                Some(NextAction::Pay),
            ),
            (
                Some(S::Succeeded),
                CheckoutState::Authorized,
                Some(NextAction::Continue),
            ),
        ];
        for (authorize, state, next) in cases {
            let derived = derive(auto, 500, Some(&facts(authorize, None, 0)), true);
            assert_eq!(
                derived,
                Derived {
                    state,
                    next_action: next
                },
                "{authorize:?}"
            );
        }
        // No new attempt (exhausted or expired): a decline offers nothing.
        assert_eq!(
            derive(auto, 500, Some(&facts(Some(S::Declined), None, 0)), false).next_action,
            None
        );
        assert_eq!(
            derive(auto, 500, None, true),
            Derived {
                state: CheckoutState::Open,
                next_action: Some(NextAction::Pay)
            }
        );
        // Manual capture stops at the hold.
        assert_eq!(
            derive(
                CaptureMethod::Manual,
                500,
                Some(&facts(Some(S::Succeeded), None, 0)),
                true
            ),
            Derived {
                state: CheckoutState::Authorized,
                next_action: None
            }
        );
    }

    #[test]
    fn a_capture_that_never_executed_is_retried_a_bounded_number_of_times() {
        let mut f = facts(Some(S::Succeeded), Some(S::NotExecuted), 0);
        f.capture_tries = MAX_CAPTURE_TRIES - 1;
        assert_eq!(
            derive(CaptureMethod::Automatic, 5, Some(&f), true).next_action,
            Some(NextAction::Continue)
        );
        f.capture_tries = MAX_CAPTURE_TRIES;
        assert_eq!(
            derive(CaptureMethod::Automatic, 5, Some(&f), true),
            Derived {
                state: CheckoutState::Authorized,
                next_action: None
            }
        );
    }

    #[test]
    fn a_new_attempt_waits_for_the_last_to_end_without_money() {
        assert!(admits_new_attempt(None));
        assert!(admits_new_attempt(Some(Some(S::Declined))));
        assert!(admits_new_attempt(Some(Some(S::NotExecuted))));
        for open in [
            None,
            Some(S::Pending),
            Some(S::Uncertain),
            Some(S::Succeeded),
        ] {
            assert!(!admits_new_attempt(Some(open)), "{open:?}");
        }
    }

    #[test]
    fn derived_keys_and_secrets_have_no_digits_to_mistake_for_a_card() {
        let session = Uuid::from_u128(0x4242_4242_4242_4242_4242_4242_4242_4242);
        let key = attempt_key(session, 3);
        assert!(key.starts_with("checkout.") && key.ends_with(".3"), "{key}");
        assert!(!crate::card_guard::contains_pan_like(&key));
        assert!(!crate::card_guard::contains_pan_like(&capture_key(
            session, 10, 3
        )));
        assert_ne!(attempt_key(session, 1), attempt_key(Uuid::nil(), 1));
        let secret = secret_from_bytes([0x42; 32]);
        assert!(is_secret_shaped(&secret), "{secret}");
        assert!(!secret.bytes().any(|b| b.is_ascii_digit()));
        for bad in [
            "cs_sim_",
            "sk_sim_aaaa",
            &secret[..secret.len() - 1],
            &format!("{secret}a"),
            &secret.replace('c', "q"),
        ] {
            assert!(!is_secret_shaped(bad), "{bad}");
        }
    }

    #[test]
    fn descriptions_refuse_what_would_deceive_a_payer() {
        assert!(validate_description("Order 1 of the dev store").is_ok());
        assert!(validate_description(&"é".repeat(200)).is_ok());
        for bad in [
            "a\u{202E}gnp.exe".to_owned(),
            "pay\u{200B}me".to_owned(),
            "line\nbreak".to_owned(),
            "x".repeat(201),
        ] {
            assert_eq!(
                validate_description(&bad),
                Err(FieldError::Description),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn embed_origins_are_exact_loopback_origins() {
        assert_eq!(
            embed_origins(&[
                "http://LOCALHOST:5173".to_owned(),
                "https://127.0.0.1:8443".to_owned(),
                "http://localhost:5173".to_owned()
            ]),
            Ok(vec![
                "http://localhost:5173".to_owned(),
                "https://127.0.0.1:8443".to_owned()
            ])
        );
        for bad in [
            "http://example.com:80",
            "http://127.0.0.1",
            "http://127.0.0.1:5173/",
            "http://127.0.0.1:5173/path",
            "http://user@127.0.0.1:5173",
            "javascript:alert(1)",
            "http://127.0.0.1:0",
            "http://10.0.0.1:80",
            "*",
            "'none'",
            "http://127.0.0.1:5173;script-src",
        ] {
            assert_eq!(canonical_origin(bad), Err(FieldError::EmbedOrigin), "{bad}");
        }
        let five: Vec<String> = (1..=5).map(|p| format!("http://127.0.0.1:{p}")).collect();
        assert_eq!(embed_origins(&five), Err(FieldError::TooManyOrigins));
    }

    #[test]
    fn return_urls_are_http_on_loopback() {
        for ok in [
            "http://127.0.0.1:56464/thanks?order=a",
            "https://localhost/done",
            "http://[::1]:8080/",
            "http://localhost",
        ] {
            assert_eq!(validate_return_url(ok), Ok(()), "{ok}");
        }
        for bad in [
            "javascript:alert(1)",
            "JAVASCRIPT:alert(1)",
            "data:text/html,x",
            "http://evil.example/",
            "http://127.0.0.1@evil.example/",
            "http://localhost:0/",
            "//localhost/x",
            "http://localhost/ x",
            "http://localhost#frag",
        ] {
            assert_eq!(
                validate_return_url(bad),
                Err(FieldError::ReturnUrl),
                "{bad}"
            );
        }
        assert_eq!(validate_lifetime(59), Err(FieldError::Lifetime));
        assert_eq!(validate_lifetime(3_600), Ok(3_600));
    }
}
