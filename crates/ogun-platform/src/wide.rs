//! UTF-16 helpers for the Win32 calls.

use std::ffi::{OsStr, OsString};
use std::os::windows::ffi::{OsStrExt, OsStringExt};

/// A NUL-terminated UTF-16 copy of a string.
pub(crate) fn nul_terminated(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(std::iter::once(0)).collect()
}

/// The text of a NUL-terminated UTF-16 buffer (the NUL excluded).
pub(crate) fn from_buffer(buf: &[u16]) -> OsString {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    OsString::from_wide(&buf[..end])
}
