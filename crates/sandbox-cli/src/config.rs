//! Where the client talks to and with which key, checked before anything is sent (D-068).
//!
//! A sandbox client must never be pointed at something that looks like production by
//! accident: a key that is not a sandbox key is refused outright (a real processor's key
//! would be useless here and must not travel), a real processor's host is refused outright,
//! and any host that is not this machine is refused unless the caller says so explicitly
//! (a CI service container or a shared sandbox behind a proxy, D-071). The key is never
//! echoed in any message.

use std::fmt;

/// The sandbox API's default address (`scripts/stack.sh`, ARCHITECTURE "Ports").
pub const DEFAULT_API_URL: &str = "http://127.0.0.1:56080";

/// Hosts of real payment processors (and their suffixes): never a sandbox, whatever the
/// caller allows. A belt-and-braces list, not an exhaustive one; the loopback rule is the
/// real guard.
pub const REAL_PROCESSOR_HOSTS: [&str; 14] = [
    "stripe.com",
    "stripe.network",
    "adyen.com",
    "adyenpayments.com",
    "paypal.com",
    "braintreegateway.com",
    "squareup.com",
    "checkout.com",
    "authorize.net",
    "worldpay.com",
    "razorpay.com",
    "mollie.com",
    "klarna.com",
    "paddle.com",
];

/// A sandbox key, held so that `Debug` and `Display` never show it.
#[derive(Clone)]
pub struct SandboxKey(String);

impl SandboxKey {
    /// The key text, for the `Authorization` header only.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SandboxKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SandboxKey(<redacted>)")
    }
}

/// Certificate authorities trusted in addition to this machine's own (a shared sandbox
/// behind a proxy with a private CA, D-074).
#[derive(Clone, Default)]
pub struct ExtraRoots(pub(crate) Vec<reqwest::Certificate>);

impl ExtraRoots {
    /// How many certificates are trusted in addition to the system's.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// True when only the system's own authorities are trusted.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for ExtraRoots {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ExtraRoots({} certificate(s))", self.0.len())
    }
}

/// A checked client configuration.
#[derive(Debug, Clone)]
pub struct Config {
    /// The API's base URL, without a trailing slash.
    pub api_url: String,
    /// The project's sandbox key.
    pub key: SandboxKey,
    /// Extra certificate authorities for an `https://` API (`--ca-cert`).
    pub extra_roots: ExtraRoots,
}

/// Why a configuration was refused. Every message says what is wrong and what to do.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    /// No key given.
    #[error(
        "no sandbox key: set PS_API_KEY or pass --key (sandbox keys start with sk_sim_; create one with POST /v1/keys)"
    )]
    MissingKey,
    /// The `--ca-cert` file holds no certificate.
    #[error(
        "{0} holds no PEM certificate (-----BEGIN CERTIFICATE-----): --ca-cert (PS_CA_CERT) takes the certificate of the authority that signed the sandbox's HTTPS certificate"
    )]
    BadCaCert(String),
    /// Not a Payment Sandbox key. The value is never echoed.
    #[error(
        "that is not a Payment Sandbox key: sandbox keys are sk_sim_ followed by 64 hex characters. Never use a real processor's key (sk_live_, sk_test_, rk_...) with the sandbox"
    )]
    NotASandboxKey,
    /// Not an http(s) URL this client can use.
    #[error(
        "the API URL must look like http://127.0.0.1:56080 (http or https, a host, an optional port, no user, query or fragment)"
    )]
    BadUrl,
    /// A real payment processor's host.
    #[error(
        "{0} belongs to a real payment processor; Payment Sandbox never talks to one (SIMULATED MONEY ONLY)"
    )]
    RealProcessor(String),
    /// Not this machine, and not explicitly allowed.
    #[error(
        "{0} is not this machine: the sandbox runs on loopback (127.0.0.1, ::1, localhost). If it runs in a CI service container or on a shared host you control, allow it explicitly with --allow-non-local (or PS_ALLOW_NON_LOCAL=1)"
    )]
    NonLocal(String),
}

impl Config {
    /// Checks `api_url` and `key`. `allow_non_local` admits hosts other than loopback; it
    /// never admits a real processor's host or a non-sandbox key.
    pub fn new(
        api_url: &str,
        key: Option<&str>,
        allow_non_local: bool,
    ) -> Result<Self, ConfigError> {
        let host = url_host(api_url)?;
        if is_real_processor(&host) {
            return Err(ConfigError::RealProcessor(host));
        }
        if !allow_non_local && !is_loopback_host(&host) {
            return Err(ConfigError::NonLocal(host));
        }
        let key = key
            .map(str::trim)
            .filter(|k| !k.is_empty())
            .ok_or(ConfigError::MissingKey)?;
        if sandbox_domain::keys::hash_presented(key).is_err() {
            return Err(ConfigError::NotASandboxKey);
        }
        Ok(Self {
            api_url: api_url.trim_end_matches('/').to_owned(),
            key: SandboxKey(key.to_owned()),
            extra_roots: ExtraRoots::default(),
        })
    }

    /// Also trusts the certificate authorities in `pem` (read from `source`, named in the
    /// error) for an `https://` API, on top of this machine's own.
    pub fn trusting(mut self, source: &str, pem: &[u8]) -> Result<Self, ConfigError> {
        let certificates = reqwest::Certificate::from_pem_bundle(pem)
            .ok()
            .filter(|c| !c.is_empty())
            .ok_or_else(|| ConfigError::BadCaCert(source.to_owned()))?;
        self.extra_roots.0.extend(certificates);
        Ok(self)
    }
}

/// The host of an `http(s)://host[:port][/]` URL, lowercased; anything else is `BadUrl`.
pub fn url_host(url: &str) -> Result<String, ConfigError> {
    let rest = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .ok_or(ConfigError::BadUrl)?;
    let authority = rest.strip_suffix('/').unwrap_or(rest);
    if authority.is_empty()
        || authority.contains(['/', '?', '#', '@', ' '])
        || !authority.is_ascii()
    {
        return Err(ConfigError::BadUrl);
    }
    let host = if let Some(bracketed) = authority.strip_prefix('[') {
        let (inside, after) = bracketed.split_once(']').ok_or(ConfigError::BadUrl)?;
        if !(after.is_empty() || after.strip_prefix(':').is_some_and(valid_port)) {
            return Err(ConfigError::BadUrl);
        }
        inside.to_owned()
    } else {
        match authority.split_once(':') {
            Some((host, port)) if valid_port(port) => host.to_owned(),
            Some(_) => return Err(ConfigError::BadUrl),
            None => authority.to_owned(),
        }
    };
    if host.is_empty() {
        return Err(ConfigError::BadUrl);
    }
    Ok(host.to_ascii_lowercase())
}

fn valid_port(port: &str) -> bool {
    !port.is_empty() && port.len() <= 5 && port.parse::<u16>().is_ok_and(|p| p > 0)
}

/// Loopback: `localhost`, `*.localhost` (RFC 6761), 127.0.0.0/8 and `::1`.
pub fn is_loopback_host(host: &str) -> bool {
    let host = host.trim_end_matches('.');
    if host == "localhost" || host.ends_with(".localhost") {
        return true;
    }
    host.parse::<std::net::IpAddr>()
        .is_ok_and(|ip| ip.is_loopback())
}

/// A real processor's host or one of its subdomains.
pub fn is_real_processor(host: &str) -> bool {
    let host = host.trim_end_matches('.');
    REAL_PROCESSOR_HOSTS
        .iter()
        .any(|real| host == *real || host.ends_with(&format!(".{real}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "sk_sim_0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn loopback_urls_and_sandbox_keys_are_accepted() {
        for url in [
            "http://127.0.0.1:56080",
            "http://127.0.0.1:56080/",
            "http://localhost:56080",
            "http://dashboard.localhost:56084",
            "http://[::1]:56080",
            "http://127.9.9.9",
            "https://localhost",
        ] {
            let config = Config::new(url, Some(KEY), false).unwrap();
            assert!(!config.api_url.ends_with('/'), "{url}");
        }
    }

    #[test]
    fn production_looking_configuration_is_refused() {
        assert_eq!(
            Config::new("https://api.stripe.com", Some(KEY), true).unwrap_err(),
            ConfigError::RealProcessor("api.stripe.com".to_owned())
        );
        assert_eq!(
            Config::new("https://checkout-test.adyen.com:443", Some(KEY), true).unwrap_err(),
            ConfigError::RealProcessor("checkout-test.adyen.com".to_owned())
        );
        assert_eq!(
            Config::new("https://sandbox.example.com", Some(KEY), false).unwrap_err(),
            ConfigError::NonLocal("sandbox.example.com".to_owned())
        );
        assert_eq!(
            Config::new("http://10.0.0.5:56080", Some(KEY), false).unwrap_err(),
            ConfigError::NonLocal("10.0.0.5".to_owned())
        );
        // Explicitly allowed: a CI service container or a shared sandbox host.
        assert!(Config::new("http://sandbox:56080", Some(KEY), true).is_ok());
        // Built at run time, so no literal in the source looks like a real processor's key.
        let hex = "0123456789abcdef".repeat(4);
        for key in [
            format!("sk_{}_{hex}", "live"),
            format!("sk_{}_{hex}", "test"),
            format!("rk_{}_abc", "live"),
            "sk_sim_short".to_owned(),
            format!("sk_sim_{}", hex.to_ascii_uppercase()),
        ] {
            let refused = Config::new("http://127.0.0.1:56080", Some(&key), true).unwrap_err();
            assert_eq!(refused, ConfigError::NotASandboxKey, "{key}");
            assert!(
                !refused.to_string().contains(&key),
                "the key is never echoed"
            );
        }
        assert_eq!(
            Config::new("http://127.0.0.1:56080", Some("  "), false).unwrap_err(),
            ConfigError::MissingKey
        );
        assert_eq!(
            Config::new("http://127.0.0.1:56080", None, false).unwrap_err(),
            ConfigError::MissingKey
        );
    }

    #[test]
    fn malformed_urls_are_refused() {
        for url in [
            "",
            "127.0.0.1:56080",
            "ftp://127.0.0.1",
            "http://",
            "http://user:pw@127.0.0.1:56080",
            "http://127.0.0.1:56080/v1",
            "http://127.0.0.1:56080?x=1",
            "http://127.0.0.1:0",
            "http://127.0.0.1:99999",
            "http://[::1",
            "http://bad host",
        ] {
            assert_eq!(
                Config::new(url, Some(KEY), true).unwrap_err(),
                ConfigError::BadUrl,
                "{url}"
            );
        }
    }

    #[test]
    fn a_ca_file_must_hold_a_certificate() {
        let config = Config::new("https://localhost", Some(KEY), false).unwrap();
        for junk in [
            &b""[..],
            b"not a certificate",
            b"-----BEGIN PRIVATE KEY-----\n-----END PRIVATE KEY-----\n",
        ] {
            assert_eq!(
                config.clone().trusting("ca.pem", junk).unwrap_err(),
                ConfigError::BadCaCert("ca.pem".to_owned())
            );
        }
        assert!(config.extra_roots.is_empty());
    }

    #[test]
    fn the_key_never_shows_in_debug_output() {
        let config = Config::new("http://127.0.0.1:56080", Some(KEY), false).unwrap();
        assert!(!format!("{config:?}").contains(KEY));
    }
}
