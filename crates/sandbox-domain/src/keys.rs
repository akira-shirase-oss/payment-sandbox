//! Sandbox API keys (D-021): `sk_sim_` + 64 lowercase hex characters (32 random bytes).
//! Only the SHA-256 of a key is stored; high-entropy keys need no slow hash. The prefix is
//! deliberately unlike any real processor's, so a sandbox key can never be mistaken for a
//! live one, nor a live one accepted here.

use std::fmt;

use sha2::{Digest, Sha256};

/// Every key starts with this.
pub const KEY_PREFIX: &str = "sk_sim_";
/// Hex characters after the prefix.
pub const KEY_HEX_LEN: usize = 64;

/// What a key may do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Scope {
    /// Read payments, operations, journal and balances.
    Read,
    /// Create and change payments (implies `read`).
    PaymentsWrite,
    /// Keys, webhooks, dataset reset (implies everything).
    Admin,
}

impl Scope {
    /// Every scope.
    pub const ALL: [Self; 3] = [Self::Read, Self::PaymentsWrite, Self::Admin];

    /// Wire code.
    pub const fn code(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::PaymentsWrite => "payments:write",
            Self::Admin => "admin",
        }
    }

    /// Parses a code.
    pub fn from_code(code: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.code() == code)
    }

    /// True when holding `self` permits an action that requires `required`.
    pub const fn grants(self, required: Self) -> bool {
        matches!(
            (self, required),
            (Self::Admin, _)
                | (Self::PaymentsWrite, Self::PaymentsWrite | Self::Read)
                | (Self::Read, Self::Read)
        )
    }
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

/// True when any held scope permits `required`.
pub fn permits(held: &[Scope], required: Scope) -> bool {
    held.iter().any(|scope| scope.grants(required))
}

/// A presented key that is not a sandbox key.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("not a sandbox key")]
pub struct NotASandboxKey;

/// The SHA-256 of a key: what is stored and looked up.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyHash(pub [u8; 32]);

impl fmt::Debug for KeyHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("KeyHash(..)")
    }
}

/// Builds a key from 32 random bytes (the caller supplies the randomness, so this stays
/// pure).
pub fn key_from_bytes(random: [u8; 32]) -> String {
    format!("{KEY_PREFIX}{}", hex::encode(random))
}

/// Validates the shape of a presented key and hashes it.
pub fn hash_presented(presented: &str) -> Result<KeyHash, NotASandboxKey> {
    let Some(hex_part) = presented.strip_prefix(KEY_PREFIX) else {
        return Err(NotASandboxKey);
    };
    let shaped = hex_part.len() == KEY_HEX_LEN
        && hex_part
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if !shaped {
        return Err(NotASandboxKey);
    }
    Ok(KeyHash(Sha256::digest(presented.as_bytes()).into()))
}

/// The displayable prefix of a key (`sk_sim_` + 8 hex characters): enough to recognise it,
/// useless to an attacker.
pub fn display_prefix(key: &str) -> String {
    key.chars()
        .take(KEY_PREFIX.len().saturating_add(8))
        .collect()
}

/// Compares two equal-length secrets in time independent of where they differ.
pub fn constant_time_eq(a: &[u8; 32], b: &[u8; 32]) -> bool {
    let diff = a.iter().zip(b).fold(0_u8, |acc, (x, y)| acc | (x ^ y));
    std::hint::black_box(diff) == 0
}

/// SHA-256 of any secret, for fixed-length constant-time comparison.
pub fn digest(secret: &str) -> [u8; 32] {
    Sha256::digest(secret.as_bytes()).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_keys_have_the_sandbox_shape_and_hash() {
        let key = key_from_bytes([7; 32]);
        assert!(key.starts_with("sk_sim_"));
        assert_eq!(key.len(), 7 + 64);
        assert!(hash_presented(&key).is_ok());
        assert_eq!(display_prefix(&key), "sk_sim_07070707");
        assert_ne!(
            hash_presented(&key).unwrap(),
            hash_presented(&key_from_bytes([8; 32])).unwrap()
        );
    }

    #[test]
    fn live_looking_and_malformed_keys_are_refused() {
        let hex = "0".repeat(64);
        for bad in [
            format!("sk_live_{hex}"),
            format!("sk_test_{hex}"),
            format!("sk_sim_{}", "0".repeat(63)),
            format!("sk_sim_{}", "G".repeat(64)),
            format!("sk_sim_{}", "A".repeat(64)),
            String::new(),
        ] {
            assert_eq!(hash_presented(&bad), Err(NotASandboxKey), "{bad}");
        }
    }

    #[test]
    fn scopes_imply_downward_only() {
        use Scope::*;
        assert!(Admin.grants(PaymentsWrite) && Admin.grants(Read) && Admin.grants(Admin));
        assert!(PaymentsWrite.grants(Read) && !PaymentsWrite.grants(Admin));
        assert!(!Read.grants(PaymentsWrite) && !Read.grants(Admin));
        assert!(permits(&[Read, PaymentsWrite], PaymentsWrite));
        assert!(!permits(&[], Read));
        for scope in Scope::ALL {
            assert_eq!(Scope::from_code(scope.code()), Some(scope));
        }
    }

    #[test]
    fn constant_time_eq_compares_whole_digests() {
        assert!(constant_time_eq(&digest("a"), &digest("a")));
        assert!(!constant_time_eq(&digest("a"), &digest("b")));
    }
}
