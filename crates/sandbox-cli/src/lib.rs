//! `sandbox-cli`: the Payment Sandbox from a terminal. **SIMULATED MONEY ONLY.**
//!
//! The library holds what the binary and the tests share: the checked configuration
//! ([`config`], which refuses production-looking settings), the retrying, honest HTTP client
//! ([`client`]) and the seeded scenario library with its runner ([`scenario`]). See D-068
//! and D-069.

pub mod client;
pub mod config;
pub mod scenario;

pub use client::{Answer, Client, ClientError, RetryPolicy};
pub use config::{Allow, Config, ConfigError};

/// The notice every invocation prints first (on standard error).
pub const NOTICE: &str = sandbox_domain::SANDBOX_NOTICE;
