//! File-name sanitising for `llm.rename`. A model proposes names; only portable, inert names reach disk:
//! lowercase ASCII letters and digits with single hyphens or underscores, no path separators, no leading
//! dot, no Windows device names, bounded length.

use std::collections::HashSet;

const RESERVED: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// A safe file-name stem from free text, at most `max_bytes` long, or `None` when nothing usable is left.
#[must_use]
pub fn stem(raw: &str, max_bytes: usize) -> Option<String> {
    let mut out = String::new();
    let mut pending_sep: Option<char> = None;
    for c in raw.chars() {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            if let Some(sep) = pending_sep.take()
                && !out.is_empty()
            {
                out.push(sep);
            }
            out.push(c);
        } else if c == '_' {
            pending_sep = Some('_');
        } else if (c.is_whitespace() || matches!(c, '-' | '.' | '/' | '\\' | ':' | ',' | '+'))
            && pending_sep.is_none()
        {
            pending_sep = Some('-');
        }
        // Everything else (control characters, quotes, non-ASCII) is dropped.
    }
    out.truncate(max_bytes.max(1));
    let out = out.trim_end_matches(['-', '_']).to_owned();
    if out.is_empty() {
        return None;
    }
    if RESERVED.contains(&out.as_str()) {
        return Some(format!("file-{out}"));
    }
    Some(out)
}

/// The extension of a name (`report.PDF` gives `pdf`), when it is 1 to 10 ASCII letters or digits.
#[must_use]
pub fn extension(name: &str) -> Option<String> {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let (stem, ext) = base.rsplit_once('.')?;
    if stem.is_empty()
        || ext.is_empty()
        || ext.len() > 10
        || !ext.bytes().all(|b| b.is_ascii_alphanumeric())
    {
        return None;
    }
    Some(ext.to_ascii_lowercase())
}

/// Makes `name` unique within `taken` by appending `-2`, `-3`, ... before the extension.
pub fn unique(stem: &str, ext: Option<&str>, taken: &mut HashSet<String>) -> String {
    let join = |s: &str| match ext {
        Some(e) => format!("{s}.{e}"),
        None => s.to_owned(),
    };
    let mut candidate = join(stem);
    let mut n = 2u32;
    while !taken.insert(candidate.clone()) {
        candidate = join(&format!("{stem}-{n}"));
        n += 1;
    }
    candidate
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostile_names_become_inert() {
        assert_eq!(stem("../../etc/passwd", 100).unwrap(), "etc-passwd");
        assert_eq!(
            stem("  Annual Report: 2025 (final).pdf ", 100).unwrap(),
            "annual-report-2025-final-pdf"
        );
        assert_eq!(stem(".hidden", 100).unwrap(), "hidden");
        assert_eq!(stem("CON", 100).unwrap(), "file-con");
        assert_eq!(stem("\u{202e}gpj.exe", 100).unwrap(), "gpj-exe");
        assert!(stem("///", 100).is_none());
        assert_eq!(stem("abcdefgh", 3).unwrap(), "abc");
    }

    #[test]
    fn extensions_and_uniqueness() {
        assert_eq!(extension("a/b/Report.PDF").as_deref(), Some("pdf"));
        assert_eq!(extension(".bashrc"), None);
        let mut taken = HashSet::new();
        assert_eq!(unique("a", Some("txt"), &mut taken), "a.txt");
        assert_eq!(unique("a", Some("txt"), &mut taken), "a-2.txt");
    }
}
