//! The server's own store (redb): users, OIDC identities, workspaces, memberships, invitations, API tokens,
//! sessions, pending sign-ins, passkeys and WebAuthn challenges (O-13 sections 3 to 5).
//!
//! Bearer secrets are never stored: sessions, invitations and pending sign-ins are keyed by a BLAKE3-derived
//! digest of their secret, and API tokens keep only a BLAKE3-derived verifier. A copy of `server.redb` therefore
//! grants nothing by itself.

use std::path::Path;
use std::time::Duration;

use ogun_types::{IdGenerator, Timestamp, TokenId, UserId, WorkspaceId};
use redb::{Durability, ReadableDatabase, ReadableTable, TableDefinition};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::error::{ServerError, ServerResult};

type Table = TableDefinition<'static, &'static str, &'static [u8]>;

const META: Table = TableDefinition::new("meta");
const USERS: Table = TableDefinition::new("users");
const IDENTITIES: Table = TableDefinition::new("identities");
const WORKSPACES: Table = TableDefinition::new("workspaces");
const MEMBERS: Table = TableDefinition::new("members");
const INVITES: Table = TableDefinition::new("invites");
const TOKENS: Table = TableDefinition::new("tokens");
const SESSIONS: Table = TableDefinition::new("sessions");
const LOGINS: Table = TableDefinition::new("logins");
const PASSKEYS: Table = TableDefinition::new("passkeys");
const CHALLENGES: Table = TableDefinition::new("challenges");

const ALL: [Table; 11] = [
    META, USERS, IDENTITIES, WORKSPACES, MEMBERS, INVITES, TOKENS, SESSIONS, LOGINS, PASSKEYS,
    CHALLENGES,
];

const SCHEMA_VERSION: u32 = 1;

/// A workspace role. Ordered: owner > admin > editor > viewer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Reads everything in the workspace.
    Viewer,
    /// Viewer, plus authoring workflows, starting and cancelling runs and planning backfills.
    Editor,
    /// Editor, plus pausing and resuming workflows and managing members below admin.
    Admin,
    /// Admin, plus managing admins, owners and the approver flag.
    Owner,
}

impl Role {
    /// The wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Viewer => "viewer",
            Self::Editor => "editor",
            Self::Admin => "admin",
            Self::Owner => "owner",
        }
    }
}

impl std::str::FromStr for Role {
    type Err = ServerError;

    fn from_str(s: &str) -> ServerResult<Self> {
        match s {
            "viewer" => Ok(Self::Viewer),
            "editor" => Ok(Self::Editor),
            "admin" => Ok(Self::Admin),
            "owner" => Ok(Self::Owner),
            _ => Err(ServerError::invalid(
                "role must be viewer, editor, admin or owner",
            )),
        }
    }
}

/// A person who signs in with OIDC.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct User {
    /// Id.
    pub user_id: UserId,
    /// Name from the identity provider.
    pub display_name: String,
    /// Email from the identity provider, when it gave one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// When the account was created.
    pub created_at: Timestamp,
    /// A disabled user cannot sign in and their tokens stop working.
    #[serde(default)]
    pub disabled: bool,
}

/// A workspace: a set of members bound to one daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workspace {
    /// Id.
    pub workspace_id: WorkspaceId,
    /// Name.
    pub name: String,
    /// The `daemons` entry of `server.json` this workspace uses.
    pub daemon: String,
    /// When created.
    pub created_at: Timestamp,
}

/// A user's place in a workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Membership {
    /// Workspace.
    pub workspace_id: WorkspaceId,
    /// User.
    pub user_id: UserId,
    /// Role.
    pub role: Role,
    /// May register a passkey and relay approval decisions (granted by an owner).
    pub approver: bool,
    /// When added.
    pub added_at: Timestamp,
    /// Who added them; `None` for the host's `init` and `invite` commands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub added_by: Option<UserId>,
}

/// A one-time invitation into a workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Invite {
    /// Workspace.
    pub workspace_id: WorkspaceId,
    /// Role granted.
    pub role: Role,
    /// Approver flag granted.
    pub approver: bool,
    /// When created.
    pub created_at: Timestamp,
    /// When it stops working.
    pub expires_at: Timestamp,
    /// Who created it; `None` for the host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by: Option<UserId>,
}

/// An API token. Only a verifier of its secret is stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenRecord {
    /// Id.
    pub token_id: TokenId,
    /// Label.
    pub name: String,
    /// The one workspace it works in.
    pub workspace_id: WorkspaceId,
    /// Who created it; it never exceeds that member's current role.
    pub created_by: UserId,
    /// Its role ceiling (viewer or editor).
    pub role: Role,
    /// Hex BLAKE3-derived verifier of the secret.
    pub verifier: String,
    /// When created.
    pub created_at: Timestamp,
    /// When it expires.
    pub expires_at: Timestamp,
    /// Last use, at most once a minute.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used_at: Option<Timestamp>,
    /// Revoked tokens stay listed and never authenticate.
    #[serde(default)]
    pub revoked: bool,
}

/// A browser session. Keyed by a digest of the cookie secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRecord {
    /// The user.
    pub user_id: UserId,
    /// The provider used to sign in.
    pub provider: String,
    /// Sign-in time (absolute expiry runs from here).
    pub created_at: Timestamp,
    /// Last request (idle expiry runs from here).
    pub last_seen_at: Timestamp,
}

/// An OIDC sign-in in flight. Keyed by a digest of `state`.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingLogin {
    /// Provider id.
    pub provider: String,
    /// The nonce the ID token must carry.
    pub nonce: String,
    /// The PKCE verifier.
    pub pkce_verifier: String,
    /// Digest of the browser-binding cookie.
    pub binding: String,
    /// Digest of the invitation code, when signing in to accept one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invite: Option<String>,
    /// When started.
    pub created_at: Timestamp,
}

impl std::fmt::Debug for PendingLogin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingLogin")
            .field("provider", &self.provider)
            .field("created_at", &self.created_at)
            .finish_non_exhaustive()
    }
}

/// A passkey registered for approvals in one workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Passkey {
    /// WebAuthn credential id, base64url.
    pub credential_id: String,
    /// Owner.
    pub user_id: UserId,
    /// Workspace (and so daemon) it was proposed to.
    pub workspace_id: WorkspaceId,
    /// Uncompressed SEC1 P-256 public key, base64url.
    pub public_key: String,
    /// First 16 hex digits of BLAKE3 over the key, as the daemon reported it.
    pub fingerprint: String,
    /// Last signature counter seen.
    pub sign_count: u32,
    /// Relying party id.
    pub rp_id: String,
    /// When registered.
    pub created_at: Timestamp,
}

/// A registration challenge waiting for its attestation. Keyed by the session digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistrationChallenge {
    /// The challenge, base64url.
    pub challenge: String,
    /// The user.
    pub user_id: UserId,
    /// The workspace.
    pub workspace_id: WorkspaceId,
    /// When issued.
    pub created_at: Timestamp,
}

#[derive(Debug, Serialize, Deserialize)]
struct Meta {
    schema_version: u32,
    created_at: Timestamp,
}

fn storage(e: impl std::fmt::Display) -> ServerError {
    tracing::error!(error = %e, "server store error");
    ServerError::internal("server store error")
}

fn get<T: DeserializeOwned>(
    t: &impl ReadableTable<&'static str, &'static [u8]>,
    key: &str,
) -> ServerResult<Option<T>> {
    match t.get(key).map_err(storage)? {
        None => Ok(None),
        Some(v) => serde_json::from_slice(v.value())
            .map(Some)
            .map_err(|_| ServerError::internal("server store is corrupt")),
    }
}

fn put<T: Serialize>(
    t: &mut redb::Table<&'static str, &'static [u8]>,
    key: &str,
    value: &T,
) -> ServerResult<()> {
    let bytes = serde_json::to_vec(value).map_err(storage)?;
    t.insert(key, bytes.as_slice()).map_err(storage)?;
    Ok(())
}

fn all<T: DeserializeOwned>(
    t: &impl ReadableTable<&'static str, &'static [u8]>,
) -> ServerResult<Vec<(String, T)>> {
    let mut out = Vec::new();
    for row in t.iter().map_err(storage)? {
        let (k, v) = row.map_err(storage)?;
        let value = serde_json::from_slice(v.value())
            .map_err(|_| ServerError::internal("server store is corrupt"))?;
        out.push((k.value().to_owned(), value));
    }
    Ok(out)
}

fn member_key(w: WorkspaceId, u: UserId) -> String {
    format!("{w}/{u}")
}

fn identity_key(issuer: &str, subject: &str) -> String {
    format!("{issuer}\n{subject}")
}

fn expired(at: Timestamp, ttl: Duration, now: Timestamp) -> bool {
    at.checked_add(ttl).is_none_or(|end| end <= now)
}

/// The store.
#[derive(Debug)]
pub struct Store {
    db: redb::Database,
    ids: IdGenerator,
}

impl Store {
    /// Opens or creates `<dir>/server.redb`, creating `dir` owner-only.
    ///
    /// # Errors
    ///
    /// `api.unavailable` when another process holds the store; `internal.bug` for storage errors.
    pub fn open(dir: &Path) -> ServerResult<Self> {
        std::fs::create_dir_all(dir).map_err(|e| {
            ServerError::unavailable(format!("cannot create {}: {}", dir.display(), e.kind()))
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
        }
        let db = redb::Database::create(dir.join("server.redb")).map_err(|e| match e {
            redb::DatabaseError::DatabaseAlreadyOpen => ServerError::unavailable(
                "the server store is open in another process (stop ogun-server first)",
            ),
            other => storage(other),
        })?;
        let store = Self {
            db,
            ids: IdGenerator::new(),
        };
        let txn = store.write()?;
        {
            for t in ALL {
                txn.open_table(t).map_err(storage)?;
            }
            let mut meta = txn.open_table(META).map_err(storage)?;
            match get::<Meta>(&meta, "store")? {
                Some(m) if m.schema_version > SCHEMA_VERSION => {
                    return Err(ServerError::unavailable(
                        "the server store was written by a newer ogun-server",
                    ));
                }
                Some(_) => {}
                None => put(
                    &mut meta,
                    "store",
                    &Meta {
                        schema_version: SCHEMA_VERSION,
                        created_at: Timestamp::now(),
                    },
                )?,
            }
        }
        txn.commit().map_err(storage)?;
        Ok(store)
    }

    fn write(&self) -> ServerResult<redb::WriteTransaction> {
        let mut txn = self.db.begin_write().map_err(storage)?;
        txn.set_durability(Durability::Immediate).map_err(storage)?;
        Ok(txn)
    }

    fn read_one<T: DeserializeOwned>(&self, table: Table, key: &str) -> ServerResult<Option<T>> {
        let txn = self.db.begin_read().map_err(storage)?;
        let t = txn.open_table(table).map_err(storage)?;
        get(&t, key)
    }

    fn read_all<T: DeserializeOwned>(&self, table: Table) -> ServerResult<Vec<(String, T)>> {
        let txn = self.db.begin_read().map_err(storage)?;
        let t = txn.open_table(table).map_err(storage)?;
        all(&t)
    }

    fn write_one<T: Serialize>(&self, table: Table, key: &str, value: &T) -> ServerResult<()> {
        let txn = self.write()?;
        put(&mut txn.open_table(table).map_err(storage)?, key, value)?;
        txn.commit().map_err(storage)
    }

    fn take<T: DeserializeOwned>(&self, table: Table, key: &str) -> ServerResult<Option<T>> {
        let txn = self.write()?;
        let value = {
            let mut t = txn.open_table(table).map_err(storage)?;
            let v: Option<T> = get(&t, key)?;
            if v.is_some() {
                t.remove(key).map_err(storage)?;
            }
            v
        };
        txn.commit().map_err(storage)?;
        Ok(value)
    }

    fn delete(&self, table: Table, key: &str) -> ServerResult<()> {
        let txn = self.write()?;
        txn.open_table(table)
            .map_err(storage)?
            .remove(key)
            .map_err(storage)?;
        txn.commit().map_err(storage)
    }

    // ---- workspaces ----

    /// Creates a workspace bound to a daemon.
    ///
    /// # Errors
    ///
    /// Storage errors.
    pub fn create_workspace(&self, name: &str, daemon: &str) -> ServerResult<Workspace> {
        let w = Workspace {
            workspace_id: self.ids.workspace_id(),
            name: name.to_owned(),
            daemon: daemon.to_owned(),
            created_at: Timestamp::now(),
        };
        self.write_one(WORKSPACES, &w.workspace_id.to_string(), &w)?;
        Ok(w)
    }

    /// One workspace.
    ///
    /// # Errors
    ///
    /// Storage errors.
    pub fn workspace(&self, id: WorkspaceId) -> ServerResult<Option<Workspace>> {
        self.read_one(WORKSPACES, &id.to_string())
    }

    /// Every workspace.
    ///
    /// # Errors
    ///
    /// Storage errors.
    pub fn workspaces(&self) -> ServerResult<Vec<Workspace>> {
        Ok(self
            .read_all(WORKSPACES)?
            .into_iter()
            .map(|(_, w)| w)
            .collect())
    }

    // ---- users and identities ----

    /// The user an OIDC identity belongs to.
    ///
    /// # Errors
    ///
    /// Storage errors.
    pub fn user_by_identity(&self, issuer: &str, subject: &str) -> ServerResult<Option<User>> {
        let Some(id) = self.read_one::<UserId>(IDENTITIES, &identity_key(issuer, subject))? else {
            return Ok(None);
        };
        self.user(id)
    }

    /// One user.
    ///
    /// # Errors
    ///
    /// Storage errors.
    pub fn user(&self, id: UserId) -> ServerResult<Option<User>> {
        self.read_one(USERS, &id.to_string())
    }

    /// Refreshes a user's name and email from a new sign-in.
    ///
    /// # Errors
    ///
    /// Storage errors.
    pub fn update_profile(
        &self,
        id: UserId,
        display_name: &str,
        email: Option<&str>,
    ) -> ServerResult<()> {
        let txn = self.write()?;
        {
            let mut t = txn.open_table(USERS).map_err(storage)?;
            let key = id.to_string();
            if let Some(mut u) = get::<User>(&t, &key)? {
                u.display_name = display_name.to_owned();
                u.email = email.map(str::to_owned);
                put(&mut t, &key, &u)?;
            }
        }
        txn.commit().map_err(storage)
    }

    /// Accepts an invitation: creates the user for a new identity (or reuses the identity's user), adds the
    /// membership and consumes the invitation, in one transaction.
    ///
    /// # Errors
    ///
    /// `server.sign_in_failed` when the invitation is unknown or expired; storage errors.
    pub fn accept_invite(
        &self,
        invite_key: &str,
        issuer: &str,
        subject: &str,
        display_name: &str,
        email: Option<&str>,
    ) -> ServerResult<(User, Membership)> {
        let now = Timestamp::now();
        let txn = self.write()?;
        let result = {
            let mut invites = txn.open_table(INVITES).map_err(storage)?;
            let invite: Invite = get(&invites, invite_key)?.ok_or_else(|| {
                ServerError::new(
                    "server.sign_in_failed",
                    "the invitation is unknown or was already used",
                )
            })?;
            invites.remove(invite_key).map_err(storage)?;
            if invite.expires_at <= now {
                return Err(ServerError::new(
                    "server.sign_in_failed",
                    "the invitation has expired",
                ));
            }
            let mut identities = txn.open_table(IDENTITIES).map_err(storage)?;
            let mut users = txn.open_table(USERS).map_err(storage)?;
            let ikey = identity_key(issuer, subject);
            let user = match get::<UserId>(&identities, &ikey)? {
                Some(id) => get::<User>(&users, &id.to_string())?
                    .ok_or_else(|| ServerError::internal("identity without a user"))?,
                None => {
                    let u = User {
                        user_id: self.ids.user_id(),
                        display_name: display_name.to_owned(),
                        email: email.map(str::to_owned),
                        created_at: now,
                        disabled: false,
                    };
                    put(&mut users, &u.user_id.to_string(), &u)?;
                    put(&mut identities, &ikey, &u.user_id)?;
                    u
                }
            };
            let mut members = txn.open_table(MEMBERS).map_err(storage)?;
            let mkey = member_key(invite.workspace_id, user.user_id);
            let membership = match get::<Membership>(&members, &mkey)? {
                Some(mut m) => {
                    m.role = m.role.max(invite.role);
                    m.approver |= invite.approver;
                    m
                }
                None => Membership {
                    workspace_id: invite.workspace_id,
                    user_id: user.user_id,
                    role: invite.role,
                    approver: invite.approver,
                    added_at: now,
                    added_by: invite.created_by,
                },
            };
            put(&mut members, &mkey, &membership)?;
            (user, membership)
        };
        txn.commit().map_err(storage)?;
        Ok(result)
    }

    // ---- memberships ----

    /// A user's membership in a workspace.
    ///
    /// # Errors
    ///
    /// Storage errors.
    pub fn membership(&self, w: WorkspaceId, u: UserId) -> ServerResult<Option<Membership>> {
        self.read_one(MEMBERS, &member_key(w, u))
    }

    /// Every membership of a user.
    ///
    /// # Errors
    ///
    /// Storage errors.
    pub fn memberships_of(&self, u: UserId) -> ServerResult<Vec<Membership>> {
        Ok(self
            .read_all::<Membership>(MEMBERS)?
            .into_iter()
            .map(|(_, m)| m)
            .filter(|m| m.user_id == u)
            .collect())
    }

    /// Every member of a workspace with their user record.
    ///
    /// # Errors
    ///
    /// Storage errors.
    pub fn members_of(&self, w: WorkspaceId) -> ServerResult<Vec<(Membership, Option<User>)>> {
        let mut out = Vec::new();
        for (_, m) in self.read_all::<Membership>(MEMBERS)? {
            if m.workspace_id == w {
                let u = self.user(m.user_id)?;
                out.push((m, u));
            }
        }
        Ok(out)
    }

    /// Changes a member's role or approver flag. The last owner of a workspace cannot be demoted.
    ///
    /// # Errors
    ///
    /// `server.not_found`, `api.permission_denied` for the last owner, storage errors.
    pub fn update_membership(
        &self,
        w: WorkspaceId,
        u: UserId,
        role: Option<Role>,
        approver: Option<bool>,
    ) -> ServerResult<Membership> {
        let txn = self.write()?;
        let m = {
            let mut t = txn.open_table(MEMBERS).map_err(storage)?;
            let key = member_key(w, u);
            let mut m: Membership =
                get(&t, &key)?.ok_or_else(|| ServerError::not_found("no such member"))?;
            if let Some(r) = role {
                if m.role == Role::Owner && r != Role::Owner && Self::owners(&t, w)? <= 1 {
                    return Err(ServerError::denied("a workspace keeps at least one owner"));
                }
                m.role = r;
            }
            if let Some(a) = approver {
                m.approver = a;
            }
            put(&mut t, &key, &m)?;
            m
        };
        txn.commit().map_err(storage)?;
        Ok(m)
    }

    fn owners(
        t: &impl ReadableTable<&'static str, &'static [u8]>,
        w: WorkspaceId,
    ) -> ServerResult<usize> {
        Ok(all::<Membership>(t)?
            .into_iter()
            .filter(|(_, m)| m.workspace_id == w && m.role == Role::Owner)
            .count())
    }

    /// Removes a member. The last owner cannot be removed.
    ///
    /// # Errors
    ///
    /// `server.not_found`, `api.permission_denied` for the last owner, storage errors.
    pub fn remove_membership(&self, w: WorkspaceId, u: UserId) -> ServerResult<()> {
        let txn = self.write()?;
        {
            let mut t = txn.open_table(MEMBERS).map_err(storage)?;
            let key = member_key(w, u);
            let m: Membership =
                get(&t, &key)?.ok_or_else(|| ServerError::not_found("no such member"))?;
            if m.role == Role::Owner && Self::owners(&t, w)? <= 1 {
                return Err(ServerError::denied("a workspace keeps at least one owner"));
            }
            t.remove(key.as_str()).map_err(storage)?;
        }
        txn.commit().map_err(storage)
    }

    // ---- invitations ----

    /// Stores an invitation under the digest of its code.
    ///
    /// # Errors
    ///
    /// Storage errors.
    pub fn put_invite(&self, key: &str, invite: &Invite) -> ServerResult<()> {
        self.write_one(INVITES, key, invite)
    }

    /// Looks an invitation up without consuming it.
    ///
    /// # Errors
    ///
    /// Storage errors.
    pub fn invite(&self, key: &str) -> ServerResult<Option<Invite>> {
        self.read_one(INVITES, key)
    }

    // ---- tokens ----

    /// Mints a token id.
    #[must_use]
    pub fn new_token_id(&self) -> TokenId {
        self.ids.token_id()
    }

    /// Stores a token record.
    ///
    /// # Errors
    ///
    /// Storage errors.
    pub fn put_token(&self, t: &TokenRecord) -> ServerResult<()> {
        self.write_one(TOKENS, &t.token_id.to_string(), t)
    }

    /// One token.
    ///
    /// # Errors
    ///
    /// Storage errors.
    pub fn token(&self, id: TokenId) -> ServerResult<Option<TokenRecord>> {
        self.read_one(TOKENS, &id.to_string())
    }

    /// Every token.
    ///
    /// # Errors
    ///
    /// Storage errors.
    pub fn tokens(&self) -> ServerResult<Vec<TokenRecord>> {
        Ok(self.read_all(TOKENS)?.into_iter().map(|(_, t)| t).collect())
    }

    // ---- sessions ----

    /// Stores a session.
    ///
    /// # Errors
    ///
    /// Storage errors.
    pub fn put_session(&self, key: &str, s: &SessionRecord) -> ServerResult<()> {
        self.write_one(SESSIONS, key, s)
    }

    /// One session.
    ///
    /// # Errors
    ///
    /// Storage errors.
    pub fn session(&self, key: &str) -> ServerResult<Option<SessionRecord>> {
        self.read_one(SESSIONS, key)
    }

    /// Ends a session.
    ///
    /// # Errors
    ///
    /// Storage errors.
    pub fn delete_session(&self, key: &str) -> ServerResult<()> {
        self.delete(SESSIONS, key)
    }

    // ---- pending sign-ins ----

    /// Stores a pending sign-in under the digest of its `state`.
    ///
    /// # Errors
    ///
    /// Storage errors.
    pub fn put_login(&self, key: &str, l: &PendingLogin) -> ServerResult<()> {
        self.write_one(LOGINS, key, l)
    }

    /// Takes (and removes) a pending sign-in: each `state` works once.
    ///
    /// # Errors
    ///
    /// Storage errors.
    pub fn take_login(&self, key: &str) -> ServerResult<Option<PendingLogin>> {
        self.take(LOGINS, key)
    }

    // ---- passkeys and challenges ----

    /// Stores a passkey.
    ///
    /// # Errors
    ///
    /// Storage errors.
    pub fn put_passkey(&self, p: &Passkey) -> ServerResult<()> {
        self.write_one(PASSKEYS, &p.credential_id, p)
    }

    /// One passkey.
    ///
    /// # Errors
    ///
    /// Storage errors.
    pub fn passkey(&self, credential_id: &str) -> ServerResult<Option<Passkey>> {
        self.read_one(PASSKEYS, credential_id)
    }

    /// A user's passkeys in a workspace.
    ///
    /// # Errors
    ///
    /// Storage errors.
    pub fn passkeys_of(&self, u: UserId, w: WorkspaceId) -> ServerResult<Vec<Passkey>> {
        Ok(self
            .read_all::<Passkey>(PASSKEYS)?
            .into_iter()
            .map(|(_, p)| p)
            .filter(|p| p.user_id == u && p.workspace_id == w)
            .collect())
    }

    /// Raises a passkey's signature counter; fails when the new counter does not increase (a cloned
    /// authenticator), unless both are zero (authenticators without counters).
    ///
    /// # Errors
    ///
    /// `server.webauthn_invalid` on a counter that went backwards; storage errors.
    pub fn bump_sign_count(&self, credential_id: &str, count: u32) -> ServerResult<()> {
        let txn = self.write()?;
        {
            let mut t = txn.open_table(PASSKEYS).map_err(storage)?;
            let mut p: Passkey =
                get(&t, credential_id)?.ok_or_else(|| ServerError::not_found("no such passkey"))?;
            if (count != 0 || p.sign_count != 0) && count <= p.sign_count {
                return Err(ServerError::webauthn(
                    "the authenticator's signature counter did not increase; it may be cloned",
                ));
            }
            p.sign_count = count;
            put(&mut t, credential_id, &p)?;
        }
        txn.commit().map_err(storage)
    }

    /// Stores a registration challenge for a session (replacing any earlier one).
    ///
    /// # Errors
    ///
    /// Storage errors.
    pub fn put_challenge(&self, session_key: &str, c: &RegistrationChallenge) -> ServerResult<()> {
        self.write_one(CHALLENGES, session_key, c)
    }

    /// Takes a session's registration challenge: each works once.
    ///
    /// # Errors
    ///
    /// Storage errors.
    pub fn take_challenge(&self, session_key: &str) -> ServerResult<Option<RegistrationChallenge>> {
        self.take(CHALLENGES, session_key)
    }

    // ---- housekeeping ----

    /// Removes expired sessions, sign-ins, challenges and invitations.
    ///
    /// # Errors
    ///
    /// Storage errors.
    pub fn purge(&self, idle: Duration, absolute: Duration) -> ServerResult<usize> {
        let now = Timestamp::now();
        let ten_min = Duration::from_secs(600);
        let txn = self.write()?;
        let mut removed = 0;
        {
            let mut t = txn.open_table(SESSIONS).map_err(storage)?;
            let dead: Vec<String> = all::<SessionRecord>(&t)?
                .into_iter()
                .filter(|(_, s)| {
                    expired(s.last_seen_at, idle, now) || expired(s.created_at, absolute, now)
                })
                .map(|(k, _)| k)
                .collect();
            for k in &dead {
                t.remove(k.as_str()).map_err(storage)?;
            }
            removed += dead.len();
        }
        {
            let mut t = txn.open_table(LOGINS).map_err(storage)?;
            let dead: Vec<String> = all::<PendingLogin>(&t)?
                .into_iter()
                .filter(|(_, l)| expired(l.created_at, ten_min, now))
                .map(|(k, _)| k)
                .collect();
            for k in &dead {
                t.remove(k.as_str()).map_err(storage)?;
            }
            removed += dead.len();
        }
        {
            let mut t = txn.open_table(CHALLENGES).map_err(storage)?;
            let dead: Vec<String> = all::<RegistrationChallenge>(&t)?
                .into_iter()
                .filter(|(_, c)| expired(c.created_at, ten_min, now))
                .map(|(k, _)| k)
                .collect();
            for k in &dead {
                t.remove(k.as_str()).map_err(storage)?;
            }
            removed += dead.len();
        }
        {
            let mut t = txn.open_table(INVITES).map_err(storage)?;
            let dead: Vec<String> = all::<Invite>(&t)?
                .into_iter()
                .filter(|(_, i)| i.expires_at <= now)
                .map(|(k, _)| k)
                .collect();
            for k in &dead {
                t.remove(k.as_str()).map_err(storage)?;
            }
            removed += dead.len();
        }
        txn.commit().map_err(storage)?;
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn invite(store: &Store, w: &Workspace, role: Role, key: &str) {
        store
            .put_invite(
                key,
                &Invite {
                    workspace_id: w.workspace_id,
                    role,
                    approver: role == Role::Owner,
                    created_at: Timestamp::now(),
                    expires_at: Timestamp::now()
                        .checked_add(Duration::from_secs(3600))
                        .unwrap(),
                    created_by: None,
                },
            )
            .unwrap();
    }

    #[test]
    fn invitations_are_single_use_and_keep_an_owner() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let w = store.create_workspace("main", "default").unwrap();
        invite(&store, &w, Role::Owner, "k1");
        let (u, m) = store
            .accept_invite("k1", "https://idp", "alice", "Alice", None)
            .unwrap();
        assert_eq!(m.role, Role::Owner);
        assert!(m.approver);
        assert!(
            store
                .accept_invite("k1", "https://idp", "bob", "Bob", None)
                .is_err()
        );
        assert_eq!(
            store
                .user_by_identity("https://idp", "alice")
                .unwrap()
                .unwrap()
                .user_id,
            u.user_id
        );
        assert!(
            store
                .update_membership(w.workspace_id, u.user_id, Some(Role::Viewer), None)
                .is_err()
        );
        assert!(store.remove_membership(w.workspace_id, u.user_id).is_err());
        invite(&store, &w, Role::Viewer, "k2");
        let (b, _) = store
            .accept_invite("k2", "https://idp", "bob", "Bob", None)
            .unwrap();
        store.remove_membership(w.workspace_id, b.user_id).unwrap();
    }

    #[test]
    fn sign_counters_only_increase() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let ids = IdGenerator::new();
        store
            .put_passkey(&Passkey {
                credential_id: "c1".into(),
                user_id: ids.user_id(),
                workspace_id: ids.workspace_id(),
                public_key: "k".into(),
                fingerprint: "f".into(),
                sign_count: 5,
                rp_id: "x".into(),
                created_at: Timestamp::now(),
            })
            .unwrap();
        assert!(store.bump_sign_count("c1", 5).is_err());
        store.bump_sign_count("c1", 6).unwrap();
        assert!(store.bump_sign_count("c1", 0).is_err());
    }
}
