//! The closed template language of Zeus 08 section 5.1, the only expression form in the IR (ADR 0003).
//!
//! ```text
//! template    = { literal | placeholder }
//! literal     = any character except "{" and "}", or "{{" (a literal "{"), or "}}" (a literal "}")
//! placeholder = "{" field { "|" filter } "}"
//! field       = [a-z][a-z0-9_]{0,31}
//! filter      = "lower" | "upper" | "trim" | "slug" | "strip_ext" | "truncate:" N | "default:" TEXT
//! ```
//!
//! Output is plain text; it is never evaluated again and becomes a path component only after sanitising.
//! `map`, `file.copy`, `file.move`, `partition` and `crawl` render templates over an item's fields.

use serde_json::Value;

/// Limits of Zeus 08 section 5.1 rule 4.
const MAX_BYTES: usize = 512;
const MAX_PLACEHOLDERS: usize = 16;
const MAX_FILTERS: usize = 4;

/// A template that failed to parse, or an item it cannot render.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TemplateError {
    /// The template breaks the grammar or a limit (`plan.template_invalid`).
    #[error("invalid template: {0}")]
    Invalid(&'static str),
    /// A placeholder's field has no value and no `default` filter came first.
    #[error("the template field {0} has no value")]
    Missing(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Filter {
    Lower,
    Upper,
    Trim,
    Slug,
    StripExt,
    Truncate(usize),
    Default(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Part {
    Lit(String),
    Field { name: String, filters: Vec<Filter> },
}

/// A parsed template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Template {
    parts: Vec<Part>,
}

impl Template {
    /// Parses a template.
    ///
    /// # Errors
    ///
    /// [`TemplateError::Invalid`].
    pub fn parse(text: &str) -> Result<Self, TemplateError> {
        if text.len() > MAX_BYTES {
            return Err(TemplateError::Invalid("longer than 512 bytes"));
        }
        let mut parts = Vec::new();
        let mut lit = String::new();
        let mut chars = text.char_indices().peekable();
        let mut placeholders = 0;
        while let Some((i, c)) = chars.next() {
            match c {
                '{' if chars.peek().map(|(_, c)| *c) == Some('{') => {
                    chars.next();
                    lit.push('{');
                }
                '}' if chars.peek().map(|(_, c)| *c) == Some('}') => {
                    chars.next();
                    lit.push('}');
                }
                '}' => return Err(TemplateError::Invalid("a stray }")),
                '{' => {
                    let rest = &text[i + 1..];
                    let end = rest
                        .find('}')
                        .ok_or(TemplateError::Invalid("{ without a matching }"))?;
                    let body = &rest[..end];
                    for _ in 0..body.chars().count() + 1 {
                        chars.next();
                    }
                    placeholders += 1;
                    if placeholders > MAX_PLACEHOLDERS {
                        return Err(TemplateError::Invalid("more than 16 placeholders"));
                    }
                    if !lit.is_empty() {
                        parts.push(Part::Lit(std::mem::take(&mut lit)));
                    }
                    parts.push(parse_placeholder(body)?);
                }
                c => lit.push(c),
            }
        }
        if !lit.is_empty() {
            parts.push(Part::Lit(lit));
        }
        Ok(Self { parts })
    }

    /// The field names the template reads.
    pub fn fields(&self) -> impl Iterator<Item = &str> {
        self.parts.iter().filter_map(|p| match p {
            Part::Field { name, .. } => Some(name.as_str()),
            Part::Lit(_) => None,
        })
    }

    /// Renders the template; `value` returns a field's value, `None` when it has none.
    ///
    /// # Errors
    ///
    /// [`TemplateError::Missing`].
    pub fn render(&self, value: &dyn Fn(&str) -> Option<String>) -> Result<String, TemplateError> {
        let mut out = String::new();
        for part in &self.parts {
            match part {
                Part::Lit(s) => out.push_str(s),
                Part::Field { name, filters } => {
                    let mut v = value(name).filter(|s| !s.is_empty());
                    for f in filters {
                        v = match (f, v) {
                            (Filter::Default(d), None) => Some(d.clone()),
                            (Filter::Default(_), Some(s)) => Some(s),
                            (_, None) => return Err(TemplateError::Missing(name.clone())),
                            (f, Some(s)) => Some(apply(f, &s)),
                        };
                    }
                    match v {
                        Some(s) => out.push_str(&s),
                        None => return Err(TemplateError::Missing(name.clone())),
                    }
                }
            }
        }
        Ok(out)
    }
}

fn parse_placeholder(body: &str) -> Result<Part, TemplateError> {
    let mut segs = body.split('|');
    let name = segs.next().unwrap_or("");
    let mut b = name.bytes();
    let ok = name.len() <= 32
        && b.next().is_some_and(|c| c.is_ascii_lowercase())
        && b.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_');
    if !ok {
        return Err(TemplateError::Invalid(
            "a field name is not [a-z][a-z0-9_]{0,31}",
        ));
    }
    let mut filters = Vec::new();
    for seg in segs {
        if filters.len() == MAX_FILTERS {
            return Err(TemplateError::Invalid(
                "more than 4 filters in a placeholder",
            ));
        }
        filters.push(match seg {
            "lower" => Filter::Lower,
            "upper" => Filter::Upper,
            "trim" => Filter::Trim,
            "slug" => Filter::Slug,
            "strip_ext" => Filter::StripExt,
            s if s.starts_with("truncate:") => {
                let n: usize = s["truncate:".len()..]
                    .parse()
                    .map_err(|_| TemplateError::Invalid("truncate needs 1 to 255"))?;
                if !(1..=255).contains(&n) {
                    return Err(TemplateError::Invalid("truncate needs 1 to 255"));
                }
                Filter::Truncate(n)
            }
            s if s.starts_with("default:") => {
                let text = &s["default:".len()..];
                if text.chars().count() > 64 || text.chars().any(char::is_control) {
                    return Err(TemplateError::Invalid(
                        "default text is too long or has control characters",
                    ));
                }
                Filter::Default(text.to_owned())
            }
            _ => return Err(TemplateError::Invalid("unknown filter")),
        });
    }
    Ok(Part::Field {
        name: name.to_owned(),
        filters,
    })
}

fn apply(f: &Filter, s: &str) -> String {
    match f {
        Filter::Lower => s.to_lowercase(),
        Filter::Upper => s.to_uppercase(),
        Filter::Trim => s.trim().to_owned(),
        Filter::Slug => {
            let mut out = String::new();
            let mut dash = false;
            for c in s.chars().flat_map(char::to_lowercase) {
                if c.is_alphanumeric() {
                    if dash && !out.is_empty() {
                        out.push('-');
                    }
                    dash = false;
                    out.push(c);
                } else {
                    dash = true;
                }
            }
            out
        }
        Filter::StripExt => match s.rfind('.') {
            Some(i)
                if i > 0
                    && (1..=8).contains(&(s.len() - i - 1))
                    && s[i + 1..].bytes().all(|b| b.is_ascii_alphanumeric()) =>
            {
                s[..i].to_owned()
            }
            _ => s.to_owned(),
        },
        Filter::Truncate(n) => s.chars().take(*n).collect(),
        Filter::Default(_) => s.to_owned(),
    }
}

/// The template text of a JSON value: strings as they are, integers in decimal, booleans by name;
/// `null`, arrays and objects have no value.
#[must_use]
pub fn value_text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// `filename`'s extension built-in: the part after the last `.` when 1 to 8 ASCII alphanumerics, lowercased.
#[must_use]
pub fn ext_of(name: &str) -> String {
    match name.rfind('.') {
        Some(i)
            if (1..=8).contains(&(name.len() - i - 1))
                && name[i + 1..].bytes().all(|b| b.is_ascii_alphanumeric()) =>
        {
            name[i + 1..].to_ascii_lowercase()
        }
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders() {
        let t = Template::parse("arxiv-{id|lower}-{title|slug|truncate:10}.{ext|default:pdf}{{x}}")
            .unwrap();
        let out = t
            .render(&|f| match f {
                "id" => Some("AB12".into()),
                "title" => Some("Hello, World of Air!".into()),
                _ => None,
            })
            .unwrap();
        assert_eq!(out, "arxiv-ab12-hello-worl.pdf{x}");
        assert!(matches!(
            Template::parse("{x|upper}").unwrap().render(&|_| None),
            Err(TemplateError::Missing(_))
        ));
        assert!(Template::parse("{X}").is_err());
        assert!(Template::parse("a}").is_err());
        assert!(Template::parse("{a|nope}").is_err());
        assert_eq!(apply(&Filter::StripExt, "report.final.pdf"), "report.final");
    }
}
