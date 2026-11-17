//! The browser-facing sites (D-061): the hosted checkout and the dashboard, each on its own
//! listener and so its own origin (D-029), each a static app served from memory under a
//! Content-Security-Policy that pins its inline scripts by hash, with a small same-origin
//! JSON API of its own. Neither exposes `/v1`; neither holds payment authority of its own:
//! the checkout acts only for the session whose secret it was given, the dashboard only
//! for the key that signed in, and only to read (simulator controls aside, D-063).

pub mod checkout;
pub mod dashboard;
pub mod site;

use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, header};

use crate::error::ApiError;

/// Fetch metadata: who started the request (`same-origin`, `same-site`, `cross-site`,
/// `none`).
const SEC_FETCH_SITE: HeaderName = HeaderName::from_static("sec-fetch-site");

/// Refuses an API request a browser says another origin started (D-062): cookies are not
/// isolated by port, so a sibling origin on the same host must not even make a site run a
/// query on its user's behalf. Requests without fetch metadata (non-browser clients) pass;
/// state changes are checked further by [`require_same_origin`].
pub fn refuse_cross_site(headers: &HeaderMap) -> Result<(), ApiError> {
    match headers.get(SEC_FETCH_SITE) {
        None => Ok(()),
        Some(value) if value.as_bytes() == b"same-origin" => Ok(()),
        Some(_) => Err(ApiError::CrossOrigin),
    }
}

/// Refuses a state-changing request unless its `Origin` is exactly `origin` (and its fetch
/// metadata, when present, says `same-origin`). Browsers always send `Origin` on a `POST`;
/// another origin cannot forge it.
pub fn require_same_origin(
    method: &Method,
    headers: &HeaderMap,
    origin: &str,
) -> Result<(), ApiError> {
    if method == Method::GET || method == Method::HEAD {
        return refuse_cross_site(headers);
    }
    refuse_cross_site(headers)?;
    let mut origins = headers.get_all(header::ORIGIN).iter();
    match (origins.next(), origins.next()) {
        (Some(value), None) if value.as_bytes() == origin.as_bytes() => Ok(()),
        _ => Err(ApiError::CrossOrigin),
    }
}

/// The policy of a site's pages. `script_src` lists the inline scripts' hashes; styles,
/// fonts, images and connections are this origin only; nothing may be a form target, a
/// base, a plugin or a frame (`frame_ancestors` says who may frame the page).
pub fn page_policy(script_hashes: &str, frame_ancestors: &str) -> String {
    format!(
        "default-src 'none'; script-src 'self' {script_hashes}; style-src 'self'; img-src 'self'; \
         font-src 'self'; connect-src 'self'; manifest-src 'self'; base-uri 'none'; \
         form-action 'none'; object-src 'none'; frame-ancestors {frame_ancestors}"
    )
}

/// The policy of everything that is not a page (assets, JSON).
pub const RESOURCE_POLICY: &str = "default-src 'none'; frame-ancestors 'none'";

/// Headers every response of a site carries.
pub fn harden(headers: &mut HeaderMap) {
    for (name, value) in [
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        (header::REFERRER_POLICY, "no-referrer"),
        (
            HeaderName::from_static("cross-origin-resource-policy"),
            "same-origin",
        ),
        (
            HeaderName::from_static("cross-origin-opener-policy"),
            "same-origin",
        ),
        (
            HeaderName::from_static("permissions-policy"),
            "camera=(), microphone=(), geolocation=(), payment=(), usb=(), clipboard-read=()",
        ),
    ] {
        headers.insert(name, HeaderValue::from_static(value));
    }
    headers
        .entry(header::CONTENT_SECURITY_POLICY)
        .or_insert(HeaderValue::from_static(RESOURCE_POLICY));
    headers
        .entry(header::CACHE_CONTROL)
        .or_insert(HeaderValue::from_static("no-store"));
}

/// Standard base64 with padding (RFC 4648 §4), for CSP hash sources.
pub fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for (i, shift) in [18_u32, 12, 6, 0].into_iter().enumerate() {
            if i <= chunk.len() {
                out.push(char::from(TABLE[((n >> shift) & 0x3f) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// A cookie's value from a `Cookie` header, if present exactly once.
pub fn cookie<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    let mut found = None;
    for value in headers.get_all(header::COOKIE) {
        let Ok(text) = value.to_str() else { continue };
        for pair in text.split(';') {
            if let Some((k, v)) = pair.trim().split_once('=')
                && k == name
            {
                if found.is_some() {
                    // Two cookies of one name (one planted by a sibling origin): trust neither.
                    return None;
                }
                found = Some(v);
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_rfc_vectors() {
        for (input, expected) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(input.as_bytes()), expected, "{input:?}");
        }
        assert_eq!(base64(&[0xfb, 0xff]), "+/8=");
    }

    #[test]
    fn state_changes_need_this_exact_origin() {
        let origin = "http://127.0.0.1:56083";
        let with = |pairs: &[(&str, &str)]| {
            let mut h = HeaderMap::new();
            for (k, v) in pairs {
                h.append(
                    HeaderName::from_bytes(k.as_bytes()).unwrap(),
                    HeaderValue::from_str(v).unwrap(),
                );
            }
            h
        };
        let post = Method::POST;
        assert!(require_same_origin(&post, &with(&[("origin", origin)]), origin).is_ok());
        assert!(
            require_same_origin(
                &post,
                &with(&[("origin", origin), ("sec-fetch-site", "same-origin")]),
                origin
            )
            .is_ok()
        );
        for refused in [
            with(&[]),
            with(&[("origin", "http://127.0.0.1:56084")]),
            with(&[("origin", "null")]),
            with(&[("origin", origin), ("sec-fetch-site", "same-site")]),
            with(&[("origin", origin), ("origin", origin)]),
        ] {
            assert!(
                matches!(
                    require_same_origin(&post, &refused, origin),
                    Err(ApiError::CrossOrigin)
                ),
                "{refused:?}"
            );
        }
        // Reads need no Origin, but a browser's cross-site read is refused too.
        assert!(require_same_origin(&Method::GET, &with(&[]), origin).is_ok());
        assert!(
            require_same_origin(
                &Method::GET,
                &with(&[("sec-fetch-site", "cross-site")]),
                origin
            )
            .is_err()
        );
    }

    #[test]
    fn a_doubled_cookie_is_trusted_by_neither_copy() {
        let mut h = HeaderMap::new();
        h.insert(
            header::COOKIE,
            HeaderValue::from_static("a=1; __Host-s=abc; b=2"),
        );
        assert_eq!(cookie(&h, "__Host-s"), Some("abc"));
        h.append(header::COOKIE, HeaderValue::from_static("__Host-s=planted"));
        assert_eq!(cookie(&h, "__Host-s"), None);
        assert_eq!(cookie(&HeaderMap::new(), "__Host-s"), None);
    }
}
