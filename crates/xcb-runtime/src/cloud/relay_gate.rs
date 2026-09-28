//! Quiesce complete relay passes without stopping local provider work.
//!
//! The legacy supervisor lock is shared by modern owners and transitions.
//! Its shared hold excludes older daemons, which take it exclusively. A
//! second, exclusive lock admits one modern supervisor. Neither lock inode
//! is replaced or removed, including when a supervisor exits.

use std::fs::{File, OpenOptions};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::{Error, Result, private};

const LEGACY_LOCK: &str = "supervisor.lock";
const OWNER_LOCK: &str = "supervisor.owner.lock";
const PUMP_LOCK: &str = "relay.pump.lock";
const POLL: Duration = Duration::from_millis(10);

struct LockFile {
    path: PathBuf,
    file: File,
}

impl LockFile {
    fn open(directory: &Path, name: &str) -> Result<Self> {
        let path = private::directory(directory)?.join(name);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(
                (rustix::fs::OFlags::NOFOLLOW
                    | rustix::fs::OFlags::NONBLOCK
                    | rustix::fs::OFlags::CLOEXEC)
                    .bits() as i32,
            )
            .open(&path)?;
        let lock = Self { path, file };
        lock.check()?;
        Ok(lock)
    }

    fn check(&self) -> Result<()> {
        check(&self.path, &self.file)
    }

    fn try_hold(self, shared: bool) -> Result<Option<HeldLock>> {
        self.check()?;
        let result = if shared {
            self.file.try_lock_shared()
        } else {
            self.file.try_lock()
        };
        match result {
            Ok(()) => {
                let held = HeldLock {
                    path: self.path,
                    lock: private::ExclusiveLock::held(self.file),
                };
                held.check()?;
                Ok(Some(held))
            }
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(error)) => Err(error.into()),
        }
    }
}

fn check(path: &Path, file: &File) -> Result<()> {
    private::check_directory(path.parent().ok_or(Error::PrivateState)?)?;
    private::check_file(file, 0)?;
    if file.metadata()?.mode() & 0o777 != 0o600 {
        return Err(Error::PrivateState);
    }
    private::same_file(path, file)
}

struct HeldLock {
    path: PathBuf,
    lock: private::ExclusiveLock,
}

impl HeldLock {
    fn check(&self) -> Result<()> {
        check(&self.path, self.lock.file())
    }
}

/// Held for the supervisor's entire lifetime. Shared legacy exclusion is
/// acquired first, so an old daemon and a modern daemon can never coexist.
pub(crate) struct SupervisorLocks {
    legacy: HeldLock,
    owner: HeldLock,
}

impl SupervisorLocks {
    pub(crate) fn check(&self) -> Result<()> {
        self.legacy.check()?;
        self.owner.check()
    }
}

pub(crate) fn supervisor_locks(root: &Path) -> Result<Option<SupervisorLocks>> {
    let directory = root.join("managed");
    let Some(legacy) = LockFile::open(&directory, LEGACY_LOCK)?.try_hold(true)? else {
        return Ok(None);
    };
    let Some(owner) = LockFile::open(&directory, OWNER_LOCK)?.try_hold(false)? else {
        return Ok(None);
    };
    let locks = SupervisorLocks { legacy, owner };
    locks.check()?;
    Ok(Some(locks))
}

/// One complete boot, command pump and projection pass. Drop only when the
/// awaited pass has returned; cancelling a pass is not a quiescence proof.
pub(crate) struct PumpGuard(HeldLock);

impl PumpGuard {
    pub(crate) fn check(&self) -> Result<()> {
        self.0.check()
    }
}

pub(crate) fn try_pump(root: &Path) -> Result<Option<PumpGuard>> {
    Ok(LockFile::open(&root.join("cloud"), PUMP_LOCK)?
        .try_hold(false)?
        .map(PumpGuard))
}

/// Blocks legacy startup even if a verified modern owner exits meanwhile.
/// When no modern owner exists, also holds its startup lock through commit.
pub(crate) struct TransitionGuard {
    legacy: HeldLock,
    owner: Option<HeldLock>,
    pump: PumpGuard,
}

impl TransitionGuard {
    /// Recheck stable lock names immediately before a custody publication.
    pub(crate) fn check(&self) -> Result<()> {
        self.legacy.check()?;
        if let Some(owner) = &self.owner {
            owner.check()?;
        }
        self.pump.check()
    }
}

/// Wait for a legacy supervisor to drain or for a complete modern relay
/// pass to finish. A timeout leaves the running pass and provider tasks alone.
pub(crate) async fn transition(root: &Path, timeout: Duration) -> Result<TransitionGuard> {
    let deadline = tokio::time::Instant::now() + timeout;
    let directory = root.join("managed");
    let legacy = loop {
        if let Some(held) = LockFile::open(&directory, LEGACY_LOCK)?.try_hold(true)? {
            break held;
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(Error::Conflict(
                "another xcb build is running; let its active tasks finish and its supervisor exit, then retry sign-in",
            ));
        }
        tokio::time::sleep(POLL).await;
    };
    // A shared legacy hold alone is not proof of a compatible modern
    // supervisor: validate its published exact executable while its owner
    // lock is held. A starting owner gets the same bounded registration wait.
    let owner = loop {
        legacy.check()?;
        let owner = LockFile::open(&directory, OWNER_LOCK)?.try_hold(false)?;
        if owner.is_some() {
            break owner;
        }
        match crate::managed_supervisor::check_relay_boundary(root) {
            Ok(()) => break None,
            Err(error) if tokio::time::Instant::now() >= deadline => return Err(error),
            Err(_) => tokio::time::sleep(POLL).await,
        }
    };
    let pump = loop {
        legacy.check()?;
        if let Some(owner) = &owner {
            owner.check()?;
        }
        if let Some(pump) = try_pump(root)? {
            break pump;
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(Error::Conflict(
                "the relay is finishing an operation; retry sign-in after it completes",
            ));
        }
        tokio::time::sleep(POLL).await;
    };
    let guard = TransitionGuard {
        legacy,
        owner,
        pump,
    };
    guard.check()?;
    Ok(guard)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    fn fixture() -> (tempfile::TempDir, PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        (directory, root)
    }

    #[tokio::test]
    async fn transition_waits_for_completed_pass_without_cancelling_it() {
        let (_directory, root) = fixture();
        let pump = try_pump(&root).unwrap().unwrap();
        let waiting_root = root.clone();
        let waiting =
            tokio::spawn(async move { transition(&waiting_root, Duration::from_secs(2)).await });
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(!waiting.is_finished());
        pump.check().unwrap();
        drop(pump);
        let guard = waiting.await.unwrap().unwrap();
        assert!(try_pump(&root).unwrap().is_none());
        assert!(supervisor_locks(&root).unwrap().is_none());
        guard.check().unwrap();
        drop(guard);
        assert!(try_pump(&root).unwrap().is_some());
    }

    #[tokio::test]
    async fn timeout_preserves_the_inflight_pass_and_releases_startup_exclusion() {
        let (_directory, root) = fixture();
        let pump = try_pump(&root).unwrap().unwrap();
        assert!(transition(&root, Duration::from_millis(20)).await.is_err());
        pump.check().unwrap();
        assert!(try_pump(&root).unwrap().is_none());
        assert!(supervisor_locks(&root).unwrap().is_some());
    }

    #[tokio::test]
    async fn legacy_owner_must_exit_before_transition_and_cannot_restart_during_it() {
        let (_directory, root) = fixture();
        let managed = root.join("managed");
        let old = LockFile::open(&managed, LEGACY_LOCK)
            .unwrap()
            .try_hold(false)
            .unwrap()
            .unwrap();
        assert!(supervisor_locks(&root).unwrap().is_none());
        assert!(transition(&root, Duration::from_millis(20)).await.is_err());
        old.check().unwrap();
        drop(old);
        let transition = transition(&root, Duration::from_secs(1)).await.unwrap();
        assert!(
            LockFile::open(&managed, LEGACY_LOCK)
                .unwrap()
                .try_hold(false)
                .unwrap()
                .is_none()
        );
        transition.check().unwrap();
    }

    #[tokio::test]
    async fn modern_owner_exit_does_not_admit_a_legacy_start_during_transition() {
        let (_directory, root) = fixture();
        let owner = supervisor_locks(&root).unwrap().unwrap();
        let _identity = crate::managed_supervisor::SupervisorIdentity::register(&root).unwrap();
        assert!(supervisor_locks(&root).unwrap().is_none());
        let transition = transition(&root, Duration::from_secs(1)).await.unwrap();
        drop(owner);
        assert!(
            LockFile::open(&root.join("managed"), LEGACY_LOCK)
                .unwrap()
                .try_hold(false)
                .unwrap()
                .is_none()
        );
        // A modern replacement may dispatch local tasks, but its relay pass
        // remains excluded until the credential transition completes.
        let replacement = supervisor_locks(&root).unwrap().unwrap();
        assert!(try_pump(&root).unwrap().is_none());
        replacement.check().unwrap();
        transition.check().unwrap();
    }

    #[tokio::test]
    async fn unknown_shared_owner_is_not_assumed_compatible() {
        let (_directory, root) = fixture();
        let owner = supervisor_locks(&root).unwrap().unwrap();
        assert!(transition(&root, Duration::from_millis(20)).await.is_err());
        owner.check().unwrap();
    }

    #[tokio::test]
    async fn unsafe_or_replaced_lock_names_are_rejected() {
        let (_directory, root) = fixture();
        let guard = transition(&root, Duration::from_secs(1)).await.unwrap();
        let pump_path = root.join("cloud").join(PUMP_LOCK);
        std::fs::rename(&pump_path, root.join("cloud/old-pump")).unwrap();
        private::create(&pump_path, &[]).unwrap();
        assert!(guard.check().is_err());
        drop(guard);
        std::fs::set_permissions(&pump_path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(try_pump(&root).is_err());
        std::fs::remove_file(&pump_path).unwrap();
        symlink(root.join("cloud/old-pump"), &pump_path).unwrap();
        assert!(try_pump(&root).is_err());
    }
}
