//! Owner-only access on Windows: the counterpart of the 0700 and 0600 modes
//! that keep the data directory and `runtime.json` private on Unix.
//!
//! # What "private" means here
//!
//! An object is private when its owner is a trusted principal and its DACL
//! grants nothing to anyone else. The trusted principals are:
//!
//! - the current user (the SID in this process's token);
//! - `SYSTEM` (`S-1-5-18`) and `BUILTIN\Administrators` (`S-1-5-32-544`):
//!   they can take any file anyway, as root can on Unix, and Windows itself
//!   (backup, the indexer, an elevated `whoosh`) runs as them. A file an
//!   elevated process creates is owned by the Administrators group;
//! - `CREATOR OWNER` (`S-1-3-0`) and `OWNER RIGHTS` (`S-1-3-4`), which stand
//!   for the owner.
//!
//! This matches what the Windows port of OpenSSH demands of private keys.
//!
//! - A directory is refused when anyone else may change it: add or delete
//!   entries, write its attributes, change its ACL or owner. Reading is
//!   allowed, as `0755` is on Unix, but the daemon tightens a directory of
//!   its own anyway ([`is_loose`]).
//! - A file is refused when anyone else may do anything with it.
//! - Inherit-only entries count: in a directory they decide who may open
//!   the files created in it later.
//! - A missing DACL (a "NULL DACL", or a filesystem without ACLs such as
//!   FAT) grants everyone everything and is refused.
//!
//! # Creating private objects
//!
//! [`create_private_dir`] and [`create_private_file`] pass a security
//! descriptor to `CreateDirectoryW`/`CreateFileW`, so the object never
//! exists with a wider ACL, just as the Unix mode is part of `mkdir`/`open`.
//! The descriptor holds a *protected* DACL (nothing inherited from the
//! parent) granting full control to the current user, `SYSTEM` and
//! `Administrators`; a directory's entries are inherited by what is created
//! inside it, so the daemon's own files there are private too.
//!
//! This is the only module in the crate that uses `unsafe` (Win32 calls);
//! every block has a `// SAFETY:` comment.
#![allow(unsafe_code)]

use std::ffi::c_void;
use std::fs::File;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{
    ERROR_SUCCESS, GENERIC_WRITE, INVALID_HANDLE_VALUE, LocalFree,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo,
    SDDL_REVISION_1, SE_FILE_OBJECT, SetNamedSecurityInfoW,
};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, ACL_SIZE_INFORMATION, AclSizeInformation,
    DACL_SECURITY_INFORMATION, GetAce, GetAclInformation, GetSecurityDescriptorDacl,
    GetTokenInformation, OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSID,
    SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{
    CREATE_NEW, CreateDirectoryW, CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    READ_CONTROL, SYNCHRONIZE,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

const SYSTEM: &str = "S-1-5-18";
const ADMINISTRATORS: &str = "S-1-5-32-544";
const CREATOR_OWNER: &str = "S-1-3-0";
const OWNER_RIGHTS: &str = "S-1-3-4";

/// `ACCESS_ALLOWED_ACE_TYPE` and `ACCESS_DENIED_ACE_TYPE`.
const ALLOWED: u8 = 0;
const DENIED: u8 = 1;

/// Rights that change a directory or what is in it: `FILE_ADD_FILE`,
/// `FILE_ADD_SUBDIRECTORY`, `FILE_WRITE_EA`, `FILE_DELETE_CHILD`,
/// `FILE_WRITE_ATTRIBUTES`, `DELETE`, `WRITE_DAC`, `WRITE_OWNER`, and the
/// generic `GENERIC_ALL` and `GENERIC_WRITE` that inherit-only entries use.
const DIR_WRITE: u32 =
    0x2 | 0x4 | 0x10 | 0x40 | 0x100 | 0x1_0000 | 0x4_0000 | 0x8_0000 | 0x1000_0000 | 0x4000_0000;

/// What [`check_private`] is looking at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    File,
    Dir,
}

/// Why [`check_private`] refused an object.
#[derive(Debug, thiserror::Error)]
pub enum CheckError {
    /// It could not be inspected (missing, unreadable, …).
    #[error(transparent)]
    Io(#[from] io::Error),
    /// Someone else could read or change it; the reason says how.
    #[error("{0}")]
    Insecure(String),
}

/// Who owns an object and what its DACL grants to principals that are not
/// trusted (see the module docs).
#[derive(Debug, Clone)]
pub struct Access {
    /// The owner, as a string SID.
    pub owner: String,
    /// The owner is the current user, `SYSTEM` or `Administrators`.
    pub owner_trusted: bool,
    /// There is no DACL at all: everyone may do anything.
    pub no_dacl: bool,
    /// Entries that grant something to an untrusted principal: its string
    /// SID and the access mask. An entry of a type that cannot be checked
    /// (a conditional or object entry) is listed as `ACE type N` with every
    /// right.
    pub others: Vec<(String, u32)>,
    /// The object is a directory.
    pub is_dir: bool,
    /// The object is a symbolic link or a junction (it was not followed).
    pub is_link: bool,
}

impl Access {
    /// Whether anyone but the trusted principals is granted anything; the
    /// daemon makes such a directory of its own private.
    pub fn is_loose(&self) -> bool {
        self.no_dacl || self.others.iter().any(|(_, m)| m & !SYNCHRONIZE != 0)
    }
}

/// The current user's SID, as a string (`S-1-5-21-…`).
pub fn current_user_sid() -> io::Result<String> {
    let mut token = null_mut();
    // SAFETY: GetCurrentProcess returns a pseudo-handle that needs no
    // closing; `token` is a valid place for the new handle.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: OpenProcessToken succeeded, so `token` is a handle we own.
    let token = unsafe { OwnedHandle::from_raw_handle(token) };
    let mut len = 0u32;
    // SAFETY: a size query: no buffer, and `len` receives the size needed.
    // It "fails" with ERROR_INSUFFICIENT_BUFFER, which is expected.
    unsafe { GetTokenInformation(token.as_raw_handle(), TokenUser, null_mut(), 0, &mut len) };
    if len == 0 {
        return Err(io::Error::last_os_error());
    }
    // u64s, so the buffer is aligned for TOKEN_USER (pointer alignment).
    let mut buf = vec![0u64; (len as usize).div_ceil(8)];
    // SAFETY: `buf` is writable for at least `len` bytes and outlives the
    // call; the call does not retain it.
    let ok = unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            buf.as_mut_ptr().cast(),
            len,
            &mut len,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: on success the buffer starts with a TOKEN_USER whose SID
    // points into the same buffer, which is still alive and aligned.
    let user = unsafe { &*buf.as_ptr().cast::<TOKEN_USER>() };
    sid_string(user.User.Sid)
}

/// Creates `dir`, and every missing parent, owner-only. A directory that
/// already exists is left as it is (see [`make_private`]).
pub fn create_private_dir(dir: &Path) -> io::Result<()> {
    let missing: Vec<&Path> = dir
        .ancestors()
        .take_while(|a| !a.as_os_str().is_empty() && std::fs::symlink_metadata(a).is_err())
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    let sd = private_descriptor(Kind::Dir)?;
    let sa = attributes(&sd);
    for d in missing.iter().rev() {
        let path = wide(d)?;
        // SAFETY: `path` is NUL-terminated and `sa` (with the descriptor it
        // points to) lives until the call returns; neither is retained.
        if unsafe { CreateDirectoryW(path.as_ptr(), &sa) } == 0 {
            let e = io::Error::last_os_error();
            // Another process created it meanwhile.
            if !(e.kind() == io::ErrorKind::AlreadyExists && d.is_dir()) {
                return Err(e);
            }
        }
    }
    Ok(())
}

/// Creates a new, empty file (failing if it exists) that only the trusted
/// principals can open, open for writing.
pub fn create_private_file(path: &Path) -> io::Result<File> {
    let sd = private_descriptor(Kind::File)?;
    let sa = attributes(&sd);
    let name = wide(path)?;
    // SAFETY: `name` is NUL-terminated and `sa` lives until the call
    // returns; neither is retained. The share modes are std's defaults, so
    // the file can still be renamed over while open.
    let h = unsafe {
        CreateFileW(
            name.as_ptr(),
            GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            &sa,
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL,
            null_mut(),
        )
    };
    if h == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: CreateFileW succeeded, so `h` is a file handle we own.
    Ok(unsafe { File::from_raw_handle(h) })
}

/// Replaces the DACL of `path` with the owner-only one, protected from
/// inheritance. For a directory the change is propagated to what is in it
/// (whatever inherits from it gets the new entries), since on Windows a
/// private directory does not by itself keep its files private: everyone
/// holds "bypass traverse checking". Refuses a symbolic link or junction.
pub fn make_private(path: &Path) -> io::Result<()> {
    let meta = std::fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a symbolic link or junction is not accepted",
        ));
    }
    let kind = if meta.is_dir() { Kind::Dir } else { Kind::File };
    let sd = private_descriptor(kind)?;
    let (mut present, mut defaulted) = (0, 0);
    let mut dacl: *mut ACL = null_mut();
    // SAFETY: `sd` is a valid self-relative descriptor that outlives the
    // call; the out-pointers are valid. `dacl` then points into `sd`.
    if unsafe { GetSecurityDescriptorDacl(sd.0, &mut present, &mut dacl, &mut defaulted) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let name = wide(path)?;
    // SAFETY: `name` is NUL-terminated; `dacl` points into `sd`, alive
    // until the end of this function. Only the DACL is set (no owner,
    // group or SACL), so the null pointers are not read.
    let r = unsafe {
        SetNamedSecurityInfoW(
            name.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            dacl,
            null(),
        )
    };
    if r != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(r as i32));
    }
    Ok(())
}

/// Reads the owner and DACL of `path` without following a symbolic link
/// or junction in its last component.
pub fn access(path: &Path) -> io::Result<Access> {
    let file = std::fs::OpenOptions::new()
        .access_mode(READ_CONTROL)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    let meta = file.metadata()?;
    let mut owner: PSID = null_mut();
    let mut dacl: *mut ACL = null_mut();
    let mut sd = null_mut();
    // SAFETY: `file` is an open handle with READ_CONTROL, borrowed for the
    // call; the out-pointers are valid. On success `owner` and `dacl` point
    // into the descriptor returned in `sd`, which we free with LocalFree.
    let r = unsafe {
        GetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            null_mut(),
            &mut dacl,
            null_mut(),
            &mut sd,
        )
    };
    if r != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(r as i32));
    }
    let sd = Local(sd);
    let me = current_user_sid()?;
    let trusted = |sid: &str| {
        sid == me || [SYSTEM, ADMINISTRATORS, CREATOR_OWNER, OWNER_RIGHTS].contains(&sid)
    };
    let owner = sid_string(owner)?;
    let owner_trusted = owner == me || owner == SYSTEM || owner == ADMINISTRATORS;
    let mut others = Vec::new();
    if !dacl.is_null() {
        for ace in aces(dacl)? {
            // SAFETY: `ace` points at an entry inside the DACL, which lives
            // in `sd`; every entry starts with an ACE_HEADER.
            let header = unsafe { &*ace.cast::<ACE_HEADER>() };
            match header.AceType {
                ALLOWED => {
                    // SAFETY: an ACCESS_ALLOWED_ACE: header, mask, then the
                    // SID starting at `SidStart`, all inside the entry.
                    let allowed = unsafe { &*ace.cast::<ACCESS_ALLOWED_ACE>() };
                    let sid = sid_string((&raw const allowed.SidStart).cast_mut().cast())?;
                    if !trusted(&sid) {
                        others.push((sid, allowed.Mask));
                    }
                }
                DENIED => {}
                other => others.push((format!("ACE type {other}"), u32::MAX)),
            }
        }
    }
    drop(sd);
    Ok(Access {
        owner,
        owner_trusted,
        no_dacl: dacl.is_null(),
        others,
        is_dir: meta.is_dir(),
        is_link: meta.file_type().is_symlink(),
    })
}

/// Checks that `path` is a private file or directory (see the module docs).
pub fn check_private(path: &Path, kind: Kind) -> Result<(), CheckError> {
    let a = access(path)?;
    let insecure = |reason: String| Err(CheckError::Insecure(reason));
    if a.is_link {
        return insecure("a symbolic link or junction is not accepted".into());
    }
    match kind {
        Kind::File if a.is_dir => return insecure("not a regular file".into()),
        Kind::Dir if !a.is_dir => return insecure("not a directory".into()),
        _ => {}
    }
    if !a.owner_trusted {
        return insecure(format!(
            "owned by {}, not by this user ({})",
            a.owner,
            current_user_sid()?
        ));
    }
    if a.no_dacl {
        return insecure(
            "it has no access control list (a NULL DACL, or a filesystem without ACLs), \
             so anyone can read and change it"
                .into(),
        );
    }
    let forbidden = match kind {
        Kind::File => !SYNCHRONIZE,
        Kind::Dir => DIR_WRITE,
    };
    let bad: Vec<String> = a
        .others
        .iter()
        .filter(|(_, m)| m & forbidden != 0)
        .map(|(sid, m)| format!("{sid} (access 0x{m:x})"))
        .collect();
    if !bad.is_empty() {
        let what = match kind {
            Kind::File => "lets other users open it",
            Kind::Dir => "lets other users change it",
        };
        return insecure(format!(
            "its access control list {what}: {}",
            bad.join(", ")
        ));
    }
    Ok(())
}

// ------------------------------------------------------------------ helpers

/// Memory the system allocated with `LocalAlloc`, freed on drop.
struct Local(*mut c_void);

impl Drop for Local {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the pointer came from a Win32 call documented to
            // return LocalAlloc memory, and is freed exactly once.
            unsafe { LocalFree(self.0) };
        }
    }
}

/// The owner-only descriptor, in SDDL: a protected DACL (`P`) allowing full
/// access (`FA`) to the user, `SYSTEM` (`SY`) and `Administrators` (`BA`),
/// inherited by files (`OI`) and directories (`CI`) for a directory.
fn private_descriptor(kind: Kind) -> io::Result<Local> {
    let user = current_user_sid()?;
    let inherit = match kind {
        Kind::Dir => "OICI",
        Kind::File => "",
    };
    let sddl = format!("D:P(A;{inherit};FA;;;{user})(A;{inherit};FA;;;SY)(A;{inherit};FA;;;BA)");
    let sddl: Vec<u16> = sddl.encode_utf16().chain(Some(0)).collect();
    let mut sd = null_mut();
    // SAFETY: `sddl` is NUL-terminated and outlives the call; `sd` receives
    // a LocalAlloc'd descriptor, which `Local` frees.
    let ok = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut sd,
            null_mut(),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Local(sd))
}

fn attributes(sd: &Local) -> SECURITY_ATTRIBUTES {
    SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd.0,
        bInheritHandle: 0,
    }
}

/// The entries of a DACL, as pointers into it.
fn aces(dacl: *mut ACL) -> io::Result<Vec<*mut c_void>> {
    // SAFETY: an all-zero ACL_SIZE_INFORMATION is a valid value.
    let mut info: ACL_SIZE_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: `dacl` is a valid ACL (from GetSecurityInfo), and `info` is a
    // writable ACL_SIZE_INFORMATION of the size passed.
    let ok = unsafe {
        GetAclInformation(
            dacl,
            (&raw mut info).cast(),
            size_of::<ACL_SIZE_INFORMATION>() as u32,
            AclSizeInformation,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut out = Vec::with_capacity(info.AceCount as usize);
    for i in 0..info.AceCount {
        let mut ace = null_mut();
        // SAFETY: `i` is below the entry count GetAclInformation reported.
        if unsafe { GetAce(dacl, i, &mut ace) } == 0 {
            return Err(io::Error::last_os_error());
        }
        out.push(ace);
    }
    Ok(out)
}

fn sid_string(sid: PSID) -> io::Result<String> {
    let mut s = null_mut();
    // SAFETY: `sid` points at a valid SID (from the token or a descriptor
    // that is still alive); `s` receives a LocalAlloc'd string.
    if unsafe { ConvertSidToStringSidW(sid, &mut s) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let _free = Local(s.cast());
    // SAFETY: the string is NUL-terminated and alive until `_free` drops.
    let len = (0..).take_while(|&i| unsafe { *s.add(i) } != 0).count();
    // SAFETY: `len` u16s from `s` are initialised (counted above).
    let units = unsafe { std::slice::from_raw_parts(s, len) };
    Ok(String::from_utf16_lossy(units))
}

/// A NUL-terminated wide path; relative paths are made absolute.
fn wide(p: &Path) -> io::Result<Vec<u16>> {
    let abs: PathBuf = std::path::absolute(p)?;
    let mut v: Vec<u16> = abs.as_os_str().encode_wide().collect();
    if v.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path contains a NUL character",
        ));
    }
    v.push(0);
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "whoosh-winacl-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            Self(dir)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Grants Everyone (`*S-1-1-0`) modify access with `icacls`, the way a
    /// user (or a careless installer) would loosen a directory.
    fn loosen(path: &Path, inherit: bool) {
        let grant = if inherit {
            "*S-1-1-0:(OI)(CI)M"
        } else {
            "*S-1-1-0:M"
        };
        let out = Command::new("icacls")
            .arg(path)
            .args(["/grant", grant])
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
    }

    #[test]
    fn the_user_sid_is_a_sid() {
        let sid = current_user_sid().unwrap();
        assert!(sid.starts_with("S-1-5-"), "{sid}");
    }

    #[test]
    fn created_objects_are_private_from_the_start() {
        let t = TempDir::new("create");
        // Missing parents are created private too.
        let dir = t.0.join("a").join("b");
        create_private_dir(&dir).unwrap();
        create_private_dir(&dir).unwrap();
        check_private(&t.0, Kind::Dir).unwrap();
        check_private(&dir, Kind::Dir).unwrap();
        // Even inside a directory everyone may write to, a private file
        // inherits nothing.
        loosen(&dir, true);
        assert!(check_private(&dir, Kind::Dir).is_err());
        let file = dir.join("secret");
        drop(create_private_file(&file).unwrap());
        check_private(&file, Kind::File).unwrap();
        assert_eq!(
            create_private_file(&file).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        // An ordinary file there does inherit the loose entry.
        let plain = dir.join("plain");
        std::fs::write(&plain, b"x").unwrap();
        assert!(matches!(
            check_private(&plain, Kind::File),
            Err(CheckError::Insecure(_))
        ));
        assert!(matches!(
            check_private(&plain, Kind::Dir),
            Err(CheckError::Insecure(_))
        ));
    }

    #[test]
    fn make_private_tightens_and_reaches_what_inherits() {
        let t = TempDir::new("tighten");
        std::fs::create_dir_all(&t.0).unwrap();
        loosen(&t.0, true);
        let inner = t.0.join("inner.db");
        std::fs::write(&inner, b"x").unwrap();
        let a = access(&t.0).unwrap();
        assert!(a.is_loose(), "{a:?}");
        assert!(check_private(&inner, Kind::File).is_err());

        make_private(&t.0).unwrap();
        assert!(!access(&t.0).unwrap().is_loose());
        check_private(&t.0, Kind::Dir).unwrap();
        // The file only inherited the loose entry; it is gone now.
        check_private(&inner, Kind::File).unwrap();

        // An explicit entry on a file is the file's own: tighten the file.
        loosen(&inner, false);
        assert!(check_private(&inner, Kind::File).is_err());
        make_private(&inner).unwrap();
        check_private(&inner, Kind::File).unwrap();
    }

    #[test]
    fn links_and_missing_objects_are_refused() {
        let t = TempDir::new("links");
        create_private_dir(&t.0).unwrap();
        let missing = t.0.join("missing");
        match check_private(&missing, Kind::File) {
            Err(CheckError::Io(e)) => assert_eq!(e.kind(), io::ErrorKind::NotFound),
            other => panic!("{other:?}"),
        }
        // A junction needs no privilege (a symbolic link may).
        let target = t.0.join("target");
        create_private_dir(&target).unwrap();
        let junction = t.0.join("junction");
        let out = Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&junction)
            .arg(&target)
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        match check_private(&junction, Kind::Dir) {
            Err(CheckError::Insecure(reason)) => assert!(reason.contains("junction"), "{reason}"),
            other => panic!("{other:?}"),
        }
        assert!(make_private(&junction).is_err());
    }
}
