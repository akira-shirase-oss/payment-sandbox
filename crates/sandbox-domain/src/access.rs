//! Who may call the local processes' internal routes (D-046, review finding 10).
//!
//! The simulated network and the development store listen on loopback, but loopback is not
//! a credential: any local process, and any webhook target the server is told to call, can
//! reach those ports. So:
//!
//! - the network's executions, readbacks (and so fences) and listings need the **network
//!   credential** the server holds (`PS_SIM_TOKEN`);
//! - test controls (the network's faults and virtual clock, the development store's
//!   `PUT /config`) exist only when a startup flag turns them on, and then need a separate
//!   **operator credential**. The flag is on by default only in the `dev` profile.
//!
//! Pure policy, no I/O: the binaries read their environment and ask this module.

use std::fmt;

use sha2::{Digest as _, Sha256};

/// Shortest accepted credential.
pub const MIN_CREDENTIAL_LEN: usize = 32;

/// Where a process runs, which decides only the control plane's default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    /// A developer's machine: test controls are on unless switched off.
    Dev,
    /// Continuous integration: off unless switched on.
    Ci,
    /// Anything other people or processes share (the default): off unless switched on.
    Shared,
}

impl Profile {
    /// Parses `PS_PROFILE`; unset is [`Profile::Shared`]. An unknown value is refused, so a
    /// typo never silently selects a default.
    pub fn parse(raw: Option<&str>) -> Result<Self, AccessError> {
        match raw {
            None | Some("shared") => Ok(Self::Shared),
            Some("dev") => Ok(Self::Dev),
            Some("ci") => Ok(Self::Ci),
            Some(_) => Err(AccessError::UnknownProfile),
        }
    }
}

/// A shared secret presented as `Authorization: Bearer <credential>`. Only its SHA-256 is
/// kept; `Debug` never prints it.
#[derive(Clone, PartialEq, Eq)]
pub struct Credential([u8; 32]);

impl Credential {
    /// Accepts a credential of at least [`MIN_CREDENTIAL_LEN`] visible characters.
    pub fn new(raw: &str) -> Result<Self, AccessError> {
        if raw.len() < MIN_CREDENTIAL_LEN
            || raw.chars().any(|c| c.is_whitespace() || c.is_control())
        {
            return Err(AccessError::WeakCredential);
        }
        Ok(Self(Sha256::digest(raw.as_bytes()).into()))
    }

    /// True when the `Authorization` header value is `Bearer <this credential>`. Digests
    /// are compared without an early exit.
    pub fn admits(&self, authorization: Option<&str>) -> bool {
        let Some(presented) = authorization.and_then(|v| v.strip_prefix("Bearer ")) else {
            return false;
        };
        let digest: [u8; 32] = Sha256::digest(presented.as_bytes()).into();
        digest
            .iter()
            .zip(self.0.iter())
            .fold(0_u8, |acc, (a, b)| acc | (a ^ b))
            == 0
    }
}

impl fmt::Debug for Credential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Credential(<redacted>)")
    }
}

/// Why a process refuses its access configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AccessError {
    /// `PS_PROFILE` is not `dev`, `ci` or `shared`.
    #[error("the profile must be dev, ci or shared")]
    UnknownProfile,
    /// The control flag is not `on` or `off`.
    #[error("the control flag must be on or off")]
    UnknownFlag,
    /// Shorter than [`MIN_CREDENTIAL_LEN`] or containing spaces or control characters.
    #[error("a credential must be at least 32 visible characters")]
    WeakCredential,
    /// The control plane is on but has no credential.
    #[error("the control plane is on, so it needs its own credential")]
    MissingControlCredential,
    /// The control plane's credential equals another credential of the process.
    #[error("the control credential must differ from every other credential")]
    SharedCredential,
}

/// Decides the control plane: `flag` (`on`/`off`) wins; otherwise it is on only in the
/// `dev` profile. When on, `credential` is required, and must differ from `others` (so the
/// server's network credential never doubles as the operator's).
pub fn control_plane(
    profile: Profile,
    flag: Option<&str>,
    credential: Option<&str>,
    others: &[&str],
) -> Result<Option<Credential>, AccessError> {
    let on = match flag {
        Some("on") => true,
        Some("off") => false,
        Some(_) => return Err(AccessError::UnknownFlag),
        None => profile == Profile::Dev,
    };
    if !on {
        return Ok(None);
    }
    let raw = credential.ok_or(AccessError::MissingControlCredential)?;
    if others.contains(&raw) {
        return Err(AccessError::SharedCredential);
    }
    Credential::new(raw).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "a-credential-of-at-least-thirty-two-chars";
    const B: &str = "another-credential-of-at-least-32-chars";

    #[test]
    fn a_credential_admits_only_its_own_bearer() {
        let c = Credential::new(A).unwrap();
        assert!(c.admits(Some(&format!("Bearer {A}"))));
        for wrong in [
            None,
            Some(A),
            Some("Bearer "),
            Some(&*format!("Bearer {B}")),
            Some(&*format!("bearer {A}")),
            Some(&*format!("Bearer {A}x")),
        ] {
            assert!(!c.admits(wrong), "{wrong:?}");
        }
        assert_eq!(format!("{c:?}"), "Credential(<redacted>)");
        for weak in ["short", "", &*format!("{A} with space"), &*format!("{A}\n")] {
            assert_eq!(Credential::new(weak), Err(AccessError::WeakCredential));
        }
    }

    #[test]
    fn the_control_plane_is_off_by_default_outside_dev() {
        for profile in [Profile::Ci, Profile::Shared] {
            assert_eq!(control_plane(profile, None, Some(B), &[A]), Ok(None));
        }
        assert!(
            control_plane(Profile::Dev, None, Some(B), &[A])
                .unwrap()
                .is_some()
        );
        assert_eq!(
            control_plane(Profile::Dev, Some("off"), None, &[A]),
            Ok(None)
        );
        assert!(
            control_plane(Profile::Shared, Some("on"), Some(B), &[A])
                .unwrap()
                .is_some()
        );
        assert_eq!(
            control_plane(Profile::Dev, None, None, &[A]),
            Err(AccessError::MissingControlCredential)
        );
        assert_eq!(
            control_plane(Profile::Dev, None, Some(A), &[A]),
            Err(AccessError::SharedCredential)
        );
        assert_eq!(
            control_plane(Profile::Dev, Some("yes"), Some(B), &[A]),
            Err(AccessError::UnknownFlag)
        );
        assert_eq!(
            Profile::parse(Some("prod")),
            Err(AccessError::UnknownProfile)
        );
        assert_eq!(Profile::parse(None), Ok(Profile::Shared));
    }
}
