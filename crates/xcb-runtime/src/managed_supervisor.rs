//! Identity and graceful retirement for the detached managed supervisor.
//!
//! The supervisor lock remains the authority for exclusive ownership. This
//! record explains which implementation owns it; a PID is never permission to
//! signal a process. Call `register` only after acquiring that lock, and call
//! `check_running` only while another process holds it.

use crate::{Error, Result, digest, private, process};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    os::unix::fs::MetadataExt,
    path::{Component, Path, PathBuf},
    time::Duration,
};

const MAX_RECORD: usize = 16 * 1024;
const RECORD_NAME: &str = "supervisor.identity.json";

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    version: u32,
    pid: u32,
    executable: PathBuf,
    sha256: String,
    package_version: String,
}

impl Record {
    fn validate(&self) -> Result<()> {
        if self.version != 1
            || self.pid == 0
            || !self.executable.is_absolute()
            || self
                .executable
                .components()
                .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
            || !xcb_core::hex64(&self.sha256)
            || self.package_version.is_empty()
            || self.package_version.len() > 64
            || !self
                .package_version
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b".-+".contains(&byte))
        {
            return Err(Error::Unavailable(
                "managed supervisor identity is invalid; inspect its private state before restarting it",
            ));
        }
        Ok(())
    }
}

/// Metadata is only a cache key for the byte check, never proof of worker exit.
#[derive(Debug, PartialEq, Eq)]
struct FileStamp {
    device: u64,
    inode: u64,
    bytes: u64,
    mode: u32,
    uid: u32,
    gid: u32,
    links: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}

impl FileStamp {
    fn read(path: &Path) -> Result<Self> {
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.is_file() {
            return Err(Error::PrivateState);
        }
        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            bytes: metadata.len(),
            mode: metadata.mode(),
            uid: metadata.uid(),
            gid: metadata.gid(),
            links: metadata.nlink(),
            modified: (metadata.mtime(), metadata.mtime_nsec()),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        })
    }
}

fn verified_stamp(executable: &Path, expected: &str) -> Result<FileStamp> {
    let before = FileStamp::read(executable)?;
    if process::executable_digest(executable)? != expected {
        return Err(Error::Unavailable("managed supervisor executable changed"));
    }
    let after = FileStamp::read(executable)?;
    if before != after {
        return Err(Error::Conflict(
            "managed supervisor executable changed during verification",
        ));
    }
    Ok(after)
}

pub(crate) struct SupervisorIdentity {
    record: Record,
    stamp: FileStamp,
    draining: bool,
}

impl SupervisorIdentity {
    /// Publish the current process's startup identity while holding the
    /// supervisor lock. A stale record may be replaced only by that lock owner.
    pub(crate) fn register(root: &Path) -> Result<Self> {
        let (executable, sha256) = process::host_identity()?;
        Self::publish(
            root,
            Record {
                version: 1,
                pid: std::process::id(),
                executable,
                sha256,
                package_version: env!("CARGO_PKG_VERSION").into(),
            },
        )
    }

    fn publish(root: &Path, record: Record) -> Result<Self> {
        record.validate()?;
        let stamp = verified_stamp(&record.executable, &record.sha256)?;
        let directory = private::directory(&root.join("managed"))?;
        let path = directory.join(RECORD_NAME);
        let bytes = serde_json::to_vec(&record)?;
        match private::read(&path, MAX_RECORD) {
            Ok(previous) => private::replace(&path, &bytes, &digest(previous))?,
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                private::create(&path, &bytes)?;
            }
            Err(error) => return Err(error),
        }
        Ok(Self {
            record,
            stamp,
            draining: false,
        })
    }

    /// A replacement or unreadable executable permanently requests draining.
    /// The caller must stop dispatching new turns, retain existing worker
    /// custody, and exit only after those workers have settled. No task record
    /// or process is changed here. Unchanged files require only a metadata read.
    pub(crate) fn binary_replaced(&mut self) -> bool {
        if self.draining {
            return true;
        }
        match FileStamp::read(&self.record.executable) {
            Ok(current) if current == self.stamp => (),
            Ok(_) => match verified_stamp(&self.record.executable, &self.record.sha256) {
                Ok(current) => self.stamp = current,
                Err(_) => self.draining = true,
            },
            Err(_) => self.draining = true,
        }
        self.draining
    }
}

/// Validate a held supervisor lock's owner record against this client's exact
/// startup image. A short retry covers registration immediately after lock
/// acquisition. Unknown legacy owners are never signalled or adopted.
pub(crate) fn check_running(root: &Path, executable: &Path) -> Result<()> {
    let (host_path, host_sha256) = process::host_identity()?;
    if executable.canonicalize()? != host_path {
        return Err(Error::Unavailable(
            "managed supervisor must use this xcb executable; restart xcb",
        ));
    }
    check_expected(root, &host_sha256)
}

fn check_expected(root: &Path, expected_sha256: &str) -> Result<()> {
    let path = private::check_directory(&root.join("managed"))?.join(RECORD_NAME);
    let mut last = None;
    for attempt in 0..5 {
        let result = match private::read(&path, MAX_RECORD) {
            Ok(bytes) => match serde_json::from_slice::<Record>(&bytes) {
                Ok(record) => record.validate().and_then(|()| {
                    if record.sha256 != expected_sha256 || record.package_version != env!("CARGO_PKG_VERSION") {
                        Err(Error::Unavailable("another xcb build owns the managed supervisor; let its active workers settle, then reopen chat after it exits"))
                    } else {
                        verified_stamp(&record.executable, &record.sha256).map(|_| ())
                    }
                }),
                Err(_) => Err(Error::Unavailable("managed supervisor identity is incompatible; inspect its private state before restarting it")),
            },
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                Err(Error::Unavailable("managed supervisor is starting or is a legacy build without an identity record; retry, or stop the verified legacy supervisor after its workers settle"))
            }
            Err(error) => return Err(error),
        };
        match result {
            Ok(()) => return Ok(()),
            Err(error) => last = Some(error),
        }
        if attempt < 4 {
            std::thread::sleep(Duration::from_millis(25));
        }
    }
    Err(last.expect("at least one identity check"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    fn fixture() -> (tempfile::TempDir, PathBuf, SupervisorIdentity) {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let executable = root.join("xcb");
        fs::write(&executable, b"original executable").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let identity = SupervisorIdentity::publish(
            &root,
            Record {
                version: 1,
                pid: std::process::id(),
                sha256: process::executable_digest(&executable).unwrap(),
                executable,
                package_version: env!("CARGO_PKG_VERSION").into(),
            },
        )
        .unwrap();
        (directory, root, identity)
    }

    #[test]
    fn replacement_latches_drain_without_changing_tasks_or_signalling() {
        let (_directory, root, mut identity) = fixture();
        assert!(!identity.binary_replaced());
        let preserved = root.join("managed/task-evidence");
        private::create(&preserved, b"queued work and worker custody").unwrap();
        let replacement = root.join("replacement");
        fs::write(&replacement, b"replacement executable").unwrap();
        fs::set_permissions(&replacement, fs::Permissions::from_mode(0o700)).unwrap();
        fs::rename(&replacement, &identity.record.executable).unwrap();
        assert!(identity.binary_replaced());
        fs::write(&identity.record.executable, b"original executable").unwrap();
        assert!(
            identity.binary_replaced(),
            "draining must not resume dispatch after a rollback"
        );
        assert_eq!(
            private::read(&preserved, 128).unwrap(),
            b"queued work and worker custody"
        );
    }

    #[test]
    fn same_bytes_remain_usable_but_missing_or_unsafe_executable_drains() {
        let (_directory, _root, mut identity) = fixture();
        fs::write(&identity.record.executable, b"original executable").unwrap();
        assert!(!identity.binary_replaced());
        fs::set_permissions(
            &identity.record.executable,
            fs::Permissions::from_mode(0o777),
        )
        .unwrap();
        assert!(identity.binary_replaced());
        let (_directory, _root, mut identity) = fixture();
        fs::remove_file(&identity.record.executable).unwrap();
        assert!(identity.binary_replaced());
    }

    #[test]
    fn identity_matches_only_the_expected_build_and_private_record() {
        let (_directory, root, identity) = fixture();
        assert!(check_expected(&root, &identity.record.sha256).is_ok());
        assert!(
            check_expected(&root, &"a".repeat(64))
                .unwrap_err()
                .to_string()
                .contains("another xcb build")
        );
        let path = root.join("managed").join(RECORD_NAME);
        fs::remove_file(&path).unwrap();
        assert!(
            check_expected(&root, &identity.record.sha256)
                .unwrap_err()
                .to_string()
                .contains("legacy build")
        );
        let target = root.join("managed/identity-copy");
        private::create(&target, &serde_json::to_vec(&identity.record).unwrap()).unwrap();
        symlink(&target, &path).unwrap();
        assert!(check_expected(&root, &identity.record.sha256).is_err());
    }

    #[test]
    fn a_lock_owners_new_registration_replaces_stale_identity() {
        let (_directory, root, identity) = fixture();
        let mut next = identity.record;
        next.pid = next.pid.saturating_add(1);
        let replacement = SupervisorIdentity::publish(&root, next).unwrap();
        let bytes = private::read(&root.join("managed").join(RECORD_NAME), MAX_RECORD).unwrap();
        let stored: Record = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(stored.pid, replacement.record.pid);
        assert_eq!(
            fs::metadata(root.join("managed").join(RECORD_NAME))
                .unwrap()
                .mode()
                & 0o777,
            0o600
        );
        assert!(check_expected(&root, &stored.sha256).is_ok());
    }

    #[test]
    fn registration_uses_the_verified_process_startup_image() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let identity = SupervisorIdentity::register(&root).unwrap();
        let (path, sha256) = process::host_identity().unwrap();
        assert_eq!(identity.record.executable, path);
        assert_eq!(identity.record.sha256, sha256);
        assert_eq!(identity.record.pid, std::process::id());
        assert!(check_running(&root, &path).is_ok());
    }
}
