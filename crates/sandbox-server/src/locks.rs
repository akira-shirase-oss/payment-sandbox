//! Per-payment serialization ahead of the database (D-033).
//!
//! Captures, voids and refunds of one payment serialize on its row lock anyway (D-014). If
//! they waited for that lock *inside PostgreSQL*, every waiter would hold a pooled
//! connection while it waits, and one hot payment could exhaust the pool for everyone (the
//! first refund-race smoke did exactly that: 1,030 pool timeouts). So requests for the same
//! payment queue here first, on one of 1,024 fair async mutexes chosen by the payment id,
//! and take a connection only when it is their turn. The wait is bounded: past it the
//! request is shed with `503` and can be retried under the same key.

use std::hash::{BuildHasher, Hasher};
use std::time::Duration;

use tokio::sync::{Mutex, MutexGuard};
use uuid::Uuid;

/// Number of stripes. Two payments sharing a stripe merely queue behind each other.
pub const STRIPES: usize = 1024;

/// The stripes.
#[derive(Debug)]
pub struct PaymentLocks {
    stripes: Box<[Mutex<()>]>,
    hasher: std::hash::RandomState,
}

/// The wait for a payment's turn exceeded its bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the payment is busy; retry with the same Idempotency-Key")]
pub struct Busy;

impl Default for PaymentLocks {
    fn default() -> Self {
        Self {
            stripes: (0..STRIPES).map(|_| Mutex::new(())).collect(),
            hasher: std::hash::RandomState::new(),
        }
    }
}

impl PaymentLocks {
    /// Waits (FIFO) for `payment`'s turn, at most `wait`.
    pub async fn lock(&self, payment: Uuid, wait: Duration) -> Result<MutexGuard<'_, ()>, Busy> {
        let mut hasher = self.hasher.build_hasher();
        hasher.write_u128(payment.as_u128());
        let index = usize::try_from(hasher.finish() % STRIPES as u64).unwrap_or(0);
        tokio::time::timeout(wait, self.stripes[index].lock())
            .await
            .map_err(|_| Busy)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn the_same_payment_waits_and_the_wait_is_bounded() {
        let locks = PaymentLocks::default();
        let id = Uuid::from_u128(1);
        let held = locks.lock(id, Duration::from_secs(1)).await.unwrap();
        assert_eq!(
            locks.lock(id, Duration::from_millis(50)).await.err(),
            Some(Busy)
        );
        drop(held);
        assert!(locks.lock(id, Duration::from_millis(50)).await.is_ok());
    }
}
