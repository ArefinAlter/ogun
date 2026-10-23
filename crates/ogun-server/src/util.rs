//! Small helpers: base64url, randomness, hex, constant-time comparison and time.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ogun_types::Timestamp;
use zeroize::Zeroizing;

use crate::error::{ServerError, ServerResult};

/// Base64url without padding (WebAuthn, PKCE, cookies).
#[must_use]
pub fn b64url(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Decodes base64url without padding. Trailing `=` padding is tolerated.
///
/// # Errors
///
/// `api.invalid_params` naming `what` when the text is not base64url.
pub fn b64url_decode(text: &str, what: &str) -> ServerResult<Vec<u8>> {
    URL_SAFE_NO_PAD
        .decode(text.trim_end_matches('='))
        .map_err(|_| ServerError::invalid(format!("{what} is not base64url")))
}

/// `n` bytes from the OS random source.
///
/// # Errors
///
/// `api.unavailable` when the OS has no entropy source.
pub fn random_bytes<const N: usize>() -> ServerResult<Zeroizing<[u8; N]>> {
    let mut out = Zeroizing::new([0u8; N]);
    getrandom::fill(&mut out[..]).map_err(|_| ServerError::unavailable("no OS random source"))?;
    Ok(out)
}

/// A fresh random secret of 32 bytes in base64url (43 characters).
///
/// # Errors
///
/// `api.unavailable` when the OS has no entropy source.
pub fn random_token() -> ServerResult<Zeroizing<String>> {
    let bytes = random_bytes::<32>()?;
    Ok(Zeroizing::new(b64url(bytes.as_slice())))
}

/// Lowercase hex.
#[must_use]
pub fn hex(bytes: &[u8]) -> String {
    ogun_api::auth::to_hex(bytes)
}

/// Parses 64 lowercase hex digits.
#[must_use]
pub fn parse_hex32(text: &str) -> Option<[u8; 32]> {
    let b = text.as_bytes();
    if b.len() != 64 {
        return None;
    }
    let digit = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    };
    let mut out = [0u8; 32];
    for (i, pair) in b.chunks_exact(2).enumerate() {
        out[i] = (digit(pair[0])? << 4) | digit(pair[1])?;
    }
    Some(out)
}

/// Constant-time equality of two byte strings (the length is not secret).
#[must_use]
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

/// The store key for a bearer secret: hex of an unkeyed BLAKE3 hash. A copy of the store does not reveal live
/// sessions or invitations.
#[must_use]
pub fn secret_key(context: &str, secret: &[u8]) -> String {
    hex(blake3::derive_key(context, secret).as_slice())
}

/// Now, as an Ogun timestamp.
#[must_use]
pub fn now() -> Timestamp {
    Timestamp::now()
}

/// Milliseconds since the Unix epoch.
#[must_use]
pub fn now_ms() -> i64 {
    Timestamp::now().unix_millis()
}

/// `true` when `text` is 64 lowercase hex digits (a BLAKE3 digest).
#[must_use]
pub fn is_blake3_hex(text: &str) -> bool {
    parse_hex32(text).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64url_round_trip_and_hex() {
        let s = b64url(b"\xff\xfe ogun");
        assert!(!s.contains('='));
        assert_eq!(b64url_decode(&s, "x").unwrap(), b"\xff\xfe ogun");
        assert!(b64url_decode("***", "x").is_err());
        let h = hex(&[0xab; 32]);
        assert_eq!(parse_hex32(&h), Some([0xab; 32]));
        assert!(parse_hex32("AB").is_none());
        assert!(ct_eq(b"abc", b"abc"));
        assert!(!ct_eq(b"abc", b"abd"));
        assert!(!ct_eq(b"abc", b"ab"));
    }
}
