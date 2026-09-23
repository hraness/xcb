use crate::{Error, Result, digest, private};
use rusqlite::{Connection, OpenFlags};
use std::{
    fs::{self, OpenOptions},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    sync::{Condvar, Mutex, OnceLock},
    time::{Duration, Instant},
};

/// In-process serialization around coordination database setup and use. A
/// flag under a mutex with a condition variable gives waiters a bounded,
/// wake-on-release wait instead of a sleep loop.
static LOCAL_WRITER: (Mutex<bool>, Condvar) = (Mutex::new(false), Condvar::new());
const WAIT: Duration = Duration::from_secs(5);

struct LocalGuard;
impl Drop for LocalGuard {
    fn drop(&mut self) {
        if let Ok(mut held) = LOCAL_WRITER.0.lock() {
            *held = false;
        }
        LOCAL_WRITER.1.notify_one();
    }
}
fn local_writer(deadline: Instant) -> Result<LocalGuard> {
    let (flag, released) = &LOCAL_WRITER;
    let mut held = flag
        .lock()
        .map_err(|_| Error::Conflict("workspace writer lock poisoned"))?;
    while *held {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(Error::Conflict("workspace writer is busy; retry"));
        }
        held = released
            .wait_timeout(held, remaining)
            .map_err(|_| Error::Conflict("workspace writer lock poisoned"))?
            .0;
    }
    *held = true;
    Ok(LocalGuard)
}

/// The checked coordination directory, lock database path and open SQLite
/// handle for one workspace. The private-directory check (creation,
/// ownership, mode and realpath) and the connection's journal pragma run
/// once; later acquisitions reverify the directory and file identities with
/// single lstats so a replaced lock file is never silently locked.
pub(crate) struct Coordination {
    workspace: PathBuf,
    root: PathBuf,
    checked: OnceLock<Checked>,
    connection: Mutex<Option<Cached>>,
}
struct Checked {
    path: PathBuf,
    dev: u64,
    ino: u64,
}
/// One open lock-database handle pinned to the inode it was verified against.
struct Cached {
    connection: Connection,
    dev: u64,
    ino: u64,
}

/// Holds `BEGIN IMMEDIATE` on the workspace's cached connection until drop.
/// Releasing commits the empty transaction: the lock file carries mutual
/// exclusion only, never data.
pub(crate) struct WriteLock<'a> {
    slot: std::sync::MutexGuard<'a, Option<Cached>>,
    _local: LocalGuard,
}

pub(crate) fn default_root() -> Result<PathBuf> {
    if let Some(root) = std::env::var_os("XCB_COORDINATION_ROOT") {
        return Ok(PathBuf::from(root));
    }
    let home = std::env::var_os("HOME").ok_or(Error::PrivateState)?;
    Ok(PathBuf::from(home).join(".local/share/xcb-coordination"))
}

impl Coordination {
    pub(crate) fn new(workspace: &Path, root: &Path) -> Result<Self> {
        if !root.is_absolute() || root.starts_with(workspace) || workspace.starts_with(root) {
            return Err(Error::Conflict(
                "write coordination must be outside the workspace",
            ));
        }
        Ok(Self {
            workspace: workspace.to_owned(),
            root: root.to_owned(),
            checked: OnceLock::new(),
            connection: Mutex::new(None),
        })
    }
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }
    /// The lock database path inside the checked private directory. Called
    /// under the local writer so setup never races another in-process writer.
    fn database(&self) -> Result<&Path> {
        let checked = match self.checked.get() {
            Some(checked) => checked,
            None => {
                let directory = private::directory(&self.root)?;
                let metadata = fs::symlink_metadata(&directory)?;
                let workspace = self.workspace.to_str().ok_or(Error::PrivateState)?;
                let path = directory.join(format!("{}.sqlite", digest(workspace.as_bytes())));
                self.checked.get_or_init(|| Checked {
                    path,
                    dev: metadata.dev(),
                    ino: metadata.ino(),
                })
            }
        };
        let directory = checked.path.parent().ok_or(Error::PrivateState)?;
        let current = fs::symlink_metadata(directory)?;
        if !current.is_dir()
            || current.dev() != checked.dev
            || current.ino() != checked.ino
            || current.uid() != rustix::process::getuid().as_raw()
            || current.mode() & 0o077 != 0
        {
            return Err(Error::PrivateState);
        }
        Ok(&checked.path)
    }
}

fn file_identity(metadata: &fs::Metadata, dev: u64, ino: u64) -> bool {
    metadata.is_file()
        && metadata.dev() == dev
        && metadata.ino() == ino
        && metadata.uid() == rustix::process::getuid().as_raw()
        && metadata.mode() & 0o077 == 0
        && metadata.nlink() == 1
}

impl Coordination {
    /// Open and validate the lock database once: private file, correct
    /// journal mode and identity pinned for later lstat checks.
    fn open_connection(&self, path: &Path, deadline: Instant) -> Result<Cached> {
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(
                (rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::CLOEXEC).bits() as i32,
            )
            .open(path)
        {
            Ok(file) => file.sync_all()?,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(error) => return Err(error.into()),
        }
        let before = private::open_file(path, 64 * 1024)?.metadata()?;
        let connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        connection.busy_timeout(WAIT.saturating_sub(deadline.elapsed()))?;
        let journal: String =
            connection.pragma_query_value(None, "journal_mode", |row| row.get(0))?;
        if !journal.eq_ignore_ascii_case("delete") {
            return Err(Error::Conflict("workspace coordination format changed"));
        }
        let after = fs::symlink_metadata(path)?;
        if !file_identity(&after, before.dev(), before.ino()) {
            return Err(Error::PrivateState);
        }
        Ok(Cached {
            connection,
            dev: before.dev(),
            ino: before.ino(),
        })
    }
}

impl WriteLock<'_> {
    pub(crate) fn acquire(coordination: &Coordination) -> Result<WriteLock<'_>> {
        let started = Instant::now();
        let deadline = started + WAIT;
        let local = local_writer(deadline)?;
        let mut slot = coordination
            .connection
            .lock()
            .map_err(|_| Error::Conflict("workspace writer lock poisoned"))?;
        let path = coordination.database()?;
        let cached = match slot.as_mut() {
            // A cached handle is still valid only while the lock path still
            // names the inode it was opened and verified against.
            Some(cached) if file_identity(&fs::symlink_metadata(path)?, cached.dev, cached.ino) => {
                cached
            }
            Some(_) => {
                // The lock file was replaced: close the stale handle and
                // fail closed rather than locking the new inode sight unseen.
                slot.take();
                return Err(Error::PrivateState);
            }
            None => slot.insert(coordination.open_connection(path, deadline)?),
        };
        cached
            .connection
            .busy_timeout(WAIT.saturating_sub(started.elapsed()))?;
        if let Err(error) = cached.connection.execute_batch("BEGIN IMMEDIATE") {
            // Do not reuse a handle whose transaction state is unknown.
            slot.take();
            return Err(error.into());
        }
        // The file could have been replaced between the identity check and
        // the lock; a mismatched inode must release the new transaction and
        // drop the stale handle rather than lock a shadow inode.
        let after = fs::symlink_metadata(path)?;
        if !file_identity(&after, cached.dev, cached.ino) {
            let _ = cached.connection.execute_batch("ROLLBACK");
            slot.take();
            return Err(Error::PrivateState);
        }
        Ok(WriteLock {
            slot,
            _local: local,
        })
    }
}

impl Drop for WriteLock<'_> {
    fn drop(&mut self) {
        if let Some(cached) = self.slot.as_mut()
            && cached.connection.execute_batch("COMMIT").is_err()
        {
            // A release that cannot be proven makes the handle's state
            // unknown; closing it rolls back rather than reusing it.
            self.slot.take();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_writer_waiters_wake_on_release_and_time_out_when_held() {
        let first = local_writer(Instant::now() + WAIT).unwrap();
        let started = Instant::now();
        let error = local_writer(started + Duration::from_millis(50))
            .err()
            .expect("held writer times out");
        assert!(error.to_string().contains("busy"));
        assert!(started.elapsed() < WAIT);
        let (released, waiter) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            let guard = local_writer(Instant::now() + WAIT).unwrap();
            released.send(Instant::now()).unwrap();
            drop(guard);
        });
        std::thread::sleep(Duration::from_millis(20));
        let dropped = Instant::now();
        drop(first);
        let woke = waiter.recv_timeout(WAIT).unwrap();
        thread.join().unwrap();
        assert!(woke.saturating_duration_since(dropped) < Duration::from_millis(500));
    }

    #[test]
    fn coordination_checks_the_private_directory_once_and_reverifies_its_identity() {
        let temporary = tempfile::tempdir().unwrap();
        let base = temporary.path().canonicalize().unwrap();
        let workspace = base.join("work");
        fs::create_dir(&workspace).unwrap();
        let root = base.join("coordination");
        let coordination = Coordination::new(&workspace, &root).unwrap();
        assert!(!root.exists());
        let _guard = local_writer(Instant::now() + WAIT).unwrap();
        let path = coordination.database().unwrap().to_owned();
        assert!(root.is_dir());
        assert_eq!(path.parent().unwrap(), root);
        assert_eq!(coordination.database().unwrap(), path);
        fs::rename(&root, base.join("moved")).unwrap();
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(coordination.database().is_err());
        assert!(Coordination::new(&workspace, &workspace.join("locks")).is_err());
        assert!(Coordination::new(&workspace, &base).is_err());
    }
    use std::os::unix::fs::PermissionsExt;
}
