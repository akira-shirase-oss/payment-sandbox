//! Bytes written with the letters `a`–`p`, one per half-byte. Values the sandbox hands out
//! and later reads back (explanation cursors, checkout secrets, dashboard sessions) use it
//! so they contain no digits at all: the card guard (D-019, D-043) can never mistake one for
//! a card number, wherever it ends up (a query string, a header, a log line).

const ALPHABET: &[u8; 16] = b"abcdefghijklmnop";

/// `bytes` as letters, two per byte.
pub fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        out.push(char::from(ALPHABET[usize::from(byte >> 4)]));
        out.push(char::from(ALPHABET[usize::from(byte & 0x0f)]));
    }
    out
}

/// The bytes `text` encodes, or `None` if it is not an even run of `a`–`p`.
pub fn decode(text: &str) -> Option<Vec<u8>> {
    let nibble = |c: u8| {
        ALPHABET
            .iter()
            .position(|&a| a == c)
            .and_then(|n| u8::try_from(n).ok())
    };
    let (pairs, rest) = text.as_bytes().as_chunks::<2>();
    if !rest.is_empty() {
        return None;
    }
    pairs
        .iter()
        .map(|[hi, lo]| Some((nibble(*hi)? << 4) | nibble(*lo)?))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_refuses_anything_else() {
        let bytes: Vec<u8> = (0..=255).collect();
        let text = encode(&bytes);
        assert!(text.bytes().all(|b| (b'a'..=b'p').contains(&b)));
        assert_eq!(decode(&text).unwrap(), bytes);
        assert_eq!(encode(&[0x00, 0xff, 0x4a]), "aappek");
        for bad in ["a", "aq", "AA", "a1", "ab c"] {
            assert_eq!(decode(bad), None, "{bad:?}");
        }
        assert_eq!(decode(""), Some(Vec::new()));
    }
}
