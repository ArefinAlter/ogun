//! Byte offsets to LSP positions and back.
//!
//! LSP positions are a zero-based line and a character offset counted in the negotiated
//! encoding: UTF-16 code units by default, UTF-8 bytes when the client offers it.

use lsp_types::{Position, Range};

/// How `Position::character` counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    /// UTF-16 code units (the LSP default).
    Utf16,
    /// UTF-8 bytes.
    Utf8,
}

/// Line starts of one document, for offset conversions.
#[derive(Debug, Clone)]
pub struct LineIndex {
    starts: Vec<usize>,
    encoding: Encoding,
}

impl LineIndex {
    /// Indexes `text`. Lines end at `\n`; a preceding `\r` stays on its line.
    #[must_use]
    pub fn new(text: &str, encoding: Encoding) -> Self {
        let mut starts = vec![0];
        starts.extend(
            text.bytes()
                .enumerate()
                .filter(|(_, b)| *b == b'\n')
                .map(|(i, _)| i + 1),
        );
        Self { starts, encoding }
    }

    /// The position of a byte offset. Offsets past the end clamp to the end.
    #[must_use]
    pub fn position(&self, text: &str, offset: usize) -> Position {
        let offset = floor_boundary(text, offset.min(text.len()));
        let line = self.starts.partition_point(|s| *s <= offset) - 1;
        let start = self.starts[line];
        let segment = &text[start..offset];
        let character = match self.encoding {
            Encoding::Utf8 => segment.len(),
            Encoding::Utf16 => segment.chars().map(char::len_utf16).sum(),
        };
        Position {
            line: u32::try_from(line).unwrap_or(u32::MAX),
            character: u32::try_from(character).unwrap_or(u32::MAX),
        }
    }

    /// The byte offset of a position. A character past the end of its line clamps to the line
    /// end; a line past the end clamps to the end of the text.
    #[must_use]
    pub fn offset(&self, text: &str, position: Position) -> usize {
        let line = position.line as usize;
        let Some(&start) = self.starts.get(line) else {
            return text.len();
        };
        let end = self
            .starts
            .get(line + 1)
            .map_or(text.len(), |next| next - 1);
        let line_text = &text[start..end];
        let want = position.character as usize;
        let mut counted = 0;
        for (i, c) in line_text.char_indices() {
            if counted >= want {
                return start + i;
            }
            counted += match self.encoding {
                Encoding::Utf8 => c.len_utf8(),
                Encoding::Utf16 => c.len_utf16(),
            };
        }
        end
    }

    /// The range of `[start, end)`.
    #[must_use]
    pub fn range(&self, text: &str, start: usize, end: usize) -> Range {
        Range {
            start: self.position(text, start),
            end: self.position(text, end.max(start)),
        }
    }

    /// The offset where `line` (zero-based) starts, if the document has it.
    #[must_use]
    pub fn line_start(&self, line: usize) -> Option<usize> {
        self.starts.get(line).copied()
    }
}

fn floor_boundary(text: &str, mut offset: usize) -> usize {
    while offset > 0 && !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_in_both_encodings() {
        let text = "{\n  \"a\": \"é😀x\"\r\n}";
        for enc in [Encoding::Utf16, Encoding::Utf8] {
            let idx = LineIndex::new(text, enc);
            for (offset, _) in text.char_indices() {
                let p = idx.position(text, offset);
                assert_eq!(idx.offset(text, p), offset, "{enc:?} {offset}");
            }
        }
        let idx = LineIndex::new(text, Encoding::Utf16);
        let x = text.find('x').unwrap();
        assert_eq!(idx.position(text, x).character, 11);
        let idx8 = LineIndex::new(text, Encoding::Utf8);
        assert_eq!(idx8.position(text, x).character, 14);
        assert_eq!(
            idx.offset(
                text,
                Position {
                    line: 9,
                    character: 0
                }
            ),
            text.len()
        );
    }
}
