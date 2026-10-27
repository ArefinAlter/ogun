//! `ladder.get` and `ladder.put`: the provider ladder (`llm.json`, O-03 section 8.1) and the spend meter.

use std::sync::Arc;

use ogun_api::{ApiError, ApiResult};
use ogun_llm::config::LadderConfig;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{args, out};
use crate::redact::looks_like_secret;
use crate::server::Conn;
use crate::state::Daemon;

#[derive(Deserialize)]
struct Put {
    providers: Vec<Value>,
    #[serde(default)]
    #[allow(dead_code)]
    idempotency_key: Option<String>,
}

fn get(d: &Daemon) -> ApiResult<Value> {
    let ladder = match LadderConfig::load(&d.paths.state_dir) {
        Ok(Some(l)) => serde_json::to_value(l).map_err(|e| ApiError::internal(e.to_string()))?,
        Ok(None) => json!({ "version": 1, "providers": [] }),
        Err(e) => {
            return Ok(
                json!({ "available": false, "error": e.to_string(), "ladder": { "version": 1, "providers": [] } }),
            );
        }
    };
    let month = &ogun_types::Timestamp::now().to_rfc3339()[..7];
    let (cost, entries) = d.studio.agent.month_spend(month);
    let settings = d.studio.agent.settings();
    let (store, note) = d.secrets.kind();
    Ok(json!({
        "available": true,
        "ladder": ladder,
        "spend": {
            "month_usd_micro": cost,
            "cap_usd_micro": super::agent::usd_micro(settings.day_cap_usd).saturating_mul(31),
            "entries": entries,
        },
        "secret_store": { "kind": store, "note": note },
    }))
}

/// Routes `ladder.*`.
pub fn dispatch(d: &Arc<Daemon>, conn: &Conn, method: &str, p: Value) -> Option<ApiResult<Value>> {
    Some(match method {
        "ladder.get" => get(d),
        "ladder.put" => args::<Put>(p).and_then(|put| {
            let doc = json!({ "version": 1, "providers": put.providers });
            if looks_like_secret(&doc.to_string().replace("api_key_secret", "")) {
                return Err(ApiError::new(
                    "agent.secret_refused",
                    "the provider list looks like it contains a secret; a provider names the secret that holds its key (api_key_secret), never the key",
                ));
            }
            let ladder = LadderConfig::from_value(&doc).map_err(|e| ApiError::new("llm.config_invalid", e.to_string()))?;
            ladder.save(&d.paths.state_dir).map_err(|e| ApiError::new("llm.config_invalid", e.to_string()))?;
            tracing::info!(by = %conn.label(), providers = ladder.providers.len(), "provider ladder saved");
            d.studio.agent.changed_for_models();
            out(&json!({}))
        }),
        _ => return None,
    })
}
