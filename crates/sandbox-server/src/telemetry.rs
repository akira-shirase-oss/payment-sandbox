//! Logging. Request bodies, keys and tokens are never logged; spans carry the route, path
//! (no query string) and request id only.

use tracing_subscriber::{EnvFilter, fmt, prelude::*};

use crate::config::LogFormat;

/// Installs the global subscriber.
pub fn init(format: LogFormat, filter: &str) -> anyhow::Result<()> {
    let filter = EnvFilter::try_new(filter)?;
    let registry = tracing_subscriber::registry().with(filter);
    match format {
        LogFormat::Json => registry
            .with(fmt::layer().json().with_current_span(true))
            .try_init()?,
        LogFormat::Pretty => registry.with(fmt::layer()).try_init()?,
    }
    Ok(())
}
