//! Directory helpers: content snapshots and their differences, tree digests, and plain recursive copies.
//!
//! Symbolic links are never followed: a snapshot records a link as a file whose content is its target text, and a
//! copy recreates nothing for links on platforms without them. That keeps a task directory from pulling files from
//! outside itself into an environment.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use ogun_lineage::TrajectoryKind;
use serde::{Deserialize, Serialize};

use crate::{BenchError, BenchResult};

/// Most files one snapshot or copy visits.
pub const MAX_FILES: usize = 200_000;

/// One file in a snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// BLAKE3 of the content (or `unreadable`).
    pub blake3: String,
    /// Size.
    pub size_bytes: u64,
}

/// Relative path (with `/` separators) to entry.
pub type Snapshot = BTreeMap<String, Entry>;

fn rel(root: &Path, p: &Path) -> String {
    p.strip_prefix(root)
        .unwrap_or(p)
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

fn hash_file(path: &Path) -> (String, u64) {
    match std::fs::File::open(path) {
        Ok(mut f) => {
            let mut h = blake3::Hasher::new();
            match std::io::copy(&mut f, &mut h) {
                Ok(n) => (h.finalize().to_hex().to_string(), n),
                Err(_) => ("unreadable".into(), 0),
            }
        }
        Err(_) => ("unreadable".into(), 0),
    }
}

/// Walks `root` and hashes every regular file. Directories named in `skip` (relative, first component) are left
/// out.
///
/// # Errors
///
/// [`BenchError::Io`] when `root` cannot be read; [`BenchError::Environment`] past [`MAX_FILES`].
pub fn snapshot(root: &Path, skip: &[&str]) -> BenchResult<Snapshot> {
    let mut out = Snapshot::new();
    if !root.exists() {
        return Ok(out);
    }
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries =
            std::fs::read_dir(&dir).map_err(BenchError::io(format!("read {}", rel(root, &dir))))?;
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(ft) = entry.file_type() else { continue };
            let r = rel(root, &path);
            if skip
                .iter()
                .any(|s| r == *s || r.starts_with(&format!("{s}/")))
            {
                continue;
            }
            if ft.is_dir() {
                stack.push(path);
            } else if ft.is_symlink() {
                let target = std::fs::read_link(&path)
                    .map(|t| t.to_string_lossy().into_owned())
                    .unwrap_or_default();
                out.insert(
                    r,
                    Entry {
                        blake3: blake3::hash(target.as_bytes()).to_hex().to_string(),
                        size_bytes: 0,
                    },
                );
            } else if ft.is_file() {
                let (blake3, size_bytes) = hash_file(&path);
                out.insert(r, Entry { blake3, size_bytes });
            }
            if out.len() > MAX_FILES {
                return Err(BenchError::Environment(format!(
                    "more than {MAX_FILES} files in a workspace"
                )));
            }
        }
    }
    Ok(out)
}

/// BLAKE3 over the sorted `(path, blake3, size)` list: the identity of a directory's content.
#[must_use]
pub fn tree_digest(snap: &Snapshot) -> String {
    let mut h = blake3::Hasher::new();
    for (path, e) in snap {
        h.update(path.as_bytes());
        h.update(&[0]);
        h.update(e.blake3.as_bytes());
        h.update(&e.size_bytes.to_be_bytes());
    }
    h.finalize().to_hex().to_string()
}

/// The file events between two snapshots, in path order.
#[must_use]
pub fn diff(before: &Snapshot, after: &Snapshot) -> Vec<TrajectoryKind> {
    let mut out = vec![];
    for (path, a) in after {
        match before.get(path) {
            None => out.push(TrajectoryKind::FileCreated {
                path: path.clone(),
                blake3: a.blake3.clone(),
                size_bytes: a.size_bytes,
            }),
            Some(b) if b != a => out.push(TrajectoryKind::FilePatch {
                path: path.clone(),
                before_blake3: b.blake3.clone(),
                blake3: a.blake3.clone(),
                size_bytes: a.size_bytes,
            }),
            Some(_) => {}
        }
    }
    for (path, b) in before {
        if !after.contains_key(path) {
            out.push(TrajectoryKind::FileDeleted {
                path: path.clone(),
                before_blake3: b.blake3.clone(),
            });
        }
    }
    out.sort_by(|x, y| path_of(x).cmp(path_of(y)));
    out
}

fn path_of(k: &TrajectoryKind) -> &str {
    match k {
        TrajectoryKind::FileCreated { path, .. }
        | TrajectoryKind::FilePatch { path, .. }
        | TrajectoryKind::FileDeleted { path, .. } => path,
        _ => "",
    }
}

/// Copies `from` into `to` recursively (regular files and directories only; links are skipped).
///
/// # Errors
///
/// [`BenchError::Io`].
pub fn copy_dir(from: &Path, to: &Path) -> BenchResult<u64> {
    let mut n = 0u64;
    std::fs::create_dir_all(to).map_err(BenchError::io("create a copy target"))?;
    let mut stack: Vec<(PathBuf, PathBuf)> = vec![(from.to_path_buf(), to.to_path_buf())];
    while let Some((src, dst)) = stack.pop() {
        for entry in std::fs::read_dir(&src)
            .map_err(BenchError::io(format!("read {}", src.display())))?
            .flatten()
        {
            let Ok(ft) = entry.file_type() else { continue };
            let target = dst.join(entry.file_name());
            if ft.is_dir() {
                std::fs::create_dir_all(&target).map_err(BenchError::io("create a directory"))?;
                stack.push((entry.path(), target));
            } else if ft.is_file() {
                std::fs::copy(entry.path(), &target)
                    .map_err(BenchError::io(format!("copy {}", entry.path().display())))?;
                n += 1;
                if n > MAX_FILES as u64 {
                    return Err(BenchError::Environment(format!(
                        "more than {MAX_FILES} files to copy"
                    )));
                }
            }
        }
    }
    Ok(n)
}

/// Makes every file and directory under `root` read-only (best effort; the container mounts it read-only anyway).
pub fn make_read_only(root: &Path) {
    let mut stack = vec![root.to_path_buf()];
    while let Some(p) = stack.pop() {
        if let Ok(rd) = std::fs::read_dir(&p) {
            for e in rd.flatten() {
                if e.file_type().is_ok_and(|t| t.is_dir()) {
                    stack.push(e.path());
                } else if let Ok(meta) = e.metadata() {
                    let mut perm = meta.permissions();
                    perm.set_readonly(true);
                    let _ = std::fs::set_permissions(e.path(), perm);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_reports_created_patched_and_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        std::fs::write(r.join("a"), "1").unwrap();
        std::fs::write(r.join("b"), "2").unwrap();
        std::fs::create_dir(r.join(".git")).unwrap();
        std::fs::write(r.join(".git").join("x"), "x").unwrap();
        let before = snapshot(r, &[".git"]).unwrap();
        assert_eq!(before.len(), 2);
        std::fs::write(r.join("a"), "changed").unwrap();
        std::fs::remove_file(r.join("b")).unwrap();
        std::fs::create_dir(r.join("d")).unwrap();
        std::fs::write(r.join("d").join("c"), "3").unwrap();
        let after = snapshot(r, &[".git"]).unwrap();
        let events = diff(&before, &after);
        assert_eq!(events.len(), 3);
        assert!(matches!(&events[0], TrajectoryKind::FilePatch { path, .. } if path == "a"));
        assert!(matches!(&events[1], TrajectoryKind::FileDeleted { path, .. } if path == "b"));
        assert!(matches!(&events[2], TrajectoryKind::FileCreated { path, .. } if path == "d/c"));
        assert_ne!(tree_digest(&before), tree_digest(&after));
    }
}
