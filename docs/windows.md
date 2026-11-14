# Whoosh on Windows

What the engine, daemon and CLI do differently on Windows, and why. The
whole Rust test suite runs on Windows in CI (`windows-2025`) and blocks a
merge like Linux and macOS do.

## Data directory and private files

The data directory is `%LOCALAPPDATA%\whoosh` (`WHOOSH_DATA_DIR` overrides
it). It holds `runtime.json` (the daemon's port and token), the state
database, the journal and the logs.

On Unix these are kept private with modes 0700 and 0600. On Windows the
same job is done with access control lists (`whoosh_proto::winacl`):

- **Created private.** The daemon, the CLI (when it starts the daemon) and
  the store create the data directory, `logs\`, the journal and state
  directories and `runtime.json` with a security descriptor in the create
  call: a *protected* DACL (nothing inherited from the parent) granting full
  control to the current user, `SYSTEM` and `Administrators`, inherited by
  everything created inside. No moment exists where the object has a wider
  ACL.
- **Checked like the modes.** Every client reads `runtime.json` through
  `whoosh_proto::runtime::read_runtime`, which refuses:
  - a symbolic link or junction;
  - an owner other than the current user, `SYSTEM` or `Administrators`;
  - a missing DACL (a NULL DACL, or a filesystem without ACLs such as FAT),
    which grants everyone everything;
  - a file whose DACL grants anyone else any access, or a directory whose
    DACL lets anyone else change it (add or delete entries, write
    attributes, change the ACL or owner). Inherit-only entries count.
- **Tightened when loose.** A data directory of ours that grants others
  anything gets the owner-only DACL, and the change is passed on to what
  inherits from it: on Windows everyone holds "bypass traverse checking",
  so a private directory does not by itself protect the files in it. A
  loose state database or log file is tightened the same way.

`SYSTEM` and `Administrators` are trusted because they can take any file
anyway, as root can on Unix, and because an elevated process creates files
owned by the `Administrators` group. This matches what the Windows port of
OpenSSH demands of private keys.

## Durability without a directory fsync

Linux and macOS make a new file's name durable with an `fsync` of its
directory. Windows has no directory flush that works for unprivileged
handles on every filesystem, and the store does not use one: `sync_dir` is a
no-op there. The journal's crash safety still holds, for these reasons (the
full argument is in `crates/store/src/sys.rs`, on `sync_dir`):

- **New files.** On NTFS, creating a file and naming it in its directory
  are logged in the volume's metadata log before they take effect, and the
  log is a single ordered stream. Every file whose name must survive a power
  cut (a journal file, the database and its WAL, a part file) is created,
  grown by a write, then flushed with `FlushFileBuffers`. For that flush to
  keep its promise the new size must be durable, and committing the size's
  log record commits the earlier creation records too. On FAT/exFAT the
  size lives in the directory entry itself.
- **Renames** (`finish`, a corrupt database moved aside) use `MoveFileExW`
  with `MOVEFILE_WRITE_THROUGH`, which returns only once the move is on
  disk.

Not verified: ReFS (copy-on-write metadata, expected to behave the same)
and one corner case, a WAL that a crashed process created without ever
syncing it and that SQLite then overwrites in place. Neither has been
tested with real power cuts.

## Mark of the web

A finished download carries a `Zone.Identifier` alternate data stream, as
browsers write it:

```
[ZoneTransfer]
ZoneId=3
ReferrerUrl=https://example.com/download
HostUrl=https://cdn.example.com/file.exe
```

`ZoneId=3` is the Internet zone, so SmartScreen, Office's Protected View
and the "this file came from another computer" prompt apply. The engine
marks the part file just before its final rename and the stream moves with
it, so the finished file is never seen without it. The URLs keep scheme,
host, port and path only: user names, passwords, query strings and
fragments are dropped, since download links often carry tokens there.
Filesystems without streams (FAT, exFAT) get no mark; the download still
completes, with a warning in the log. There is no setting: the mark is
always written.

macOS gets the `com.apple.quarantine` attribute (`0081;<time>;Whoosh;`);
Linux gets `user.xdg.origin.url` and `user.xdg.referrer.url` where the
filesystem allows user attributes.

## Starting the daemon at logon

```
whoosh daemon autostart on
whoosh daemon autostart status
whoosh daemon autostart off
```

`on` adds a value named `Whoosh daemon` under
`HKCU\Software\Microsoft\Windows\CurrentVersion\Run`: per user, no
administrator rights. The value runs `whoosh-autostart.exe`, which ships
next to `whoosh.exe`:

- `whoosh-daemon` is a console program, so starting it from the Run key
  would open a console window at every logon. `whoosh-autostart` is a
  GUI-subsystem program: it starts the daemon detached, with no window, and
  exits.
- A data directory other than the default is passed with `--data-dir`.
- If a daemon is already running for the data directory (the desktop app
  started one, say), the new one notices and exits.

Task Manager's "Startup apps" page (and Settings > Apps > Startup) can
switch the entry off without removing it. `status` reports that as
"switched off"; `on` switches it back on.

For the desktop app's "launch at login", `whoosh_client::autostart` offers
the same calls (`enable`, `disable`, `status`) for any program under any
value name. On macOS and Linux they return "unsupported": there the desktop
app registers itself (a launch agent, an XDG autostart entry).

## Stopping the daemon

`whoosh daemon stop` and the desktop app use the `daemon.shutdown` request.
A daemon started in a console also stops on Ctrl-C, Ctrl-Break, closing the
console, log-off and shutdown. `whoosh get` and `whoosh watch` treat
Ctrl-Break like Ctrl-C (pause the download, exit 130), since in a console
Ctrl-Break would otherwise kill them outright.

## Testing on Windows

The CI runners are the only Windows machines this project tests on. The
Windows forms of the Unix-only tests use:

- `icacls` to loosen a directory or file the way a user would, and
  `mklink /J` for a link (a junction needs no privilege);
- `GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT)` to a child started in its
  own process group, in place of `kill -INT`/`-TERM`;
- `tasklist`/`taskkill` in place of `kill -0`/`-9`;
- the peak working set in place of `/proc/self/status`;
- a test hook in the store that makes writes or syncs of one file fail,
  in place of `/dev/full` and `/dev/null` (Linux keeps the device tests
  too).

The registry test uses a value name of its own, so running the suite on a
developer machine does not touch the user's `Whoosh daemon` entry.
