//! The `local-files` connector: a directory on this computer.

use std::path::{Path, PathBuf};

use super::{Check, Delivered, ResolvedConnection, check, clean_relative};
use crate::InputArtifact;
use crate::error::{OpError, OpResult, config, io as io_err};

fn root(c: &ResolvedConnection) -> OpResult<PathBuf> {
    let dir = c
        .text("directory")
        .ok_or_else(|| config("the connection has no directory"))?;
    let p = PathBuf::from(dir);
    if !p.is_absolute() {
        return Err(config("the directory must be an absolute path"));
    }
    Ok(p)
}

pub(super) fn probe(c: &ResolvedConnection) -> Vec<Check> {
    let mut out = Vec::new();
    let dir = match root(c) {
        Ok(d) => d,
        Err(e) => return vec![check("directory", false, e.error.message)],
    };
    if !dir.is_dir() {
        out.push(check(
            "directory",
            false,
            "the directory does not exist (create it first; Ogun never creates the top folder)",
        ));
        return out;
    }
    out.push(check("directory", true, "the directory exists"));
    let probe = dir.join(format!(".ogun-probe-{}.tmp", std::process::id()));
    let written = std::fs::write(&probe, b"ogun").and_then(|()| std::fs::read(&probe));
    let removed = std::fs::remove_file(&probe);
    match (written, removed) {
        (Ok(b), Ok(())) if b == b"ogun" => out.push(check(
            "write",
            true,
            "a probe file was written, read back and removed",
        )),
        (Err(e), _) | (_, Err(e)) => out.push(check(
            "write",
            false,
            format!("the directory is not writable: {}", e.kind()),
        )),
        _ => out.push(check("write", false, "the probe file did not read back")),
    }
    out
}

pub(super) fn put(c: &ResolvedConnection, rel: &str, input: &InputArtifact) -> OpResult<Delivered> {
    let root = root(c)?;
    let top = root
        .canonicalize()
        .map_err(|e| io_err("opening the connection's directory", &e))?;
    let parts = clean_relative(rel)?;
    if parts.is_empty() {
        return Err(config("export: the target path is empty"));
    }
    let mut dest = top.clone();
    for p in &parts {
        dest.push(p);
    }
    let parent = dest.parent().unwrap_or(&top).to_path_buf();
    std::fs::create_dir_all(&parent).map_err(|e| io_err("creating the target folder", &e))?;
    // A link inside the directory must not lead the copy outside it.
    let real_parent = parent
        .canonicalize()
        .map_err(|e| io_err("checking the target folder", &e))?;
    if !real_parent.starts_with(&top) {
        return Err(OpError::new(
            "op.sandbox_violation",
            "the target leaves the connection's directory",
        ));
    }
    let tmp = real_parent.join(format!(".ogun-{}.partial", &input.blake3[..12]));
    std::fs::copy(&input.path, &tmp).map_err(|e| io_err("copying to the target", &e))?;
    std::fs::rename(&tmp, &dest).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        io_err("placing the file", &e)
    })?;
    Ok(Delivered {
        target: shown(&parts),
        rows: None,
        etag: None,
    })
}

fn shown(parts: &[String]) -> String {
    parts.join("/")
}

#[allow(dead_code)]
fn is_inside(root: &Path, p: &Path) -> bool {
    p.starts_with(root)
}
