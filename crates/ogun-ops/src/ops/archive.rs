//! `archive.extract`: safe extraction of zip, tar, tar.gz and gzip files (ADR 0004: one of the small,
//! safety-relevant operators Ogun implements natively).
//!
//! Refused, with `op.unsafe_archive`: absolute entry paths, drive letters, `..`, backslashes, NUL;
//! symbolic links that point outside the tree; hard links; device files and FIFOs; duplicate entries;
//! encrypted zip entries. Symbolic links that stay inside the tree are skipped (not materialised) and
//! counted. Every limit is checked against the bytes actually written, not the sizes the archive declares:
//! total bytes (default 10 GiB), entry count (default 10 000) and the ratio of written to archive bytes
//! (default 100). File modes are not preserved (no setuid, no executables).
//!
//! Configuration: `{"max_total_bytes", "max_entries", "max_ratio"}`, each at most the default.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use serde_json::{Map, json};
use zeus_verify::archive::{ArchiveGuard, ArchiveLimits, EntryInfo, entry_path_escapes};

use super::{CoreResult, Job, input_name};
use crate::OutputFile;
use crate::error::{OpError, OpResult, config, io as io_err};
use crate::files;
use crate::metadata::merged;

/// Extraction limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Total bytes written.
    pub max_total_bytes: u64,
    /// Entries.
    pub max_entries: u64,
    /// Written bytes per archive byte.
    pub max_ratio: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_total_bytes: 10 << 30,
            max_entries: 10_000,
            max_ratio: 100,
        }
    }
}

/// One extracted file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extracted {
    /// The entry path inside the archive, `/`-separated.
    pub rel: String,
    /// Where it was written.
    pub path: PathBuf,
    /// Bytes written.
    pub size_bytes: u64,
}

fn unsafe_archive(message: impl Into<String>) -> OpError {
    OpError::new("op.unsafe_archive", message)
}

struct State<'a> {
    dest: &'a Path,
    limits: Limits,
    guard: ArchiveGuard,
    archive_bytes: u64,
    written: u64,
    out: Vec<Extracted>,
    skipped_links: u64,
}

impl State<'_> {
    fn check_name(&self, name: &str) -> OpResult<PathBuf> {
        if entry_path_escapes(name) || name.contains('\\') {
            return Err(unsafe_archive(
                "an archive entry path is absolute, has .. or a drive letter",
            ));
        }
        files::safe_relative(name.trim_end_matches('/'))
            .map_err(|_| unsafe_archive("an archive entry path is not a plain relative path"))
    }

    fn write_file(
        &mut self,
        name: &str,
        rel: &Path,
        declared: Option<u64>,
        compressed: u64,
        reader: &mut dyn Read,
    ) -> OpResult<()> {
        self.guard
            .observe(&EntryInfo {
                path: name,
                compressed_bytes: compressed,
                uncompressed_bytes: declared.unwrap_or(0),
                depth: 0,
                is_link: false,
            })
            .map_err(|v| unsafe_archive(format!("archive limit: {v}")))?;
        let path = self.dest.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| io_err("creating an extraction directory", &e))?;
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| match e.kind() {
                io::ErrorKind::AlreadyExists => {
                    unsafe_archive("an archive has two entries with the same path")
                }
                _ => io_err("creating an extracted file", &e),
            })?;
        let ratio_cap = self
            .archive_bytes
            .max(1)
            .saturating_mul(self.limits.max_ratio)
            .saturating_add(1 << 20);
        let cap = self
            .limits
            .max_total_bytes
            .min(ratio_cap)
            .saturating_sub(self.written);
        let entry_cap = declared.map_or(cap, |d| d.min(cap));
        let mut limited = reader.take(entry_cap.saturating_add(1));
        let n = io::copy(&mut limited, &mut file).map_err(|e| match e.kind() {
            io::ErrorKind::InvalidData | io::ErrorKind::UnexpectedEof => {
                unsafe_archive("an archive entry is corrupt")
            }
            _ => io_err("writing an extracted file", &e),
        })?;
        if n > entry_cap {
            let _ = std::fs::remove_file(&path);
            return Err(if declared.is_some_and(|d| n > d) && n <= cap {
                unsafe_archive("an archive entry is larger than it declares")
            } else {
                unsafe_archive("the archive expands beyond the byte or ratio limit")
            });
        }
        file.flush()
            .map_err(|e| io_err("writing an extracted file", &e))?;
        self.written += n;
        self.out.push(Extracted {
            rel: rel.to_string_lossy().replace('\\', "/"),
            path,
            size_bytes: n,
        });
        Ok(())
    }

    fn symlink(&mut self, rel: &Path, target: &Path) -> OpResult<()> {
        // `..` may still stay inside the tree, so the target is resolved lexically against the link's
        // directory instead of being refused outright.
        let target = target.to_string_lossy();
        let mut depth: i64 = rel.parent().map_or(0, |p| p.components().count() as i64);
        if target.starts_with('/') || target.contains(':') || target.contains('\\') {
            return Err(unsafe_archive(
                "an archive symlink points outside the extraction tree",
            ));
        }
        for seg in target.split('/') {
            match seg {
                "" | "." => {}
                ".." => depth -= 1,
                _ => depth += 1,
            }
            if depth < 0 {
                return Err(unsafe_archive(
                    "an archive symlink points outside the extraction tree",
                ));
            }
        }
        self.skipped_links += 1;
        Ok(())
    }
}

/// Extracts one archive file into `dest` (created).
///
/// # Errors
///
/// `op.unsafe_archive`, `op.input_invalid` for an unknown format, `op.io`.
pub fn extract_file(archive: &Path, dest: &Path, limits: &Limits) -> OpResult<Vec<Extracted>> {
    let name = archive
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    extract_named(archive, &name, dest, limits)
}

fn extract_named(
    archive: &Path,
    name: &str,
    dest: &Path,
    limits: &Limits,
) -> OpResult<Vec<Extracted>> {
    std::fs::create_dir_all(dest).map_err(|e| io_err("creating an extraction directory", &e))?;
    let archive_bytes = std::fs::metadata(archive)
        .map_err(|e| io_err("reading an archive", &e))?
        .len();
    let mut head = [0u8; 512];
    let n = {
        let mut f = File::open(archive).map_err(|e| io_err("reading an archive", &e))?;
        read_full(&mut f, &mut head).map_err(|e| io_err("reading an archive", &e))?
    };
    let head = &head[..n];
    let mut state = State {
        dest,
        limits: *limits,
        guard: ArchiveGuard::new(ArchiveLimits {
            max_ratio: limits.max_ratio,
            max_depth: 0,
            max_entries: limits.max_entries,
            max_total_bytes: limits.max_total_bytes,
        }),
        archive_bytes,
        written: 0,
        out: Vec::new(),
        skipped_links: 0,
    };
    if head.starts_with(b"PK\x03\x04") || head.starts_with(b"PK\x05\x06") {
        extract_zip(archive, &mut state)?;
    } else if head.starts_with(&[0x1f, 0x8b]) {
        let f = File::open(archive).map_err(|e| io_err("reading an archive", &e))?;
        let mut gz = flate2::read::GzDecoder::new(f);
        let mut inner = [0u8; 512];
        let m = read_full(&mut gz, &mut inner)
            .map_err(|_| unsafe_archive("a gzip stream is corrupt"))?;
        let is_tar = m >= 262 && &inner[257..262] == b"ustar";
        let f = File::open(archive).map_err(|e| io_err("reading an archive", &e))?;
        let mut gz = flate2::read::GzDecoder::new(f);
        if is_tar {
            extract_tar(&mut gz, &mut state)?;
        } else {
            let stem = name
                .strip_suffix(".gz")
                .or_else(|| name.strip_suffix(".GZ"))
                .filter(|s| !s.is_empty())
                .unwrap_or("data");
            let file_name = files::sanitize_name(stem, "data");
            let rel = PathBuf::from(&file_name);
            state.write_file(&file_name, &rel, None, archive_bytes, &mut gz)?;
        }
    } else if head.len() >= 262 && &head[257..262] == b"ustar" {
        let mut f = File::open(archive).map_err(|e| io_err("reading an archive", &e))?;
        extract_tar(&mut f, &mut state)?;
    } else {
        return Err(crate::error::input(
            "archive.extract: not a zip, tar, tar.gz or gzip file",
        ));
    }
    if state.skipped_links > 0 {
        tracing::debug!(
            links = state.skipped_links,
            "archive.extract skipped in-tree symbolic links"
        );
    }
    Ok(state.out)
}

fn read_full(r: &mut dyn Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..])? {
            0 => break,
            k => n += k,
        }
    }
    Ok(n)
}

fn extract_zip(archive: &Path, state: &mut State<'_>) -> OpResult<()> {
    let f = File::open(archive).map_err(|e| io_err("reading an archive", &e))?;
    let mut zip =
        zip::ZipArchive::new(f).map_err(|_| unsafe_archive("a zip archive is corrupt"))?;
    if zip.len() as u64 > state.limits.max_entries {
        return Err(unsafe_archive("archive limit: too many entries"));
    }
    for i in 0..zip.len() {
        let mut entry = zip
            .by_index(i)
            .map_err(|_| unsafe_archive("a zip entry is corrupt or encrypted"))?;
        let name = entry
            .name()
            .map_err(|_| unsafe_archive("a zip entry name is not valid UTF-8"))?
            .into_owned();
        let rel = state.check_name(&name)?;
        if entry.is_dir() {
            std::fs::create_dir_all(state.dest.join(&rel))
                .map_err(|e| io_err("creating an extraction directory", &e))?;
            continue;
        }
        if entry.is_symlink() {
            let mut target = String::new();
            entry
                .by_ref()
                .take(4096)
                .read_to_string(&mut target)
                .map_err(|_| unsafe_archive("a zip symlink is corrupt"))?;
            state.symlink(&rel, Path::new(&target))?;
            continue;
        }
        let mode = entry.unix_mode().unwrap_or(0o100_644);
        let file_type = mode & 0o170_000;
        if file_type != 0 && file_type != 0o100_000 {
            return Err(unsafe_archive(
                "a zip entry is a device, FIFO or other special file",
            ));
        }
        let (declared, compressed) = (entry.size(), entry.compressed_size());
        state.write_file(&name, &rel, Some(declared), compressed, &mut entry)?;
    }
    Ok(())
}

fn extract_tar(reader: &mut dyn Read, state: &mut State<'_>) -> OpResult<()> {
    let mut tar = tar::Archive::new(reader);
    let entries = tar
        .entries()
        .map_err(|_| unsafe_archive("a tar archive is corrupt"))?;
    for entry in entries {
        let mut entry = entry.map_err(|_| unsafe_archive("a tar entry is corrupt"))?;
        let name = String::from_utf8(entry.path_bytes().into_owned())
            .map_err(|_| unsafe_archive("a tar entry path is not UTF-8"))?;
        let kind = entry.header().entry_type();
        use tar::EntryType as T;
        match kind {
            T::XGlobalHeader | T::XHeader | T::GNULongName | T::GNULongLink => continue,
            _ => {}
        }
        let rel = state.check_name(&name)?;
        match kind {
            T::Directory => {
                std::fs::create_dir_all(state.dest.join(&rel))
                    .map_err(|e| io_err("creating an extraction directory", &e))?;
            }
            T::Regular | T::Continuous | T::GNUSparse => {
                let declared = entry
                    .header()
                    .size()
                    .map_err(|_| unsafe_archive("a tar entry size is corrupt"))?;
                // tar has no per-entry compressed size; the archive-wide ratio check applies instead.
                state.write_file(&name, &rel, Some(declared), declared.max(1), &mut entry)?;
            }
            T::Symlink => {
                let target = entry
                    .link_name()
                    .map_err(|_| unsafe_archive("a tar link is corrupt"))?
                    .map(|c| c.into_owned())
                    .unwrap_or_default();
                state.symlink(&rel, &target)?;
            }
            T::Link => return Err(unsafe_archive("an archive contains a hard link")),
            _ => {
                return Err(unsafe_archive(
                    "an archive contains a device, FIFO or other special file",
                ));
            }
        }
        if state.out.len() as u64 > state.limits.max_entries {
            return Err(unsafe_archive("archive limit: too many entries"));
        }
    }
    Ok(())
}

/// `archive.extract@1`.
///
/// # Errors
///
/// `op.unsafe_archive`, `op.config_invalid`, `op.input_invalid`, `op.io`.
pub fn extract(job: &Job) -> CoreResult<Vec<OutputFile>> {
    job.only(&["max_total_bytes", "max_entries", "max_ratio"])?;
    let d = Limits::default();
    let pick = |key: &str, default: u64| -> OpResult<u64> {
        match job.u64(key)? {
            None => Ok(default),
            Some(v) if v >= 1 && v <= default => Ok(v),
            Some(_) => Err(config(format!(
                "archive.extract: {key} must be 1 to {default}"
            ))),
        }
    };
    let limits = Limits {
        max_total_bytes: pick("max_total_bytes", d.max_total_bytes)?,
        max_entries: pick("max_entries", d.max_entries)?,
        max_ratio: pick("max_ratio", d.max_ratio)?,
    };
    let base = job.out_dir("artifacts")?;
    let mut out = Vec::new();
    for (i, input) in job.inputs.iter().enumerate() {
        job.check_cancel()?;
        let dest = base.join(format!("{:04}", i + 1));
        let name = input_name(input);
        let extracted = extract_named(&input.path, &name, &dest, &limits)?;
        let count = extracted.len();
        for x in extracted {
            let mut extra = Map::new();
            let file_name = Path::new(&x.rel)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            extra.insert("archive_path".into(), json!(x.rel));
            extra.insert("archive_blake3".into(), json!(input.blake3));
            extra.insert("filename".into(), json!(file_name));
            extra.insert("size_bytes".into(), json!(x.size_bytes));
            out.push(OutputFile {
                port: "artifacts".into(),
                media_type: files::media_type_for(&file_name).to_owned(),
                path: x.path,
                metadata: Some(merged(None, extra)),
            });
        }
        job.log(format!(
            "archive.extract: input {} gave {count} files",
            i + 1
        ));
        job.progress(i as u64 + 1, Some(job.inputs.len() as u64));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tar_with(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut b = tar::Builder::new(Vec::new());
        for (name, data) in entries {
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(0o644);
            h.set_entry_type(tar::EntryType::Regular);
            // Bypass the builder's own path checks to write hostile names.
            let raw = &mut h.as_old_mut().name;
            raw[..name.len()].copy_from_slice(name.as_bytes());
            h.set_cksum();
            b.append(&h, *data).unwrap();
        }
        b.into_inner().unwrap()
    }

    #[test]
    fn extracts_and_refuses_escapes() {
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("good.tar");
        std::fs::write(&good, tar_with(&[("a/b.txt", b"hello"), ("c.txt", b"x")])).unwrap();
        let out = extract_file(&good, &dir.path().join("x"), &Limits::default()).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(
            std::fs::read(dir.path().join("x/a/b.txt")).unwrap(),
            b"hello"
        );

        let bad = dir.path().join("bad.tar");
        std::fs::write(&bad, tar_with(&[("../evil.txt", b"x")])).unwrap();
        let err = extract_file(&bad, &dir.path().join("y"), &Limits::default()).unwrap_err();
        assert_eq!(err.error.code, "op.unsafe_archive");
        assert!(!dir.path().join("evil.txt").exists());
    }

    #[test]
    fn refuses_bombs() {
        let dir = tempfile::tempdir().unwrap();
        let zeros = vec![0u8; 4 << 20];
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
        gz.write_all(&tar_with(&[("z.bin", &zeros)])).unwrap();
        let path = dir.path().join("bomb.tar.gz");
        std::fs::write(&path, gz.finish().unwrap()).unwrap();
        let limits = Limits {
            max_ratio: 2,
            ..Limits::default()
        };
        let err = extract_file(&path, &dir.path().join("z"), &limits).unwrap_err();
        assert_eq!(err.error.code, "op.unsafe_archive");
    }
}
