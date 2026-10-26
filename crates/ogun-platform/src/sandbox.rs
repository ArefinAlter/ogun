//! The Windows operator sandbox (O-03 section 4; `docs/windows-sandbox.md`).
//!
//! `ogun-sandbox <plan.json> -- <program> <args...>` starts one program inside:
//!
//! * **An AppContainer.** The process gets a low-box token for the profile `dev.verne.ogun.operator`. It can open
//!   only what its access list names that profile for (or what Windows lets every application package read:
//!   most of the Windows and Program Files folders). It cannot read the person's documents, profile or the
//!   engine's state, and without a capability it cannot use the network, loopback included. A manifest that
//!   grants `network: any` gets the `internetClient` capability: outbound internet, no host filtering.
//! * **A job object.** Killed when the helper dies (the daemon killing the helper kills the whole tree), a
//!   process-count limit, a per-process memory limit and CPU-time limit from the manifest, and UI restrictions
//!   (no clipboard, no desktop, no global atoms, no handles to windows outside the job).
//! * **An explicit handle list.** Only the helper's stdin, stdout and stderr are inherited; every other handle of
//!   the daemon stays out of the child.
//! * **A working-directory jail by access list.** The scratch directory is read-write for the profile; inputs, the
//!   code file and the program's own folder are read-only. The grants are access-control entries for the profile's
//!   SID only; they are added once and left in place (they name a profile no other program runs as).
//!
//! What it does **not** isolate is stated by `ogun_ops::sandbox::Backend::status` and the O-12 matrix: a flaw in
//! Windows, files that Windows lets every application read, a disk with no quota, and hosts when the network is
//! granted.

#![allow(unsafe_code)]
#![allow(clippy::too_many_lines)]

use std::ffi::{OsStr, OsString};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::process::ExitCode;

use serde::Deserialize;
use windows_sys::Win32::Foundation::{
    CloseHandle, FALSE, GetLastError, HANDLE, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, LocalFree,
    SetHandleInformation, WAIT_OBJECT_0,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSidToSidW, EXPLICIT_ACCESS_W, GRANT_ACCESS, GetNamedSecurityInfoW,
    NO_MULTIPLE_TRUSTEE, SE_FILE_OBJECT, SetEntriesInAclW, SetNamedSecurityInfoW, TRUSTEE_IS_SID,
    TRUSTEE_IS_UNKNOWN, TRUSTEE_W,
};
use windows_sys::Win32::Security::Isolation::{
    CreateAppContainerProfile, DeriveAppContainerSidFromAppContainerName,
};
use windows_sys::Win32::Security::{
    ACE_HEADER, ACL, ACL_SIZE_INFORMATION, AclSizeInformation, DACL_SECURITY_INFORMATION, EqualSid,
    GetAce, GetAclInformation, InitializeSecurityDescriptor, PSID, SECURITY_CAPABILITIES,
    SECURITY_DESCRIPTOR, SID_AND_ATTRIBUTES, SetFileSecurityW, SetSecurityDescriptorDacl,
};
use windows_sys::Win32::System::Console::{
    GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
    JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOB_OBJECT_LIMIT_PROCESS_MEMORY, JOB_OBJECT_LIMIT_PROCESS_TIME, JOB_OBJECT_UILIMIT_DESKTOP,
    JOB_OBJECT_UILIMIT_DISPLAYSETTINGS, JOB_OBJECT_UILIMIT_EXITWINDOWS,
    JOB_OBJECT_UILIMIT_GLOBALATOMS, JOB_OBJECT_UILIMIT_HANDLES, JOB_OBJECT_UILIMIT_READCLIPBOARD,
    JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS, JOB_OBJECT_UILIMIT_WRITECLIPBOARD,
    JOBOBJECT_BASIC_UI_RESTRICTIONS, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectBasicUIRestrictions, JobObjectExtendedLimitInformation, SetInformationJobObject,
};
use windows_sys::Win32::System::Threading::{
    CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW,
    DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess, INFINITE,
    InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION,
    ResumeThread, STARTF_USESTDHANDLES, STARTUPINFOEXW, TerminateProcess,
    UpdateProcThreadAttribute, WaitForSingleObject,
};

use crate::wide::nul_terminated;

/// The AppContainer profile all operators share.
const PROFILE: &str = "dev.verne.ogun.operator";
const SE_GROUP_ENABLED: u32 = 0x0000_0004;
const PROC_THREAD_ATTRIBUTE_HANDLE_LIST: usize = 0x0002_0002;
const PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES: usize = 0x0002_0009;
const FILE_GENERIC_READ: u32 = 0x0012_0089;
const FILE_GENERIC_READ_EXECUTE: u32 = 0x0012_00A9;
const FILE_MODIFY: u32 = 0x0013_01BF;
const SUB_CONTAINERS_AND_OBJECTS_INHERIT: u32 = 3;
const NO_INHERITANCE: u32 = 0;
const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;
const OBJECT_INHERIT_ACE: u8 = 0x01;
const CONTAINER_INHERIT_ACE: u8 = 0x02;

/// What the daemon asks of the helper.
#[derive(Debug, Deserialize)]
struct Plan {
    scratch: String,
    #[serde(default)]
    read_only: Vec<String>,
    #[serde(default)]
    network: bool,
    memory_bytes: Option<u64>,
    cpu_ms: Option<u64>,
    #[serde(default)]
    env: Vec<(String, String)>,
    /// Where a private copy of a program's folder may be made when the folder cannot be opened to the profile.
    #[serde(default)]
    cache_dir: Option<String>,
}

fn err(what: &str, code: u32) -> io::Error {
    let kind = if code == 5 {
        io::ErrorKind::PermissionDenied
    } else {
        io::ErrorKind::Other
    };
    io::Error::new(kind, format!("{what} failed (Windows error {code})"))
}

fn last(what: &str) -> io::Error {
    // SAFETY: GetLastError has no preconditions.
    err(what, unsafe { GetLastError() })
}

struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
            // SAFETY: closing a handle this value owns, once.
            unsafe { CloseHandle(self.0) };
        }
    }
}

/// A SID allocated by Windows.
struct Sid {
    ptr: PSID,
    local: bool,
}

impl Drop for Sid {
    fn drop(&mut self) {
        if self.local && !self.ptr.is_null() {
            // SAFETY: allocated by ConvertStringSidToSidW (LocalAlloc). The profile SID of CreateAppContainerProfile
            // is freed with FreeSid; leaking one SID in a helper that exits at once is the safe failure.
            unsafe { LocalFree(self.ptr) };
        }
    }
}

/// The profile's SID, creating the profile the first time. Two helpers starting together may both try to create
/// it; the loser retries and finds it.
fn profile_sid() -> io::Result<Sid> {
    let name = nul_terminated(OsStr::new(PROFILE));
    let display = nul_terminated(OsStr::new("Ogun operator"));
    let desc = nul_terminated(OsStr::new("Isolated environment for Ogun operators"));
    let mut last_error = io::Error::other("no attempt was made");
    for attempt in 0..8 {
        let mut sid: PSID = std::ptr::null_mut();
        // SAFETY: valid NUL-terminated strings; no capabilities; `sid` is an out pointer.
        let hr = unsafe {
            CreateAppContainerProfile(
                name.as_ptr(),
                display.as_ptr(),
                desc.as_ptr(),
                std::ptr::null(),
                0,
                &raw mut sid,
            )
        };
        if hr >= 0 {
            return Ok(Sid {
                ptr: sid,
                local: false,
            });
        }
        let mut derived: PSID = std::ptr::null_mut();
        // SAFETY: as above.
        let d =
            unsafe { DeriveAppContainerSidFromAppContainerName(name.as_ptr(), &raw mut derived) };
        // HRESULT_FROM_WIN32(ERROR_ALREADY_EXISTS) means the profile is there: its SID is the derived one.
        if d >= 0 && (hr as u32 == 0x8007_00B7 || attempt > 0) {
            return Ok(Sid {
                ptr: derived,
                local: false,
            });
        }
        last_error = err("CreateAppContainerProfile", hr as u32);
        std::thread::sleep(std::time::Duration::from_millis(100 * (attempt + 1)));
    }
    Err(last_error)
}

fn sid_from_string(text: &str) -> io::Result<Sid> {
    let w = nul_terminated(OsStr::new(text));
    let mut sid: PSID = std::ptr::null_mut();
    // SAFETY: NUL-terminated string; the out pointer is freed with LocalFree by Sid::drop.
    if unsafe { ConvertStringSidToSidW(w.as_ptr(), &raw mut sid) } == FALSE {
        return Err(last("ConvertStringSidToSidW"));
    }
    Ok(Sid {
        ptr: sid,
        local: true,
    })
}

/// Whether the DACL already holds an allow entry for `sid` with at least `mask` (and, for a folder, both
/// inheritance flags).
fn has_grant(acl: *const ACL, sid: PSID, mask: u32, folder: bool) -> bool {
    if acl.is_null() {
        return false;
    }
    let mut info = ACL_SIZE_INFORMATION {
        AceCount: 0,
        AclBytesInUse: 0,
        AclBytesFree: 0,
    };
    // SAFETY: `acl` is a valid ACL from GetNamedSecurityInfoW; `info` is sized for AclSizeInformation.
    if unsafe {
        GetAclInformation(
            acl,
            (&raw mut info).cast(),
            size_of::<ACL_SIZE_INFORMATION>() as u32,
            AclSizeInformation,
        )
    } == FALSE
    {
        return false;
    }
    for i in 0..info.AceCount {
        let mut ace: *mut core::ffi::c_void = std::ptr::null_mut();
        // SAFETY: `i` is below the ACE count.
        if unsafe { GetAce(acl, i, &raw mut ace) } == FALSE || ace.is_null() {
            continue;
        }
        // SAFETY: an ACE begins with an ACE_HEADER.
        let header = unsafe { &*ace.cast::<ACE_HEADER>() };
        if header.AceType != ACCESS_ALLOWED_ACE_TYPE {
            continue;
        }
        // ACCESS_ALLOWED_ACE: header (4 bytes), mask (4 bytes), then the SID.
        // SAFETY: an allow entry holds a mask and a SID after its header.
        let (ace_mask, ace_sid) = unsafe {
            (
                ace.cast::<u8>().add(4).cast::<u32>().read_unaligned(),
                ace.cast::<u8>().add(8).cast::<core::ffi::c_void>(),
            )
        };
        let inherits = !folder
            || (header.AceFlags & (OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE))
                == (OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE);
        // SAFETY: both SIDs are valid.
        if ace_mask & mask == mask && inherits && unsafe { EqualSid(ace_sid, sid) } != FALSE {
            return true;
        }
    }
    false
}

/// The Windows and Program Files folders carry an entry for "all application packages": nothing to add there.
fn readable_by_every_package(path: &Path) -> bool {
    ["SystemRoot", "ProgramFiles", "ProgramFiles(x86)"]
        .iter()
        .filter_map(|v| std::env::var_os(v))
        .any(|root| {
            let (p, r) = (
                path.to_string_lossy().to_lowercase(),
                Path::new(&root).to_string_lossy().to_lowercase(),
            );
            p == r || p.starts_with(&format!("{}\\", r.trim_end_matches('\\')))
        })
}

/// Adds an allow entry for the profile on `path` (a file, or a folder and everything under it), unless one is
/// there already.
fn grant(path: &Path, sid: PSID, mask: u32, inherit: bool) -> io::Result<()> {
    let folder = path.is_dir() && inherit;
    let wide = nul_terminated(path.as_os_str());
    let mut old: *mut ACL = std::ptr::null_mut();
    let mut descriptor: *mut core::ffi::c_void = std::ptr::null_mut();
    // SAFETY: valid path; out pointers; the descriptor is freed below.
    let rc = unsafe {
        GetNamedSecurityInfoW(
            wide.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &raw mut old,
            std::ptr::null_mut(),
            &raw mut descriptor,
        )
    };
    if rc != 0 {
        return Err(err("GetNamedSecurityInfoW", rc));
    }
    let result = (|| {
        if has_grant(old, sid, mask, folder) {
            return Ok(());
        }
        let access = EXPLICIT_ACCESS_W {
            grfAccessPermissions: mask,
            grfAccessMode: GRANT_ACCESS,
            grfInheritance: if folder {
                SUB_CONTAINERS_AND_OBJECTS_INHERIT
            } else {
                NO_INHERITANCE
            },
            Trustee: TRUSTEE_W {
                pMultipleTrustee: std::ptr::null_mut(),
                MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
                TrusteeForm: TRUSTEE_IS_SID,
                TrusteeType: TRUSTEE_IS_UNKNOWN,
                ptstrName: sid.cast(),
            },
        };
        let mut new_acl: *mut ACL = std::ptr::null_mut();
        // SAFETY: one valid entry, merged into the file's current list; the result is freed below.
        let rc = unsafe { SetEntriesInAclW(1, &access, old, &raw mut new_acl) };
        if rc != 0 {
            return Err(err("SetEntriesInAclW", rc));
        }
        let rc = if folder {
            // An inheritable entry goes to everything under the folder: the system walks the tree, once (the
            // `has_grant` check above skips every later run).
            // SAFETY: valid path and list.
            unsafe {
                SetNamedSecurityInfoW(
                    wide.as_ptr().cast_mut(),
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    new_acl,
                    std::ptr::null_mut(),
                )
            }
        } else {
            // A single object's entry must not make the system walk the folder's children (the scratch folder's
            // parent holds one folder per attempt): the legacy call changes this object only.
            // SAFETY: a zeroed descriptor is initialised by the call below; the list outlives the call.
            let mut sd: SECURITY_DESCRIPTOR = unsafe { std::mem::zeroed() };
            let sd_ptr = (&raw mut sd).cast::<core::ffi::c_void>();
            // SAFETY: `sd_ptr` points at storage for a security descriptor.
            let ok = unsafe { InitializeSecurityDescriptor(sd_ptr, 1) } != FALSE
                && unsafe { SetSecurityDescriptorDacl(sd_ptr, 1, new_acl, FALSE) } != FALSE
                && unsafe { SetFileSecurityW(wide.as_ptr(), DACL_SECURITY_INFORMATION, sd_ptr) }
                    != FALSE;
            if ok { 0 } else { unsafe { GetLastError() } }
        };
        // SAFETY: allocated by SetEntriesInAclW.
        unsafe { LocalFree(new_acl.cast()) };
        if rc != 0 {
            return Err(err("setting the access list", rc));
        }
        Ok(())
    })();
    // SAFETY: allocated by GetNamedSecurityInfoW.
    unsafe { LocalFree(descriptor) };
    result
}

/// Copies the folder of `program` to `<cache_dir>/<key>` once and opens the copy to the profile; returns where the
/// program is in the copy. At most 1.5 GiB: a bigger tree (an interpreter with a data-science environment inside)
/// needs to be installed for the current user instead.
fn stage(plan: &Plan, folder: &Path, program: &Path, sid: PSID) -> io::Result<std::path::PathBuf> {
    const LIMIT: u64 = 3 << 29;
    let Some(cache) = plan.cache_dir.as_deref() else {
        return Err(io::Error::other(format!(
            "{} is not readable by the sandbox and the engine gave no place to prepare a copy",
            folder.display()
        )));
    };
    fn size(dir: &Path, total: &mut u64) -> io::Result<()> {
        for e in std::fs::read_dir(dir)? {
            let e = e?;
            let m = e.metadata()?;
            if m.is_dir() {
                size(&e.path(), total)?;
            } else {
                *total += m.len();
            }
        }
        Ok(())
    }
    fn copy(from: &Path, to: &Path) -> io::Result<()> {
        std::fs::create_dir_all(to)?;
        for e in std::fs::read_dir(from)? {
            let e = e?;
            let target = to.join(e.file_name());
            if e.metadata()?.is_dir() {
                copy(&e.path(), &target)?;
            } else {
                std::fs::copy(e.path(), &target)?;
            }
        }
        Ok(())
    }
    // FNV-1a over the folder's path, the program's size and its modified time: a changed install is a new copy.
    let meta = std::fs::metadata(program)?;
    let stamp = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs());
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in format!(
        "{}|{}|{stamp}",
        folder.display().to_string().to_lowercase(),
        meta.len()
    )
    .bytes()
    {
        h = (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
    }
    let root = Path::new(cache).join(format!("{h:016x}"));
    let done = root.join(".complete");
    if !done.exists() {
        let mut total = 0u64;
        size(folder, &mut total)?;
        if total > LIMIT {
            return Err(io::Error::other(format!(
                "{} is {} MiB, too big to copy for the sandbox; install the program for the current user or under Program Files",
                folder.display(),
                total >> 20
            )));
        }
        let _ = std::fs::remove_dir_all(&root);
        copy(folder, &root.join("program"))?;
        std::fs::write(&done, b"ok")?;
    }
    let copied = root.join("program");
    grant(&copied, sid, FILE_GENERIC_READ_EXECUTE, true)?;
    let rel = program
        .strip_prefix(folder)
        .map_err(|_| io::Error::other("the program is not in its folder"))?;
    Ok(copied.join(rel))
}

/// A Windows command line from an argument vector (the rules `CommandLineToArgvW` reverses).
fn command_line(program: &OsStr, args: &[OsString]) -> Vec<u16> {
    fn quote(arg: &OsStr, out: &mut Vec<u16>) {
        let wide: Vec<u16> = arg.encode_wide().collect();
        let plain = !wide.is_empty()
            && !wide.iter().any(|&c| {
                c == u16::from(b' ')
                    || c == u16::from(b'\t')
                    || c == u16::from(b'"')
                    || c == u16::from(b'\n')
            });
        if plain {
            out.extend(wide);
            return;
        }
        out.push(u16::from(b'"'));
        let mut slashes = 0usize;
        for c in wide {
            if c == u16::from(b'\\') {
                slashes += 1;
                continue;
            }
            if c == u16::from(b'"') {
                out.extend(std::iter::repeat_n(u16::from(b'\\'), slashes * 2 + 1));
            } else {
                out.extend(std::iter::repeat_n(u16::from(b'\\'), slashes));
            }
            slashes = 0;
            out.push(c);
        }
        out.extend(std::iter::repeat_n(u16::from(b'\\'), slashes * 2));
        out.push(u16::from(b'"'));
    }
    let mut out = Vec::new();
    quote(program, &mut out);
    for a in args {
        out.push(u16::from(b' '));
        quote(a, &mut out);
    }
    out.push(0);
    out
}

fn environment(env: &[(String, String)]) -> Vec<u16> {
    let mut sorted: Vec<&(String, String)> = env.iter().collect();
    sorted.sort_by_key(|a| a.0.to_uppercase());
    let mut out: Vec<u16> = Vec::new();
    for (k, v) in sorted {
        out.extend(OsStr::new(k).encode_wide());
        out.push(u16::from(b'='));
        out.extend(OsStr::new(v).encode_wide());
        out.push(0);
    }
    out.push(0);
    if out.len() == 1 {
        out.push(0);
    }
    out
}

fn job(plan: &Plan) -> io::Result<Handle> {
    // SAFETY: an unnamed job object.
    let h = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    if h.is_null() {
        return Err(last("CreateJobObjectW"));
    }
    let job = Handle(h);
    // SAFETY: zero is a valid initial value for these plain structs.
    let mut ext: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    let mut flags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
        | JOB_OBJECT_LIMIT_ACTIVE_PROCESS
        | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION;
    ext.BasicLimitInformation.ActiveProcessLimit = 16;
    if let Some(bytes) = plan.memory_bytes {
        flags |= JOB_OBJECT_LIMIT_PROCESS_MEMORY;
        ext.ProcessMemoryLimit = usize::try_from(bytes).unwrap_or(usize::MAX);
    }
    if let Some(ms) = plan.cpu_ms {
        flags |= JOB_OBJECT_LIMIT_PROCESS_TIME;
        // 100-nanosecond units.
        ext.BasicLimitInformation.PerProcessUserTimeLimit =
            i64::try_from(ms.saturating_mul(10_000)).unwrap_or(i64::MAX);
    }
    ext.BasicLimitInformation.LimitFlags = flags;
    // SAFETY: `ext` is the structure the information class names, with its size.
    if unsafe {
        SetInformationJobObject(
            job.0,
            JobObjectExtendedLimitInformation,
            (&raw const ext).cast(),
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    } == FALSE
    {
        return Err(last("SetInformationJobObject (limits)"));
    }
    let ui = JOBOBJECT_BASIC_UI_RESTRICTIONS {
        UIRestrictionsClass: JOB_OBJECT_UILIMIT_DESKTOP
            | JOB_OBJECT_UILIMIT_DISPLAYSETTINGS
            | JOB_OBJECT_UILIMIT_EXITWINDOWS
            | JOB_OBJECT_UILIMIT_GLOBALATOMS
            | JOB_OBJECT_UILIMIT_HANDLES
            | JOB_OBJECT_UILIMIT_READCLIPBOARD
            | JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS
            | JOB_OBJECT_UILIMIT_WRITECLIPBOARD,
    };
    // SAFETY: as above.
    if unsafe {
        SetInformationJobObject(
            job.0,
            JobObjectBasicUIRestrictions,
            (&raw const ui).cast(),
            size_of::<JOBOBJECT_BASIC_UI_RESTRICTIONS>() as u32,
        )
    } == FALSE
    {
        return Err(last("SetInformationJobObject (UI)"));
    }
    Ok(job)
}

fn std_handle(which: u32) -> io::Result<HANDLE> {
    // SAFETY: a standard handle id.
    let h = unsafe { GetStdHandle(which) };
    if h.is_null() || h == INVALID_HANDLE_VALUE {
        return Err(io::Error::other("a standard handle is missing"));
    }
    // SAFETY: marks the handle inheritable so the child can use it.
    if unsafe { SetHandleInformation(h, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) } == FALSE {
        return Err(last("SetHandleInformation"));
    }
    Ok(h)
}

/// Starts `program` in the sandbox and waits; returns its exit code.
fn run(plan: &Plan, program: &OsStr, args: &[OsString]) -> io::Result<u32> {
    let program = program.to_owned();
    let program = program.as_os_str();
    let sid = profile_sid()?;
    // Access for the profile: the scratch directory read-write (fatal if it fails), everything the plan lists
    // read-only. A read-only grant can fail on a folder this user does not own (Windows, Program Files): those
    // are readable by every application package already, so the helper carries on and says so on stderr.
    let warn = |path: &Path, e: &io::Error| {
        use std::io::Write as _;
        let _ = writeln!(
            io::stderr(),
            "ogun-sandbox: warning: no read access could be granted on {} ({e})",
            path.display()
        );
    };
    grant(Path::new(&plan.scratch), sid.ptr, FILE_MODIFY, true)?;
    // Windows lets an AppContainer open an object only if it can also list the folder that holds it, so the
    // scratch folder's parent is listable (non-inherited): the names of sibling scratch folders are visible,
    // never their content.
    if let Some(parent) = Path::new(&plan.scratch).parent()
        && !readable_by_every_package(parent)
        && let Err(e) = grant(parent, sid.ptr, FILE_GENERIC_READ, false)
    {
        warn(Path::new(&plan.scratch), &e);
    }
    for p in &plan.read_only {
        let path = Path::new(p);
        if !path.exists() || readable_by_every_package(path) {
            continue;
        }
        let granted = if path.is_dir() {
            grant(path, sid.ptr, FILE_GENERIC_READ_EXECUTE, true)
        } else {
            // A file needs its own entry, and Windows also wants the container to be able to list the folder that
            // holds it (non-inherited: the folder's other files stay closed). Their names are visible; their
            // content is not.
            grant(path, sid.ptr, FILE_GENERIC_READ_EXECUTE, false).and_then(|()| {
                match path.parent() {
                    Some(parent) if !readable_by_every_package(parent) => {
                        grant(parent, sid.ptr, FILE_GENERIC_READ, false)
                    }
                    _ => Ok(()),
                }
            })
        };
        if let Err(e) = granted {
            warn(path, &e);
        }
    }
    let mut program = std::path::PathBuf::from(program);
    if let Some(parent) = program.parent().map(Path::to_path_buf)
        && parent.is_dir()
        && !readable_by_every_package(&parent)
    {
        match grant(&parent, sid.ptr, FILE_GENERIC_READ_EXECUTE, true) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
                // A folder installed for all users (not owned by this person) cannot be opened to the profile
                // here. A private copy under the engine's own state can.
                program = stage(plan, &parent, &program, sid.ptr)?;
            }
            Err(e) => warn(&parent, &e),
        }
    }
    let job = job(plan)?;
    let internet = if plan.network {
        Some(sid_from_string("S-1-15-3-1")?)
    } else {
        None
    };
    let mut caps: Vec<SID_AND_ATTRIBUTES> = Vec::new();
    if let Some(c) = &internet {
        caps.push(SID_AND_ATTRIBUTES {
            Sid: c.ptr,
            Attributes: SE_GROUP_ENABLED,
        });
    }
    let mut security = SECURITY_CAPABILITIES {
        AppContainerSid: sid.ptr,
        Capabilities: if caps.is_empty() {
            std::ptr::null_mut()
        } else {
            caps.as_mut_ptr()
        },
        CapabilityCount: u32::try_from(caps.len()).unwrap_or(0),
        Reserved: 0,
    };
    let mut handles = [
        std_handle(STD_INPUT_HANDLE)?,
        std_handle(STD_OUTPUT_HANDLE)?,
        std_handle(STD_ERROR_HANDLE)?,
    ];

    // The attribute list: the profile, and exactly three inheritable handles.
    let mut size = 0usize;
    // SAFETY: a null list asks for the required size.
    unsafe { InitializeProcThreadAttributeList(std::ptr::null_mut(), 2, 0, &raw mut size) };
    let mut storage = vec![0u8; size];
    let list = storage.as_mut_ptr().cast::<core::ffi::c_void>() as LPPROC_THREAD_ATTRIBUTE_LIST;
    // SAFETY: `storage` holds `size` bytes.
    if unsafe { InitializeProcThreadAttributeList(list, 2, 0, &raw mut size) } == FALSE {
        return Err(last("InitializeProcThreadAttributeList"));
    }
    struct ListGuard(LPPROC_THREAD_ATTRIBUTE_LIST);
    impl Drop for ListGuard {
        fn drop(&mut self) {
            // SAFETY: initialised above.
            unsafe { DeleteProcThreadAttributeList(self.0) };
        }
    }
    let _guard = ListGuard(list);
    // SAFETY: `security` and `handles` outlive the CreateProcessW call below.
    unsafe {
        if UpdateProcThreadAttribute(
            list,
            0,
            PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES,
            (&raw mut security).cast(),
            size_of::<SECURITY_CAPABILITIES>(),
            std::ptr::null_mut(),
            std::ptr::null(),
        ) == FALSE
        {
            return Err(last("UpdateProcThreadAttribute (container)"));
        }
        if UpdateProcThreadAttribute(
            list,
            0,
            PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
            handles.as_mut_ptr().cast(),
            size_of::<[HANDLE; 3]>(),
            std::ptr::null_mut(),
            std::ptr::null(),
        ) == FALSE
        {
            return Err(last("UpdateProcThreadAttribute (handles)"));
        }
    }
    // SAFETY: zeroed plain structs, then the fields the call reads.
    let mut si: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
    si.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
    si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    si.StartupInfo.hStdInput = handles[0];
    si.StartupInfo.hStdOutput = handles[1];
    si.StartupInfo.hStdError = handles[2];
    si.lpAttributeList = list;
    // SAFETY: as above.
    let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    let mut cmd = command_line(program.as_os_str(), args);
    let mut full = plan.env.clone();
    // Windows refuses to start an AppContainer process whose environment has no LOCALAPPDATA (error 203). The
    // child gets a folder of its own inside the scratch directory, never the person's real one.
    if !full
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case("LOCALAPPDATA"))
    {
        let local = Path::new(&plan.scratch).join(".local");
        let _ = std::fs::create_dir_all(&local);
        full.push(("LOCALAPPDATA".into(), local.display().to_string()));
    }
    let env = environment(&full);
    let cwd = nul_terminated(OsStr::new(&plan.scratch));
    // SAFETY: every pointer is valid for the call; the child starts suspended so it joins the job before it runs.
    let ok = unsafe {
        CreateProcessW(
            std::ptr::null(),
            cmd.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
            EXTENDED_STARTUPINFO_PRESENT
                | CREATE_SUSPENDED
                | CREATE_UNICODE_ENVIRONMENT
                | CREATE_NO_WINDOW,
            env.as_ptr().cast(),
            cwd.as_ptr(),
            (&raw const si.StartupInfo).cast(),
            &raw mut pi,
        )
    };
    if ok == FALSE {
        return Err(last("CreateProcessW"));
    }
    let process = Handle(pi.hProcess);
    let thread = Handle(pi.hThread);
    // SAFETY: both handles are valid.
    if unsafe { AssignProcessToJobObject(job.0, process.0) } == FALSE {
        let e = last("AssignProcessToJobObject");
        // SAFETY: the child never ran.
        unsafe { TerminateProcess(process.0, 1) };
        return Err(e);
    }
    // SAFETY: resumes the suspended primary thread.
    unsafe { ResumeThread(thread.0) };
    // SAFETY: waits for the process handle this function owns.
    let waited = unsafe { WaitForSingleObject(process.0, INFINITE) };
    if waited != WAIT_OBJECT_0 {
        return Err(last("WaitForSingleObject"));
    }
    let mut code = 0u32;
    // SAFETY: the process has exited.
    unsafe { GetExitCodeProcess(process.0, &raw mut code) };
    // Anything the program left running dies with the job when `job` drops.
    Ok(code)
}

fn read_plan(path: &Path) -> io::Result<Plan> {
    let bytes = std::fs::read(path)?;
    // The plan lists paths, never secrets, and the helper owns the file: remove it at once.
    let _ = std::fs::remove_file(path);
    serde_json::from_slice(&bytes)
        .map_err(|e| io::Error::other(format!("the plan does not parse: {e}")))
}

/// The helper's entry point: `ogun-sandbox <plan.json> -- <program> <args...>` or `ogun-sandbox --self-test`.
#[must_use]
pub fn main_entry() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let fail = |msg: &str| {
        use std::io::Write as _;
        let _ = writeln!(io::stderr(), "ogun-sandbox: {msg}");
        ExitCode::from(125)
    };
    if args.first().is_some_and(|a| a == "--self-test") {
        return match self_test() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => fail(&format!("self test failed: {e}")),
        };
    }
    let Some(dash) = args.iter().position(|a| a == "--") else {
        return fail("usage: ogun-sandbox <plan.json> -- <program> [args...]");
    };
    let (Some(plan_path), Some(program)) = (args.first().filter(|_| dash == 1), args.get(dash + 1))
    else {
        return fail("usage: ogun-sandbox <plan.json> -- <program> [args...]");
    };
    let plan = match read_plan(Path::new(plan_path)) {
        Ok(p) => p,
        Err(e) => return fail(&e.to_string()),
    };
    match run(&plan, program, &args[dash + 2..]) {
        Ok(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
        Err(e) => fail(&e.to_string()),
    }
}

/// Starts `cmd /c exit 7` in the sandbox: proves the profile, the job and the attribute list work on this machine.
///
/// # Errors
///
/// Why the sandbox cannot start a process here.
pub fn self_test() -> io::Result<()> {
    let dir = std::env::temp_dir().join(format!("ogun-sandbox-selftest-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let system = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    let plan = Plan {
        scratch: dir.display().to_string(),
        read_only: vec![],
        network: false,
        memory_bytes: Some(256 << 20),
        cpu_ms: Some(10_000),
        cache_dir: None,
        env: vec![
            ("SystemRoot".into(), system.clone()),
            ("windir".into(), system.clone()),
            ("PATH".into(), format!(r"{system}\System32;{system}")),
            ("TEMP".into(), dir.display().to_string()),
            ("TMP".into(), dir.display().to_string()),
        ],
    };
    let cmd = Path::new(&system).join("System32").join("cmd.exe");
    let result = run(
        &plan,
        cmd.as_os_str(),
        &[OsString::from("/c"), OsString::from("exit 7")],
    );
    let _ = std::fs::remove_dir_all(&dir);
    match result? {
        7 => Ok(()),
        other => Err(io::Error::other(format!(
            "the sandboxed probe exited with {other}, not 7"
        ))),
    }
}
