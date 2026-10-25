//! The gateway's Ed25519 identity (O-11 section 2).
//!
//! The key pair is created on first open and kept in `edge.key` (owner-only). A person enrols the public key on a
//! hub trust surface (`ogun edges enroll <name> <public_key>` on the hub's terminal); the hub answers with an
//! `edg_` id, which the person enters in the gateway settings. The gateway signs every `sync.offer` with this key;
//! the hub's daemon verifies the signature, so the HTTPS bridge never has to be trusted with the edge's word.

use std::fs;
use std::io::Write;
use std::path::Path;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ed25519_dalek::{Signer, SigningKey};
use zeroize::Zeroizing;

use crate::error::{GatewayError, GatewayResult};

/// The signing key. Never serialised, never printed.
pub struct EdgeKey {
    key: SigningKey,
}

impl std::fmt::Debug for EdgeKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EdgeKey")
            .field("public_key", &self.public_key())
            .finish_non_exhaustive()
    }
}

fn hex(bytes: &[u8]) -> Zeroizing<String> {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = Zeroizing::new(String::with_capacity(bytes.len() * 2));
    for b in bytes {
        out.push(char::from(DIGITS[usize::from(b >> 4)]));
        out.push(char::from(DIGITS[usize::from(b & 0x0f)]));
    }
    out
}

fn unhex_32(text: &str) -> Option<Zeroizing<[u8; 32]>> {
    let t = text.trim().as_bytes();
    if t.len() != 64 {
        return None;
    }
    let digit = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    };
    let mut out = Zeroizing::new([0u8; 32]);
    for (i, pair) in t.as_chunks::<2>().0.iter().enumerate() {
        out[i] = (digit(pair[0])? << 4) | digit(pair[1])?;
    }
    Some(out)
}

fn write_private(path: &Path, text: &str) -> GatewayResult<()> {
    let tmp = path.with_extension("key.tmp");
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut f = options.open(&tmp)?;
    f.write_all(text.as_bytes())?;
    f.sync_all()?;
    drop(f);
    fs::rename(&tmp, path)?;
    Ok(())
}

impl EdgeKey {
    /// Loads `path`, or creates a fresh key there.
    ///
    /// # Errors
    ///
    /// Storage errors, a corrupt key file, or no OS random source.
    pub fn load_or_create(path: &Path) -> GatewayResult<Self> {
        if path.exists() {
            let text = Zeroizing::new(fs::read_to_string(path)?);
            let bytes = unhex_32(&text)
                .ok_or_else(|| GatewayError::Storage("the edge key file is corrupt".into()))?;
            return Ok(Self {
                key: SigningKey::from_bytes(&bytes),
            });
        }
        let mut seed = Zeroizing::new([0u8; 32]);
        getrandom::fill(&mut seed[..]).map_err(|_| GatewayError::Random)?;
        let key = SigningKey::from_bytes(&seed);
        write_private(path, &hex(&seed[..]))?;
        Ok(Self { key })
    }

    /// The public key, base64url without padding, as `edges.enroll` takes it.
    #[must_use]
    pub fn public_key(&self) -> String {
        URL_SAFE_NO_PAD.encode(self.key.verifying_key().to_bytes())
    }

    /// The first 16 hex digits of BLAKE3 over the public key, for a person to compare on both screens.
    #[must_use]
    pub fn fingerprint(&self) -> String {
        let digest = blake3::hash(&self.key.verifying_key().to_bytes());
        digest.to_hex().as_str()[..16].to_owned()
    }

    /// An Ed25519 signature over `message`, base64url without padding.
    #[must_use]
    pub fn sign(&self, message: &[u8]) -> String {
        URL_SAFE_NO_PAD.encode(self.key.sign(message).to_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};

    #[test]
    fn key_survives_reopen_and_signatures_verify() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("edge.key");
        let a = EdgeKey::load_or_create(&path).unwrap();
        let b = EdgeKey::load_or_create(&path).unwrap();
        assert_eq!(a.public_key(), b.public_key());
        assert!(!format!("{a:?}").contains(&*hex(&a.key.to_bytes())));
        let sig = a.sign(b"offer");
        let pk: [u8; 32] = URL_SAFE_NO_PAD
            .decode(a.public_key())
            .unwrap()
            .try_into()
            .unwrap();
        let sig: [u8; 64] = URL_SAFE_NO_PAD.decode(sig).unwrap().try_into().unwrap();
        VerifyingKey::from_bytes(&pk)
            .unwrap()
            .verify(b"offer", &Signature::from_bytes(&sig))
            .unwrap();
    }
}
