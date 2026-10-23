//! Zeus credentials and the handshake proofs of Zeus spec 05 section 6.4, implemented from the spec.
//!
//! | Value | Definition |
//! |---|---|
//! | `auth_key` | `BLAKE3-derive-key("zeus 2026-10 client auth key v1", token bytes)` |
//! | `server_proof` | `BLAKE3-keyed(ipc_secret, "zeus-server-v1" \|\| client_nonce)` |
//! | `proof` | `BLAKE3-keyed(auth_key, "zeus-client-v1" \|\| credential_id \|\| server_nonce \|\| client_nonce)` |
//!
//! The token never crosses the connection, and the client checks `server_proof` before it sends a proof, so a
//! process that squatted the endpoint learns nothing. None of these types implements `Serialize`; their `Debug`
//! output is redacted.

use std::fmt;
use std::path::Path;

use zeroize::Zeroizing;

use crate::{BridgeError, BridgeResult};

const TOKEN_PREFIX: &str = "zt1_";
const AUTH_KEY_CONTEXT: &str = "zeus 2026-10 client auth key v1";
const SERVER_LABEL: &[u8] = b"zeus-server-v1";
const CLIENT_LABEL: &[u8] = b"zeus-client-v1";

fn parse_hex_32(text: &str) -> Option<[u8; 32]> {
    let bytes = text.as_bytes();
    if bytes.len() != 64 {
        return None;
    }
    let digit = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    };
    let mut out = [0u8; 32];
    for (i, pair) in bytes.chunks_exact(2).enumerate() {
        out[i] = (digit(pair[0])? << 4) | digit(pair[1])?;
    }
    Some(out)
}

fn to_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(char::from(DIGITS[usize::from(b >> 4)]));
        out.push(char::from(DIGITS[usize::from(b & 0x0f)]));
    }
    out
}

/// A fresh 32-byte nonce as 64 lowercase hex digits.
///
/// # Errors
///
/// [`BridgeError::Unavailable`] without an OS random source.
pub fn nonce() -> BridgeResult<String> {
    let mut bytes = Zeroizing::new([0u8; 32]);
    getrandom::fill(&mut bytes[..])
        .map_err(|_| BridgeError::Unavailable("no OS random source".into()))?;
    Ok(to_hex(bytes.as_slice()))
}

/// `zeusd`'s `ipc.secret`: proves the server is this user's daemon.
pub struct IpcSecret(Zeroizing<[u8; 32]>);

impl fmt::Debug for IpcSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("IpcSecret([redacted])")
    }
}

impl IpcSecret {
    /// Parses the file form: 64 lowercase hex digits and a line feed.
    ///
    /// # Errors
    ///
    /// [`BridgeError::NotLinked`] when malformed.
    pub fn parse(text: &str) -> BridgeResult<Self> {
        parse_hex_32(text.trim())
            .map(|b| Self(Zeroizing::new(b)))
            .ok_or_else(|| BridgeError::NotLinked("zeusd's ipc.secret is malformed".into()))
    }

    /// Reads `<zeus state>/ipc.secret`.
    ///
    /// # Errors
    ///
    /// [`BridgeError::NotLinked`] when missing or malformed.
    pub fn read(path: &Path) -> BridgeResult<Self> {
        let text = Zeroizing::new(std::fs::read_to_string(path).map_err(|e| {
            BridgeError::NotLinked(format!("cannot read zeusd's ipc.secret: {:?}", e.kind()))
        })?);
        Self::parse(&text)
    }

    /// The server proof for a client nonce.
    #[must_use]
    pub fn server_proof(&self, client_nonce: &str) -> String {
        let mut h = blake3::Hasher::new_keyed(&self.0);
        h.update(SERVER_LABEL);
        h.update(client_nonce.as_bytes());
        h.finalize().to_hex().to_string()
    }

    /// Constant-time check of a server proof.
    #[must_use]
    pub fn verify_server_proof(&self, client_nonce: &str, given: &str) -> bool {
        match (
            blake3::Hash::from_hex(self.server_proof(client_nonce)),
            blake3::Hash::from_hex(given),
        ) {
            (Ok(a), Ok(b)) => a == b,
            _ => false,
        }
    }
}

/// The service credential Zeus provisioned for Ogun: `<credential_id> <token>` in one file.
pub struct ZeusCredential {
    id: String,
    token: Zeroizing<[u8; 32]>,
}

impl fmt::Debug for ZeusCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ZeusCredential")
            .field("id", &self.id)
            .field("token", &"[redacted]")
            .finish()
    }
}

impl ZeusCredential {
    /// Parses the file form `<cli_ id> zt1_<64 hex>` (Zeus 05 section 6.5).
    ///
    /// # Errors
    ///
    /// [`BridgeError::NotLinked`] when malformed; the message never contains the token.
    pub fn parse(text: &str) -> BridgeResult<Self> {
        let bad = || BridgeError::NotLinked("the Zeus credential file is malformed".into());
        let mut parts = text.split_whitespace();
        let id = parts.next().ok_or_else(bad)?;
        let token = parts.next().ok_or_else(bad)?;
        if parts.next().is_some() {
            return Err(bad());
        }
        let body = id.strip_prefix("cli_").ok_or_else(bad)?;
        if body.len() != 26 || !body.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return Err(bad());
        }
        let bytes = token
            .strip_prefix(TOKEN_PREFIX)
            .and_then(parse_hex_32)
            .ok_or_else(bad)?;
        Ok(Self {
            id: id.to_owned(),
            token: Zeroizing::new(bytes),
        })
    }

    /// Reads the credential file a person pointed Ogun at.
    ///
    /// # Errors
    ///
    /// [`BridgeError::NotLinked`].
    pub fn read(path: &Path) -> BridgeResult<Self> {
        let text = Zeroizing::new(std::fs::read_to_string(path).map_err(|e| {
            BridgeError::NotLinked(format!(
                "cannot read the Zeus service credential: {:?}",
                e.kind()
            ))
        })?);
        Self::parse(&text)
    }

    /// The credential id (`cli_...`). Not secret.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    fn auth_key(&self) -> Zeroizing<[u8; 32]> {
        Zeroizing::new(blake3::derive_key(AUTH_KEY_CONTEXT, self.token.as_slice()))
    }

    /// The client proof for this connection's nonces.
    #[must_use]
    pub fn proof(&self, server_nonce: &str, client_nonce: &str) -> String {
        let key = self.auth_key();
        let mut h = blake3::Hasher::new_keyed(&key);
        h.update(CLIENT_LABEL);
        h.update(self.id.as_bytes());
        h.update(server_nonce.as_bytes());
        h.update(client_nonce.as_bytes());
        h.finalize().to_hex().to_string()
    }

    #[cfg(test)]
    fn auth_key_hex(&self) -> String {
        to_hex(self.auth_key().as_slice())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The test vector of Zeus spec 05 section 6.4.
    #[test]
    fn zeus_handshake_vector() {
        let secret =
            IpcSecret::parse("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f\n")
                .unwrap();
        let cred = ZeusCredential::parse(
            "cli_01M45KGHR0ED0WJDFXKG49029B zt1_202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f\n",
        )
        .unwrap();
        let client_nonce = "6c0f2e1b9a7d43c58e1f0a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f";
        let server_nonce = "d2a4c6e8f0123456789abcdef0123456789abcdef0123456789abcdef0123456";
        assert_eq!(
            cred.auth_key_hex(),
            "6658313f2d9f55b56a04620d4e70827b6e3c8a363d8934590749472b7430000a"
        );
        assert_eq!(
            secret.server_proof(client_nonce),
            "feef3250296ce13ec3a4cf668322916856528fe7e77e56f7b7eabf9eaaeea7b2"
        );
        assert!(secret.verify_server_proof(
            client_nonce,
            "feef3250296ce13ec3a4cf668322916856528fe7e77e56f7b7eabf9eaaeea7b2"
        ));
        assert_eq!(
            cred.proof(server_nonce, client_nonce),
            "a7bf03456a5719e1733783d8945e63cda3e4ffde13bed92018d9f6c9499adcf2"
        );
        assert!(!format!("{cred:?}").contains("zt1_"));
    }

    #[test]
    fn malformed_credentials_are_refused_without_echo() {
        let e =
            ZeusCredential::parse("cli_01M45KGHR0ED0WJDFXKG49029B zt1_ZEUS-CANARY-").unwrap_err();
        assert!(!e.to_string().contains("ZEUS-CANARY-"));
        assert!(ZeusCredential::parse("").is_err());
    }
}
