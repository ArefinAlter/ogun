//! The IPC secret and the local credential table (O-15 section 4).
//!
//! `ipc.secret` and every token file are `0600`; `credentials/` is `0700`. The table holds verifiers
//! (`auth_key`), never tokens. On first start the daemon writes the secret and a `human` credential
//! named `cli`, so the person who installed Ogun can use the CLI at once.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;

use ogun_api::auth::{ClientKind, Credential, IpcSecret, StoredCredential, Token, valid_name};
use ogun_api::{ApiError, ApiResult, OgunPaths};
use ogun_types::{CredentialId, IdGenerator, Timestamp};
use zeroize::Zeroizing;

/// Creates a directory (and parents) that only this user can read.
///
/// # Errors
///
/// `api.unavailable` on I/O failure.
pub fn private_dir(path: &Path) -> ApiResult<()> {
    let mut b = std::fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        b.mode(0o700);
    }
    b.create(path).map_err(|e| {
        ApiError::unavailable(format!("cannot create a state directory: {}", e.kind()))
    })
}

/// Writes a file atomically (temp file, fsync, rename) with mode `0600`.
///
/// # Errors
///
/// `api.unavailable` on I/O failure.
pub fn write_private(path: &Path, bytes: &[u8]) -> ApiResult<()> {
    let io = |e: std::io::Error| {
        ApiError::unavailable(format!("cannot write a private file: {}", e.kind()))
    };
    let dir = path
        .parent()
        .ok_or_else(|| ApiError::internal("private file has no parent"))?;
    let tmp = dir.join(format!(
        ".{}.tmp",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("file")
    ));
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(&tmp).map_err(io)?;
    f.write_all(bytes).map_err(io)?;
    f.sync_all().map_err(io)?;
    drop(f);
    std::fs::rename(&tmp, path).map_err(io)
}

/// The credential table, loaded at start and rewritten on every change.
#[derive(Debug)]
pub struct Credentials {
    file: std::path::PathBuf,
    rows: BTreeMap<CredentialId, StoredCredential>,
    /// Rows that live only in this process (a supervising app's per-launch credential); never written to disk.
    transient: std::collections::BTreeSet<CredentialId>,
}

impl Credentials {
    /// Loads the table, creating the IPC secret and the `cli` credential on first start.
    ///
    /// # Errors
    ///
    /// `api.unavailable` or `api.unauthenticated` (a malformed table).
    pub fn load_or_init(paths: &OgunPaths, ids: &IdGenerator) -> ApiResult<(Self, IpcSecret)> {
        private_dir(&paths.state_dir)?;
        private_dir(&paths.credentials_dir())?;
        let secret_path = paths.ipc_secret_file();
        let secret = match std::fs::read_to_string(&secret_path) {
            Ok(text) => IpcSecret::from_file_text(&Zeroizing::new(text))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let s = IpcSecret::generate()?;
                write_private(&secret_path, s.to_file_text().as_bytes())?;
                s
            }
            Err(e) => {
                return Err(ApiError::unavailable(format!(
                    "cannot read the ipc secret: {}",
                    e.kind()
                )));
            }
        };
        let file = paths.state_dir.join("credentials.table");
        let mut rows = BTreeMap::new();
        match std::fs::read_to_string(&file) {
            Ok(text) => {
                let text = Zeroizing::new(text);
                for line in text.lines().filter(|l| !l.trim().is_empty()) {
                    let c = StoredCredential::from_line(line)?;
                    rows.insert(c.id, c);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(ApiError::unavailable(format!(
                    "cannot read credentials: {}",
                    e.kind()
                )));
            }
        }
        let mut me = Self {
            file,
            rows,
            transient: std::collections::BTreeSet::new(),
        };
        if me.rows.is_empty() {
            me.add(paths, ids, "cli", ClientKind::Human)?;
        }
        Ok((me, secret))
    }

    fn save(&self) -> ApiResult<()> {
        let mut text = Zeroizing::new(String::new());
        for c in self
            .rows
            .values()
            .filter(|c| !self.transient.contains(&c.id))
        {
            text.push_str(&c.to_line());
        }
        write_private(&self.file, text.as_bytes())
    }

    /// A live credential by id.
    #[must_use]
    pub fn live(&self, id: CredentialId) -> Option<&StoredCredential> {
        self.rows.get(&id).filter(|c| !c.revoked)
    }

    /// Every credential.
    pub fn all(&self) -> impl Iterator<Item = &StoredCredential> {
        self.rows.values()
    }

    /// Creates a credential and writes its token file. Returns the stored row and the file path.
    ///
    /// # Errors
    ///
    /// `api.invalid_params` for a bad name, `credential.name_taken`, or an I/O error.
    pub fn add(
        &mut self,
        paths: &OgunPaths,
        ids: &IdGenerator,
        name: &str,
        kind: ClientKind,
    ) -> ApiResult<(StoredCredential, std::path::PathBuf)> {
        if !valid_name(name) {
            return Err(ApiError::invalid_params(
                "credential names are 1 to 64 of [a-z0-9_-]",
            ));
        }
        if self.rows.values().any(|c| !c.revoked && c.name == name) {
            return Err(ApiError::new(
                "credential.name_taken",
                "a live credential has that name",
            ));
        }
        let cred = Credential {
            id: ids.credential_id(),
            kind,
            name: name.to_owned(),
            token: Token::generate()?,
        };
        let path = paths.credential_file(name);
        write_private(&path, cred.to_file_text().as_bytes())?;
        let stored = StoredCredential {
            id: cred.id,
            kind,
            name: name.to_owned(),
            auth_key: cred.token.auth_key(),
            created_at: Timestamp::now(),
            revoked: false,
        };
        self.rows.insert(stored.id, stored.clone());
        self.save()?;
        Ok((stored, path))
    }

    /// Makes sure a live credential of this name and kind exists (and its token file), creating it when not.
    ///
    /// # Errors
    ///
    /// As [`Credentials::add`].
    pub fn ensure(
        &mut self,
        paths: &OgunPaths,
        ids: &IdGenerator,
        name: &str,
        kind: ClientKind,
    ) -> ApiResult<()> {
        if self
            .rows
            .values()
            .any(|c| !c.revoked && c.name == name && c.kind == kind)
            && paths.credential_file(name).exists()
        {
            return Ok(());
        }
        if let Some(old) = self
            .rows
            .values()
            .find(|c| !c.revoked && c.name == name)
            .map(|c| c.id)
        {
            self.revoke(paths, old)?;
        }
        self.add(paths, ids, name, kind).map(|_| ())
    }

    /// Registers a credential held only in memory: the supervising app (`ogund --supervised`) sends one on
    /// stdin at every launch, so there is no token file for another program to read and a restart of the app
    /// replaces it. An earlier transient credential of the same name is revoked.
    ///
    /// # Errors
    ///
    /// `api.invalid_params` unless it is a `human` credential named `studio`.
    pub fn add_transient(&mut self, cred: &Credential) -> ApiResult<()> {
        if cred.kind != ClientKind::Human || cred.name != "studio" {
            return Err(ApiError::invalid_params(
                "a supervising app may only register a human credential named studio",
            ));
        }
        let old: Vec<CredentialId> = self
            .transient
            .iter()
            .copied()
            .filter(|id| self.rows.get(id).is_some_and(|c| c.name == cred.name))
            .collect();
        for id in old {
            self.rows.remove(&id);
            self.transient.remove(&id);
        }
        self.rows.insert(
            cred.id,
            StoredCredential {
                id: cred.id,
                kind: cred.kind,
                name: cred.name.clone(),
                auth_key: cred.token.auth_key(),
                created_at: Timestamp::now(),
                revoked: false,
            },
        );
        self.transient.insert(cred.id);
        Ok(())
    }

    /// Revokes a credential and deletes its token file.
    ///
    /// # Errors
    ///
    /// `credential.not_found` or an I/O error.
    pub fn revoke(&mut self, paths: &OgunPaths, id: CredentialId) -> ApiResult<StoredCredential> {
        let row = self
            .rows
            .get_mut(&id)
            .ok_or_else(|| ApiError::new("credential.not_found", "no such credential"))?;
        row.revoked = true;
        let out = row.clone();
        let _ = std::fs::remove_file(paths.credential_file(&out.name));
        self.save()?;
        Ok(out)
    }
}
