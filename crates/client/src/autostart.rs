//! Starting a program when the user logs on: per user, without
//! administrator rights.
//!
//! - **Windows:** a value under
//!   `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`, the per-user list
//!   Explorer runs at logon. Task Manager's "Startup apps" page (and
//!   Settings > Apps > Startup) can switch an entry off without deleting
//!   it; Windows records that under
//!   `HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run`
//!   (a binary value whose first byte is odd when disabled). [`status`]
//!   reports such an entry as [`Status::DisabledByUser`]; [`enable`] clears
//!   the switch, since the user asked for it again.
//! - **Elsewhere:** [`enable`] and friends fail with
//!   [`std::io::ErrorKind::Unsupported`] for now. The desktop app's "launch
//!   at login" covers macOS (a launch agent) and Linux (XDG autostart).
//!
//! The daemon's entry ([`DAEMON_ENTRY`]) runs `whoosh-autostart`
//! ([`launcher_exe`]), not `whoosh-daemon` itself: the daemon is a console
//! program, so Explorer would open a console window for it at every logon.
//! `whoosh-autostart` is a GUI-subsystem program that starts the daemon
//! detached, with no window, and exits. The desktop app can register itself
//! the same way under a name of its own.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

/// The value name of the daemon's entry.
pub const DAEMON_ENTRY: &str = "Whoosh daemon";

/// The launcher's executable name, without the platform's suffix.
pub const LAUNCHER: &str = "whoosh-autostart";

/// Whether an entry starts at logon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// No entry.
    Off,
    /// The entry exists and is enabled; `command` is what runs.
    On { command: String },
    /// The entry exists but the user switched it off in Task Manager or
    /// Settings.
    DisabledByUser { command: String },
}

/// `whoosh-autostart` next to the running executable (the `whoosh` CLI and
/// the launcher ship side by side), or `None` if it is not there.
pub fn launcher_exe() -> Option<PathBuf> {
    let name = format!("{LAUNCHER}{}", std::env::consts::EXE_SUFFIX);
    let exe = std::env::current_exe().ok()?.with_file_name(name);
    exe.is_file().then_some(exe)
}

/// Starts `program` with `args` at every logon, under the value name
/// `name`, replacing an entry of that name and clearing a "disabled" switch
/// the user set for it.
pub fn enable(name: &str, program: &Path, args: &[OsString]) -> io::Result<()> {
    imp::enable(name, &command_line(program, args))
}

/// Removes the entry; `Ok(false)` if there was none.
pub fn disable(name: &str) -> io::Result<bool> {
    imp::disable(name)
}

pub fn status(name: &str) -> io::Result<Status> {
    imp::status(name)
}

/// The command line for `program` and `args`, quoted the way
/// `CommandLineToArgvW` (and the C runtime) split it again: every argument
/// with a space, tab or quote (and every empty one) is quoted, quotes are
/// escaped, and backslashes before a quote are doubled. The program is
/// always quoted, so a path with spaces cannot run something else (the
/// `C:\Program.exe` trap).
pub fn command_line(program: &Path, args: &[OsString]) -> String {
    let mut s = format!("\"{}\"", program.display());
    for a in args {
        s.push(' ');
        quote_into(&a.to_string_lossy(), &mut s);
    }
    s
}

fn quote_into(arg: &str, out: &mut String) {
    if !arg.is_empty() && !arg.contains([' ', '\t', '"']) {
        out.push_str(arg);
        return;
    }
    out.push('"');
    let mut backslashes = 0;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            c => {
                out.extend(std::iter::repeat_n('\\', backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    out.extend(std::iter::repeat_n('\\', backslashes * 2));
    out.push('"');
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod imp {
    use std::io;
    use std::ptr::null_mut;

    use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
    use windows_sys::Win32::System::Registry::{
        HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_BINARY, RRF_RT_REG_SZ, RegDeleteKeyValueW,
        RegGetValueW, RegSetKeyValueW,
    };

    use super::Status;

    const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    const APPROVED: &str =
        r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    fn check(r: u32) -> io::Result<()> {
        if r == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(r as i32))
        }
    }

    pub(super) fn enable(name: &str, command: &str) -> io::Result<()> {
        let (key, value, data) = (wide(RUN), wide(name), wide(command));
        // SAFETY: every pointer is to a NUL-terminated UTF-16 buffer that
        // outlives the call; the data size is the buffer's, NUL included,
        // in bytes, as REG_SZ requires. Nothing is retained.
        check(unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                value.as_ptr(),
                REG_SZ,
                data.as_ptr().cast(),
                (data.len() * 2) as u32,
            )
        })?;
        // The user's "disabled" switch, if any: they asked for it again.
        delete(APPROVED, name).map(drop)
    }

    pub(super) fn disable(name: &str) -> io::Result<bool> {
        let removed = delete(RUN, name)?;
        delete(APPROVED, name)?;
        Ok(removed)
    }

    pub(super) fn status(name: &str) -> io::Result<Status> {
        let Some(bytes) = get(RUN, name, RRF_RT_REG_SZ)? else {
            return Ok(Status::Off);
        };
        let units: Vec<u16> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&c| u16::from_le_bytes(c))
            .take_while(|&u| u != 0)
            .collect();
        let command = String::from_utf16_lossy(&units);
        let disabled = get(APPROVED, name, RRF_RT_REG_BINARY)?
            .and_then(|b| b.first().copied())
            .is_some_and(|flag| flag & 1 == 1);
        Ok(if disabled {
            Status::DisabledByUser { command }
        } else {
            Status::On { command }
        })
    }

    /// Deletes a value; `Ok(false)` if it (or its key) did not exist.
    fn delete(key: &str, name: &str) -> io::Result<bool> {
        let (key, value) = (wide(key), wide(name));
        // SAFETY: both pointers are to NUL-terminated UTF-16 buffers that
        // outlive the call.
        let r = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), value.as_ptr()) };
        if r == ERROR_FILE_NOT_FOUND {
            return Ok(false);
        }
        check(r).map(|()| true)
    }

    /// Reads a value of the given type; `Ok(None)` if it does not exist.
    fn get(key: &str, name: &str, flags: u32) -> io::Result<Option<Vec<u8>>> {
        let (key, value) = (wide(key), wide(name));
        let mut len = 0u32;
        // SAFETY: a size query: no buffer, and `len` receives the size.
        let r = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                value.as_ptr(),
                flags,
                null_mut(),
                null_mut(),
                &mut len,
            )
        };
        if r == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        check(r)?;
        let mut buf = vec![0u8; len as usize];
        // SAFETY: `buf` is writable for `len` bytes and outlives the call.
        // (A value that grew in between fails with ERROR_MORE_DATA.)
        let r = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                value.as_ptr(),
                flags,
                null_mut(),
                buf.as_mut_ptr().cast(),
                &mut len,
            )
        };
        check(r)?;
        buf.truncate(len as usize);
        Ok(Some(buf))
    }

    #[cfg(test)]
    pub(super) fn set_approved(name: &str, flag: u8) -> io::Result<()> {
        use windows_sys::Win32::System::Registry::REG_BINARY;
        // What Task Manager writes: a flag byte, three zero bytes and a
        // FILETIME (left zero here).
        let mut data = [0u8; 12];
        data[0] = flag;
        let (key, value) = (wide(APPROVED), wide(name));
        // SAFETY: NUL-terminated key and value names; `data` is 12 bytes.
        check(unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                value.as_ptr(),
                REG_BINARY,
                data.as_ptr().cast(),
                data.len() as u32,
            )
        })
    }
}

#[cfg(not(windows))]
mod imp {
    use std::io;

    use super::Status;

    fn unsupported() -> io::Error {
        io::Error::new(
            io::ErrorKind::Unsupported,
            "starting at logon is set up by the desktop app on this OS \
             (or with a systemd user unit or a launch agent)",
        )
    }

    pub(super) fn enable(_name: &str, _command: &str) -> io::Result<()> {
        Err(unsupported())
    }

    pub(super) fn disable(_name: &str) -> io::Result<bool> {
        Err(unsupported())
    }

    pub(super) fn status(_name: &str) -> io::Result<Status> {
        Err(unsupported())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_lines_split_back_into_the_same_arguments() {
        let args: Vec<OsString> = [
            "--data-dir",
            r"C:\Users\Ana Ruiz\data\",
            "plain",
            "",
            r#"say "hi""#,
            r"back\\slash",
        ]
        .into_iter()
        .map(OsString::from)
        .collect();
        assert_eq!(
            command_line(
                Path::new(r"C:\Program Files\Whoosh\whoosh-autostart.exe"),
                &args
            ),
            r#""C:\Program Files\Whoosh\whoosh-autostart.exe" --data-dir "C:\Users\Ana Ruiz\data\\" plain "" "say \"hi\"" back\\slash"#
        );
    }

    /// The real registry, under a value name of its own.
    #[cfg(windows)]
    #[test]
    fn enable_disable_and_the_users_switch() {
        let name = format!("whoosh-test-{}", std::process::id());
        let program = Path::new(r"C:\Program Files\Whoosh\whoosh-autostart.exe");
        let args = [OsString::from("--data-dir"), OsString::from(r"C:\d d")];
        let command = command_line(program, &args);
        assert_eq!(status(&name).unwrap(), Status::Off);
        assert!(!disable(&name).unwrap());

        enable(&name, program, &args).unwrap();
        assert_eq!(
            status(&name).unwrap(),
            Status::On {
                command: command.clone()
            }
        );
        // Task Manager's "Disable" (3), then its "Enable" (2).
        imp::set_approved(&name, 3).unwrap();
        assert_eq!(
            status(&name).unwrap(),
            Status::DisabledByUser {
                command: command.clone()
            }
        );
        imp::set_approved(&name, 2).unwrap();
        assert_eq!(
            status(&name).unwrap(),
            Status::On {
                command: command.clone()
            }
        );
        // Enabling again clears the user's switch.
        imp::set_approved(&name, 3).unwrap();
        enable(&name, program, &args).unwrap();
        assert_eq!(status(&name).unwrap(), Status::On { command });

        assert!(disable(&name).unwrap());
        assert_eq!(status(&name).unwrap(), Status::Off);
        assert!(!disable(&name).unwrap());
    }

    #[cfg(not(windows))]
    #[test]
    fn elsewhere_it_is_unsupported() {
        let e = status("x").unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::Unsupported);
    }
}
