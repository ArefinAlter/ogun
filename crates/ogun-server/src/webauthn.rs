//! WebAuthn for approvers (O-13 section 8, O-15 section 7): passkey registration with attestation verification
//! (formats `none` and `packed`, ES256 only) and assertion verification for approval decisions.
//!
//! The bridge verifies every assertion before relaying it, and the daemon verifies it again with the key a person
//! confirmed on the host terminal. Attestation is checked for internal consistency (signatures, certificate
//! requirements of WebAuthn section 8.2.1); it is not chained to a vendor root, because the trust anchor for an
//! approver key is the host confirmation, not the authenticator's make.

use ciborium::Value;
use der::{Decode as _, Encode as _};
use p256::ecdsa::signature::Verifier as _;
use p256::ecdsa::{Signature, VerifyingKey};
use p256::pkcs8::DecodePublicKey as _;
use serde::Deserialize;
use sha2::{Digest as _, Sha256};

use crate::error::{ServerError, ServerResult};
use crate::util::{b64url, b64url_decode, ct_eq};

const FLAG_UP: u8 = 0x01;
const FLAG_UV: u8 = 0x04;
const FLAG_AT: u8 = 0x40;
/// COSE algorithm ES256.
pub const ALG_ES256: i64 = -7;

fn sha256(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

/// The challenge of an approval decision: `BLAKE3(approval_id || request_hash || decision)` over the UTF-8 text of
/// the three values, no separators (the id and hash have fixed shapes).
#[must_use]
pub fn approval_challenge(approval_id: &str, request_hash: &str, decision: &str) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(approval_id.as_bytes());
    h.update(request_hash.as_bytes());
    h.update(decision.as_bytes());
    *h.finalize().as_bytes()
}

/// The fingerprint a person compares on the host: the first 16 hex digits of BLAKE3 over the uncompressed SEC1
/// public key.
#[must_use]
pub fn fingerprint(sec1: &[u8]) -> String {
    blake3::hash(sec1).to_hex()[..16].to_owned()
}

#[derive(Debug, Deserialize)]
struct ClientData {
    #[serde(rename = "type")]
    kind: String,
    challenge: String,
    origin: String,
    #[serde(default, rename = "crossOrigin")]
    cross_origin: Option<bool>,
}

fn check_client_data(
    raw: &[u8],
    kind: &str,
    challenge: &[u8],
    origin: &str,
) -> ServerResult<[u8; 32]> {
    let c: ClientData = serde_json::from_slice(raw)
        .map_err(|_| ServerError::webauthn("clientDataJSON is not valid JSON"))?;
    if c.kind != kind {
        return Err(ServerError::webauthn(format!(
            "clientDataJSON.type must be {kind}"
        )));
    }
    let got = b64url_decode(&c.challenge, "clientDataJSON.challenge")?;
    if !ct_eq(&got, challenge) {
        return Err(ServerError::webauthn(
            "clientDataJSON.challenge does not match",
        ));
    }
    if c.origin != origin {
        return Err(ServerError::webauthn(
            "clientDataJSON.origin is not this server",
        ));
    }
    if c.cross_origin == Some(true) {
        return Err(ServerError::webauthn("cross-origin ceremonies are refused"));
    }
    Ok(sha256(raw))
}

/// Parsed authenticator data.
#[derive(Debug)]
struct AuthData {
    rp_id_hash: [u8; 32],
    flags: u8,
    sign_count: u32,
    attested: Option<Attested>,
}

#[derive(Debug)]
struct Attested {
    aaguid: [u8; 16],
    credential_id: Vec<u8>,
    sec1: Vec<u8>,
}

fn int_of(v: &Value) -> Option<i128> {
    match v {
        Value::Integer(i) => Some(i128::from(*i)),
        _ => None,
    }
}

fn map_get(map: &[(Value, Value)], key: i128) -> Option<&Value> {
    map.iter()
        .find(|(k, _)| int_of(k) == Some(key))
        .map(|(_, v)| v)
}

fn text_get<'a>(map: &'a [(Value, Value)], key: &str) -> Option<&'a Value> {
    map.iter()
        .find(|(k, _)| matches!(k, Value::Text(t) if t == key))
        .map(|(_, v)| v)
}

/// A COSE_Key that must be EC2, P-256, ES256; returns the uncompressed SEC1 point.
fn cose_es256(key: &Value) -> ServerResult<Vec<u8>> {
    let Value::Map(m) = key else {
        return Err(ServerError::webauthn(
            "credential public key is not a COSE map",
        ));
    };
    let bad = || ServerError::webauthn("only ES256 (EC2, P-256) passkeys are accepted");
    if map_get(m, 1).and_then(int_of) != Some(2) {
        return Err(bad());
    }
    if map_get(m, 3).and_then(int_of) != Some(i128::from(ALG_ES256)) {
        return Err(bad());
    }
    if map_get(m, -1).and_then(int_of) != Some(1) {
        return Err(bad());
    }
    let (Some(Value::Bytes(x)), Some(Value::Bytes(y))) = (map_get(m, -2), map_get(m, -3)) else {
        return Err(bad());
    };
    if x.len() != 32 || y.len() != 32 {
        return Err(bad());
    }
    let mut sec1 = Vec::with_capacity(65);
    sec1.push(0x04);
    sec1.extend_from_slice(x);
    sec1.extend_from_slice(y);
    VerifyingKey::from_sec1_bytes(&sec1)
        .map_err(|_| ServerError::webauthn("the public key is not a point on P-256"))?;
    Ok(sec1)
}

fn parse_auth_data(raw: &[u8]) -> ServerResult<AuthData> {
    let short = || ServerError::webauthn("authenticator data is truncated");
    if raw.len() < 37 {
        return Err(short());
    }
    let mut rp_id_hash = [0u8; 32];
    rp_id_hash.copy_from_slice(&raw[..32]);
    let flags = raw[32];
    let sign_count = u32::from_be_bytes([raw[33], raw[34], raw[35], raw[36]]);
    let attested = if flags & FLAG_AT != 0 {
        let rest = &raw[37..];
        if rest.len() < 18 {
            return Err(short());
        }
        let mut aaguid = [0u8; 16];
        aaguid.copy_from_slice(&rest[..16]);
        let len = usize::from(u16::from_be_bytes([rest[16], rest[17]]));
        if len > 1023 || rest.len() < 18 + len {
            return Err(short());
        }
        let credential_id = rest[18..18 + len].to_vec();
        let mut cursor = std::io::Cursor::new(&rest[18 + len..]);
        let key: Value = ciborium::de::from_reader(&mut cursor)
            .map_err(|_| ServerError::webauthn("credential public key is not CBOR"))?;
        Some(Attested {
            aaguid,
            credential_id,
            sec1: cose_es256(&key)?,
        })
    } else {
        None
    };
    Ok(AuthData {
        rp_id_hash,
        flags,
        sign_count,
        attested,
    })
}

fn check_flags_and_rp(a: &AuthData, rp_id: &str) -> ServerResult<()> {
    if !ct_eq(&a.rp_id_hash, &sha256(rp_id.as_bytes())) {
        return Err(ServerError::webauthn(
            "rpIdHash does not match this server's relying party id",
        ));
    }
    if a.flags & FLAG_UP == 0 {
        return Err(ServerError::webauthn("the user-present flag is not set"));
    }
    if a.flags & FLAG_UV == 0 {
        return Err(ServerError::webauthn(
            "the user-verified flag is not set (the authenticator must verify the person)",
        ));
    }
    Ok(())
}

fn verify_es256(key: &VerifyingKey, message: &[u8], der_sig: &[u8]) -> ServerResult<()> {
    let sig = Signature::from_der(der_sig)
        .map_err(|_| ServerError::webauthn("the signature is not DER ECDSA"))?;
    key.verify(message, &sig)
        .map_err(|_| ServerError::webauthn("the signature does not verify"))
}

const OID_OU: der::asn1::ObjectIdentifier = der::asn1::ObjectIdentifier::new_unwrap("2.5.4.11");
const OID_BASIC_CONSTRAINTS: der::asn1::ObjectIdentifier =
    der::asn1::ObjectIdentifier::new_unwrap("2.5.29.19");
const OID_FIDO_AAGUID: der::asn1::ObjectIdentifier =
    der::asn1::ObjectIdentifier::new_unwrap("1.3.6.1.4.1.45724.1.1.4");

fn attr_text(v: &der::Any) -> Option<String> {
    if let Ok(s) = v.decode_as::<der::asn1::Utf8StringRef<'_>>() {
        return Some(s.as_str().to_owned());
    }
    if let Ok(s) = v.decode_as::<der::asn1::PrintableStringRef<'_>>() {
        return Some(s.as_str().to_owned());
    }
    None
}

/// Packed attestation with an attestation certificate (WebAuthn section 8.2).
fn verify_packed_x5c(
    cert_der: &[u8],
    signed: &[u8],
    sig: &[u8],
    aaguid: &[u8; 16],
) -> ServerResult<()> {
    let cert = x509_cert::Certificate::from_der(cert_der)
        .map_err(|_| ServerError::webauthn("the attestation certificate is not DER X.509"))?;
    let tbs = &cert.tbs_certificate;
    if tbs.version != x509_cert::certificate::Version::V3 {
        return Err(ServerError::webauthn(
            "the attestation certificate must be X.509 v3",
        ));
    }
    let ou = tbs
        .subject
        .0
        .iter()
        .flat_map(|rdn| rdn.0.iter())
        .find(|atv| atv.oid == OID_OU)
        .and_then(|atv| attr_text(&atv.value));
    if ou.as_deref() != Some("Authenticator Attestation") {
        return Err(ServerError::webauthn(
            "the attestation certificate's OU must be \"Authenticator Attestation\"",
        ));
    }
    for ext in tbs.extensions.iter().flatten() {
        if ext.extn_id == OID_BASIC_CONSTRAINTS {
            let bc = x509_cert::ext::pkix::BasicConstraints::from_der(ext.extn_value.as_bytes())
                .map_err(|_| ServerError::webauthn("bad basic constraints"))?;
            if bc.ca {
                return Err(ServerError::webauthn(
                    "the attestation certificate must not be a CA",
                ));
            }
        }
        if ext.extn_id == OID_FIDO_AAGUID {
            if ext.critical {
                return Err(ServerError::webauthn(
                    "the AAGUID extension must not be critical",
                ));
            }
            let inner = der::asn1::OctetString::from_der(ext.extn_value.as_bytes())
                .map_err(|_| ServerError::webauthn("bad AAGUID extension"))?;
            if inner.as_bytes() != aaguid {
                return Err(ServerError::webauthn(
                    "the certificate's AAGUID does not match the authenticator data",
                ));
            }
        }
    }
    let spki = tbs
        .subject_public_key_info
        .to_der()
        .map_err(|_| ServerError::webauthn("bad attestation key"))?;
    let key = VerifyingKey::from_public_key_der(&spki).map_err(|_| {
        ServerError::webauthn("the attestation certificate key must be P-256 (ES256)")
    })?;
    verify_es256(&key, signed, sig)
}

/// A verified registration.
#[derive(Debug, Clone)]
pub struct Registration {
    /// Credential id, base64url.
    pub credential_id: String,
    /// Uncompressed SEC1 P-256 public key.
    pub sec1: Vec<u8>,
    /// Signature counter at registration.
    pub sign_count: u32,
    /// Attestation format.
    pub fmt: String,
}

/// Verifies a registration (WebAuthn section 7.1) for `rp_id` and `origin` against the challenge the server
/// issued. All inputs are base64url.
///
/// # Errors
///
/// `server.webauthn_invalid` naming the failed check.
pub fn verify_registration(
    rp_id: &str,
    origin: &str,
    challenge: &[u8],
    credential_id: &str,
    client_data_json: &str,
    attestation_object: &str,
) -> ServerResult<Registration> {
    let cdata = b64url_decode(client_data_json, "client_data_json")?;
    let client_hash = check_client_data(&cdata, "webauthn.create", challenge, origin)?;
    let att = b64url_decode(attestation_object, "attestation_object")?;
    let obj: Value = ciborium::de::from_reader(att.as_slice())
        .map_err(|_| ServerError::webauthn("attestationObject is not CBOR"))?;
    let Value::Map(obj) = obj else {
        return Err(ServerError::webauthn("attestationObject is not a map"));
    };
    let Some(Value::Text(fmt)) = text_get(&obj, "fmt") else {
        return Err(ServerError::webauthn("attestationObject has no fmt"));
    };
    let Some(Value::Bytes(auth_raw)) = text_get(&obj, "authData") else {
        return Err(ServerError::webauthn("attestationObject has no authData"));
    };
    let Some(Value::Map(stmt)) = text_get(&obj, "attStmt") else {
        return Err(ServerError::webauthn("attestationObject has no attStmt"));
    };
    let auth = parse_auth_data(auth_raw)?;
    check_flags_and_rp(&auth, rp_id)?;
    let attested = auth
        .attested
        .as_ref()
        .ok_or_else(|| ServerError::webauthn("authData carries no attested credential"))?;
    let claimed = b64url_decode(credential_id, "credential_id")?;
    if !ct_eq(&claimed, &attested.credential_id) {
        return Err(ServerError::webauthn(
            "credential_id does not match the authenticator data",
        ));
    }
    let mut signed = auth_raw.clone();
    signed.extend_from_slice(&client_hash);
    match fmt.as_str() {
        "none" => {
            if !stmt.is_empty() {
                return Err(ServerError::webauthn(
                    "format none must have an empty attStmt",
                ));
            }
        }
        "packed" => {
            if text_get(stmt, "alg").and_then(int_of) != Some(i128::from(ALG_ES256)) {
                return Err(ServerError::webauthn("packed attestation must use ES256"));
            }
            let Some(Value::Bytes(sig)) = text_get(stmt, "sig") else {
                return Err(ServerError::webauthn("packed attestation has no sig"));
            };
            match text_get(stmt, "x5c") {
                Some(Value::Array(chain)) => {
                    let Some(Value::Bytes(leaf)) = chain.first() else {
                        return Err(ServerError::webauthn("packed x5c is empty"));
                    };
                    verify_packed_x5c(leaf, &signed, sig, &attested.aaguid)?;
                }
                Some(_) => return Err(ServerError::webauthn("packed x5c is not an array")),
                None => {
                    let key = VerifyingKey::from_sec1_bytes(&attested.sec1)
                        .map_err(|_| ServerError::webauthn("bad credential key"))?;
                    verify_es256(&key, &signed, sig)?;
                }
            }
        }
        other => {
            return Err(ServerError::webauthn(format!(
                "attestation format {other} is not supported; ask for none or packed"
            )));
        }
    }
    Ok(Registration {
        credential_id: b64url(&attested.credential_id),
        sec1: attested.sec1.clone(),
        sign_count: auth.sign_count,
        fmt: fmt.clone(),
    })
}

/// Verifies an assertion (WebAuthn section 7.2) with a stored key; returns the new signature counter.
///
/// # Errors
///
/// `server.webauthn_invalid` naming the failed check.
pub fn verify_assertion(
    rp_id: &str,
    origin: &str,
    challenge: &[u8],
    sec1: &[u8],
    client_data_json: &str,
    authenticator_data: &str,
    signature: &str,
) -> ServerResult<u32> {
    let cdata = b64url_decode(client_data_json, "client_data_json")?;
    let client_hash = check_client_data(&cdata, "webauthn.get", challenge, origin)?;
    let auth_raw = b64url_decode(authenticator_data, "authenticator_data")?;
    let auth = parse_auth_data(&auth_raw)?;
    check_flags_and_rp(&auth, rp_id)?;
    let sig = b64url_decode(signature, "signature")?;
    let key = VerifyingKey::from_sec1_bytes(sec1)
        .map_err(|_| ServerError::internal("stored passkey is not a P-256 key"))?;
    let mut signed = auth_raw;
    signed.extend_from_slice(&client_hash);
    verify_es256(&key, &signed, &sig)?;
    Ok(auth.sign_count)
}

#[cfg(test)]
mod tests {
    use p256::ecdsa::SigningKey;
    use p256::ecdsa::signature::Signer as _;

    use super::*;

    const RP: &str = "ogun.example.com";
    const ORIGIN: &str = "https://ogun.example.com";

    fn key() -> SigningKey {
        SigningKey::from_slice(&[7u8; 32]).unwrap()
    }

    fn cose(vk: &VerifyingKey) -> Vec<u8> {
        let point = vk.to_encoded_point(false);
        let v = Value::Map(vec![
            (Value::Integer(1.into()), Value::Integer(2.into())),
            (Value::Integer(3.into()), Value::Integer((-7).into())),
            (Value::Integer((-1).into()), Value::Integer(1.into())),
            (
                Value::Integer((-2).into()),
                Value::Bytes(point.x().unwrap().to_vec()),
            ),
            (
                Value::Integer((-3).into()),
                Value::Bytes(point.y().unwrap().to_vec()),
            ),
        ]);
        let mut out = Vec::new();
        ciborium::ser::into_writer(&v, &mut out).unwrap();
        out
    }

    fn auth_data(flags: u8, count: u32, cred: Option<(&[u8], &VerifyingKey)>) -> Vec<u8> {
        let mut a = sha256(RP.as_bytes()).to_vec();
        a.push(flags);
        a.extend_from_slice(&count.to_be_bytes());
        if let Some((id, vk)) = cred {
            a.extend_from_slice(&[0u8; 16]);
            a.extend_from_slice(&u16::try_from(id.len()).unwrap().to_be_bytes());
            a.extend_from_slice(id);
            a.extend_from_slice(&cose(vk));
        }
        a
    }

    fn client_data(kind: &str, challenge: &[u8]) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "type": kind, "challenge": b64url(challenge), "origin": ORIGIN, "crossOrigin": false
        }))
        .unwrap()
    }

    fn att_object(fmt: &str, auth: &[u8], stmt: Vec<(Value, Value)>) -> Vec<u8> {
        let v = Value::Map(vec![
            (Value::Text("fmt".into()), Value::Text(fmt.into())),
            (Value::Text("attStmt".into()), Value::Map(stmt)),
            (Value::Text("authData".into()), Value::Bytes(auth.to_vec())),
        ]);
        let mut out = Vec::new();
        ciborium::ser::into_writer(&v, &mut out).unwrap();
        out
    }

    #[test]
    fn registers_none_and_packed_self_and_asserts() {
        let sk = key();
        let vk = *sk.verifying_key();
        let challenge = [9u8; 32];
        let cred = b"credential-1";
        let auth = auth_data(FLAG_UP | FLAG_UV | FLAG_AT, 0, Some((cred, &vk)));
        let cdata = client_data("webauthn.create", &challenge);

        let none = att_object("none", &auth, vec![]);
        let reg = verify_registration(
            RP,
            ORIGIN,
            &challenge,
            &b64url(cred),
            &b64url(&cdata),
            &b64url(&none),
        )
        .unwrap();
        assert_eq!(reg.sec1, vk.to_encoded_point(false).as_bytes());
        assert_eq!(reg.fmt, "none");

        let mut signed = auth.clone();
        signed.extend_from_slice(&sha256(&cdata));
        let sig: Signature = sk.sign(&signed);
        let packed = att_object(
            "packed",
            &auth,
            vec![
                (Value::Text("alg".into()), Value::Integer((-7).into())),
                (
                    Value::Text("sig".into()),
                    Value::Bytes(sig.to_der().as_bytes().to_vec()),
                ),
            ],
        );
        verify_registration(
            RP,
            ORIGIN,
            &challenge,
            &b64url(cred),
            &b64url(&cdata),
            &b64url(&packed),
        )
        .unwrap();

        // Wrong challenge, wrong origin and a missing UV flag all fail.
        assert!(
            verify_registration(
                RP,
                ORIGIN,
                &[1u8; 32],
                &b64url(cred),
                &b64url(&cdata),
                &b64url(&none)
            )
            .is_err()
        );
        assert!(
            verify_registration(
                RP,
                "https://evil.example",
                &challenge,
                &b64url(cred),
                &b64url(&cdata),
                &b64url(&none)
            )
            .is_err()
        );
        let no_uv = att_object(
            "none",
            &auth_data(FLAG_UP | FLAG_AT, 0, Some((cred, &vk))),
            vec![],
        );
        assert!(
            verify_registration(
                RP,
                ORIGIN,
                &challenge,
                &b64url(cred),
                &b64url(&cdata),
                &b64url(&no_uv)
            )
            .is_err()
        );

        // An assertion over an approval challenge.
        let ch = approval_challenge(
            "oap_01J00000000000000000000000",
            &"ab".repeat(32),
            "approve",
        );
        let a = auth_data(FLAG_UP | FLAG_UV, 1, None);
        let cd = client_data("webauthn.get", &ch);
        let mut msg = a.clone();
        msg.extend_from_slice(&sha256(&cd));
        let s: Signature = sk.sign(&msg);
        let count = verify_assertion(
            RP,
            ORIGIN,
            &ch,
            &reg.sec1,
            &b64url(&cd),
            &b64url(&a),
            &b64url(s.to_der().as_bytes()),
        )
        .unwrap();
        assert_eq!(count, 1);
        let other = approval_challenge("oap_01J00000000000000000000000", &"ab".repeat(32), "deny");
        assert!(
            verify_assertion(
                RP,
                ORIGIN,
                &other,
                &reg.sec1,
                &b64url(&cd),
                &b64url(&a),
                &b64url(s.to_der().as_bytes())
            )
            .is_err()
        );
        assert_eq!(fingerprint(&reg.sec1).len(), 16);
    }
}
