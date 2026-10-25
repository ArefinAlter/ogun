//! File helpers for operators: digests, safe relative paths inside scratch, links and media types.
//!
//! Every path an operator writes is inside its attempt's scratch directory. Relative names that come from
//! configuration, templates, archives or child operators go through [`safe_relative`] (no absolute paths,
//! no `..`, no empty or `.` components, no separators or NUL inside a component), and paths reported by a
//! child are checked with [`inside`] after resolving symlinks.

use std::fs::File;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

use zeus_verify::hash::DualHasher;

use crate::error::{OpResult, config, io as io_err};

/// BLAKE3, SHA-256 and size of a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDigests {
    /// BLAKE3, lowercase hex.
    pub blake3: String,
    /// SHA-256, lowercase hex.
    pub sha256: String,
    /// Size in bytes.
    pub size_bytes: u64,
}

/// Hashes a file with BLAKE3 and SHA-256 (blocking).
///
/// # Errors
///
/// The I/O error.
pub fn digests(path: &Path) -> io::Result<FileDigests> {
    let mut file = File::open(path)?;
    let mut hasher = DualHasher::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let d = hasher.finalize();
    Ok(FileDigests {
        blake3: d.blake3.to_hex(),
        sha256: d.sha256.to_hex(),
        size_bytes: d.size_bytes,
    })
}

/// BLAKE3 and size of a file (blocking).
///
/// # Errors
///
/// The I/O error.
pub fn blake3_of(path: &Path) -> io::Result<(String, u64)> {
    let mut file = File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let n = io::copy(&mut file, &mut hasher)?;
    Ok((hasher.finalize().to_hex().to_string(), n))
}

/// Checks a relative path from configuration or an archive and returns it as a `PathBuf`.
///
/// # Errors
///
/// `op.config_invalid` naming the rule, never the path.
pub fn safe_relative(rel: &str) -> OpResult<PathBuf> {
    if rel.is_empty() || rel.len() > 4096 || rel.contains('\0') || rel.contains('\\') {
        return Err(config(
            "a relative path is empty, too long or contains NUL or a backslash",
        ));
    }
    if rel.starts_with('/') || (rel.len() > 1 && rel.as_bytes()[1] == b':') {
        return Err(config("a relative path is absolute"));
    }
    let mut out = PathBuf::new();
    for seg in rel.split('/') {
        if seg.is_empty() {
            continue;
        }
        if seg == "." || seg == ".." {
            return Err(config("a relative path contains . or .."));
        }
        if seg.len() > 255 || seg.chars().any(char::is_control) || seg.contains(':') {
            return Err(config(
                "a path component is too long or contains a control character or colon",
            ));
        }
        out.push(seg);
    }
    if out.as_os_str().is_empty() {
        return Err(config("a relative path has no components"));
    }
    if out.components().any(|c| !matches!(c, Component::Normal(_))) {
        return Err(config("a relative path is not plain"));
    }
    Ok(out)
}

/// True when `path` (after resolving symlinks) is inside `root` (after resolving symlinks).
#[must_use]
pub fn inside(root: &Path, path: &Path) -> bool {
    match (root.canonicalize(), path.canonicalize()) {
        (Ok(r), Ok(p)) => p.starts_with(&r) && p != r,
        _ => false,
    }
}

/// A single safe file name from untrusted text (Zeus 01 section 12.3 rules).
#[must_use]
pub fn sanitize_name(raw: &str, fallback: &str) -> String {
    let options = zeus_fs::names::SanitizeOptions {
        fallback: fallback.to_owned(),
        ..Default::default()
    };
    zeus_fs::sanitize_filename(raw, &options).map_or_else(|_| fallback.to_owned(), |n| n.name)
}

/// `dir/name`, or `dir/stem (n).ext` when that exists.
#[must_use]
pub fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let p = Path::new(name);
    let stem = p
        .file_stem()
        .map_or_else(|| name.to_owned(), |s| s.to_string_lossy().into_owned());
    let ext = p.extension().map(|e| e.to_string_lossy().into_owned());
    for n in 1..100_000 {
        let candidate = match &ext {
            Some(e) => dir.join(format!("{stem} ({n}).{e}")),
            None => dir.join(format!("{stem} ({n})")),
        };
        if !candidate.exists() {
            return candidate;
        }
    }
    dir.join(format!("{stem}-{}", std::process::id()))
}

/// Places `src` at `dst` without copying when possible: a hard link (stored objects are immutable),
/// falling back to a copy across file systems.
///
/// # Errors
///
/// `op.io`.
pub fn link_or_copy(src: &Path, dst: &Path) -> OpResult<()> {
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).map_err(|e| io_err("creating an output directory", &e))?;
    }
    if std::fs::hard_link(src, dst).is_ok() {
        return Ok(());
    }
    std::fs::copy(src, dst).map_err(|e| io_err("copying an input into scratch", &e))?;
    Ok(())
}

/// Creates `dir` and its parents.
///
/// # Errors
///
/// `op.io`.
pub fn ensure_dir(dir: &Path) -> OpResult<()> {
    std::fs::create_dir_all(dir).map_err(|e| io_err("creating a scratch directory", &e))
}

/// A media type guessed from a file name's extension; `application/octet-stream` when unknown.
#[must_use]
pub fn media_type_for(name: &str) -> &'static str {
    let lower = name.to_ascii_lowercase();
    let table: &[(&str, &str)] = &[
        (".csv.gz", "application/gzip"),
        (".tar.gz", "application/gzip"),
        (".tgz", "application/gzip"),
        (".gz", "application/gzip"),
        (".zip", "application/zip"),
        (".tar", "application/x-tar"),
        (".jsonl", "application/x-ndjson"),
        (".ndjson", "application/x-ndjson"),
        (".json", "application/json"),
        (".csv", "text/csv"),
        (".tsv", "text/tab-separated-values"),
        (".txt", "text/plain"),
        (".md", "text/markdown"),
        (".html", "text/html"),
        (".htm", "text/html"),
        (".xml", "application/xml"),
        (".pdf", "application/pdf"),
        (".parquet", "application/vnd.apache.parquet"),
        (".png", "image/png"),
        (".jpg", "image/jpeg"),
        (".jpeg", "image/jpeg"),
        (".tif", "image/tiff"),
        (".tiff", "image/tiff"),
        (".webp", "image/webp"),
        (".mp3", "audio/mpeg"),
        (".wav", "audio/wav"),
        (".flac", "audio/flac"),
        (".mp4", "video/mp4"),
        (".webm", "video/webm"),
        (".mkv", "video/x-matroska"),
        (
            ".docx",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        ),
        (".epub", "application/epub+zip"),
        (".duckdb", "application/vnd.duckdb"),
        (".sqlite", "application/vnd.sqlite3"),
        (".yaml", "application/yaml"),
        (".yml", "application/yaml"),
    ];
    table
        .iter()
        .find(|(ext, _)| lower.ends_with(ext))
        .map_or("application/octet-stream", |(_, mt)| mt)
}

/// True for media types and names that hold JSON Lines.
#[must_use]
pub fn is_jsonl(media_type: &str, path: &Path) -> bool {
    let mt = media_type.split(';').next().unwrap_or("").trim();
    matches!(
        mt,
        "application/x-ndjson" | "application/jsonl" | "application/jsonlines"
    ) || path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("jsonl") || e.eq_ignore_ascii_case("ndjson"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths() {
        assert_eq!(safe_relative("a/b.txt").unwrap(), PathBuf::from("a/b.txt"));
        assert_eq!(safe_relative("a//b/").unwrap(), PathBuf::from("a/b"));
        for bad in [
            "",
            "/etc/passwd",
            "../x",
            "a/../../x",
            "a\\b",
            "C:x",
            "./a",
            "a\0b",
        ] {
            assert!(safe_relative(bad).is_err(), "{bad:?}");
        }
    }
}
