//! A passphrase-encrypted vault: one JSON file, XChaCha20-Poly1305 over the whole map, key from Argon2id.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use argon2::{Algorithm, Argon2, Params, Version};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::{KeystoreError, KeystoreResult, valid_name};

/// Argon2id cost: 19 MiB, 2 passes, 1 lane (the OWASP minimum profile).
const MEM_KIB: u32 = 19 * 1024;
const PASSES: u32 = 2;
const AAD: &[u8] = b"ogun-keystore-v1";

#[derive(Serialize, Deserialize)]
struct Header {
    version: u32,
    salt: String,
    nonce: String,
    ciphertext: String,
}

/// An opened vault. The decrypted map lives in memory, zeroizing, behind a lock.
pub struct EncryptedFile {
    path: PathBuf,
    key: Zeroizing<[u8; 32]>,
    salt: [u8; 16],
    values: Mutex<BTreeMap<String, Zeroizing<String>>>,
}

fn derive(passphrase: &str, salt: &[u8]) -> KeystoreResult<Zeroizing<[u8; 32]>> {
    let params =
        Params::new(MEM_KIB, PASSES, 1, Some(32)).map_err(|e| KeystoreError::Io(e.to_string()))?;
    let mut key = Zeroizing::new([0u8; 32]);
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(passphrase.as_bytes(), salt, key.as_mut())
        .map_err(|e| KeystoreError::Io(e.to_string()))?;
    Ok(key)
}

impl EncryptedFile {
    /// Opens the vault at `path`, creating an empty one when the file does not exist.
    ///
    /// # Errors
    ///
    /// [`KeystoreError::Locked`] when the passphrase does not open an existing file.
    pub fn open(path: &Path, passphrase: &Zeroizing<String>) -> KeystoreResult<Self> {
        match std::fs::read(path) {
            Ok(bytes) => {
                let h: Header =
                    serde_json::from_slice(&bytes).map_err(|_| KeystoreError::Locked)?;
                let salt: [u8; 16] = STANDARD
                    .decode(&h.salt)
                    .ok()
                    .and_then(|v| v.try_into().ok())
                    .ok_or(KeystoreError::Locked)?;
                let nonce = STANDARD
                    .decode(&h.nonce)
                    .map_err(|_| KeystoreError::Locked)?;
                let ct = STANDARD
                    .decode(&h.ciphertext)
                    .map_err(|_| KeystoreError::Locked)?;
                let key = derive(passphrase, &salt)?;
                let cipher = XChaCha20Poly1305::new_from_slice(key.as_ref())
                    .map_err(|_| KeystoreError::Locked)?;
                let nonce =
                    XNonce::try_from(nonce.as_slice()).map_err(|_| KeystoreError::Locked)?;
                let plain = Zeroizing::new(
                    cipher
                        .decrypt(&nonce, Payload { msg: &ct, aad: AAD })
                        .map_err(|_| KeystoreError::Locked)?,
                );
                let map: BTreeMap<String, String> =
                    serde_json::from_slice(&plain).map_err(|_| KeystoreError::Locked)?;
                Ok(Self {
                    path: path.to_owned(),
                    key,
                    salt,
                    values: Mutex::new(
                        map.into_iter()
                            .map(|(k, v)| (k, Zeroizing::new(v)))
                            .collect(),
                    ),
                })
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let mut salt = [0u8; 16];
                getrandom::fill(&mut salt).map_err(|e| KeystoreError::Io(e.to_string()))?;
                let key = derive(passphrase, &salt)?;
                let me = Self {
                    path: path.to_owned(),
                    key,
                    salt,
                    values: Mutex::new(BTreeMap::new()),
                };
                me.save()?;
                Ok(me)
            }
            Err(e) => Err(KeystoreError::Io(e.kind().to_string())),
        }
    }

    fn save(&self) -> KeystoreResult<()> {
        let values = self
            .values
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let plain: BTreeMap<&str, &str> = values
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let plain = Zeroizing::new(
            serde_json::to_vec(&plain).map_err(|e| KeystoreError::Io(e.to_string()))?,
        );
        let mut nonce = [0u8; 24];
        getrandom::fill(&mut nonce).map_err(|e| KeystoreError::Io(e.to_string()))?;
        let cipher = XChaCha20Poly1305::new_from_slice(self.key.as_ref())
            .map_err(|e| KeystoreError::Io(e.to_string()))?;
        let ct = cipher
            .encrypt(
                &XNonce::from(nonce),
                Payload {
                    msg: &plain,
                    aad: AAD,
                },
            )
            .map_err(|_| KeystoreError::Io("encryption failed".into()))?;
        let header = Header {
            version: 1,
            salt: STANDARD.encode(self.salt),
            nonce: STANDARD.encode(nonce),
            ciphertext: STANDARD.encode(ct),
        };
        let bytes = serde_json::to_vec(&header).map_err(|e| KeystoreError::Io(e.to_string()))?;
        let tmp = self.path.with_extension("tmp");
        std::fs::write(&tmp, bytes).map_err(|e| KeystoreError::Io(e.kind().to_string()))?;
        std::fs::rename(&tmp, &self.path).map_err(|e| KeystoreError::Io(e.kind().to_string()))
    }

    pub(crate) fn set(&self, name: &str, value: &Zeroizing<String>) -> KeystoreResult<()> {
        if !valid_name(name) {
            return Err(KeystoreError::BadName);
        }
        self.values
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(name.to_owned(), value.clone());
        self.save()
    }

    pub(crate) fn get(&self, name: &str) -> KeystoreResult<Option<Zeroizing<String>>> {
        Ok(self
            .values
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(name)
            .cloned())
    }

    pub(crate) fn delete(&self, name: &str) -> KeystoreResult<bool> {
        let had = self
            .values
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(name)
            .is_some();
        if had {
            self.save()?;
        }
        Ok(had)
    }
}
