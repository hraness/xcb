//! Windows primitives behind xcb's private-state custody.
//!
//! Unix builds use `std::os::unix` and `rustix` directly and never link this
//! crate's code: everything here is `cfg(windows)`. The Windows equivalents of
//! the Unix custody rules are:
//!
//! - owner uid → the file's owner SID is the process user (or the token's
//!   default owner, which is `Administrators` for an elevated administrator);
//! - `mode & 0o077 == 0` → the DACL is present and every allow entry that
//!   applies to the object names the user, the token owner, `SYSTEM`, or
//!   `Administrators` (the Windows counterparts of root);
//! - `mkdir(0o700)` → `CreateDirectoryW` with a protected DACL that grants
//!   only the user, inherited by everything created inside;
//! - `O_NOFOLLOW` → `FILE_FLAG_OPEN_REPARSE_POINT` and a check that the
//!   opened object is not a reparse point (symlink, junction, mount point);
//! - `(st_dev, st_ino, st_nlink)` → volume serial, file index, link count
//!   from `GetFileInformationByHandle`;
//! - a process group (`setpgid`, `killpg`) → a kill-on-close Job Object that
//!   the child joins before its first instruction runs.

#[cfg(windows)]
mod job;
#[cfg(windows)]
mod windows;

#[cfg(windows)]
pub use job::*;

#[cfg(windows)]
pub use windows::*;
