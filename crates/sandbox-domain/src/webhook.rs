//! Webhooks: thin event payloads (D-025), signatures (D-020), the retry schedule (D-032)
//! and the egress policy (D-023). Pure: the server signs and delivers, the development
//! store verifies, both with this code.
//!
//! Signature: HMAC-SHA256, keyed by the whole `whsec_sim_…` secret, over
//! `"{event_id}.{unix_seconds}.{body}"`, sent as
//! `Sandbox-Signature: t=<unix_seconds>,v1=<hex>[,v1=<hex>]` with the event id in
//! `Sandbox-Event-Id`. During a rotation both secrets sign. A verifier accepts a timestamp
//! within 300 s of its own clock (the replay window) and any `v1` that matches one of its
//! secrets.

use std::net::IpAddr;
use std::time::Duration;

use hmac::{KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use uuid::Uuid;

/// Header carrying the signature.
pub const SIGNATURE_HEADER: &str = "sandbox-signature";
/// Header carrying the event id.
pub const EVENT_ID_HEADER: &str = "sandbox-event-id";
/// Replay window, seconds either side of the verifier's clock.
pub const TOLERANCE_SECS: i64 = 300;
/// Every signing secret starts with this.
pub const SECRET_PREFIX: &str = "whsec_sim_";
/// Attempts before a delivery is `dead` (D-032).
pub const MAX_ATTEMPTS: i32 = 12;
/// Deadline of one delivery attempt.
pub const ATTEMPT_DEADLINE: Duration = Duration::from_secs(5);
/// Largest receiver response read.
pub const MAX_RESPONSE_BYTES: usize = 64 * 1024;

type HmacSha256 = hmac::Hmac<Sha256>;

/// A thin event: what happened to which payment, in which order. Consumers re-read the
/// payment from the API (D-025), so duplicates and reordering converge on current state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventPayload {
    /// Event id (the deduplication key).
    pub id: Uuid,
    /// `{operation kind}.{final state}`, e.g. `capture.succeeded`.
    #[serde(rename = "type")]
    pub kind: String,
    /// The project.
    pub project_id: Uuid,
    /// The payment.
    pub payment_id: Uuid,
    /// The operation that resolved.
    pub operation_id: Uuid,
    /// Per-payment order, starting at 1.
    pub sequence: i64,
    /// Unix seconds (database clock) when the event was recorded.
    pub created_at: i64,
    /// Always true.
    pub simulated: bool,
}

/// A new signing secret from 32 random bytes (the caller supplies the randomness).
pub fn secret_from_bytes(random: [u8; 32]) -> String {
    format!("{SECRET_PREFIX}{}", hex::encode(random))
}

/// True for a well-formed signing secret.
pub fn is_secret(secret: &str) -> bool {
    secret.strip_prefix(SECRET_PREFIX).is_some_and(|h| {
        h.len() == 64
            && h.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

fn mac(secret: &str, event_id: &str, timestamp: i64, body: &[u8]) -> HmacSha256 {
    // HMAC accepts keys of any length; this cannot fail.
    let mut mac = <HmacSha256 as KeyInit>::new_from_slice(secret.as_bytes())
        .unwrap_or_else(|_| unreachable!("HMAC takes keys of any length"));
    mac.update(event_id.as_bytes());
    mac.update(b".");
    mac.update(timestamp.to_string().as_bytes());
    mac.update(b".");
    mac.update(body);
    mac
}

/// The hex `v1` signature of one secret.
pub fn sign(secret: &str, event_id: &str, timestamp: i64, body: &[u8]) -> String {
    hex::encode(
        mac(secret, event_id, timestamp, body)
            .finalize()
            .into_bytes(),
    )
}

/// The full header value, signed by every secret given (two during a rotation).
pub fn signature_header(secrets: &[&str], event_id: &str, timestamp: i64, body: &[u8]) -> String {
    let mut header = format!("t={timestamp}");
    for secret in secrets {
        header.push_str(",v1=");
        header.push_str(&sign(secret, event_id, timestamp, body));
    }
    header
}

/// Why a delivery was not accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum VerifyError {
    /// The header is missing parts or malformed.
    #[error("malformed signature header")]
    Malformed,
    /// The timestamp is outside the replay window.
    #[error("signature timestamp outside the replay window")]
    Stale,
    /// No signature matches any secret.
    #[error("no signature matches")]
    Mismatch,
}

/// Verifies a delivery against the receiver's secrets at its clock `now` (Unix seconds).
pub fn verify(
    secrets: &[&str],
    header: &str,
    event_id: &str,
    body: &[u8],
    now: i64,
) -> Result<(), VerifyError> {
    let mut timestamp = None;
    let mut signatures = Vec::new();
    for part in header.split(',') {
        match part.split_once('=') {
            Some(("t", v)) => timestamp = v.parse::<i64>().ok(),
            Some(("v1", v)) => signatures.push(hex::decode(v).map_err(|_| VerifyError::Malformed)?),
            _ => return Err(VerifyError::Malformed),
        }
    }
    let timestamp = timestamp.ok_or(VerifyError::Malformed)?;
    if signatures.is_empty() || signatures.len() > 4 {
        return Err(VerifyError::Malformed);
    }
    if now.abs_diff(timestamp) > TOLERANCE_SECS.unsigned_abs() {
        return Err(VerifyError::Stale);
    }
    for secret in secrets {
        for signature in &signatures {
            // `verify_slice` compares in constant time.
            if mac(secret, event_id, timestamp, body)
                .verify_slice(signature)
                .is_ok()
            {
                return Ok(());
            }
        }
    }
    Err(VerifyError::Mismatch)
}

/// The backoff ceiling before attempt `attempt + 1` (D-032): 1 s, 2 s, 4 s … capped at
/// 10 minutes. The deliverer waits a uniformly random time up to it (full jitter).
pub fn retry_ceiling(attempt: i32) -> Duration {
    let exponent = u32::try_from(attempt.clamp(1, 30).saturating_sub(1)).unwrap_or(0);
    Duration::from_secs(1u64.checked_shl(exponent).unwrap_or(u64::MAX).min(600))
}

/// A webhook target that passed the URL rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// Host as written (`127.0.0.1`, `localhost`, `[::1]`).
    pub host: String,
    /// Port.
    pub port: u16,
    /// Path and query, starting with `/`.
    pub path: String,
}

/// Why a webhook URL was refused (D-023).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum EgressError {
    /// Not `http://host:port/...`.
    #[error("webhook URLs look like http://127.0.0.1:<port>/<path>")]
    Malformed,
    /// Credentials, fragments or anything unusual in the URL.
    #[error("webhook URLs may not carry credentials or fragments")]
    Unusual,
    /// The host is not loopback (the local/CI mode delivers to this machine only).
    #[error("in the local/CI mode webhooks may only target loopback addresses")]
    NotLoopback,
}

/// Parses a webhook URL: plain `http`, an explicit port, a loopback host name or literal.
pub fn parse_target(url: &str) -> Result<Target, EgressError> {
    if url.len() > 2_048 || url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(EgressError::Malformed);
    }
    let rest = url.strip_prefix("http://").ok_or(EgressError::Malformed)?;
    if rest.contains('#') || rest.contains('\\') {
        return Err(EgressError::Unusual);
    }
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    if authority.contains('@') {
        return Err(EgressError::Unusual);
    }
    let (host, port) = authority.rsplit_once(':').ok_or(EgressError::Malformed)?;
    let port: u16 = port.parse().map_err(|_| EgressError::Malformed)?;
    if port == 0 || host.is_empty() {
        return Err(EgressError::Malformed);
    }
    let loopback_name = host.eq_ignore_ascii_case("localhost");
    let literal = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host)
        .parse::<IpAddr>()
        .ok();
    match literal {
        Some(ip) if !ip_allowed(ip) => return Err(EgressError::NotLoopback),
        None if !loopback_name => return Err(EgressError::NotLoopback),
        _ => {}
    }
    Ok(Target {
        host: host.to_ascii_lowercase(),
        port,
        path: path.to_owned(),
    })
}

/// The egress rule for a resolved address: loopback only, IPv4-mapped IPv6 included.
pub fn ip_allowed(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback(),
        IpAddr::V6(v6) => {
            v6.is_loopback() || v6.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str =
        "whsec_sim_0000000000000000000000000000000000000000000000000000000000000000";
    const OTHER: &str =
        "whsec_sim_1111111111111111111111111111111111111111111111111111111111111111";

    #[test]
    fn a_signed_delivery_verifies_and_any_change_breaks_it() {
        let header = signature_header(&[SECRET], "evt_1", 1_000, b"{\"a\":1}");
        assert_eq!(
            verify(&[SECRET], &header, "evt_1", b"{\"a\":1}", 1_000),
            Ok(())
        );
        assert_eq!(
            verify(&[SECRET], &header, "evt_1", b"{\"a\":2}", 1_000),
            Err(VerifyError::Mismatch)
        );
        assert_eq!(
            verify(&[SECRET], &header, "evt_2", b"{\"a\":1}", 1_000),
            Err(VerifyError::Mismatch)
        );
        assert_eq!(
            verify(&[OTHER], &header, "evt_1", b"{\"a\":1}", 1_000),
            Err(VerifyError::Mismatch)
        );
        let moved = header.replace("t=1000", "t=1001");
        assert_eq!(
            verify(&[SECRET], &moved, "evt_1", b"{\"a\":1}", 1_000),
            Err(VerifyError::Mismatch)
        );
    }

    #[test]
    fn the_replay_window_is_three_hundred_seconds_either_way() {
        let header = signature_header(&[SECRET], "e", 10_000, b"x");
        assert!(verify(&[SECRET], &header, "e", b"x", 10_300).is_ok());
        assert!(verify(&[SECRET], &header, "e", b"x", 9_700).is_ok());
        assert_eq!(
            verify(&[SECRET], &header, "e", b"x", 10_301),
            Err(VerifyError::Stale)
        );
        assert_eq!(
            verify(&[SECRET], &header, "e", b"x", 9_699),
            Err(VerifyError::Stale)
        );
    }

    #[test]
    fn during_a_rotation_either_secret_verifies() {
        let header = signature_header(&[OTHER, SECRET], "e", 5, b"x");
        assert_eq!(header.matches("v1=").count(), 2);
        assert!(verify(&[SECRET], &header, "e", b"x", 5).is_ok());
        assert!(verify(&[OTHER], &header, "e", b"x", 5).is_ok());
    }

    #[test]
    fn malformed_headers_are_refused() {
        for header in [
            "",
            "t=5",
            "v1=00",
            "t=x,v1=00",
            "t=5,v1=zz",
            "t=5,v2=00",
            "t=5,v1=00,v1=00,v1=00,v1=00,v1=00",
        ] {
            assert_eq!(
                verify(&[SECRET], header, "e", b"x", 5),
                Err(VerifyError::Malformed),
                "{header:?}"
            );
        }
    }

    #[test]
    fn retries_back_off_to_ten_minutes() {
        let ceilings: Vec<u64> = (1..=12).map(|a| retry_ceiling(a).as_secs()).collect();
        assert_eq!(ceilings, [1, 2, 4, 8, 16, 32, 64, 128, 256, 512, 600, 600]);
    }

    #[test]
    fn only_loopback_targets_are_allowed() {
        for ok in [
            "http://127.0.0.1:56082/webhooks",
            "http://localhost:9/x?y=1",
            "http://[::1]:8080/",
            "http://127.8.9.10:80",
        ] {
            assert!(parse_target(ok).is_ok(), "{ok}");
        }
        let refused = [
            ("https://127.0.0.1:443/", EgressError::Malformed),
            ("http://127.0.0.1/", EgressError::Malformed),
            ("http://10.0.0.5:80/", EgressError::NotLoopback),
            ("http://192.168.1.10:80/", EgressError::NotLoopback),
            ("http://172.16.0.1:80/", EgressError::NotLoopback),
            (
                "http://169.254.169.254:80/latest/meta-data",
                EgressError::NotLoopback,
            ),
            ("http://[fe80::1]:80/", EgressError::NotLoopback),
            ("http://93.184.216.34:80/", EgressError::NotLoopback),
            ("http://example.com:80/", EgressError::NotLoopback),
            ("http://0.0.0.0:80/", EgressError::NotLoopback),
            ("http://user:pw@127.0.0.1:80/", EgressError::Unusual),
            ("http://127.0.0.1:80/#frag", EgressError::Unusual),
            ("http://127.0.0.1:0/", EgressError::Malformed),
        ];
        for (url, want) in refused {
            assert_eq!(parse_target(url), Err(want), "{url}");
        }
        assert!(ip_allowed("::ffff:127.0.0.1".parse().unwrap()));
        assert!(!ip_allowed("::ffff:10.0.0.1".parse().unwrap()));
    }

    #[test]
    fn secrets_have_the_sandbox_shape() {
        let secret = secret_from_bytes([0xab; 32]);
        assert!(is_secret(&secret));
        assert!(!is_secret("whsec_live_abc"));
        assert!(!is_secret(&secret.to_uppercase()));
    }
}
