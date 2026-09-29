//! The file facts the custody checks compare, read the same way on every
//! platform. On Unix a [`Stamp`] is exactly `lstat`/`fstat`: `dev`, `ino`,
//! `nlink`, `uid == getuid()`, and the group/other permission bits. On
//! Windows it comes from the object's handle through `xcb-platform`: volume
//! serial, file index, link count, owner SID, and whether the DACL grants
//! anyone else (see that crate's docs for the mapping).

use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Stamp {
    pub dev: u64,
    pub ino: u64,
    pub links: u64,
    pub len: u64,
    /// The object is a regular file (never a symlink or reparse point).
    pub file: bool,
    /// The object is a directory (never a symlink or reparse point).
    pub dir: bool,
    /// Owned by the current user.
    pub owned: bool,
    /// No access for group or other (`mode & 0o077 == 0`).
    pub private: bool,
    /// No write access for group or other (`mode & 0o022 == 0`).
    pub unshared_write: bool,
}

#[cfg(unix)]
fn stamp(metadata: &std::fs::Metadata) -> Stamp {
    use std::os::unix::fs::MetadataExt;
    Stamp {
        dev: metadata.dev(),
        ino: metadata.ino(),
        links: metadata.nlink(),
        len: metadata.size(),
        file: metadata.file_type().is_file(),
        dir: metadata.file_type().is_dir(),
        owned: metadata.uid() == rustix::process::getuid().as_raw(),
        private: metadata.mode() & 0o077 == 0,
        unshared_write: metadata.mode() & 0o022 == 0,
    }
}

#[cfg(windows)]
fn stamp(facts: &xcb_platform::Facts) -> Stamp {
    Stamp {
        dev: facts.volume,
        ino: facts.index,
        links: facts.links,
        len: facts.len,
        file: facts.kind == xcb_platform::Kind::File,
        dir: facts.kind == xcb_platform::Kind::Directory,
        owned: facts.owned,
        private: facts.private,
        // A DACL that grants no one else grants no one else write access; a
        // shared DACL is treated as shared for writing too.
        unshared_write: facts.private,
    }
}

/// `lstat(path)`: the facts of `path` itself, never a link's target.
pub(crate) fn lstat(path: &Path) -> io::Result<Stamp> {
    #[cfg(unix)]
    {
        Ok(stamp(&std::fs::symlink_metadata(path)?))
    }
    #[cfg(windows)]
    {
        Ok(stamp(&xcb_platform::path_facts(path)?))
    }
}

/// `fstat(file)`: the facts of an open handle.
pub(crate) fn fstat(file: &File) -> io::Result<Stamp> {
    #[cfg(unix)]
    {
        Ok(stamp(&file.metadata()?))
    }
    #[cfg(windows)]
    {
        Ok(stamp(&xcb_platform::file_facts(file)?))
    }
}

/// Open without following a final symlink: `O_NOFOLLOW | O_CLOEXEC`, plus
/// `O_NONBLOCK` when `nonblock` (so a FIFO cannot stall the open). On
/// Windows, `FILE_FLAG_OPEN_REPARSE_POINT`; callers still check the kind.
pub(crate) fn no_follow(options: &mut OpenOptions, nonblock: bool) -> &mut OpenOptions {
    #[cfg(unix)]
    {
        use rustix::fs::OFlags;
        use std::os::unix::fs::OpenOptionsExt;
        let mut flags = OFlags::NOFOLLOW | OFlags::CLOEXEC;
        if nonblock {
            flags |= OFlags::NONBLOCK;
        }
        options.custom_flags(flags.bits() as i32)
    }
    #[cfg(windows)]
    {
        let _ = nonblock;
        xcb_platform::no_follow(options)
    }
}

/// Create new files as `0600`. On Windows a new file inherits the owner-only
/// DACL of the private directory it is created in.
pub(crate) fn owner_only(options: &mut OpenOptions) -> &mut OpenOptions {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600)
    }
    #[cfg(windows)]
    {
        options
    }
}

/// Whether a process with this id exists: `Some(true)` when it does (a
/// permission refusal also proves presence), `Some(false)` when no such
/// process exists, `None` when the probe itself failed. A number that exists
/// is never proof that it is the same process.
pub(crate) fn process_exists(pid: u32) -> Option<bool> {
    #[cfg(unix)]
    {
        let pid = rustix::process::Pid::from_raw(i32::try_from(pid).ok()?)?;
        match rustix::process::test_kill_process(pid) {
            Ok(()) | Err(rustix::io::Errno::PERM) => Some(true),
            Err(rustix::io::Errno::SRCH) => Some(false),
            Err(_) => None,
        }
    }
    #[cfg(windows)]
    {
        xcb_platform::process_exists(pid)
    }
}

/// Whether an open file's permission bits are exactly `mode` (`st_mode &
/// 0o777`). On Windows a private mode (`0o600`, `0o700`) means an owner-only
/// DACL; any other mode is never satisfied there.
pub(crate) fn has_mode(file: &File, mode: u32) -> io::Result<bool> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(file.metadata()?.mode() & 0o777 == mode)
    }
    #[cfg(windows)]
    {
        Ok(mode & 0o077 == 0 && xcb_platform::file_facts(file)?.private)
    }
}
