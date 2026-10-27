//! The daemon's named pipe on Windows (O-15 section 4).
//!
//! * [`create_server`] makes a pipe instance whose access control list names only the user who runs the daemon, so
//!   no other account (and no network client) can open it. The default list of a named pipe lets "Everyone" read.
//! * [`client_process`] asks the system which process holds the client end (`GetNamedPipeClientProcessId`) and
//!   which executable it runs (`QueryFullProcessImageNameW`). The daemon compares that file with the binaries it
//!   pins next to itself; a client cannot choose the answer.
//!
//! Honest limit (O-15 section 4.4): a process of the same user that can start the pinned executable can still
//! start it. The checks narrow what an agent can do; they do not make a hostile same-user program harmless.

#![allow(unsafe_code)]

use std::io;
use std::os::windows::io::{AsRawHandle, RawHandle};
use std::path::PathBuf;

use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use windows_sys::Win32::Foundation::{CloseHandle, FALSE, HANDLE, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::System::Pipes::GetNamedPipeClientProcessId;
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
    QueryFullProcessImageNameW,
};

use crate::wide::{from_buffer, nul_terminated};

/// What the system says about the process on the other end of a pipe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipePeer {
    /// Process id.
    pub pid: u32,
    /// The executable it runs, when the system lets this process read it.
    pub exe: Option<PathBuf>,
}

/// Closes a handle when dropped.
struct Owned(HANDLE);

impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the handle was returned by a successful call and is closed once.
            unsafe { CloseHandle(self.0) };
        }
    }
}

/// The security identifier of the user running this process, as an SDDL string (`S-1-5-21-...`).
fn current_user_sid() -> io::Result<String> {
    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: `token` is a valid out pointer; the pseudo handle from GetCurrentProcess needs no closing.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) } == FALSE {
        return Err(io::Error::last_os_error());
    }
    let token = Owned(token);
    let mut needed = 0u32;
    // SAFETY: a null buffer with length 0 asks for the required size.
    unsafe { GetTokenInformation(token.0, TokenUser, std::ptr::null_mut(), 0, &raw mut needed) };
    if needed == 0 {
        return Err(io::Error::last_os_error());
    }
    // 8-byte aligned storage for a TOKEN_USER and the SID it points into.
    let mut buf = vec![0u64; (needed as usize).div_ceil(8)];
    // SAFETY: `buf` holds at least `needed` bytes and is aligned for TOKEN_USER.
    if unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            buf.as_mut_ptr().cast(),
            needed,
            &raw mut needed,
        )
    } == FALSE
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the call above filled `buf` with a TOKEN_USER whose SID lives inside `buf`.
    let sid = unsafe { (*buf.as_ptr().cast::<TOKEN_USER>()).User.Sid };
    let mut text: *mut u16 = std::ptr::null_mut();
    // SAFETY: `sid` is valid for the life of `buf`; `text` is an out pointer freed with LocalFree below.
    if unsafe { ConvertSidToStringSidW(sid, &raw mut text) } == FALSE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: ConvertSidToStringSidW returned a NUL-terminated wide string at `text`.
    let out = unsafe {
        let mut len = 0usize;
        while *text.add(len) != 0 {
            len += 1;
        }
        let s = String::from_utf16_lossy(std::slice::from_raw_parts(text, len));
        LocalFree(text.cast());
        s
    };
    Ok(out)
}

/// Creates one instance of the daemon's pipe.
///
/// The instance rejects remote clients and grants full access to the current user and to nobody else. With
/// `first` set it must be the first instance of the name, so a program that squats the name makes the daemon
/// fail to start rather than serve beside it (O-15 section 2, rule 4).
///
/// # Errors
///
/// The operating system's error, for example when the name is taken.
pub fn create_server(name: &str, first: bool) -> io::Result<NamedPipeServer> {
    // D:P = a protected DACL (no inherited entries); one allow entry, generic-all, for this user's SID.
    let sddl = nul_terminated(format!("D:P(A;;GA;;;{})", current_user_sid()?).as_ref());
    let mut descriptor = std::ptr::null_mut();
    // SAFETY: `sddl` is NUL-terminated; `descriptor` receives a LocalAlloc'd descriptor freed below.
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &raw mut descriptor,
            std::ptr::null_mut(),
        )
    } == FALSE
    {
        return Err(io::Error::last_os_error());
    }
    let mut attributes = SECURITY_ATTRIBUTES {
        nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>()).unwrap_or(u32::MAX),
        lpSecurityDescriptor: descriptor,
        bInheritHandle: FALSE,
    };
    let mut options = ServerOptions::new();
    options
        .first_pipe_instance(first)
        .reject_remote_clients(true);
    // SAFETY: `attributes` and the descriptor it points to outlive the call; the pipe copies the descriptor.
    let created =
        unsafe { options.create_with_security_attributes_raw(name, (&raw mut attributes).cast()) };
    // SAFETY: the descriptor came from the conversion call above and is no longer used.
    unsafe { LocalFree(descriptor) };
    created
}

/// Which process holds the client end of a connected pipe instance.
///
/// # Errors
///
/// The operating system's error; the daemon then treats the peer as unknown and not interactive.
pub fn client_process(pipe: &impl AsRawHandle) -> io::Result<PipePeer> {
    let handle: RawHandle = pipe.as_raw_handle();
    let mut pid = 0u32;
    // SAFETY: `handle` is a live pipe handle owned by `pipe`; `pid` is a valid out pointer.
    if unsafe { GetNamedPipeClientProcessId(handle.cast(), &raw mut pid) } == FALSE {
        return Err(io::Error::last_os_error());
    }
    Ok(PipePeer {
        pid,
        exe: image_path(pid),
    })
}

/// The executable of a process, if this user may query it.
#[must_use]
pub fn image_path(pid: u32) -> Option<PathBuf> {
    // SAFETY: plain call; a null result means the process is gone or protected.
    let process = Owned(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, pid) });
    if process.0.is_null() {
        return None;
    }
    let mut buf = vec![0u16; 32_768];
    let mut len = u32::try_from(buf.len()).unwrap_or(32_768);
    // SAFETY: `buf` has room for `len` UTF-16 units; flag 0 asks for the Win32 path.
    if unsafe { QueryFullProcessImageNameW(process.0, 0, buf.as_mut_ptr(), &raw mut len) } == FALSE
    {
        return None;
    }
    Some(PathBuf::from(from_buffer(&buf[..len as usize])))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_current_user_has_a_sid() {
        let sid = current_user_sid().unwrap();
        assert!(sid.starts_with("S-1-5-"), "{sid}");
    }

    #[test]
    fn this_process_has_an_image_path() {
        let me = image_path(std::process::id()).unwrap();
        assert_eq!(
            me.canonicalize().unwrap(),
            std::env::current_exe().unwrap().canonicalize().unwrap()
        );
    }

    #[tokio::test]
    async fn a_pipe_reports_its_client_and_refuses_a_second_first_instance() {
        use tokio::net::windows::named_pipe::ClientOptions;
        let name = format!(r"\\.\pipe\ogun-platform-test-{}", std::process::id());
        let server = create_server(&name, true).unwrap();
        assert!(create_server(&name, true).is_err(), "a squatter must fail");
        let client = ClientOptions::new().open(&name).unwrap();
        server.connect().await.unwrap();
        let peer = client_process(&server).unwrap();
        assert_eq!(peer.pid, std::process::id());
        assert!(peer.exe.is_some());
        drop(client);
    }
}
