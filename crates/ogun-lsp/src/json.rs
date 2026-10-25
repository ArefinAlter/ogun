//! A tolerant JSON scanner that keeps byte ranges.
//!
//! Editors send documents while they are being typed, so the text is often not valid JSON: an
//! unterminated string, a missing comma, a member without a value. This scanner never fails. It
//! builds the best tree it can, with the byte range of every value and member name, so that
//! validator diagnostics land on the member their JSON Pointer names (the same job as
//! `extensions/vscode/src/locate.js`), and so that hover, completion and go-to-definition know
//! what the cursor is on.
//!
//! Ranges are byte offsets into the UTF-8 text and always fall on character boundaries.

/// Nesting deeper than this is not scanned; the rest of the text becomes one opaque value.
const MAX_DEPTH: usize = 128;

/// A JSON value with its byte range `[start, end)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Value {
    /// Offset of the first byte (`{`, `[`, `"` or the first character of a literal).
    pub start: usize,
    /// Offset just past the last byte.
    pub end: usize,
    /// What kind of value it is.
    pub kind: Kind,
}

/// The kinds of value the scanner distinguishes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// An object, possibly not closed yet.
    Object {
        /// Members in document order.
        members: Vec<Member>,
        /// The closing `}` was found.
        closed: bool,
    },
    /// An array, possibly not closed yet.
    Array {
        /// Items in document order.
        items: Vec<Value>,
        /// The closing `]` was found.
        closed: bool,
    },
    /// A string. `text` is the decoded content.
    String {
        /// Decoded content, without quotes.
        text: String,
        /// The closing quote was found.
        closed: bool,
    },
    /// A number, `true`, `false`, `null` or anything else that is not one of the above.
    Scalar,
}

/// One object member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    /// Decoded member name.
    pub key: String,
    /// Range of the name, quotes included.
    pub key_start: usize,
    /// End of the name, past the closing quote when there is one.
    pub key_end: usize,
    /// The name's closing quote was found.
    pub key_closed: bool,
    /// Offset of the `:`, when present.
    pub colon: Option<usize>,
    /// The value, when one was written.
    pub value: Option<Value>,
    /// Where the member ends: past its value, or past the colon and the whitespace after it.
    pub end: usize,
}

impl Value {
    /// The members, when this is an object.
    #[must_use]
    pub fn members(&self) -> &[Member] {
        match &self.kind {
            Kind::Object { members, .. } => members,
            _ => &[],
        }
    }

    /// The items, when this is an array.
    #[must_use]
    pub fn items(&self) -> &[Value] {
        match &self.kind {
            Kind::Array { items, .. } => items,
            _ => &[],
        }
    }

    /// The decoded string, when this is a string.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match &self.kind {
            Kind::String { text, .. } => Some(text),
            _ => None,
        }
    }

    /// The first member with this name.
    #[must_use]
    pub fn member(&self, key: &str) -> Option<&Member> {
        self.members().iter().find(|m| m.key == key)
    }

    /// The value of the first member with this name.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.member(key).and_then(|m| m.value.as_ref())
    }

    /// True for objects and arrays.
    #[must_use]
    pub fn is_container(&self) -> bool {
        matches!(self.kind, Kind::Object { .. } | Kind::Array { .. })
    }

    /// The range of the string's content, without its quotes. `None` for anything but a string.
    #[must_use]
    pub fn string_content(&self) -> Option<(usize, usize)> {
        match &self.kind {
            Kind::String { closed, .. } => {
                let end = if *closed { self.end - 1 } else { self.end };
                Some((self.start + 1, end.max(self.start + 1)))
            }
            _ => None,
        }
    }

    /// True when `offset` (a cursor position between characters) is on this value.
    #[must_use]
    pub fn contains(&self, offset: usize) -> bool {
        match &self.kind {
            Kind::Object { closed, .. }
            | Kind::Array { closed, .. }
            | Kind::String { closed, .. } => {
                self.start < offset && (offset < self.end || (!closed && offset <= self.end))
            }
            Kind::Scalar => self.start <= offset && offset <= self.end,
        }
    }
}

impl Member {
    /// True when `offset` is on the member name.
    #[must_use]
    pub fn key_contains(&self, offset: usize) -> bool {
        self.key_start < offset
            && (offset < self.key_end || (!self.key_closed && offset <= self.key_end))
    }

    /// The range of the name's content, without quotes.
    #[must_use]
    pub fn key_content(&self) -> (usize, usize) {
        let end = if self.key_closed {
            self.key_end - 1
        } else {
            self.key_end
        };
        (self.key_start + 1, end.max(self.key_start + 1))
    }
}

struct Scanner<'a> {
    text: &'a str,
    pos: usize,
}

fn is_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r' | b'\n')
}

impl Scanner<'_> {
    fn peek(&self) -> Option<u8> {
        self.text.as_bytes().get(self.pos).copied()
    }

    fn ws(&mut self) {
        while self.peek().is_some_and(is_ws) {
            self.pos += 1;
        }
    }

    fn bump_char(&mut self) {
        if let Some(c) = self.text[self.pos..].chars().next() {
            self.pos += c.len_utf8();
        }
    }

    fn hex4(&self, at: usize) -> Option<u32> {
        let digits = self.text.get(at..at + 4)?;
        if !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        u32::from_str_radix(digits, 16).ok()
    }

    /// Scans a string starting at `"`. Stops at the closing quote, a line break or the end.
    fn string(&mut self) -> (String, usize, usize, bool) {
        let start = self.pos;
        self.pos += 1;
        let mut out = String::new();
        loop {
            match self.peek() {
                None | Some(b'\n' | b'\r') => return (out, start, self.pos, false),
                Some(b'"') => {
                    self.pos += 1;
                    return (out, start, self.pos, true);
                }
                Some(b'\\') => {
                    let Some(esc) = self.text.as_bytes().get(self.pos + 1).copied() else {
                        self.pos += 1;
                        return (out, start, self.pos, false);
                    };
                    match esc {
                        b'n' => out.push('\n'),
                        b't' => out.push('\t'),
                        b'r' => out.push('\r'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'u' => {
                            if let Some(unit) = self.hex4(self.pos + 2) {
                                let mut used = 6;
                                let ch = if (0xD800..0xDC00).contains(&unit)
                                    && self.text.get(self.pos + 6..self.pos + 8) == Some("\\u")
                                {
                                    match self.hex4(self.pos + 8) {
                                        Some(low) if (0xDC00..0xE000).contains(&low) => {
                                            used = 12;
                                            char::from_u32(
                                                0x10000 + ((unit - 0xD800) << 10) + (low - 0xDC00),
                                            )
                                        }
                                        _ => None,
                                    }
                                } else {
                                    char::from_u32(unit)
                                };
                                out.push(ch.unwrap_or('\u{FFFD}'));
                                self.pos += used;
                                continue;
                            }
                            out.push('\u{FFFD}');
                        }
                        b'\n' | b'\r' => {
                            self.pos += 1;
                            return (out, start, self.pos, false);
                        }
                        _ => {}
                    }
                    self.pos += 1;
                    self.bump_char();
                }
                Some(_) => {
                    let c = self.text[self.pos..].chars().next().unwrap_or('\u{FFFD}');
                    out.push(c);
                    self.pos += c.len_utf8().max(1);
                }
            }
        }
    }

    /// Scans a literal: everything up to a delimiter.
    fn scalar(&mut self) -> Value {
        let start = self.pos;
        while let Some(c) = self.text[self.pos..].chars().next() {
            if c.is_ascii() && (is_ws(c as u8) || b",}]:[{\"".contains(&(c as u8))) {
                break;
            }
            self.pos += c.len_utf8();
        }
        Value {
            start,
            end: self.pos,
            kind: Kind::Scalar,
        }
    }

    fn value(&mut self, depth: usize) -> Option<Value> {
        self.ws();
        match self.peek()? {
            b'{' | b'[' if depth >= MAX_DEPTH => {
                let start = self.pos;
                self.pos = self.text.len();
                Some(Value {
                    start,
                    end: self.pos,
                    kind: Kind::Scalar,
                })
            }
            b'{' => Some(self.object(depth)),
            b'[' => Some(self.array(depth)),
            b'"' => {
                let (text, start, end, closed) = self.string();
                Some(Value {
                    start,
                    end,
                    kind: Kind::String { text, closed },
                })
            }
            b',' | b'}' | b']' | b':' => None,
            _ => Some(self.scalar()),
        }
    }

    fn object(&mut self, depth: usize) -> Value {
        let start = self.pos;
        self.pos += 1;
        let mut members = Vec::new();
        let done = |s: &Self, members, closed| Value {
            start,
            end: s.pos,
            kind: Kind::Object { members, closed },
        };
        loop {
            self.ws();
            match self.peek() {
                None => return done(self, members, false),
                Some(b'}') => {
                    self.pos += 1;
                    return done(self, members, true);
                }
                // A stray `]` closes the enclosing array; leave it for the caller.
                Some(b']') => return done(self, members, false),
                Some(b',') => self.pos += 1,
                Some(b'"') => {
                    let (key, key_start, key_end, key_closed) = self.string();
                    self.ws();
                    let mut colon = None;
                    let mut value = None;
                    if self.peek() == Some(b':') {
                        colon = Some(self.pos);
                        self.pos += 1;
                        self.ws();
                        if !matches!(self.peek(), None | Some(b',' | b'}' | b']')) {
                            value = self.value(depth + 1);
                        }
                    }
                    let end = value.as_ref().map_or(self.pos, |v| v.end);
                    members.push(Member {
                        key,
                        key_start,
                        key_end,
                        key_closed,
                        colon,
                        value,
                        end,
                    });
                }
                Some(_) => {
                    // A name without quotes, a stray colon or a literal: skip it.
                    let before = self.pos;
                    let _ = self.value(depth + 1);
                    if self.pos == before {
                        self.bump_char();
                    }
                }
            }
        }
    }

    fn array(&mut self, depth: usize) -> Value {
        let start = self.pos;
        self.pos += 1;
        let mut items = Vec::new();
        let done = |s: &Self, items, closed| Value {
            start,
            end: s.pos,
            kind: Kind::Array { items, closed },
        };
        loop {
            self.ws();
            match self.peek() {
                None => return done(self, items, false),
                Some(b']') => {
                    self.pos += 1;
                    return done(self, items, true);
                }
                // A stray `}` closes the enclosing object.
                Some(b'}') => return done(self, items, false),
                Some(b',' | b':') => self.pos += 1,
                Some(_) => {
                    let before = self.pos;
                    match self.value(depth + 1) {
                        Some(v) => items.push(v),
                        None if self.pos == before => self.bump_char(),
                        None => {}
                    }
                }
            }
        }
    }
}

/// Scans a document. `None` when the text holds no value at all (empty or only whitespace).
#[must_use]
pub fn parse(text: &str) -> Option<Value> {
    let mut s = Scanner { text, pos: 0 };
    s.value(0)
}

/// Splits a JSON Pointer (RFC 6901) into its unescaped reference tokens.
#[must_use]
pub fn pointer_tokens(pointer: &str) -> Vec<String> {
    if pointer.is_empty() {
        return Vec::new();
    }
    pointer
        .strip_prefix('/')
        .unwrap_or(pointer)
        .split('/')
        .map(|t| t.replace("~1", "/").replace("~0", "~"))
        .collect()
}

/// What a JSON Pointer resolved to.
#[derive(Debug, Clone, Copy)]
pub struct Target<'a> {
    /// The value.
    pub value: &'a Value,
    /// The member whose value it is, when the last token named an object member.
    pub member: Option<&'a Member>,
}

/// Resolves a JSON Pointer in the tree.
#[must_use]
pub fn locate<'a>(root: &'a Value, pointer: &str) -> Option<Target<'a>> {
    let mut target = Target {
        value: root,
        member: None,
    };
    for token in pointer_tokens(pointer) {
        target = match &target.value.kind {
            Kind::Object { members, .. } => {
                let m = members.iter().find(|m| m.key == token)?;
                Target {
                    value: m.value.as_ref()?,
                    member: Some(m),
                }
            }
            Kind::Array { items, .. } => {
                let index: usize = token.parse().ok()?;
                Target {
                    value: items.get(index)?,
                    member: None,
                }
            }
            _ => return None,
        };
    }
    Some(target)
}

/// Resolves the longest prefix of the pointer that exists; the root when nothing does.
#[must_use]
pub fn locate_nearest<'a>(root: &'a Value, pointer: &str) -> Target<'a> {
    let mut p = pointer;
    loop {
        if let Some(t) = locate(root, p) {
            return t;
        }
        match p.rfind('/') {
            Some(i) => p = &p[..i],
            None => {
                return Target {
                    value: root,
                    member: None,
                };
            }
        }
    }
}

/// The byte range a diagnostic about `pointer` should cover.
///
/// A string or literal is covered whole. A container is not, because underlining a whole node
/// hides everything inside it: an object or array that is a member value gets its member name,
/// anything else its first line. The whole document (`""`) gets its first character.
#[must_use]
pub fn diagnostic_range(text: &str, root: Option<&Value>, pointer: &str) -> (usize, usize) {
    let Some(root) = root else {
        return (0, 0);
    };
    let target = locate_nearest(root, pointer);
    let v = target.value;
    if !v.is_container() {
        return (v.start, v.end);
    }
    if std::ptr::eq(v, root) {
        let next = text[v.start..]
            .chars()
            .next()
            .map_or(v.start, |c| v.start + c.len_utf8());
        return (v.start, next);
    }
    if let Some(m) = target.member {
        return (m.key_start, m.key_end);
    }
    let line_end = text[v.start..v.end]
        .find('\n')
        .map_or(v.end, |i| v.start + i);
    (v.start, line_end.max(v.start))
}

/// One step of a path into the tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Seg {
    /// An object member name.
    Key(String),
    /// An array index.
    Index(usize),
}

/// A borrowed path step, convenient for slice patterns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum P<'a> {
    /// An object member name.
    K(&'a str),
    /// An array index.
    I(usize),
}

/// Borrows a path for matching with slice patterns.
#[must_use]
pub fn view(path: &[Seg]) -> Vec<P<'_>> {
    path.iter()
        .map(|s| match s {
            Seg::Key(k) => P::K(k.as_str()),
            Seg::Index(i) => P::I(*i),
        })
        .collect()
}

/// What the cursor is on.
#[derive(Debug, Clone)]
pub enum Spot<'a> {
    /// On a value: a string (inside its quotes) or a literal.
    Value {
        /// Path from the root to the value.
        path: Vec<Seg>,
        /// The value.
        value: &'a Value,
    },
    /// Where a value is expected but none is written yet: after a member's colon, or between
    /// array items. The path ends with the member name or the index the new item would get.
    EmptyValue {
        /// Path from the root to the missing value.
        path: Vec<Seg>,
    },
    /// On a member name, or in an object where a new member name would go.
    Key {
        /// Path from the root to the object.
        object_path: Vec<Seg>,
        /// The object.
        object: &'a Value,
        /// The member whose name the cursor is on; `None` for a new member.
        member: Option<&'a Member>,
    },
}

/// Finds what the cursor at byte `offset` is on.
#[must_use]
pub fn spot_at(root: &Value, offset: usize) -> Option<Spot<'_>> {
    if !root.contains(offset) {
        return None;
    }
    descend(root, offset, Vec::new())
}

fn descend(v: &Value, offset: usize, path: Vec<Seg>) -> Option<Spot<'_>> {
    match &v.kind {
        Kind::Object { members, .. } => {
            for m in members {
                if m.key_contains(offset) {
                    return Some(Spot::Key {
                        object_path: path,
                        object: v,
                        member: Some(m),
                    });
                }
                if let Some(val) = &m.value
                    && val.contains(offset)
                {
                    let mut p = path;
                    p.push(Seg::Key(m.key.clone()));
                    return descend(val, offset, p);
                }
                if let Some(colon) = m.colon {
                    let until = m.value.as_ref().map_or(m.end, |val| val.start);
                    if offset > colon
                        && offset <= until
                        && m.value.as_ref().is_none_or(|val| offset < val.start)
                    {
                        let mut p = path;
                        p.push(Seg::Key(m.key.clone()));
                        return Some(Spot::EmptyValue { path: p });
                    }
                }
            }
            Some(Spot::Key {
                object_path: path,
                object: v,
                member: None,
            })
        }
        Kind::Array { items, .. } => {
            for (i, item) in items.iter().enumerate() {
                if item.contains(offset) {
                    let mut p = path;
                    p.push(Seg::Index(i));
                    return descend(item, offset, p);
                }
            }
            let index = items.iter().filter(|it| it.end <= offset).count();
            let mut p = path;
            p.push(Seg::Index(index));
            Some(Spot::EmptyValue { path: p })
        }
        Kind::String { .. } | Kind::Scalar => Some(Spot::Value { path, value: v }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = r#"{
  "workflow": "w",
  "nodes": [
    { "id": "a", "op": "hash@1" },
    { "id": "b/c~d", "inputs": ["a.artifacts", "x.y"], "config": { "url": "https://e.org/?q=\"1\"" } }
  ]
}"#;

    fn at(p: &str) -> Option<&'static str> {
        let root = parse(DOC)?;
        let t = locate(&root, p)?;
        Some(&DOC[t.value.start..t.value.end])
    }

    #[test]
    fn locates_values_by_pointer_like_the_vscode_locator() {
        assert_eq!(at("/workflow"), Some("\"w\""));
        assert_eq!(at("/nodes/0/op"), Some("\"hash@1\""));
        assert_eq!(at("/nodes/1/inputs/1"), Some("\"x.y\""));
        assert_eq!(at("/nodes/1/id"), Some("\"b/c~d\""));
        assert!(at("/nodes/1/config").unwrap().starts_with("{ \"url\""));
        assert_eq!(
            at("/nodes/1/config/url"),
            Some(r#""https://e.org/?q=\"1\"""#)
        );
        assert_eq!(at(""), Some(DOC));
        assert_eq!(at("/nodes/0/policy"), None);
        let root = parse(DOC).unwrap();
        let t = locate_nearest(&root, "/nodes/0/policy/retries");
        assert_eq!(
            &DOC[t.value.start..t.value.end],
            r#"{ "id": "a", "op": "hash@1" }"#
        );
        let id = locate(&root, "/nodes/1/id").unwrap();
        assert_eq!(id.value.as_str(), Some("b/c~d"));
    }

    #[test]
    fn diagnostic_ranges_are_small() {
        let root = parse(DOC);
        let (s, e) = diagnostic_range(DOC, root.as_ref(), "/nodes/1/config");
        assert_eq!(&DOC[s..e], "\"config\"");
        let (s, e) = diagnostic_range(DOC, root.as_ref(), "");
        assert_eq!(&DOC[s..e], "{");
        let (s, e) = diagnostic_range(DOC, root.as_ref(), "/nodes/0");
        assert_eq!(&DOC[s..e], r#"{ "id": "a", "op": "hash@1" }"#);
        assert_eq!(diagnostic_range("", None, "/x"), (0, 0));
    }

    #[test]
    fn tolerates_unfinished_documents() {
        let text = "{\n  \"nodes\": [\n    { \"id\": \"a\", \"op\": \"ha\n";
        let root = parse(text).unwrap();
        let op = locate(&root, "/nodes/0/op").unwrap();
        assert_eq!(op.value.as_str(), Some("ha"));
        let end = text.find("ha").unwrap() + 2;
        match spot_at(&root, end).unwrap() {
            Spot::Value { path, .. } => {
                assert_eq!(view(&path), [P::K("nodes"), P::I(0), P::K("op")]);
            }
            other => panic!("{other:?}"),
        }
        let text = r#"{"nodes": [{"id": "a", "inputs": [ ], "op": }]}"#;
        let root = parse(text).unwrap();
        let in_inputs = text.find("[ ]").unwrap() + 1;
        match spot_at(&root, in_inputs).unwrap() {
            Spot::EmptyValue { path } => assert_eq!(
                view(&path),
                [P::K("nodes"), P::I(0), P::K("inputs"), P::I(0)]
            ),
            other => panic!("{other:?}"),
        }
        let after_colon = text.find("\"op\": ").unwrap() + 6;
        match spot_at(&root, after_colon).unwrap() {
            Spot::EmptyValue { path } => {
                assert_eq!(view(&path), [P::K("nodes"), P::I(0), P::K("op")]);
            }
            other => panic!("{other:?}"),
        }
        assert!(parse("").is_none());
        assert!(parse("{\"a\": [1, 2,, }").is_some());
        assert!(parse("\"\\u").is_some());
        assert!(
            parse("{\"é\": \"ü\\ud83d\\ude00\"}")
                .unwrap()
                .get("é")
                .unwrap()
                .as_str()
                == Some("ü😀")
        );
    }

    #[test]
    fn spots_member_names_and_new_members() {
        let text = r#"{"policy": { "retries": 3,  }}"#;
        let root = parse(text).unwrap();
        let in_key = text.find("retries").unwrap() + 2;
        match spot_at(&root, in_key).unwrap() {
            Spot::Key {
                object_path,
                member,
                ..
            } => {
                assert_eq!(view(&object_path), [P::K("policy")]);
                assert_eq!(member.unwrap().key, "retries");
            }
            other => panic!("{other:?}"),
        }
        let new_member = text.find(",  ").unwrap() + 2;
        match spot_at(&root, new_member).unwrap() {
            Spot::Key { member, .. } => assert!(member.is_none()),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn deep_nesting_does_not_overflow() {
        let text = "[".repeat(100_000);
        assert!(parse(&text).is_some());
    }
}
