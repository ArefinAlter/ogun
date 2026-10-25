//! Credentials and the mutual challenge-response handshake (O-15 section 4), the same construction as Zeus 05.
//!
//! ```text
//! client                                              ogund
//!   daemon.hello {protocol, client, client_nonce}  ->
//!                <-  {protocol, daemon, server_nonce, server_proof}
//!   verify server_proof = BLAKE3-keyed(ipc_secret, "ogun-server-v1" || client_nonce)
//!   daemon.authenticate {credential_id, proof}  ->
//!                <-  {kind, name, interactive}
//! ```
//!
//! * `ipc_secret` is 32 random bytes in a `0600` file. A process that squatted the endpoint cannot produce
//!   `server_proof`, so the client sends it nothing.
//! * The token never crosses the wire. Both sides derive `auth_key = BLAKE3-derive-key(context, token)`, and
//!   `proof = BLAKE3-keyed(auth_key, "ogun-client-v1" || credential_id || server_nonce || client_nonce)`. Both nonces
//!   are fresh per connection, so a recorded proof does not replay.
//! * Comparisons are constant time (`blake3::Hash` equality).
//! * The credential fixes the [`ClientKind`]; a client never claims it.

use std::fmt;
use std::str::FromStr;

use blake3::Hash;
use ogun_types::{CredentialId, Timestamp};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::error::{ApiError, ApiResult};

const TOKEN_PREFIX: &str = "ot1_";
const AUTH_KEY_CONTEXT: &str = "ogun 2026-10 client auth key v1";
const SERVER_LABEL: &[u8] = b"ogun-server-v1";
const CLIENT_LABEL: &[u8] = b"ogun-client-v1";

/// What a credential's holder is. Fixed when the credential is created, by a person (O-15 section 4.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientKind {
    /// A person at the CLI or in Studio. The only kind that may decide an approval, and only with evidence.
    Human,
    /// An agent: MCP, A2A, data-platform integrations, scripts. Never decides approvals or manages credentials.
    Agent,
    /// A machine producer: the ingest bridge, edge sync, the Zeus session bridge. Pushes records and reads status.
    Service,
    /// An HTTP bridge (`ogun-server`) acting for authenticated users. Decides approvals only with WebAuthn evidence.
    Bridge,
}

impl ClientKind {
    /// The wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Human => "human",
            Self::Agent => "agent",
            Self::Service => "service",
            Self::Bridge => "bridge",
        }
    }
}

impl FromStr for ClientKind {
    type Err = ApiError;

    fn from_str(s: &str) -> ApiResult<Self> {
        match s {
            "human" => Ok(Self::Human),
            "agent" => Ok(Self::Agent),
            "service" => Ok(Self::Service),
            "bridge" => Ok(Self::Bridge),
            _ => Err(ApiError::invalid_params("unknown client kind")),
        }
    }
}

impl fmt::Display for ClientKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Lowercase hex of some bytes.
#[must_use]
pub fn to_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(char::from(DIGITS[usize::from(b >> 4)]));
        out.push(char::from(DIGITS[usize::from(b & 0x0f)]));
    }
    out
}

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
    for (i, pair) in bytes.as_chunks::<2>().0.iter().enumerate() {
        out[i] = (digit(pair[0])? << 4) | digit(pair[1])?;
    }
    Some(out)
}

/// 32 bytes from the OS random source.
///
/// # Errors
///
/// `api.unavailable` when the OS has no entropy source.
pub fn random_32() -> ApiResult<Zeroizing<[u8; 32]>> {
    let mut bytes = Zeroizing::new([0u8; 32]);
    getrandom::fill(&mut bytes[..]).map_err(|_| ApiError::unavailable("no OS random source"))?;
    Ok(bytes)
}

fn hash_eq(expected: &str, given: &str) -> bool {
    match (Hash::from_hex(expected), Hash::from_hex(given)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// A 32-byte nonce in lowercase hex, fresh per connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Nonce(String);

impl Nonce {
    /// A fresh nonce.
    ///
    /// # Errors
    ///
    /// `api.unavailable` without an OS random source.
    pub fn generate() -> ApiResult<Self> {
        Ok(Self(to_hex(random_32()?.as_slice())))
    }

    /// The hex text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Nonce {
    type Error = ApiError;

    fn try_from(text: String) -> ApiResult<Self> {
        if parse_hex_32(&text).is_some() {
            Ok(Self(text))
        } else {
            Err(ApiError::invalid_params(
                "nonce must be 64 lowercase hex digits",
            ))
        }
    }
}

impl From<Nonce> for String {
    fn from(n: Nonce) -> Self {
        n.0
    }
}

/// The secret that proves a server is this user's daemon. Lives in a user-only file and never leaves the machine.
pub struct IpcSecret(Zeroizing<[u8; 32]>);

impl fmt::Debug for IpcSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("IpcSecret([REDACTED])")
    }
}

impl IpcSecret {
    /// A fresh secret, written once by `ogund` on first start.
    ///
    /// # Errors
    ///
    /// `api.unavailable` without an OS random source.
    pub fn generate() -> ApiResult<Self> {
        Ok(Self(random_32()?))
    }

    /// Parses the file form: 64 hex digits, surrounding whitespace allowed.
    ///
    /// # Errors
    ///
    /// `api.unauthenticated` when the file is malformed.
    pub fn from_file_text(text: &str) -> ApiResult<Self> {
        parse_hex_32(text.trim())
            .map(|b| Self(Zeroizing::new(b)))
            .ok_or_else(|| ApiError::new("api.unauthenticated", "ipc secret file is malformed"))
    }

    /// The file form.
    #[must_use]
    pub fn to_file_text(&self) -> Zeroizing<String> {
        Zeroizing::new(format!("{}\n", to_hex(self.0.as_slice())))
    }

    /// `server_proof` for a client nonce.
    #[must_use]
    pub fn server_proof(&self, client_nonce: &Nonce) -> String {
        let mut h = blake3::Hasher::new_keyed(&self.0);
        h.update(SERVER_LABEL);
        h.update(client_nonce.as_str().as_bytes());
        h.finalize().to_hex().to_string()
    }

    /// Verifies a `server_proof` in constant time.
    #[must_use]
    pub fn verify_server_proof(&self, client_nonce: &Nonce, proof: &str) -> bool {
        hash_eq(&self.server_proof(client_nonce), proof)
    }
}

/// The bearer secret a client holds: `ot1_` and 64 hex digits. Zeroized on drop, never printed, never serialised.
pub struct Token(Zeroizing<[u8; 32]>);

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token([REDACTED])")
    }
}

impl Token {
    /// A fresh token.
    ///
    /// # Errors
    ///
    /// `api.unavailable` without an OS random source.
    pub fn generate() -> ApiResult<Self> {
        Ok(Self(random_32()?))
    }

    /// Parses `ot1_<64 hex>`.
    ///
    /// # Errors
    ///
    /// `api.unauthenticated` for anything else.
    pub fn parse(text: &str) -> ApiResult<Self> {
        text.trim()
            .strip_prefix(TOKEN_PREFIX)
            .and_then(parse_hex_32)
            .map(|b| Self(Zeroizing::new(b)))
            .ok_or_else(|| ApiError::new("api.unauthenticated", "malformed token"))
    }

    /// The text form, for the owner-only credential file.
    #[must_use]
    pub fn to_text(&self) -> Zeroizing<String> {
        Zeroizing::new(format!("{TOKEN_PREFIX}{}", to_hex(self.0.as_slice())))
    }

    /// The key both sides derive. The daemon stores this, never the token.
    #[must_use]
    pub fn auth_key(&self) -> AuthKey {
        AuthKey(Zeroizing::new(blake3::derive_key(
            AUTH_KEY_CONTEXT,
            self.0.as_slice(),
        )))
    }
}

/// Key derived from a [`Token`]: the daemon's stored verifier for a credential.
#[derive(Clone)]
pub struct AuthKey(Zeroizing<[u8; 32]>);

impl fmt::Debug for AuthKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AuthKey([REDACTED])")
    }
}

impl AuthKey {
    /// Rebuilds from stored hex.
    ///
    /// # Errors
    ///
    /// `api.unauthenticated` when malformed.
    pub fn from_hex(text: &str) -> ApiResult<Self> {
        parse_hex_32(text.trim())
            .map(|b| Self(Zeroizing::new(b)))
            .ok_or_else(|| ApiError::new("api.unauthenticated", "malformed stored credential"))
    }

    /// Hex, for the daemon's owner-only credential table.
    #[must_use]
    pub fn to_hex(&self) -> Zeroizing<String> {
        Zeroizing::new(to_hex(self.0.as_slice()))
    }

    /// The client's proof for one connection.
    #[must_use]
    pub fn proof(
        &self,
        credential_id: CredentialId,
        server_nonce: &Nonce,
        client_nonce: &Nonce,
    ) -> String {
        let mut h = blake3::Hasher::new_keyed(&self.0);
        h.update(CLIENT_LABEL);
        h.update(credential_id.to_string().as_bytes());
        h.update(server_nonce.as_str().as_bytes());
        h.update(client_nonce.as_str().as_bytes());
        h.finalize().to_hex().to_string()
    }

    /// Verifies a proof in constant time.
    #[must_use]
    pub fn verify(
        &self,
        credential_id: CredentialId,
        server_nonce: &Nonce,
        client_nonce: &Nonce,
        proof: &str,
    ) -> bool {
        hash_eq(
            &self.proof(credential_id, server_nonce, client_nonce),
            proof,
        )
    }
}

/// What a client holds: its credential id, its kind and its token. Read from an owner-only file.
///
/// File form, one line: `<ocl_id> <kind> <name> ot1_<hex>`.
pub struct Credential {
    /// The credential id.
    pub id: CredentialId,
    /// The kind the daemon will give this connection.
    pub kind: ClientKind,
    /// A short label (`cli`, `studio`, `mcp`, `ingest`, ...).
    pub name: String,
    /// The secret.
    pub token: Token,
}

impl fmt::Debug for Credential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credential")
            .field("id", &self.id)
            .field("kind", &self.kind)
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

/// A valid credential name: 1 to 64 of `[a-z0-9_-]`.
#[must_use]
pub fn valid_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name
            .bytes()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-'))
}

impl Credential {
    /// Parses the file form.
    ///
    /// # Errors
    ///
    /// `api.unauthenticated` when malformed.
    pub fn from_file_text(text: &str) -> ApiResult<Self> {
        let bad = || ApiError::new("api.unauthenticated", "credential file is malformed");
        let mut parts = text.split_whitespace();
        let id = parts.next().and_then(|t| t.parse().ok()).ok_or_else(bad)?;
        let kind = parts.next().and_then(|t| t.parse().ok()).ok_or_else(bad)?;
        let name = parts
            .next()
            .filter(|n| valid_name(n))
            .ok_or_else(bad)?
            .to_owned();
        let token = Token::parse(parts.next().ok_or_else(bad)?)?;
        if parts.next().is_some() {
            return Err(bad());
        }
        Ok(Self {
            id,
            kind,
            name,
            token,
        })
    }

    /// The file form.
    #[must_use]
    pub fn to_file_text(&self) -> Zeroizing<String> {
        Zeroizing::new(format!(
            "{} {} {} {}\n",
            self.id,
            self.kind,
            self.name,
            self.token.to_text().as_str()
        ))
    }
}

/// A credential as the daemon stores it: the verifier, not the token.
#[derive(Debug, Clone)]
pub struct StoredCredential {
    /// The id.
    pub id: CredentialId,
    /// Kind fixed at creation.
    pub kind: ClientKind,
    /// Label.
    pub name: String,
    /// Verifier.
    pub auth_key: AuthKey,
    /// When it was created.
    pub created_at: Timestamp,
    /// Revoked credentials stay listed and never authenticate again.
    pub revoked: bool,
}

impl StoredCredential {
    /// The daemon's table line: `<id> <kind> <name> <created_at> <revoked 0|1> <auth_key hex>`.
    #[must_use]
    pub fn to_line(&self) -> Zeroizing<String> {
        Zeroizing::new(format!(
            "{} {} {} {} {} {}\n",
            self.id,
            self.kind,
            self.name,
            self.created_at.to_rfc3339(),
            u8::from(self.revoked),
            self.auth_key.to_hex().as_str()
        ))
    }

    /// Parses one table line.
    ///
    /// # Errors
    ///
    /// `api.unauthenticated` when malformed.
    pub fn from_line(line: &str) -> ApiResult<Self> {
        let bad = || ApiError::new("api.unauthenticated", "credential table is malformed");
        let p: Vec<&str> = line.split_whitespace().collect();
        let [id, kind, name, created, revoked, key] = p.as_slice() else {
            return Err(bad());
        };
        Ok(Self {
            id: id.parse().map_err(|_| bad())?,
            kind: kind.parse().map_err(|_| bad())?,
            name: (*name).to_owned(),
            created_at: Timestamp::parse(created).map_err(|_| bad())?,
            revoked: match *revoked {
                "0" => false,
                "1" => true,
                _ => return Err(bad()),
            },
            auth_key: AuthKey::from_hex(key)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use ogun_types::IdGenerator;

    use super::*;

    #[test]
    fn handshake_round_trip_and_tamper() {
        let secret = IpcSecret::generate().unwrap();
        let cn = Nonce::generate().unwrap();
        let sp = secret.server_proof(&cn);
        assert!(secret.verify_server_proof(&cn, &sp));
        let other = IpcSecret::generate().unwrap();
        assert!(!other.verify_server_proof(&cn, &sp));

        let token = Token::generate().unwrap();
        let id = IdGenerator::new().credential_id();
        let sn = Nonce::generate().unwrap();
        let proof = token.auth_key().proof(id, &sn, &cn);
        assert!(token.auth_key().verify(id, &sn, &cn, &proof));
        let replay_nonce = Nonce::generate().unwrap();
        assert!(!token.auth_key().verify(id, &replay_nonce, &cn, &proof));
        assert!(!token.auth_key().verify(id, &sn, &cn, "zz"));
    }

    #[test]
    fn credential_files_round_trip_and_never_debug_the_token() {
        let cred = Credential {
            id: IdGenerator::new().credential_id(),
            kind: ClientKind::Human,
            name: "cli".into(),
            token: Token::generate().unwrap(),
        };
        let text = cred.to_file_text();
        let back = Credential::from_file_text(&text).unwrap();
        assert_eq!(back.id, cred.id);
        assert_eq!(back.kind, ClientKind::Human);
        let shown = format!("{cred:?}");
        assert!(!shown.contains("ot1_"));
        assert!(Credential::from_file_text("ocl_x human cli ot1_00").is_err());

        let stored = StoredCredential {
            id: cred.id,
            kind: cred.kind,
            name: cred.name.clone(),
            auth_key: cred.token.auth_key(),
            created_at: Timestamp::now(),
            revoked: false,
        };
        let line = stored.to_line();
        let parsed = StoredCredential::from_line(&line).unwrap();
        assert_eq!(parsed.id, stored.id);
        assert!(!format!("{parsed:?}").contains(parsed.auth_key.to_hex().as_str()));
    }
}
