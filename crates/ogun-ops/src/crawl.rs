//! `crawl`: a bounded crawl that discovers URLs (O-03 section 7: outputs `urls` and `documents`).
//!
//! Every page fetch goes through the zeus-core HTTP transport wrapped in Ogun's [`ScopedTransport`]:
//! only `allowed_domains` (default: the exact hosts of the start URLs, so the crawl is same-origin), every
//! redirect hop checked, robots.txt honoured for the token `ogun`, `min_interval_ms` between requests to a
//! host (default 1000), no cookies and no session material. Configuration:
//!
//! | Member | Default | Meaning |
//! |---|---|---|
//! | `start` / `start_urls` | | One start URL, or 1 to 100. |
//! | `allowed_domains` | start hosts | Zeus 08 section 4.3. |
//! | `headers`, `query_secrets` | | Added to requests to allowed hosts; values may be `secret_ref`s. |
//! | `select` | `{"css": "a[href]", "attr": "href"}` | `{"css", "attr"}`, `{"json_path"}` (RFC 9535) or `{"regex", "group"}`. |
//! | `emit` | `{value}` | A template (Zeus 08 section 5.1) over `value` (and the JSONPath's last member name or the CSS attribute name) giving each URL; relative results resolve against the page. |
//! | `pagination` | none | `rel_next`, `next_selector`, `page_param`, `offset_limit` or `cursor` (Zeus 08 section 4.6), each with `max_pages`. |
//! | `follow` | none | `{"css", "attr"}`: links to crawl one level deeper, within `allowed_domains`. |
//! | `max_pages` | 100 | Documents fetched, at most 10 000. |
//! | `max_depth` | 0, or 1 with `follow` | At most 8. |
//! | `max_urls` | 100 000 | URLs emitted, at most 1 000 000. |
//! | `max_document_bytes` | 8 MiB | Per document, at most 64 MiB. |
//! | `save_documents` | `true` | Write fetched documents to `documents`. |
//! | `respect_robots` | `true` | Must be `true`. |
//!
//! Output `urls` is one JSON Lines file of `{"url"}` rows; `documents` holds each fetched page with
//! metadata `url` (redacted for the record), `status`, `depth` and `page`.

use std::collections::{BTreeSet, HashSet, VecDeque};
use std::sync::Arc;

use jsonpath_rust::JsonPath;
use serde_json::{Map, Value, json};
use url::Url;

use crate::error::{OpError, OpResult, config};
use crate::net::domains::AllowedDomains;
use crate::net::fetch::{Document, get, production_transport};
use crate::net::transport::{Scope, ScopedTransport};
use crate::net::{allowed_domains, injection, min_interval};
use crate::ops::{CoreFail, CoreResult, Job};
use crate::secrets::{ResolvedSecrets, redact, redact_record};
use crate::template::{Template, value_text};
use crate::{Environment, OutputFile, files};

#[derive(Debug, Clone)]
enum Select {
    Css {
        selector: String,
        attr: Option<String>,
    },
    JsonPath(String),
    Regex {
        re: regex::Regex,
        group: usize,
    },
}

#[derive(Debug, Clone)]
enum Paging {
    RelNext,
    NextSelector(Select),
    PageParam {
        param: String,
        start: i64,
        step: i64,
        end: Option<i64>,
    },
    OffsetLimit {
        offset_param: String,
        limit_param: String,
        limit: u64,
        start: u64,
    },
    Cursor {
        path: String,
        param: String,
    },
}

#[derive(Debug, Clone)]
struct Chain {
    first: Url,
    k: u64,
    max_pages: u64,
    cursors: HashSet<String>,
}

fn parse_select(v: &Value) -> OpResult<Select> {
    let o = v
        .as_object()
        .ok_or_else(|| config("crawl: a selector must be an object"))?;
    if let Some(css) = o.get("css").and_then(Value::as_str) {
        scraper::Selector::parse(css)
            .map_err(|_| config("crawl: a CSS selector does not parse"))?;
        let attr = o
            .get("attr")
            .and_then(Value::as_str)
            .map(str::to_ascii_lowercase);
        return Ok(Select::Css {
            selector: css.to_owned(),
            attr,
        });
    }
    if let Some(p) = o.get("json_path").and_then(Value::as_str) {
        if !p.starts_with('$') || p.len() > 1024 {
            return Err(config(
                "crawl: a JSONPath must start with $ and be at most 1024 bytes",
            ));
        }
        Value::Null
            .query(p)
            .map_err(|_| config("crawl: a JSONPath does not parse"))?;
        return Ok(Select::JsonPath(p.to_owned()));
    }
    if let Some(r) = o.get("regex").and_then(Value::as_str) {
        if r.len() > 1024 {
            return Err(config("crawl: a pattern is longer than 1024 bytes"));
        }
        let re = regex::RegexBuilder::new(r)
            .size_limit(1 << 20)
            .build()
            .map_err(|_| config("crawl: a pattern does not compile"))?;
        let group = o.get("group").and_then(Value::as_u64).unwrap_or(0) as usize;
        if group >= re.captures_len() {
            return Err(config("crawl: a pattern has no such group"));
        }
        return Ok(Select::Regex { re, group });
    }
    Err(config("crawl: a selector needs css, json_path or regex"))
}

/// The name a selected value is also available under in `emit` (`id` for `$.results[*].id`).
fn alias(select: &Select) -> Option<String> {
    match select {
        Select::Css { attr, .. } => attr.clone(),
        Select::JsonPath(p) => {
            let last = p
                .rsplit(['.', '['])
                .next()?
                .trim_end_matches(']')
                .trim_matches(['\'', '"']);
            let mut b = last.bytes();
            (b.next().is_some_and(|c| c.is_ascii_lowercase())
                && b.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
                && last.len() <= 32)
                .then(|| last.to_owned())
        }
        Select::Regex { .. } => None,
    }
}

fn select_values(select: &Select, doc: &Document) -> OpResult<Vec<String>> {
    let text = String::from_utf8_lossy(&doc.body);
    Ok(match select {
        Select::Css { selector, attr } => {
            let html = scraper::Html::parse_document(&text);
            let sel = scraper::Selector::parse(selector)
                .map_err(|_| config("crawl: a CSS selector does not parse"))?;
            html.select(&sel)
                .take(10_000)
                .filter_map(|el| match attr {
                    Some(a) => el.value().attr(a).map(str::to_owned),
                    None => Some(
                        el.text()
                            .collect::<Vec<_>>()
                            .join(" ")
                            .split_whitespace()
                            .collect::<Vec<_>>()
                            .join(" "),
                    ),
                })
                .filter(|s| !s.is_empty())
                .collect()
        }
        Select::JsonPath(p) => {
            let value: Value = serde_json::from_slice(&doc.body).map_err(|_| {
                OpError::new(
                    "op.input_invalid",
                    format!("crawl: {} is not JSON", redact(doc.url.as_str())),
                )
            })?;
            value
                .query(p)
                .map_err(|_| config("crawl: a JSONPath does not parse"))?
                .into_iter()
                .take(10_000)
                .filter_map(value_text)
                .collect()
        }
        Select::Regex { re, group } => re
            .captures_iter(&text)
            .take(10_000)
            .filter_map(|c| c.get(*group).map(|m| m.as_str().to_owned()))
            .collect(),
    })
}

fn rel_next(doc: &Document) -> Option<String> {
    if let Some(link) = &doc.link {
        for part in link.split(',') {
            let mut segs = part.split(';');
            let target = segs.next()?.trim();
            if segs.any(|p| {
                let p = p.trim().to_ascii_lowercase();
                p.strip_prefix("rel=")
                    .is_some_and(|v| v.trim_matches('"').split_whitespace().any(|t| t == "next"))
            }) {
                return Some(
                    target
                        .trim_start_matches('<')
                        .trim_end_matches('>')
                        .to_owned(),
                );
            }
        }
    }
    if doc.content_type.contains("html") {
        let html = scraper::Html::parse_document(&String::from_utf8_lossy(&doc.body));
        for css in ["link[rel]", "a[rel]"] {
            let sel = scraper::Selector::parse(css).ok()?;
            for el in html.select(&sel) {
                let rel = el.value().attr("rel").unwrap_or("").to_ascii_lowercase();
                if rel.split_whitespace().any(|t| t == "next")
                    && let Some(href) = el.value().attr("href")
                {
                    return Some(href.to_owned());
                }
            }
        }
    }
    None
}

/// Sets one query parameter as Zeus 08 section 4.6 rule 1 says: the first occurrence is replaced, later
/// ones removed, otherwise it is appended; everything else is kept as written.
fn set_param(url: &Url, name: &str, value: &str) -> Url {
    let enc = |s: &str| -> String {
        s.bytes()
            .map(|b| {
                if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
                    char::from(b).to_string()
                } else {
                    format!("%{b:02X}")
                }
            })
            .collect()
    };
    let mut out = url.clone();
    let query = url.query().unwrap_or("");
    let mut parts: Vec<String> = Vec::new();
    let mut done = false;
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let raw_name = pair.split('=').next().unwrap_or("");
        let decoded: String = url::form_urlencoded::parse(raw_name.as_bytes())
            .next()
            .map(|(k, _)| k.into_owned())
            .unwrap_or_default();
        if decoded == name {
            if !done {
                parts.push(format!("{}={}", enc(name), enc(value)));
                done = true;
            }
        } else {
            parts.push(pair.to_owned());
        }
    }
    if !done {
        parts.push(format!("{}={}", enc(name), enc(value)));
    }
    out.set_query(Some(&parts.join("&")));
    out
}

fn parse_paging(v: &Value) -> OpResult<(Paging, u64)> {
    let o = v
        .as_object()
        .ok_or_else(|| config("crawl: pagination must be an object"))?;
    let max_pages = o.get("max_pages").and_then(Value::as_u64).unwrap_or(10);
    let s = |k: &str| o.get(k).and_then(Value::as_str).map(str::to_owned);
    let need = |k: &str| s(k).ok_or_else(|| config(format!("crawl: pagination needs {k}")));
    let paging = match o.get("strategy").and_then(Value::as_str) {
        Some("rel_next") => Paging::RelNext,
        Some("next_selector") => Paging::NextSelector(parse_select(
            o.get("selector")
                .ok_or_else(|| config("crawl: next_selector needs selector"))?,
        )?),
        Some("page_param") => Paging::PageParam {
            param: need("param")?,
            start: o.get("start").and_then(Value::as_i64).unwrap_or(1),
            step: o.get("step").and_then(Value::as_i64).unwrap_or(1),
            end: o.get("end").and_then(Value::as_i64),
        },
        Some("offset_limit") => {
            let limit = o.get("limit").and_then(Value::as_u64).unwrap_or(0);
            if !(1..=1000).contains(&limit) {
                return Err(config("crawl: offset_limit needs limit 1 to 1000"));
            }
            Paging::OffsetLimit {
                offset_param: need("offset_param")?,
                limit_param: need("limit_param")?,
                limit,
                start: o.get("start_offset").and_then(Value::as_u64).unwrap_or(0),
            }
        }
        Some("cursor") => Paging::Cursor {
            path: need("cursor_path")?,
            param: need("param")?,
        },
        Some("infinite_scroll") => {
            return Err(config(
                "crawl: infinite_scroll needs a browser session; use zeus.session_acquire",
            ));
        }
        _ => return Err(config("crawl: unknown pagination strategy")),
    };
    Ok((paging, max_pages))
}

fn first_url(paging: Option<&Paging>, url: &Url) -> Url {
    match paging {
        Some(Paging::PageParam { param, start, .. }) => set_param(url, param, &start.to_string()),
        Some(Paging::OffsetLimit {
            offset_param,
            limit_param,
            limit,
            start,
        }) => set_param(
            &set_param(url, offset_param, &start.to_string()),
            limit_param,
            &limit.to_string(),
        ),
        _ => url.clone(),
    }
}

fn next_url(paging: &Paging, chain: &mut Chain, doc: &Document, items: usize) -> Option<Url> {
    if matches!(doc.status, 404 | 410) {
        return None;
    }
    chain.k += 1;
    if chain.k >= chain.max_pages {
        return None;
    }
    match paging {
        Paging::RelNext => rel_next(doc).and_then(|h| doc.url.join(&h).ok()),
        Paging::NextSelector(sel) => select_values(sel, doc)
            .ok()?
            .into_iter()
            .next()
            .and_then(|h| doc.url.join(&h).ok()),
        Paging::PageParam {
            param,
            start,
            step,
            end,
        } => {
            if items == 0 {
                return None;
            }
            let v = start.checked_add(step.checked_mul(i64::try_from(chain.k).ok()?)?)?;
            if end.is_some_and(|e| v > e) {
                return None;
            }
            Some(set_param(&chain.first, param, &v.to_string()))
        }
        Paging::OffsetLimit {
            offset_param,
            limit_param,
            limit,
            start,
        } => {
            if (items as u64) < *limit {
                return None;
            }
            let off = start + chain.k * limit;
            Some(set_param(
                &set_param(&chain.first, offset_param, &off.to_string()),
                limit_param,
                &limit.to_string(),
            ))
        }
        Paging::Cursor { path, param } => {
            let value: Value = serde_json::from_slice(&doc.body).ok()?;
            let cursor = value
                .query(path)
                .ok()?
                .into_iter()
                .next()
                .and_then(value_text)?;
            if cursor.is_empty() || !chain.cursors.insert(cursor.clone()) {
                return None;
            }
            Some(set_param(&chain.first, param, &cursor))
        }
    }
}

fn ext_for(content_type: &str) -> &'static str {
    match content_type {
        t if t.contains("html") => "html",
        t if t.contains("json") => "json",
        t if t.contains("xml") => "xml",
        t if t.starts_with("text/") => "txt",
        _ => "bin",
    }
}

/// Runs `crawl@1`.
///
/// # Errors
///
/// See the module documentation; network failures of single pages are logged, not fatal.
#[allow(clippy::too_many_lines)]
pub async fn crawl(
    job: &Job,
    env: &Environment,
    secrets: &ResolvedSecrets,
) -> CoreResult<Vec<OutputFile>> {
    job.only(&[
        "start",
        "start_urls",
        "allowed_domains",
        "headers",
        "query_secrets",
        "select",
        "emit",
        "pagination",
        "follow",
        "max_pages",
        "max_depth",
        "max_urls",
        "min_interval_ms",
        "max_document_bytes",
        "save_documents",
        "respect_robots",
    ])?;
    if job.bool("respect_robots")? == Some(false) {
        return Err(config("crawl: respect_robots must be true").into());
    }
    if !env.network_access {
        return Err(OpError::new(
            "op.network_denied",
            "crawl needs network access, which this environment denies",
        )
        .into());
    }
    let mut starts: Vec<String> = job.strings("start_urls")?.unwrap_or_default();
    if let Some(s) = job.str("start")? {
        starts.push(s.to_owned());
    }
    if starts.is_empty() || starts.len() > 100 {
        return Err(config("crawl needs start or 1 to 100 start_urls").into());
    }
    let starts: Vec<Url> = starts
        .iter()
        .map(|s| {
            Url::parse(s)
                .ok()
                .filter(|u| matches!(u.scheme(), "http" | "https"))
                .ok_or_else(|| config("crawl: a start URL is not an absolute http(s) URL"))
        })
        .collect::<OpResult<_>>()?;
    let allowed = match allowed_domains(job.cfg().get("allowed_domains"))? {
        Some(a) => a,
        None => AllowedDomains::of_hosts(starts.iter().filter_map(|u| u.host_str()))?,
    };
    if let Some(u) = starts.iter().find(|u| !allowed.allows_url(u)) {
        return Err(OpError::new(
            "op.network_denied",
            format!(
                "the start URL {} is outside allowed_domains",
                redact(u.as_str())
            ),
        )
        .into());
    }
    let cap = |key: &str, default: u64, max: u64| -> OpResult<u64> {
        match job.u64(key)? {
            None => Ok(default),
            Some(v) if (1..=max).contains(&v) => Ok(v),
            Some(_) => Err(config(format!("crawl: {key} must be 1 to {max}"))),
        }
    };
    let max_pages = cap("max_pages", 100, 10_000)?;
    let max_urls = cap("max_urls", 100_000, 1_000_000)?;
    let max_bytes =
        usize::try_from(cap("max_document_bytes", 8 << 20, 64 << 20)?).unwrap_or(8 << 20);
    let select = job.cfg().get("select").map(parse_select).transpose()?;
    let follow = job.cfg().get("follow").map(parse_select).transpose()?;
    let max_depth = match job.u64("max_depth")? {
        None => u64::from(follow.is_some()),
        Some(d) if d <= 8 => d,
        Some(_) => return Err(config("crawl: max_depth must be 0 to 8").into()),
    };
    let paging = job.cfg().get("pagination").map(parse_paging).transpose()?;
    let emit = Template::parse(job.str("emit")?.unwrap_or("{value}"))
        .map_err(|e| config(format!("crawl: emit: {e}")))?;
    let save = job.bool("save_documents")?.unwrap_or(true);
    let scope = Arc::new(Scope::new(
        allowed.clone(),
        injection(job.cfg(), secrets)?,
        min_interval(job.cfg().get("min_interval_ms"), 1000)?,
        env.network_access,
    ));
    let transport = ScopedTransport::new(production_transport()?, scope);

    let mut frontier: VecDeque<(Url, u64, Option<Chain>)> = VecDeque::new();
    for s in &starts {
        let chain = paging.as_ref().map(|(_, mp)| Chain {
            first: s.clone(),
            k: 0,
            max_pages: (*mp).min(max_pages),
            cursors: HashSet::new(),
        });
        frontier.push_back((first_url(paging.as_ref().map(|(p, _)| p), s), 0, chain));
    }
    let mut seen_pages: HashSet<String> = HashSet::new();
    let mut urls: BTreeSet<String> = BTreeSet::new();
    let mut ordered: Vec<String> = Vec::new();
    let mut outputs: Vec<OutputFile> = Vec::new();
    let mut fetched = 0u64;
    let mut failures = 0u64;
    let alias_name = select.as_ref().and_then(alias);
    while let Some((url, depth, mut chain)) = frontier.pop_front() {
        job.check_cancel()?;
        if fetched >= max_pages {
            job.log(format!("crawl: stopped at max_pages ({max_pages})"));
            break;
        }
        let mut key = url.clone();
        key.set_fragment(None);
        if !seen_pages.insert(key.to_string()) {
            continue;
        }
        let accept = if matches!(select, Some(Select::JsonPath(_))) {
            "application/json"
        } else {
            "text/html,application/xhtml+xml,application/json;q=0.9,*/*;q=0.8"
        };
        let doc = match get(&transport, &url, accept, max_bytes).await {
            Ok(d) => d,
            Err(e) => {
                if matches!(e.error.code.as_str(), "op.network_denied") && fetched == 0 {
                    return Err(CoreFail::Op(e));
                }
                failures += 1;
                job.log(format!("crawl: {}", e.error.message));
                continue;
            }
        };
        fetched += 1;
        job.progress(fetched, Some(max_pages));
        if !(200..300).contains(&doc.status) {
            job.log(format!(
                "crawl: {} answered {}",
                redact(doc.url.as_str()),
                doc.status
            ));
            continue;
        }
        if save {
            let dir = job.out_dir("documents")?;
            let path = dir.join(format!("{fetched:05}.{}", ext_for(&doc.content_type)));
            std::fs::write(&path, &doc.body)
                .map_err(|e| crate::error::io("writing a document", &e))?;
            outputs.push(OutputFile {
                port: "documents".into(),
                path,
                media_type: if doc.content_type.is_empty() {
                    "application/octet-stream".into()
                } else {
                    doc.content_type.clone()
                },
                metadata: Some(json!({
                    "url": redact_record(doc.url.as_str()),
                    "status": doc.status,
                    "depth": depth,
                    "page": fetched,
                })),
            });
        }
        // Items.
        let effective = select.clone().or_else(|| {
            doc.content_type.contains("html").then(|| Select::Css {
                selector: "a[href]".into(),
                attr: Some("href".into()),
            })
        });
        let values = match &effective {
            Some(s) => select_values(s, &doc)?,
            None => {
                return Err(
                    config("crawl: select is required for documents that are not HTML").into(),
                );
            }
        };
        let item_count = values.len();
        let alias_name = alias_name
            .clone()
            .or_else(|| effective.as_ref().and_then(alias));
        for v in values {
            if urls.len() as u64 >= max_urls {
                break;
            }
            let rendered = emit
                .render(&|f| (f == "value" || alias_name.as_deref() == Some(f)).then(|| v.clone()))
                .map_err(|e| config(format!("crawl: emit: {e}")))?;
            let Some(u) = Url::parse(&rendered)
                .ok()
                .or_else(|| doc.url.join(&rendered).ok())
            else {
                continue;
            };
            if !matches!(u.scheme(), "http" | "https") {
                continue;
            }
            let mut u = u;
            u.set_fragment(None);
            if urls.insert(u.to_string()) {
                ordered.push(u.to_string());
            }
        }
        // Deeper pages.
        if let Some(f) = &follow
            && depth < max_depth
        {
            for href in select_values(f, &doc)? {
                if let Ok(next) = doc.url.join(&href)
                    && allowed.allows_url(&next)
                {
                    frontier.push_back((next, depth + 1, None));
                }
            }
        }
        // The next page of this chain.
        if let (Some((p, _)), Some(c)) = (paging.as_ref(), chain.as_mut())
            && let Some(next) = next_url(p, c, &doc, item_count)
        {
            if allowed.allows_url(&next) {
                frontier.push_front((next, depth, chain));
            } else {
                job.log("crawl: a next page is outside allowed_domains; the chain ends");
            }
        }
    }
    let dir = job.out_dir("urls")?;
    let path = dir.join("urls.jsonl");
    let mut body = Vec::new();
    for u in &ordered {
        body.extend(
            serde_json::to_vec(&json!({"url": u}))
                .map_err(|_| crate::error::internal("url row"))?,
        );
        body.push(b'\n');
    }
    std::fs::write(&path, body).map_err(|e| crate::error::io("writing urls", &e))?;
    let mut md = Map::new();
    md.insert("count".into(), json!(ordered.len()));
    md.insert("pages".into(), json!(fetched));
    md.insert("failures".into(), json!(failures));
    outputs.push(OutputFile {
        port: "urls".into(),
        path,
        media_type: "application/x-ndjson".into(),
        metadata: Some(Value::Object(md)),
    });
    files::ensure_dir(&job.scratch)?;
    Ok(outputs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_parameters() {
        let u = Url::parse("https://a.example.org/x?b=1&page=2&page=3&c=%2F").unwrap();
        assert_eq!(
            set_param(&u, "page", "7").as_str(),
            "https://a.example.org/x?b=1&page=7&c=%2F"
        );
        let u = Url::parse("https://a.example.org/x").unwrap();
        assert_eq!(
            set_param(&u, "q", "a b").as_str(),
            "https://a.example.org/x?q=a%20b"
        );
    }

    #[test]
    fn aliases() {
        assert_eq!(
            alias(&Select::JsonPath("$.results[*].id".into())).as_deref(),
            Some("id")
        );
        assert_eq!(
            alias(&Select::Css {
                selector: "a".into(),
                attr: Some("href".into())
            })
            .as_deref(),
            Some("href")
        );
    }
}
