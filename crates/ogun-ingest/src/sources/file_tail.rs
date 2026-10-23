//! `stream.file_tail`: follows a JSON Lines or CSV file written by a data logger (O-10 section 2).
//!
//! * The byte offset of the last committed line is durable: it is written (atomically, after the
//!   append commits) to `<log_dir>/<workflow>/<node>.tail.json` together with the file's identity
//!   (device and inode on Unix) and the CSV header. A crash between the commit and the offset
//!   write re-reads those lines; the stream's `dedupe_key` removes the copies.
//! * **Rotation:** when the path names another file than the one being read, the old file is read
//!   to its end and then the new one from offset 0.
//! * **Truncation:** when the file is shorter than the offset, it is read again from 0.
//! * Only complete lines (ending in `\n`) are read; a partial last line waits for the next poll.
//!
//! Node configuration: `path` (or `path` in `ingest.json`), `format` (`jsonl` or `csv`),
//! `columns` (CSV without a header line; otherwise the first line is the header), `poll_ms`
//! (default 1 000), `start` (`beginning` or `end`, for a file seen for the first time; default
//! `beginning`).

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::applog::Reply;
use crate::decode::{csv_fields, csv_object, json_objects, now_ms};
use crate::status::Counters;

use super::Ctx;

/// Most bytes read per poll.
const MAX_READ_BYTES: usize = 4 * 1024 * 1024;
/// Most lines per append.
const MAX_LINES_PER_APPEND: usize = 1_000;

/// The durable position.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Position {
    /// File identity: `dev:ino` on Unix, empty elsewhere.
    pub identity: String,
    /// Bytes consumed.
    pub offset: u64,
    /// CSV header read from the file, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<Vec<String>>,
}

/// A file's identity.
pub fn identity(meta: &std::fs::Metadata) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        format!("{}:{}", meta.dev(), meta.ino())
    }
    #[cfg(not(unix))]
    {
        meta.created()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis().to_string())
            .unwrap_or_default()
    }
}

fn load_position(path: &Path) -> Option<Position> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

fn save_position(path: &Path, pos: &Position) -> Result<(), String> {
    let bytes = serde_json::to_vec(pos).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, &bytes)
        .and_then(|()| std::fs::File::open(&tmp)?.sync_all())
        .and_then(|()| std::fs::rename(&tmp, path))
        .map_err(|e| format!("cannot save the tail offset: {:?}", e.kind()))
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Format {
    Jsonl,
    Csv(Option<Vec<String>>),
}

struct Tail {
    path: PathBuf,
    file: Option<std::fs::File>,
    pos: Position,
}

/// Splits `buf` into complete lines; returns them and the bytes they span.
pub fn complete_lines(buf: &[u8]) -> (Vec<&[u8]>, usize) {
    let Some(end) = buf.iter().rposition(|b| *b == b'\n') else {
        return (Vec::new(), 0);
    };
    let lines = buf[..end].split(|b| *b == b'\n').collect();
    (lines, end + 1)
}

impl Tail {
    fn read_chunk(&mut self) -> std::io::Result<Vec<u8>> {
        let Some(f) = self.file.as_mut() else {
            return Ok(Vec::new());
        };
        f.seek(SeekFrom::Start(self.pos.offset))?;
        let mut buf = Vec::new();
        f.take(MAX_READ_BYTES as u64).read_to_end(&mut buf)?;
        Ok(buf)
    }

    /// Opens the file when none is open, follows a rotation once the current file has been read
    /// to its end (`at_end`), and restarts a truncated file from 0.
    fn check_rotation(&mut self, at_end: bool) {
        let Ok(meta) = std::fs::metadata(&self.path) else {
            return;
        };
        let id = identity(&meta);
        if self.file.is_none() {
            if let Ok(f) = std::fs::File::open(&self.path) {
                if id != self.pos.identity {
                    self.pos = Position {
                        identity: id,
                        offset: 0,
                        header: None,
                    };
                } else if meta.len() < self.pos.offset {
                    self.pos.offset = 0;
                    self.pos.header = None;
                }
                self.file = Some(f);
            }
            return;
        }
        if id != self.pos.identity {
            if at_end && let Ok(f) = std::fs::File::open(&self.path) {
                tracing::info!(file = %self.path.display(), "file rotated; following the new file");
                self.file = Some(f);
                self.pos = Position {
                    identity: id,
                    offset: 0,
                    header: None,
                };
            }
        } else if meta.len() < self.pos.offset {
            tracing::info!(file = %self.path.display(), "file truncated; reading from the start");
            self.pos.offset = 0;
            self.pos.header = None;
        }
    }
}

/// Runs `stream.file_tail`.
///
/// # Errors
///
/// A configuration error.
pub async fn run(ctx: Ctx) -> Result<(), String> {
    let path = ctx
        .overrides
        .path
        .clone()
        .or_else(|| ctx.spec.config_str("path").map(PathBuf::from))
        .ok_or("stream.file_tail needs a path")?;
    let format = match ctx.spec.config_str("format").unwrap_or("jsonl") {
        "jsonl" | "ndjson" => Format::Jsonl,
        "csv" => {
            let cols = ctx.spec.config_strings("columns");
            Format::Csv((!cols.is_empty()).then_some(cols))
        }
        _ => return Err("format must be jsonl or csv".into()),
    };
    let poll = Duration::from_millis(ctx.spec.config_u64("poll_ms").unwrap_or(1_000).max(100));
    let from_end = match ctx.spec.config_str("start").unwrap_or("beginning") {
        "beginning" => false,
        "end" => true,
        _ => return Err("start must be beginning or end".into()),
    };
    std::fs::create_dir_all(&ctx.state_dir)
        .map_err(|e| format!("cannot create the state directory: {:?}", e.kind()))?;
    let pos_file = ctx.state_dir.join(format!("{}.tail.json", ctx.spec.node));
    let pos = match load_position(&pos_file) {
        Some(p) => p,
        None if from_end => std::fs::metadata(&path)
            .map(|m| Position {
                identity: identity(&m),
                offset: m.len(),
                header: None,
            })
            .unwrap_or_default(),
        None => Position::default(),
    };
    let mut tail = Tail {
        path: path.clone(),
        file: None,
        pos,
    };
    let id = ctx.spec.id();
    let origin = format!(
        "file:{}",
        path.file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
    );
    let mut at_end = false;
    loop {
        tail.check_rotation(at_end);
        let buf = match tail.read_chunk() {
            Ok(b) => b,
            Err(e) => {
                Counters::add(&ctx.counters.source_errors, 1);
                ctx.counters.set_error(format!("file tail: {:?}", e.kind()));
                tail.file = None;
                tokio::time::sleep(poll).await;
                continue;
            }
        };
        let (lines, _) = complete_lines(&buf);
        // Read to the end of the file (a partial last line may remain: it is either completed by
        // the writer, or dropped when the file is rotated away).
        at_end = buf.len() < MAX_READ_BYTES;
        if lines.is_empty() {
            tokio::time::sleep(poll).await;
            continue;
        }
        // Commit in slices so one append stays bounded; the offset advances per slice.
        let mut offset = tail.pos.offset;
        let mut iter = lines.into_iter().peekable();
        let mut refused = false;
        while iter.peek().is_some() {
            let mut objects = Vec::new();
            let mut invalid = 0u64;
            let mut slice_bytes = 0u64;
            for line in iter.by_ref().take(MAX_LINES_PER_APPEND) {
                slice_bytes += line.len() as u64 + 1;
                let text = String::from_utf8_lossy(line);
                if text.trim().is_empty() {
                    continue;
                }
                match &format {
                    Format::Jsonl => {
                        let (mut o, bad) = json_objects(line);
                        invalid += bad;
                        objects.append(&mut o);
                    }
                    Format::Csv(cols) => {
                        let header = cols.clone().or_else(|| tail.pos.header.clone());
                        match header {
                            Some(h) => match csv_object(&h, &text) {
                                Some(o) => objects.push(o),
                                None => invalid += 1,
                            },
                            None => {
                                tail.pos.header = csv_fields(&text)
                                    .map(|f| f.into_iter().map(|s| s.trim().to_owned()).collect());
                            }
                        }
                    }
                }
            }
            Counters::add(&ctx.counters.messages, 1);
            let (records, bad) = ctx.shaper.shape_all(objects, &origin, now_ms(), None);
            Counters::add(&ctx.counters.invalid, invalid + bad);
            match ctx.log.append(records).await {
                Reply::Committed { .. } => {
                    offset += slice_bytes;
                    tail.pos.offset = offset;
                    if let Err(e) = save_position(&pos_file, &tail.pos) {
                        ctx.counters.set_error(e.clone());
                        tracing::error!(source = %id, error = %e, "cannot save the tail offset");
                    }
                }
                Reply::Refused => {
                    refused = true;
                    break;
                }
            }
        }
        if refused {
            tracing::warn!(source = %id, "ingest log refused lines; retrying from the last committed line");
            if tail.pos.offset == 0 {
                // The header line will be read again.
                tail.pos.header = None;
            }
            at_end = false;
        }
        if refused || at_end {
            tokio::time::sleep(poll).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_complete_lines_are_consumed() {
        let (lines, n) = complete_lines(b"a\nb\nparti");
        assert_eq!(lines, vec![&b"a"[..], &b"b"[..]]);
        assert_eq!(n, 4);
        let (lines, n) = complete_lines(b"partial");
        assert!(lines.is_empty());
        assert_eq!(n, 0);
    }

    #[test]
    fn rotation_and_truncation_reset_the_offset() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log.jsonl");
        std::fs::write(&path, b"one\ntwo\n").unwrap();
        let mut t = Tail {
            path: path.clone(),
            file: None,
            pos: Position::default(),
        };
        t.check_rotation(false);
        assert_eq!(t.pos.offset, 0);
        t.pos.offset = 8;
        // Truncated in place.
        std::fs::write(&path, b"x\n").unwrap();
        t.check_rotation(false);
        assert_eq!(t.pos.offset, 0);
        // Rotated: a new file at the path; switch once the old one is read to its end.
        t.pos.offset = 2;
        std::fs::rename(&path, dir.path().join("log.jsonl.1")).unwrap();
        std::fs::write(&path, b"new\n").unwrap();
        t.check_rotation(false);
        assert_eq!(t.pos.offset, 2, "keep reading the old file until its end");
        t.check_rotation(true);
        assert_eq!(t.pos.offset, 0);
        assert_eq!(t.read_chunk().unwrap(), b"new\n");
    }
}
