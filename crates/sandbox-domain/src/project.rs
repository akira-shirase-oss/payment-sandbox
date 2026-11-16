//! Project slugs: the name a reset confirmation is typed against (D-022).

use std::fmt;

/// A validated slug: 1–63 of `[a-z0-9-]`, starting and ending alphanumeric. The database
/// repeats the rule as a `CHECK`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProjectSlug(String);

/// Why a slug was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "slug must be 1-63 lowercase letters, digits or dashes, starting and ending with a letter or digit"
)]
pub struct InvalidSlug;

impl ProjectSlug {
    /// Validates a slug.
    pub fn parse(raw: &str) -> Result<Self, InvalidSlug> {
        let bytes = raw.as_bytes();
        let edge_ok = |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit();
        let ok = (1..=63).contains(&bytes.len())
            && bytes.iter().all(|b| edge_ok(*b) || *b == b'-')
            && bytes.first().copied().is_some_and(edge_ok)
            && bytes.last().copied().is_some_and(edge_ok);
        if ok {
            Ok(Self(raw.to_owned()))
        } else {
            Err(InvalidSlug)
        }
    }

    /// The slug text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProjectSlug {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Validates a display name: 1–200 characters, no control characters.
pub fn validate_name(name: &str) -> Result<&str, &'static str> {
    let count = name.chars().count();
    if !(1..=200).contains(&count) {
        return Err("name must be 1 to 200 characters");
    }
    if name.chars().any(char::is_control) {
        return Err("name may not contain control characters");
    }
    Ok(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_follow_the_rule() {
        for ok in ["a", "store-1", "dev-store", "x".repeat(63).as_str()] {
            assert!(ProjectSlug::parse(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "-a",
            "a-",
            "Store",
            "a_b",
            "a b",
            "x".repeat(64).as_str(),
            "caf\u{e9}",
        ] {
            assert_eq!(ProjectSlug::parse(bad), Err(InvalidSlug), "{bad:?}");
        }
    }

    #[test]
    fn names_are_bounded_and_printable() {
        assert!(validate_name("Dev store").is_ok());
        assert!(validate_name("").is_err());
        assert!(validate_name("a\nb").is_err());
        assert!(validate_name(&"n".repeat(201)).is_err());
    }
}
