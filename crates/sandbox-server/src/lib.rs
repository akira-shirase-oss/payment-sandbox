//! The Payment Sandbox API server. **SIMULATED MONEY ONLY**: no real cards, balances or
//! banking connection; every response carries `Sandbox-Simulated: true`.
//!
//! The binary (`main.rs`) is thin; everything is here so tests can drive the router
//! in-process and the drills can run the real binary.

pub mod auth;
pub mod config;
pub mod deliverer;
pub mod error;
pub mod extract;
pub mod faults;
pub mod handlers;
pub mod lifecycle;
pub mod locks;
pub mod middleware;
pub mod network;
pub mod reconciler;
pub mod reports;
pub mod routes;
pub mod secrets;
pub mod shutdown;
pub mod state;
pub mod supervisor;
pub mod telemetry;

pub use config::Config;
pub use routes::app;
pub use state::AppState;
