//! Identity, liveness and graceful retirement for the detached managed
//! supervisor.
//!
//! The modern supervisor owner lock remains the exclusive ownership
//! authority. This record explains which implementation owns it; a PID is
//! never permission to signal a process. Call `register` (or publish a prepared identity) only
//! after acquiring that lock, and call `check_running` only while another
//! process holds it.
//!
//! The heartbeat file is a progress hint for clients, never authority: its
//! modification time moves while the supervisor loop is still ticking, so a
//! client can tell a live owner from one whose loop stopped making progress.

use crate::{Error, Result, digest, private, process};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, OpenOptions},
    path::{Component, Path, PathBuf},
    time::{Duration, Instant, SystemTime},
};

const MAX_RECORD: usize = 16 * 1024;
const RECORD_NAME: &str = "supervisor.identity.json";
const HEARTBEAT_NAME: &str = "supervisor.heartbeat";
/// The supervisor loop refreshes its heartbeat at most this often.
const HEARTBEAT_EVERY: Duration = Duration::from_secs(5);
/// A live owner whose heartbeat is older than this has stopped making
/// progress. Far above any legitimate single loop pass.
const UNRESPONSIVE_AFTER: Duration = Duration::from_secs(120);
/// A stale heartbeat is re-read this long before the owner is reported: a
/// supervisor waking from sleep refreshes it on its next 250 ms tick.
const UNRESPONSIVE_RECHECK: Duration = Duration::from_millis(400);
/// Identity reads retried while a freshly locked supervisor registers.
const IDENTITY_ATTEMPTS: u32 = 10;

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
        #[cfg(unix)]
        let identity = xcb_core::FileIdentity::of(&metadata);
        #[cfg(windows)]
        let identity = xcb_core::FileIdentity::of_path(path)?;
        Ok(Self {
            device: identity.dev,
            inode: identity.ino,
            bytes: identity.size,
            mode: identity.mode,
            uid: identity.uid,
            gid: identity.gid,
            links: identity.links,
            modified: identity.mtime,
            changed: identity.ctime,
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
    /// The managed state directory holding the record and heartbeat.
    directory: PathBuf,
    /// Wall-clock time of the last heartbeat refresh. Wall time, not a
    /// monotonic clock, so the first tick after a sleep refreshes at once.
    heartbeat_at: SystemTime,
}

/// A verified startup identity that is not published yet. Hashing the
/// executable takes a moment, so it happens before the lock is taken and the
/// record lands right after acquisition.
pub(crate) struct PreparedIdentity {
    record: Record,
    stamp: FileStamp,
}

impl PreparedIdentity {
    /// Publish while holding the supervisor lock. The heartbeat is refreshed
    /// first, so a client never pairs this record with a predecessor's stale
    /// heartbeat. A stale record may be replaced only by that lock owner.
    pub(crate) fn publish(self, root: &Path) -> Result<SupervisorIdentity> {
        let directory = private::directory(&root.join("managed"))?;
        touch_heartbeat(&directory)?;
        let heartbeat_at = SystemTime::now();
        let path = directory.join(RECORD_NAME);
        let bytes = serde_json::to_vec(&self.record)?;
        match private::read(&path, MAX_RECORD) {
            Ok(previous) => private::replace(&path, &bytes, &digest(previous))?,
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                private::create(&path, &bytes)?;
            }
            Err(error) => return Err(error),
        }
        Ok(SupervisorIdentity {
            record: self.record,
            stamp: self.stamp,
            draining: false,
            directory,
            heartbeat_at,
        })
    }
}

impl SupervisorIdentity {
    /// Publish the current process's startup identity while holding the
    /// supervisor lock. A stale record may be replaced only by that lock owner.
    #[cfg(test)]
    pub(crate) fn register(root: &Path) -> Result<Self> {
        Self::prepare()?.publish(root)
    }

    /// Verify this process's startup image without publishing it.
    pub(crate) fn prepare() -> Result<PreparedIdentity> {
        let (executable, sha256) = process::host_identity()?;
        Self::prepare_record(Record {
            version: 1,
            pid: std::process::id(),
            executable,
            sha256,
            package_version: env!("CARGO_PKG_VERSION").into(),
        })
    }

    fn prepare_record(record: Record) -> Result<PreparedIdentity> {
        record.validate()?;
        let stamp = verified_stamp(&record.executable, &record.sha256)?;
        Ok(PreparedIdentity { record, stamp })
    }

    #[cfg(test)]
    fn publish(root: &Path, record: Record) -> Result<Self> {
        Self::prepare_record(record)?.publish(root)
    }

    /// Refresh the heartbeat when it is due. Returns false only when a due
    /// refresh failed, so the caller can surface it once.
    pub(crate) fn heartbeat(&mut self) -> bool {
        let now = SystemTime::now();
        let due = match now.duration_since(self.heartbeat_at) {
            Ok(elapsed) => elapsed >= HEARTBEAT_EVERY,
            // The clock stepped back: refresh so the file follows it.
            Err(_) => true,
        };
        if !due {
            return true;
        }
        match touch_heartbeat(&self.directory) {
            Ok(()) => {
                self.heartbeat_at = now;
                true
            }
            Err(_) => false,
        }
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

/// Refresh (or create) the heartbeat file. Only its modification time
/// carries meaning; the file stays empty.
fn touch_heartbeat(directory: &Path) -> Result<()> {
    let path = directory.join(HEARTBEAT_NAME);
    let file = match crate::os::no_follow(OpenOptions::new().write(true), true).open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // A freshly created file carries the current time.
            return private::create(&path, &[]);
        }
        Err(error) => return Err(error.into()),
    };
    private::check_file(&file, 0)?;
    file.set_modified(SystemTime::now())?;
    Ok(())
}

/// How long ago the owner last refreshed its heartbeat; `None` when there is
/// no readable heartbeat to judge by.
fn heartbeat_age(directory: &Path) -> Option<Duration> {
    let file = private::open_file(&directory.join(HEARTBEAT_NAME), 0).ok()?;
    let modified = file.metadata().ok()?.modified().ok()?;
    // A heartbeat from the future (clock stepped back) counts as fresh.
    Some(
        SystemTime::now()
            .duration_since(modified)
            .unwrap_or_default(),
    )
}

/// Whether the published owner record names `pid`: the confirmation a
/// client waits for after spawning a supervisor.
pub(crate) fn registered(root: &Path, pid: u32) -> bool {
    let Ok(directory) = private::check_directory(&root.join("managed")) else {
        return false;
    };
    private::read(&directory.join(RECORD_NAME), MAX_RECORD)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Record>(&bytes).ok())
        .is_some_and(|record| record.validate().is_ok() && record.pid == pid)
}

/// Validate a held supervisor lock's owner record against this client's exact
/// startup image, then check that the owner is alive and still making
/// progress. A short retry covers registration immediately after lock
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

/// Validate the complete-pass relay boundary using this client's exact
/// executable, not a package version or an unbound capability flag. Called
/// while the transition holds shared legacy exclusion and a modern owner
/// holds its separate exclusive lock. No network or liveness wait occurs.
pub(crate) fn check_relay_boundary(root: &Path) -> Result<()> {
    let (_, expected_sha256) = process::host_identity()?;
    let directory = private::check_directory(&root.join("managed"))?;
    let bytes = match private::read(&directory.join(RECORD_NAME), MAX_RECORD) {
        Ok(bytes) => bytes,
        Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(Error::Unavailable(
                "the background xcb supervisor is still starting or cannot pause its relay; let it finish starting or let its active tasks finish, then retry sign-in",
            ));
        }
        Err(error) => return Err(error),
    };
    let record: Record = serde_json::from_slice(&bytes).map_err(|_| {
        Error::Unavailable("the running xcb supervisor cannot safely pause its relay; let its active tasks finish and retry sign-in")
    })?;
    record.validate()?;
    if record.sha256 != expected_sha256 || record.package_version != env!("CARGO_PKG_VERSION") {
        return Err(Error::Unavailable(
            "the running xcb supervisor is a different build; let its active tasks finish and retry sign-in",
        ));
    }
    verified_stamp(&record.executable, &record.sha256)?;
    owner_alive(record.pid)
}

fn check_expected(root: &Path, expected_sha256: &str) -> Result<()> {
    let directory = private::check_directory(&root.join("managed"))?;
    let path = directory.join(RECORD_NAME);
    let mut last = None;
    for attempt in 0..IDENTITY_ATTEMPTS {
        let result = match private::read(&path, MAX_RECORD) {
            Ok(bytes) => match serde_json::from_slice::<Record>(&bytes) {
                Ok(record) => record.validate().and_then(|()| {
                    if record.sha256 != expected_sha256 || record.package_version != env!("CARGO_PKG_VERSION") {
                        Err(Error::Unavailable("another xcb build owns the managed supervisor; let its active workers settle, then reopen chat after it exits"))
                    } else {
                        verified_stamp(&record.executable, &record.sha256)
                            .and_then(|_| owner_alive(record.pid))
                            .map(|()| record.pid)
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
            Ok(pid) => return check_progress(&directory, pid),
            Err(error) => last = Some(error),
        }
        if attempt + 1 < IDENTITY_ATTEMPTS {
            std::thread::sleep(Duration::from_millis(25));
        }
    }
    Err(last.expect("at least one identity check"))
}

/// The recorded owner must still exist as one of this user's processes. A
/// record naming a gone (or foreign, hence reused) PID while the lock is held
/// means another process holds the lock: a supervisor that has not published
/// its record yet, or a client briefly taking the same lock.
fn owner_alive(pid: u32) -> Result<()> {
    #[cfg(unix)]
    let alive = i32::try_from(pid)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
        .is_some_and(|pid| rustix::process::test_kill_process(pid).is_ok());
    #[cfg(windows)]
    let alive = xcb_platform::process_exists(pid) == Some(true);
    if alive {
        Ok(())
    } else {
        Err(Error::Unavailable(
            "the background supervisor is starting, or another xcb process briefly holds its lock; retry in a moment",
        ))
    }
}

/// A live owner whose heartbeat stopped moving has a wedged loop: it holds
/// the lock but dispatches nothing. xcb never signals it; the person running
/// xcb gets the process number and the one safe way to replace it. Work it
/// was running stays held and asks for recovery at the next start.
fn check_progress(directory: &Path, pid: u32) -> Result<()> {
    let recheck = Instant::now() + UNRESPONSIVE_RECHECK;
    loop {
        match heartbeat_age(directory) {
            None => return Ok(()),
            Some(age) if age <= UNRESPONSIVE_AFTER => return Ok(()),
            Some(_) if Instant::now() >= recheck => break,
            Some(_) => std::thread::sleep(Duration::from_millis(50)),
        }
    }
    Err(Error::Guided {
        message: format!(
            "xcb's background supervisor (process {pid}) has not responded for more than {} minutes. Stop it with `kill -9 {pid}`, then reopen xcb; tasks it was running will ask you to recover them",
            UNRESPONSIVE_AFTER.as_secs() / 60
        ),
        next: None,
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};

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

    /// A PID that certainly names no live process: a reaped child.
    fn exited_pid() -> u32 {
        let mut child = std::process::Command::new("/usr/bin/true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        pid
    }

    fn age_heartbeat(root: &Path, by: Duration) {
        fs::OpenOptions::new()
            .write(true)
            .open(root.join("managed").join(HEARTBEAT_NAME))
            .unwrap()
            .set_modified(SystemTime::now() - by)
            .unwrap();
    }

    #[test]
    fn a_lock_owners_new_registration_replaces_stale_identity() {
        let (_directory, root, identity) = fixture();
        let mut stale = identity.record;
        stale.pid = exited_pid();
        let stale = SupervisorIdentity::publish(&root, stale).unwrap();
        // A held lock whose record names a gone process is not a running
        // supervisor: its successor has not registered yet.
        assert!(
            check_expected(&root, &stale.record.sha256)
                .unwrap_err()
                .to_string()
                .contains("starting")
        );
        let mut next = stale.record;
        next.pid = std::process::id();
        let replacement = SupervisorIdentity::publish(&root, next).unwrap();
        let bytes = private::read(&root.join("managed").join(RECORD_NAME), MAX_RECORD).unwrap();
        let stored: Record = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(stored.pid, replacement.record.pid);
        assert!(registered(&root, stored.pid));
        assert!(!registered(&root, stored.pid.saturating_add(1)));
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
    fn a_live_owner_whose_loop_stopped_is_reported_with_its_process_number() {
        let (_directory, root, mut identity) = fixture();
        let sha256 = identity.record.sha256.clone();
        assert!(check_expected(&root, &sha256).is_ok());
        age_heartbeat(&root, UNRESPONSIVE_AFTER + Duration::from_secs(60));
        let started = Instant::now();
        let error = check_expected(&root, &sha256).unwrap_err().to_string();
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "bounded recheck"
        );
        assert!(error.contains("has not responded"), "{error}");
        assert!(
            error.contains(&format!("kill -9 {}", std::process::id())),
            "{error}"
        );
        // A heartbeat inside the bound, or none at all, never blocks a client.
        age_heartbeat(&root, UNRESPONSIVE_AFTER - Duration::from_secs(10));
        assert!(check_expected(&root, &sha256).is_ok());
        fs::remove_file(root.join("managed").join(HEARTBEAT_NAME)).unwrap();
        assert!(check_expected(&root, &sha256).is_ok());
        // The next due refresh recreates it with the current time.
        identity.heartbeat_at = SystemTime::now() - HEARTBEAT_EVERY;
        assert!(identity.heartbeat());
        assert!(heartbeat_age(&root.join("managed")).unwrap() < Duration::from_secs(5));
    }

    #[test]
    fn heartbeat_refreshes_at_most_once_per_interval() {
        let (_directory, root, mut identity) = fixture();
        let directory = root.join("managed");
        age_heartbeat(&root, Duration::from_secs(30));
        // Published moments ago: not due, so the file keeps its old time.
        assert!(identity.heartbeat());
        assert!(heartbeat_age(&directory).unwrap() >= Duration::from_secs(29));
        identity.heartbeat_at = SystemTime::now() - HEARTBEAT_EVERY;
        assert!(identity.heartbeat());
        assert!(heartbeat_age(&directory).unwrap() < Duration::from_secs(5));
        assert_eq!(
            fs::metadata(directory.join(HEARTBEAT_NAME)).unwrap().mode() & 0o777,
            0o600
        );
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
