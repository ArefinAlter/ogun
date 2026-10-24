//! robots.txt (RFC 9309) for the product token `ogun`.
//!
//! The group whose `user-agent` matches `ogun` (case-insensitive) applies, else the `*` group. The
//! longest matching `allow` or `disallow` path wins; on a tie `allow` wins. `*` matches any sequence and a
//! final `$` anchors the end. A 4xx answer (other than 429) allows everything; 429, 5xx and network
//! failure disallow everything (RFC 9309 section 2.3.1.3 and 2.3.1.4). At most 500 KiB is parsed.

/// The product token Ogun matches in robots.txt.
pub const TOKEN: &str = "ogun";

/// Parsed rules for one origin.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Robots {
    rules: Vec<(bool, String)>,
    disallow_all: bool,
}

impl Robots {
    /// Everything allowed.
    #[must_use]
    pub fn allow_all() -> Self {
        Self::default()
    }

    /// Everything disallowed.
    #[must_use]
    pub fn disallow_all() -> Self {
        Self {
            rules: Vec::new(),
            disallow_all: true,
        }
    }

    /// Parses a robots.txt body.
    #[must_use]
    pub fn parse(body: &str) -> Self {
        let body = if body.len() > 500 * 1024 {
            let mut end = 500 * 1024;
            while !body.is_char_boundary(end) {
                end -= 1;
            }
            &body[..end]
        } else {
            body
        };
        // (user agents, rules) per group.
        type Group = (Vec<String>, Vec<(bool, String)>);
        let mut groups: Vec<Group> = Vec::new();
        let mut in_agents = false;
        for raw in body.lines() {
            let line = raw.split('#').next().unwrap_or("").trim();
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            let key = key.trim().to_ascii_lowercase();
            let value = value.trim();
            match key.as_str() {
                "user-agent" => {
                    if !in_agents {
                        groups.push((Vec::new(), Vec::new()));
                        in_agents = true;
                    }
                    if let Some(g) = groups.last_mut() {
                        g.0.push(value.to_ascii_lowercase());
                    }
                }
                "allow" | "disallow" => {
                    in_agents = false;
                    if let Some(g) = groups.last_mut()
                        && !value.is_empty()
                    {
                        g.1.push((key == "allow", value.to_owned()));
                    }
                }
                _ => {}
            }
        }
        let pick = |want: &str| {
            groups
                .iter()
                .filter(|(agents, _)| agents.iter().any(|a| a == want))
                .flat_map(|(_, rules)| rules.iter().cloned())
                .collect::<Vec<_>>()
        };
        let mine = pick(TOKEN);
        let rules = if groups.iter().any(|(a, _)| a.iter().any(|x| x == TOKEN)) {
            mine
        } else {
            pick("*")
        };
        Self {
            rules,
            disallow_all: false,
        }
    }

    /// Whether a path (with its query) may be fetched. `/robots.txt` itself is always allowed.
    #[must_use]
    pub fn allows(&self, path_and_query: &str) -> bool {
        if path_and_query == "/robots.txt" {
            return true;
        }
        if self.disallow_all {
            return false;
        }
        let mut best: Option<(usize, bool)> = None;
        for (allow, pattern) in &self.rules {
            if matches(pattern, path_and_query) {
                let len = pattern.len();
                best = match best {
                    Some((l, a)) if l > len || (l == len && a) => Some((l, a)),
                    _ => Some((len, *allow)),
                };
            }
        }
        best.is_none_or(|(_, allow)| allow)
    }
}

fn matches(pattern: &str, path: &str) -> bool {
    let (pattern, anchored) = match pattern.strip_suffix('$') {
        Some(p) => (p, true),
        None => (pattern, false),
    };
    // An unanchored pattern matches a prefix: the same as the pattern followed by `*`.
    let mut p: Vec<u8> = pattern.as_bytes().to_vec();
    if !anchored {
        p.push(b'*');
    }
    glob(&p, path.as_bytes())
}

/// `*` matches any sequence; everything else matches itself. Linear-time greedy matching with one
/// backtrack point.
fn glob(p: &[u8], s: &[u8]) -> bool {
    let (mut pi, mut si) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;
    while si < s.len() {
        if pi < p.len() && p[pi] == b'*' {
            star = Some((pi, si));
            pi += 1;
        } else if pi < p.len() && p[pi] == s[si] {
            pi += 1;
            si += 1;
        } else if let Some((sp, ss)) = star {
            pi = sp + 1;
            si = ss + 1;
            star = Some((sp, ss + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|b| *b == b'*')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc_9309_examples() {
        let r = Robots::parse(
            "User-agent: *\nDisallow: /private\nAllow: /private/ok\n\nUser-agent: other\nDisallow: /\n",
        );
        assert!(r.allows("/public"));
        assert!(!r.allows("/private/x"));
        assert!(r.allows("/private/ok/1"));
        let r =
            Robots::parse("User-agent: Ogun\nDisallow: /*.pdf$\n\nUser-agent: *\nDisallow: /\n");
        assert!(r.allows("/a.html"));
        assert!(!r.allows("/a.pdf"));
        assert!(r.allows("/a.pdf?x=1"));
        assert!(!Robots::disallow_all().allows("/"));
        assert!(Robots::disallow_all().allows("/robots.txt"));
    }
}
