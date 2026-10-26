//! Connections (O-16 section 5): named, configured uses of a connector. Plain settings, and the *names* of the
//! secrets that hold the rest. A connection holds no secret value; `connections.test` really checks the target.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use ogun_api::{ApiError, ApiResult};
use ogun_connectors::Manifest;
use ogun_ops::error::{OpError, OpResult};
use ogun_ops::export::{Check, ResolvedConnection};
use ogun_types::Timestamp;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{Docs, args, out};
use crate::redact;
use crate::secrets::SecretStore;
use crate::server::Conn;
use crate::state::{Daemon, WorkflowRow, lock};

/// A stored connection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConnectionDoc {
    /// `cnx_` and a ULID.
    pub connection_id: String,
    /// Unique name.
    pub name: String,
    /// The connector id.
    pub connector: String,
    /// The authentication method id (empty for a connector without one).
    pub auth: String,
    /// Plain settings.
    #[serde(default)]
    pub settings: BTreeMap<String, Value>,
    /// Auth field to the name of the secret that holds it.
    #[serde(default)]
    pub secret_refs: BTreeMap<String, String>,
    /// `untested`, `healthy` or `failing`.
    pub state: String,
    /// When last tested.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_test_at: Option<Timestamp>,
    /// How long that test took.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_test_ms: Option<u64>,
    /// Why it failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// Created.
    pub created_at: Timestamp,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct File {
    items: Vec<ConnectionDoc>,
}

/// The connection store, shared with the `export` operator.
pub struct Connections {
    docs: Docs,
    items: Mutex<Vec<ConnectionDoc>>,
    secrets: Arc<SecretStore>,
}

impl std::fmt::Debug for Connections {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connections").finish_non_exhaustive()
    }
}

impl Connections {
    /// Loads `settings/connections.json`.
    #[must_use]
    pub fn load(docs: &Docs, secrets: Arc<SecretStore>) -> Self {
        let f: File = docs.read("connections");
        Self {
            docs: docs.clone(),
            items: Mutex::new(f.items),
            secrets,
        }
    }

    fn save(&self, items: &[ConnectionDoc]) -> ApiResult<()> {
        self.docs.write(
            "connections",
            &File {
                items: items.to_vec(),
            },
        )
    }

    fn find(&self, name: &str) -> Option<ConnectionDoc> {
        lock(&self.items).iter().find(|c| c.name == name).cloned()
    }
}

impl ogun_ops::export::Connections for Connections {
    fn resolve(&self, name: &str) -> OpResult<ResolvedConnection> {
        let c = self.find(name).ok_or_else(|| {
            OpError::new(
                "op.config_invalid",
                format!("there is no connection named {name}"),
            )
        })?;
        let mut secrets = BTreeMap::new();
        for (field, secret) in &c.secret_refs {
            let v = ogun_ops::Secrets::resolve(self.secrets.as_ref(), secret).ok_or_else(|| {
                OpError::new(
                    "secret.not_found",
                    format!("the secret named {secret} (for {field}) is not stored"),
                )
            })?;
            secrets.insert(field.clone(), v);
        }
        Ok(ResolvedConnection {
            name: c.name,
            connector: c.connector,
            settings: c.settings,
            secrets,
        })
    }
}

fn valid_name(n: &str) -> bool {
    let mut it = n.bytes();
    it.next().is_some_and(|b| b.is_ascii_lowercase())
        && n.len() <= 64
        && n.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    name: String,
    connector: String,
    #[serde(default)]
    auth: String,
    #[serde(default)]
    settings: BTreeMap<String, Value>,
    #[serde(default)]
    secret_refs: BTreeMap<String, String>,
}

fn bad(msg: impl Into<String>) -> ApiError {
    ApiError::invalid_params(format!("connection: {}", msg.into()))
}

fn check_input(i: &Input) -> ApiResult<&'static Manifest> {
    if !valid_name(&i.name) {
        return Err(bad(
            "a name is a lowercase letter and then up to 63 of a-z, 0-9, _ and -",
        ));
    }
    let m = ogun_connectors::get(&i.connector).ok_or_else(|| bad("no such connector"))?;
    let method = if m.auth.is_empty() {
        None
    } else {
        Some(
            m.auth
                .iter()
                .find(|a| a.id == i.auth)
                .ok_or_else(|| bad("the authentication method is not one of the connector's"))?,
        )
    };
    for (k, v) in &i.settings {
        let s = m
            .settings
            .iter()
            .find(|s| &s.name == k)
            .ok_or_else(|| bad(format!("{} has no setting named {k}", m.id)))?;
        match (s.kind.as_str(), v) {
            ("string", Value::String(t)) => {
                if t.len() > 1024 || t.chars().any(char::is_control) {
                    return Err(bad(format!(
                        "the setting {k} is too long or has control characters"
                    )));
                }
                if redact::looks_like_secret(t) {
                    return Err(bad(format!(
                        "the setting {k} looks like a secret; store secrets by name (they go in the credential store, not in a connection)"
                    )));
                }
            }
            ("integer", Value::Number(n)) if n.is_i64() => {}
            ("boolean", Value::Bool(_)) => {}
            ("enum", Value::String(t))
                if s.options.iter().any(|o| o.as_str() == Some(t.as_str())) => {}
            _ => return Err(bad(format!("the setting {k} has the wrong type or value"))),
        }
    }
    for (field, name) in &i.secret_refs {
        let known = method.is_some_and(|a| a.fields.iter().any(|f| &f.name == field && f.secret));
        if !known {
            return Err(bad(format!(
                "{field} is not a secret field of this authentication method"
            )));
        }
        if !crate::secrets::valid_name(name) || name.len() > 64 || redact::looks_like_secret(name) {
            return Err(bad(format!(
                "the value for {field} must be the NAME of a stored secret, not the secret itself"
            )));
        }
    }
    Ok(m)
}

fn view(d: &Daemon, c: &ConnectionDoc) -> Value {
    let used_by = used_by(d, &c.name);
    json!({
        "connection_id": c.connection_id,
        "name": c.name,
        "connector": c.connector,
        "auth": c.auth,
        "settings": c.settings,
        "secret_refs": c.secret_refs,
        "state": c.state,
        "last_test_at": c.last_test_at,
        "last_test_ms": c.last_test_ms,
        "last_error": c.last_error,
        "created_at": c.created_at,
        "used_by": used_by,
    })
}

/// The workflows whose latest version names this connection in a node's `connection` setting.
fn used_by(d: &Daemon, name: &str) -> Vec<String> {
    let Ok(rows) = d
        .store
        .list_docs::<WorkflowRow>(ogun_store::Collection::Workflows, "")
    else {
        return Vec::new();
    };
    rows.into_iter()
        .filter_map(|(_, r)| {
            let wf = d.workflow(&r.latest_hash).ok()?;
            wf.nodes
                .iter()
                .any(|n| n.config.get("connection").and_then(Value::as_str) == Some(name))
                .then_some(r.name)
        })
        .collect()
}

/// A hostname with `{setting}` placeholders filled from the connection's settings and the manifest defaults.
fn fill(template: &str, settings: &BTreeMap<String, Value>, m: &Manifest) -> String {
    let mut out = String::new();
    let mut rest = template;
    while let Some(i) = rest.find('{') {
        out.push_str(&rest[..i]);
        let Some(j) = rest[i..].find('}') else { break };
        let key = &rest[i + 1..i + j];
        let value = settings.get(key).cloned().or_else(|| {
            m.settings
                .iter()
                .find(|s| s.name == key)
                .and_then(|s| s.default.clone())
        });
        match value {
            Some(Value::String(s)) => out.push_str(&s),
            Some(Value::Number(n)) => out.push_str(&n.to_string()),
            _ => out.push_str(&format!("{{{key}}}")),
        }
        rest = &rest[i + j + 1..];
    }
    out.push_str(rest);
    out
}

async fn test(d: &Arc<Daemon>, name: &str) -> ApiResult<Value> {
    let started = std::time::Instant::now();
    let c = d
        .studio
        .connections
        .find(name)
        .ok_or_else(|| ApiError::new("connection.not_found", "no connection by that name"))?;
    let m = ogun_connectors::get(&c.connector).ok_or_else(|| {
        ApiError::new(
            "connection.not_found",
            "the connector of this connection is no longer in the catalog",
        )
    })?;
    let mut checks: Vec<Check> = Vec::new();
    let mk = |name: &str, ok: bool, detail: String| Check {
        name: name.into(),
        ok,
        detail,
    };

    // 1. The settings are complete.
    let missing: Vec<&str> = m
        .settings
        .iter()
        .filter(|s| s.required && s.default.is_none() && !c.settings.contains_key(&s.name))
        .map(|s| s.name.as_str())
        .collect();
    checks.push(if missing.is_empty() {
        mk(
            "settings",
            true,
            "every required setting has a value".into(),
        )
    } else {
        mk(
            "settings",
            false,
            format!("missing: {}", missing.join(", ")),
        )
    });

    // 2. Every secret the method needs is stored (the value is not read here).
    let stored = d.secrets.names();
    let method = m.auth.iter().find(|a| a.id == c.auth);
    let mut secret_problems = Vec::new();
    for f in method
        .map(|a| a.fields.as_slice())
        .unwrap_or_default()
        .iter()
        .filter(|f| f.secret && f.required)
    {
        match c.secret_refs.get(&f.name) {
            None => secret_problems.push(format!("{} has no secret name", f.name)),
            Some(s) if !stored.contains(s) => {
                secret_problems.push(format!("the secret {s} (for {}) is not stored", f.name))
            }
            Some(_) => {}
        }
    }
    checks.push(if secret_problems.is_empty() {
        mk(
            "secrets",
            true,
            "every required secret is stored (values are not read back)".into(),
        )
    } else {
        mk("secrets", false, secret_problems.join("; "))
    });

    // 3. The hosts, with placeholders filled.
    let mut settings = c.settings.clone();
    for s in &m.settings {
        if let (false, Some(def)) = (settings.contains_key(&s.name), &s.default) {
            settings.insert(s.name.clone(), def.clone());
        }
    }
    let hosts: Vec<String> = m
        .egress
        .domains
        .iter()
        .map(|h| fill(h, &settings, m))
        .collect();
    checks.push(mk(
        "hosts",
        true,
        if hosts.is_empty() {
            "this connector reaches nothing outside this computer".into()
        } else {
            format!("it contacts {}", hosts.join(", "))
        },
    ));

    // 4. A real probe, when this build has a driver for the connector.
    let ready = checks.iter().all(|c| c.ok);
    let mut simulated = false;
    if ready {
        match ogun_ops::export::Connections::resolve(d.studio.connections.as_ref(), name) {
            Ok(resolved) => match ogun_ops::export::probe(&resolved, true).await {
                Some(more) => checks.extend(more),
                None => {
                    simulated = true;
                    checks.push(mk(
                        "probe",
                        true,
                        format!(
                            "stopped after the manifest checks: this build has no driver for {} ({} code)",
                            m.id, m.maturity
                        ),
                    ));
                }
            },
            Err(e) => checks.push(mk("probe", false, e.error.message)),
        }
    }
    let ok = checks.iter().all(|c| c.ok);
    let elapsed = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    {
        let mut items = lock(&d.studio.connections.items);
        if let Some(row) = items.iter_mut().find(|r| r.name == name) {
            row.state = if ok { "healthy" } else { "failing" }.into();
            row.last_test_at = Some(Timestamp::now());
            row.last_test_ms = Some(elapsed);
            row.last_error = (!ok).then(|| {
                checks
                    .iter()
                    .filter(|c| !c.ok)
                    .map(|c| c.detail.clone())
                    .collect::<Vec<_>>()
                    .join("; ")
            });
        }
        let snapshot = items.clone();
        drop(items);
        d.studio.connections.save(&snapshot)?;
    }
    Ok(json!({ "ok": ok, "elapsed_ms": elapsed, "simulated": simulated, "checks": checks }))
}

#[derive(Deserialize)]
struct Put {
    connection: Input,
    #[serde(default)]
    #[allow(dead_code)]
    idempotency_key: Option<String>,
}

#[derive(Deserialize)]
struct Name {
    name: String,
}

/// Routes `connections.*`.
pub async fn dispatch(
    d: &Arc<Daemon>,
    conn: &Conn,
    method: &str,
    p: Value,
) -> Option<ApiResult<Value>> {
    Some(match method {
        "connections.list" => {
            let items = lock(&d.studio.connections.items).clone();
            Ok(json!({ "connections": items.iter().map(|c| view(d, c)).collect::<Vec<_>>() }))
        }
        "connections.put" => args::<Put>(p).and_then(|put| {
            let input = put.connection;
            check_input(&input)?;
            let store = &d.studio.connections;
            let mut items = lock(&store.items);
            let now = Timestamp::now();
            let row = match items.iter_mut().find(|c| c.name == input.name) {
                Some(existing) => {
                    existing.connector = input.connector;
                    existing.auth = input.auth;
                    existing.settings = input.settings;
                    existing.secret_refs = input.secret_refs;
                    existing.state = "untested".into();
                    existing.last_error = None;
                    existing.clone()
                }
                None => {
                    let c = ConnectionDoc {
                        connection_id: format!("cnx_{}", ulid::Ulid::generate()),
                        name: input.name,
                        connector: input.connector,
                        auth: input.auth,
                        settings: input.settings,
                        secret_refs: input.secret_refs,
                        state: "untested".into(),
                        last_test_at: None,
                        last_test_ms: None,
                        last_error: None,
                        created_at: now,
                    };
                    items.push(c.clone());
                    c
                }
            };
            let snapshot = items.clone();
            drop(items);
            store.save(&snapshot)?;
            tracing::info!(by = %conn.label(), connection = %row.name, connector = %row.connector, "connection saved");
            Ok(view(d, &row))
        }),
        "connections.delete" => args::<Name>(p).and_then(|n| {
            let store = &d.studio.connections;
            let mut items = lock(&store.items);
            let before = items.len();
            items.retain(|c| c.name != n.name);
            if items.len() == before {
                return Err(ApiError::new("connection.not_found", "no connection by that name"));
            }
            let snapshot = items.clone();
            drop(items);
            store.save(&snapshot)?;
            out(&json!({}))
        }),
        "connections.test" => match args::<Name>(p) {
            Ok(n) => {
                tracing::info!(by = %conn.label(), connection = %n.name, "connection test");
                test(d, &n.name).await
            }
            Err(e) => Err(e),
        },
        _ => return None,
    })
}
