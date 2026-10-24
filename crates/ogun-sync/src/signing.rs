//! The signing rule for offers (O-11 section 6.1).
//!
//! The signed message is the ASCII context `ogun-sync-offer-v1`, one zero byte, and then the
//! RFC 8785 (JCS) serialisation of the `offer` member exactly as it is sent:
//!
//! ```text
//! message   = "ogun-sync-offer-v1" || 0x00 || JCS(offer)
//! signature = base64url_nopad(Ed25519-sign(edge_key, message))
//! ```
//!
//! JCS is the canonicalisation `ogun-ir` uses for the workflow hash (`zeus_verify::canonical`), so
//! whatever re-serialises the offer on its way (the server bridge, the daemon's JSON-RPC layer)
//! cannot break the signature as long as it keeps the JSON value. The context string keeps an
//! offer signature from ever verifying as a signature over some other record the same key signs
//! (lineage records, O-11 section 2). Offers hold only strings and integers below 2^53, so the
//! JCS number rules never come into play.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ed25519_dalek::{Signature, VerifyingKey};
use ogun_edge::sync::Offer;
use serde_json::Value;
use zeus_verify::canonical::{check_hashable, to_jcs_bytes};

use crate::keys::EdgeKey;

/// Domain-separation context, followed by one zero byte in the message.
pub const OFFER_CONTEXT: &str = "ogun-sync-offer-v1";

/// Why an offer could not be signed or its signature checked.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SignatureError {
    /// The offer is not hashable JSON (a float, or an integer beyond 2^53 - 1).
    #[error("the offer is not canonical JSON")]
    NotCanonical,
    /// The signature is malformed or does not verify under the key.
    #[error("the offer signature does not verify")]
    Invalid,
}

/// The bytes that are signed for `offer` (a JSON value of an `ogun_edge::sync::Offer`).
///
/// # Errors
///
/// [`SignatureError::NotCanonical`] for values with floats or out-of-range integers.
pub fn offer_message(offer: &Value) -> Result<Vec<u8>, SignatureError> {
    check_hashable(offer).map_err(|_| SignatureError::NotCanonical)?;
    let jcs = to_jcs_bytes(offer).map_err(|_| SignatureError::NotCanonical)?;
    let mut message = Vec::with_capacity(OFFER_CONTEXT.len() + 1 + jcs.len());
    message.extend_from_slice(OFFER_CONTEXT.as_bytes());
    message.push(0);
    message.extend_from_slice(&jcs);
    Ok(message)
}

/// Signs an offer. Returns the JSON value to send as `offer` and the base64url signature.
///
/// # Errors
///
/// [`SignatureError::NotCanonical`] if the offer cannot be serialised canonically (sizes above
/// 2^53 - 1 bytes).
pub fn sign_offer(key: &EdgeKey, offer: &Offer) -> Result<(Value, String), SignatureError> {
    let value = serde_json::to_value(offer).map_err(|_| SignatureError::NotCanonical)?;
    let message = offer_message(&value)?;
    Ok((value, key.sign(&message)))
}

/// Checks `signature` (base64url) over `offer` under `key`, with Ed25519 strict verification
/// (no small-order keys, no malleable signatures).
///
/// # Errors
///
/// [`SignatureError`] when the offer is not canonical or the signature does not verify.
pub fn verify_signature(
    key: &VerifyingKey,
    offer: &Value,
    signature: &str,
) -> Result<(), SignatureError> {
    let message = offer_message(offer)?;
    let bytes = URL_SAFE_NO_PAD
        .decode(signature.trim())
        .map_err(|_| SignatureError::Invalid)?;
    let bytes: [u8; 64] = bytes.try_into().map_err(|_| SignatureError::Invalid)?;
    key.verify_strict(&message, &Signature::from_bytes(&bytes))
        .map_err(|_| SignatureError::Invalid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::parse_public_key;
    use ogun_edge::sync::Item;
    use serde_json::json;

    fn offer() -> Offer {
        Offer {
            edge_id: "edg_01M45MN5M0PZ0D9W0APDCGRNZD".into(),
            items: vec![Item {
                asset: "edg_01M45MN5M0PZ0D9W0APDCGRNZD/air-quality-stream/sensors.window".into(),
                partition: "w20261023T081500Z".into(),
                revision: 1,
                blake3: "a".repeat(64),
                size_bytes: 1234,
            }],
        }
    }

    #[test]
    fn signature_survives_reordering_and_whitespace() {
        let key = EdgeKey::from_seed(&[7; 32]);
        let (value, sig) = sign_offer(&key, &offer()).unwrap();
        let public = parse_public_key(&key.public_key()).unwrap();
        verify_signature(&public, &value, &sig).unwrap();
        // The same value, parsed from text with another member order, still verifies.
        let reordered: Value = serde_json::from_str(
            r#"{ "items": [ { "size_bytes": 1234, "revision": 1, "partition": "w20261023T081500Z",
                 "blake3": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                 "asset": "edg_01M45MN5M0PZ0D9W0APDCGRNZD/air-quality-stream/sensors.window" } ],
                 "edge_id": "edg_01M45MN5M0PZ0D9W0APDCGRNZD" }"#,
        )
        .unwrap();
        verify_signature(&public, &reordered, &sig).unwrap();
    }

    #[test]
    fn tampering_or_another_key_fails() {
        let key = EdgeKey::from_seed(&[7; 32]);
        let (mut value, sig) = sign_offer(&key, &offer()).unwrap();
        let other = parse_public_key(&EdgeKey::from_seed(&[8; 32]).public_key()).unwrap();
        assert_eq!(
            verify_signature(&other, &value, &sig),
            Err(SignatureError::Invalid)
        );
        value["items"][0]["size_bytes"] = json!(1235);
        let public = parse_public_key(&key.public_key()).unwrap();
        assert_eq!(
            verify_signature(&public, &value, &sig),
            Err(SignatureError::Invalid)
        );
        value["items"][0]["size_bytes"] = json!(1.5);
        assert_eq!(
            verify_signature(&public, &value, &sig),
            Err(SignatureError::NotCanonical)
        );
    }
}
