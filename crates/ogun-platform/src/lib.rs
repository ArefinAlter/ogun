//! Operating-system calls that safe Rust cannot make (CLAUDE.md constraint 10: `unsafe` only in isolated platform
//! modules).
//!
//! * [`pipe`] (Windows): who is on the other end of the daemon's named pipe, and a pipe that only this user can open
//!   (O-15 section 4.4).
//! * [`sandbox`] (Windows): the operator sandbox built from an AppContainer, a job object and an explicit handle list
//!   (O-03 section 4, `docs/windows-sandbox.md`).
//!
//! Every function that wraps a call documents what it checks. Nothing here logs or returns a secret.

// Each platform module opts in to `unsafe` on its own; the rest of the crate stays denied.
#![deny(unsafe_code)]

#[cfg(windows)]
pub mod pipe;
#[cfg(windows)]
pub mod sandbox;
#[cfg(windows)]
mod wide;
