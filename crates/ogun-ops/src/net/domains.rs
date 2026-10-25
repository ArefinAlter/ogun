//! Allowed domains (Zeus 08 section 4.3): the hosts a network operator may contact.
//!
//! Each entry is an exact lowercase host name, or `*.` and a host name, which allows every proper
//! subdomain but not the name itself. IP literals, single-label names (`localhost`) and trailing dots are
//! invalid; a wildcard needs a suffix of at least two labels. Every request and every redirect hop is
//! checked against the set.

use crate::error::{OpResult, config};

/// A set of allowed hosts.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AllowedDomains {
    exact: Vec<String>,
    suffixes: Vec<String>,
}

fn host_ok(h: &str) -> bool {
    !h.is_empty()
        && h.len() <= 253
        && !h.ends_with('.')
        && h.contains('.')
        && h.parse::<std::net::IpAddr>().is_err()
        && h.split('.').all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
        && !h
            .rsplit('.')
            .next()
            .is_some_and(|tld| tld.bytes().all(|b| b.is_ascii_digit()))
}

impl AllowedDomains {
    /// Parses a list of entries.
    ///
    /// # Errors
    ///
    /// `op.config_invalid` for an invalid entry or more than 32 entries.
    pub fn parse(entries: &[String]) -> OpResult<Self> {
        if entries.is_empty() || entries.len() > 32 {
            return Err(config("allowed_domains needs 1 to 32 entries"));
        }
        let mut out = Self::default();
        for e in entries {
            match e.strip_prefix("*.") {
                Some(suffix) => {
                    if !host_ok(suffix) {
                        return Err(config(
                            "an allowed_domains wildcard needs a host name of at least two labels",
                        ));
                    }
                    out.suffixes.push(suffix.to_owned());
                }
                None => {
                    if !host_ok(e) {
                        return Err(config(
                            "an allowed_domains entry is not a lowercase host name (no IP literals, single labels or trailing dots)",
                        ));
                    }
                    out.exact.push(e.clone());
                }
            }
        }
        Ok(out)
    }

    /// The exact hosts of some URLs (the default for operators that name their own start URLs).
    ///
    /// # Errors
    ///
    /// `op.config_invalid` when a URL's host is not a valid host name.
    pub fn of_hosts<'a>(hosts: impl IntoIterator<Item = &'a str>) -> OpResult<Self> {
        let mut out = Self::default();
        for h in hosts {
            let h = h.to_ascii_lowercase();
            if !host_ok(&h) {
                return Err(config(
                    "a URL's host is not a host name that may be allowed (IP literals and single labels are refused)",
                ));
            }
            if !out.exact.contains(&h) {
                out.exact.push(h);
            }
        }
        Ok(out)
    }

    /// Adds another set.
    pub fn extend(&mut self, other: &Self) {
        for h in &other.exact {
            if !self.exact.contains(h) {
                self.exact.push(h.clone());
            }
        }
        for s in &other.suffixes {
            if !self.suffixes.contains(s) {
                self.suffixes.push(s.clone());
            }
        }
    }

    /// Whether a host is allowed.
    #[must_use]
    pub fn allows(&self, host: &str) -> bool {
        let host = host.to_ascii_lowercase();
        let host = host.trim_end_matches('.');
        self.exact.iter().any(|e| e == host)
            || self.suffixes.iter().any(|s| {
                host.len() > s.len() + 1
                    && host.ends_with(s.as_str())
                    && host.as_bytes()[host.len() - s.len() - 1] == b'.'
            })
    }

    /// Whether a URL is http(s) with an allowed host.
    #[must_use]
    pub fn allows_url(&self, url: &url::Url) -> bool {
        matches!(url.scheme(), "http" | "https") && url.host_str().is_some_and(|h| self.allows(h))
    }

    /// True when the set is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.exact.is_empty() && self.suffixes.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules() {
        let d =
            AllowedDomains::parse(&["files.example.com".into(), "*.example.org".into()]).unwrap();
        assert!(d.allows("files.example.com"));
        assert!(!d.allows("example.com"));
        assert!(d.allows("a.b.example.org"));
        assert!(!d.allows("example.org"));
        assert!(!d.allows("badexample.org"));
        for bad in [
            "localhost",
            "127.0.0.1",
            "example.com.",
            "*.com",
            "Example.com",
        ] {
            assert!(AllowedDomains::parse(&[bad.into()]).is_err(), "{bad}");
        }
    }
}
