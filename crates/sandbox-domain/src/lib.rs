//! The Payment Sandbox's rules in pure form. **SIMULATED MONEY ONLY.**
//!
//! Everything here is deterministic and free of I/O, so the server, the simulator and the
//! tests share one definition of money, the chart of accounts, posting recipes, capacity
//! and operation state. `scripts/check-architecture.sh` keeps it that way.
//!
//! - [`currency`]: the fixed currency set and its minor-unit scales (D-010).
//! - [`money`]: checked integer amounts; never floats.
//! - [`account`]: the chart of accounts (D-012).
//! - [`journal`]: balanced, single-currency journal entries.
//! - [`recipe`]: the posting recipes of the two-phase hold model (D-011).
//! - [`payment`]: capacity sums and reservations (D-014) and the derived payment status.
//! - [`operation`]: the operation state machine, uncertain outcomes included.
//! - [`identity`]: idempotency keys, derived operation identity (D-015) and fingerprints (D-016).
//! - [`card_guard`]: raw-card detection (D-019).
//! - [`scenario`]: synthetic `tok_sim_*` tokens and the scenarios they select.
//! - [`keys`]: `sk_sim_` keys, scopes and hashing (D-021).
//! - [`project`]: project slugs and names.
// Money code may not use unchecked arithmetic; tests may (overflow panics there and fails the test).
#![cfg_attr(not(test), warn(clippy::arithmetic_side_effects))]

pub mod account;
pub mod card_guard;
pub mod currency;
pub mod identity;
pub mod journal;
pub mod keys;
pub mod money;
pub mod operation;
pub mod payment;
pub mod project;
pub mod recipe;
pub mod scenario;

pub use account::{AccountKind, Side};
pub use currency::Currency;
pub use journal::{JournalEntry, Line, PostingError};
pub use money::{Amount, AmountError, MAX_AMOUNT_MINOR};
pub use operation::{NetworkOutcome, OperationKind, OperationState};
pub use payment::{CapacityError, PaymentStatus, PaymentSums};
pub use recipe::Recipe;

/// Version of the ledger's meaning: the chart of accounts and every posting recipe. Bump it
/// whenever either changes meaning; every journal entry records the version it was posted
/// under (D-030).
pub const LEDGER_SEMANTICS_VERSION: i32 = 1;

/// The label every binary, response and page carries.
pub const SANDBOX_NOTICE: &str =
    "SIMULATED MONEY ONLY: Payment Sandbox moves no real money and accepts no real cards.";
