//! Private per-terminal input snapshots. This module never submits or acknowledges a turn.
use crate::composer::{MAX_HISTORY, MAX_HISTORY_BYTES, MAX_INPUT};
use rustix::fs::{FlockOperation, Mode, OFlags};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    fs::{self, DirBuilder, File},
    io::{self, Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt},
    path::{Path, PathBuf},
    time::SystemTime,
};
use xcb_core::{FileIdentity, session::Attachment};

pub const MAX_RECOVERY_ENTRIES: usize = 128;
const MAX_JOURNAL_BYTES: u64 = 80 * 1024 * 1024;
const MAX_DIRECTORY_BYTES: u64 = 128 * 1024 * 1024;
pub const MAX_OTHER_INPUTS: usize = 128;
const MAX_DIRECTORY_ENTRIES: usize = 2048;

/// A held flock that releases its open file description before the descriptor
/// closes. A child spawned while the lock is held can carry an inherited
/// reference to the description through its pre-exec window, so a close-only
/// release can leave the lock looking held after this process let go; an
/// explicit `LOCK_UN` cannot be kept alive by that inherited reference.
struct HeldLock(File);

impl Drop for HeldLock {
    fn drop(&mut self) {
        let _ = rustix::fs::flock(&self.0, FlockOperation::Unlock);
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RecoverySnapshot {
    pub text: String,
    /// Newest first, complete prompt bodies.
    pub history: Vec<String>,
    /// Caller-owned workspace/session identity; never used as a filesystem path.
    pub context: String,
    pub attachments: Vec<Attachment>,
    /// A submitted operation may have succeeded before the terminal lost its response.
    pub uncertain_pending: bool,
    pub task: Option<String>,
    pub operation: Option<String>,
    /// Other context drafts and outstanding requests. Each retains its original identity.
    pub other_inputs: Vec<RecoveryInput>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RecoveryInput {
    pub context: String,
    pub text: String,
    pub attachments: Vec<Attachment>,
    pub uncertain_pending: bool,
    pub operation: Option<String>,
    pub task: Option<String>,
}
#[derive(Clone, Debug)]
pub struct RecoveryEntry {
    pub id: String,
    pub saved_at: SystemTime,
    pub live: bool,
    pub kind: RecoveryEntryKind,
    path: PathBuf,
    identity: FileIdentity,
    directory_identity: (u64, u64),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryEntryKind {
    Snapshot,
    Staged,
    Reservation,
}
impl RecoveryEntry {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

pub struct RecoveryJournal {
    directory: Directory,
    id: String,
    lock: HeldLock,
    lock_identity: FileIdentity,
    snapshot_identity: Option<FileIdentity>,
    last_save_complete: bool,
}
impl RecoveryJournal {
    /// `trusted_dir` is a dedicated state subdirectory, never a project/provider path.
    /// Only its final component is created, with mode 0700; existing permissive directories fail.
    pub fn new(trusted_dir: &Path) -> io::Result<Self> {
        match DirBuilder::new().mode(0o700).create(trusted_dir) {
            Ok(()) => (),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => (),
            Err(error) => return Err(error),
        }
        let directory = Directory::open(trusted_dir)?;
        // Serialize admission across terminals, including instances that have not saved yet.
        // This stable lock is never unlinked: replacing it would split the locking domain.
        let _admission = directory.admission()?;
        ensure_capacity(&directory)?;
        let id = uuid::Uuid::new_v4().to_string();
        let lock = directory.create(&format!("{id}.lock"))?;
        rustix::fs::flock(&lock, FlockOperation::NonBlockingLockExclusive)?;
        let lock = HeldLock(lock);
        let lock_identity = FileIdentity::of(&lock.0.metadata()?);
        Ok(Self {
            directory,
            id,
            lock,
            lock_identity,
            snapshot_identity: None,
            last_save_complete: false,
        })
    }
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn path(&self) -> PathBuf {
        self.directory.path.join(format!("{}.json", self.id))
    }
    /// Revalidate the exact last saved object before treating unchanged RAM as durable.
    pub fn verify_current(&self) -> io::Result<()> {
        self.directory.check()?;
        if self.snapshot_identity.is_none() || !self.last_save_complete {
            return Err(invalid("No input snapshot has been saved"));
        }
        self.check_current()
    }
    pub fn save(&mut self, snapshot: &RecoverySnapshot) -> io::Result<()> {
        self.last_save_complete = false;
        validate_snapshot(snapshot)?;
        self.directory.check()?;
        self.check_current()?;
        let value = json!({
            "version": 2, "instance": self.id, "text": snapshot.text,
            "history": snapshot.history, "context": snapshot.context,
            "attachments": snapshot.attachments, "uncertain_pending": snapshot.uncertain_pending,
            "task": snapshot.task,
            "operation": snapshot.operation,
            "other_inputs": snapshot.other_inputs.iter().map(|input| json!({
                "context": input.context, "text": input.text, "attachments": input.attachments,
                "uncertain_pending": input.uncertain_pending, "operation": input.operation, "task": input.task,
            })).collect::<Vec<_>>(),
        });
        let bytes = serde_json::to_vec(&value).map_err(|_| invalid("Invalid input snapshot"))?;
        if bytes.len() as u64 > MAX_JOURNAL_BYTES {
            return Err(invalid("Input snapshot exceeds its limit"));
        }
        let _admission = self.directory.admission()?;
        ensure_disk_capacity(&self.directory, bytes.len() as u64)?;
        let temporary = format!("{}.{}.tmp", self.id, uuid::Uuid::new_v4());
        let result = (|| {
            let mut staged = self.directory.create(&temporary)?;
            staged.write_all(&bytes)?;
            staged.sync_all()?;
            self.directory.check()?;
            self.check_current()?;
            rustix::fs::renameat(
                &self.directory.file,
                &temporary,
                &self.directory.file,
                format!("{}.json", self.id),
            )?;
            self.snapshot_identity = Some(FileIdentity::of(&staged.metadata()?));
            self.directory.file.sync_all()
        })();
        // The unpredictable staging name belongs to this save only; never delete a sibling journal.
        let _ = rustix::fs::unlinkat(
            &self.directory.file,
            &temporary,
            rustix::fs::AtFlags::empty(),
        );
        self.last_save_complete = result.is_ok();
        result
    }
    /// Remove this instance's snapshot only. Save an empty draft to retain prompt history.
    pub fn clear(&mut self) -> io::Result<()> {
        self.last_save_complete = false;
        self.directory.check()?;
        self.check_current()?;
        if self.snapshot_identity.is_some() {
            rustix::fs::unlinkat(
                &self.directory.file,
                format!("{}.json", self.id),
                rustix::fs::AtFlags::empty(),
            )?;
            self.snapshot_identity = None;
            self.directory.file.sync_all()?;
        }
        Ok(())
    }
    fn check_current(&self) -> io::Result<()> {
        match self
            .directory
            .open_file(&format!("{}.json", self.id), MAX_JOURNAL_BYTES)
        {
            Ok(file) if self.snapshot_identity == Some(FileIdentity::of(&file.metadata()?)) => {
                Ok(())
            }
            Err(error)
                if error.kind() == io::ErrorKind::NotFound && self.snapshot_identity.is_none() =>
            {
                Ok(())
            }
            _ => Err(invalid("Input snapshot changed")),
        }
    }
    pub fn candidates(trusted_dir: &Path) -> io::Result<Vec<RecoveryEntry>> {
        candidates(trusted_dir)
    }
    pub fn read(entry: &RecoveryEntry) -> io::Result<RecoverySnapshot> {
        read(entry)
    }
    pub fn remove(entry: &RecoveryEntry) -> io::Result<()> {
        remove(entry)
    }
}
impl Drop for RecoveryJournal {
    fn drop(&mut self) {
        // The file stays locked until after this exact-name cleanup. Draft/history remain on disk.
        if self.directory.check().is_ok()
            && self
                .directory
                .open_file(&format!("{}.lock", self.id), 0)
                .is_ok_and(|file| {
                    file.metadata()
                        .is_ok_and(|meta| FileIdentity::of(&meta) == self.lock_identity)
                })
        {
            let _ = rustix::fs::unlinkat(
                &self.directory.file,
                format!("{}.lock", self.id),
                rustix::fs::AtFlags::empty(),
            );
        }
        let _ = rustix::fs::flock(&self.lock.0, FlockOperation::Unlock);
    }
}

pub fn candidates(trusted_dir: &Path) -> io::Result<Vec<RecoveryEntry>> {
    let directory = match Directory::open(trusted_dir) {
        Ok(directory) => directory,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut entries = Vec::new();
    for (count, entry) in fs::read_dir(&directory.path)?.enumerate() {
        if count >= MAX_DIRECTORY_ENTRIES {
            return Err(invalid("Too many recovery files to list"));
        }
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some((id, kind)) = recovery_file_name(name) else {
            continue;
        };
        // Metadata-only discovery also exposes oversized owned crash artifacts for explicit discard.
        let Ok(file) = directory.open_file(name, u64::MAX) else {
            continue;
        };
        let metadata = file.metadata()?;
        let live = lock_is_live(&directory, id)?;
        entries.push(RecoveryEntry {
            id: id.to_owned(),
            saved_at: metadata.modified()?,
            live,
            kind,
            path: directory.path.join(name),
            identity: FileIdentity::of(&metadata),
            directory_identity: directory.identity,
        });
    }
    let content_ids: HashSet<_> = entries
        .iter()
        .filter(|entry| entry.kind != RecoveryEntryKind::Reservation)
        .map(|entry| entry.id.clone())
        .collect();
    entries.retain(|entry| {
        entry.kind != RecoveryEntryKind::Reservation || !content_ids.contains(&entry.id)
    });
    directory.check()?;
    entries.sort_by(|left, right| {
        right
            .saved_at
            .cmp(&left.saved_at)
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(entries)
}

pub fn read(entry: &RecoveryEntry) -> io::Result<RecoverySnapshot> {
    let directory = entry_directory(entry)?;
    if entry.kind == RecoveryEntryKind::Reservation {
        return Err(invalid("This terminal reservation has no saved input"));
    }
    let file = directory.open_file(entry_name(entry)?, MAX_JOURNAL_BYTES)?;
    if FileIdentity::of(&file.metadata()?) != entry.identity {
        return Err(invalid("Input snapshot changed; reopen recovery"));
    }
    let mut bytes = Vec::new();
    (&file)
        .take(MAX_JOURNAL_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_JOURNAL_BYTES
        || FileIdentity::of(&file.metadata()?) != entry.identity
    {
        return Err(invalid("Input snapshot changed; reopen recovery"));
    }
    directory.check()?;
    let mut object = serde_json::from_slice::<Value>(&bytes)
        .map_err(|_| invalid("Invalid input snapshot"))?
        .as_object()
        .cloned()
        .ok_or_else(|| invalid("Invalid input snapshot"))?;
    let version = object.remove("version");
    if !matches!(version, Some(Value::Number(ref number)) if number.as_u64().is_some_and(|version| version == 1 || version == 2))
        || object.remove("instance") != Some(json!(entry.id))
    {
        return Err(invalid("Unsupported input snapshot"));
    }
    let text = take_string(&mut object, "text")?;
    let context = take_string(&mut object, "context")?;
    let history = serde_json::from_value(
        object
            .remove("history")
            .ok_or_else(|| invalid("Invalid history"))?,
    )
    .map_err(|_| invalid("Invalid history"))?;
    let attachments = serde_json::from_value(
        object
            .remove("attachments")
            .ok_or_else(|| invalid("Invalid attachments"))?,
    )
    .map_err(|_| invalid("Invalid attachments"))?;
    let uncertain_pending = object
        .remove("uncertain_pending")
        .and_then(|value| value.as_bool())
        .ok_or_else(|| invalid("Invalid pending status"))?;
    let task = match object.remove("task") {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) if xcb_core::Id::new(value.clone()).is_ok() => Some(value),
        _ => return Err(invalid("Invalid task identity")),
    };
    let operation = match object.remove("operation") {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) if xcb_core::Id::new(value.clone()).is_ok() => Some(value),
        _ => return Err(invalid("Invalid request identity")),
    };
    let other_inputs = match object.remove("other_inputs") {
        Some(Value::Array(inputs)) if inputs.len() <= MAX_OTHER_INPUTS => inputs
            .into_iter()
            .map(parse_input)
            .collect::<io::Result<Vec<_>>>()?,
        None if version == Some(json!(1)) => Vec::new(),
        _ => return Err(invalid("Invalid context drafts")),
    };
    if !object.is_empty() {
        return Err(invalid("Unknown input snapshot field"));
    }
    let snapshot = RecoverySnapshot {
        text,
        history,
        context,
        attachments,
        uncertain_pending,
        task,
        operation,
        other_inputs,
    };
    validate_snapshot(&snapshot)?;
    Ok(snapshot)
}

/// Remove an explicitly handled, unchanged, inactive candidate. A live terminal always wins.
pub fn remove(entry: &RecoveryEntry) -> io::Result<()> {
    let directory = entry_directory(entry)?;
    let _admission = directory.admission()?;
    let current = directory.open_file(entry_name(entry)?, u64::MAX)?;
    if FileIdentity::of(&current.metadata()?) != entry.identity {
        return Err(invalid("Input snapshot changed; reopen recovery"));
    }
    let lock_name = format!("{}.lock", entry.id);
    let lock = match directory.open_file(&lock_name, u64::MAX) {
        Ok(lock) => lock,
        Err(error) if error.kind() == io::ErrorKind::NotFound => directory.create(&lock_name)?,
        Err(error) => return Err(error),
    };
    rustix::fs::flock(&lock, FlockOperation::NonBlockingLockExclusive)
        .map_err(|_| io::Error::new(io::ErrorKind::WouldBlock, "This terminal is still active"))?;
    let lock = HeldLock(lock);
    directory.check()?;
    rustix::fs::unlinkat(
        &directory.file,
        entry_name(entry)?,
        rustix::fs::AtFlags::empty(),
    )?;
    // Remove only the lock we opened, never a replacement.
    if directory
        .open_file(&lock_name, u64::MAX)
        .is_ok_and(|other| {
            other.metadata().is_ok_and(|meta| {
                lock.0
                    .metadata()
                    .is_ok_and(|ours| FileIdentity::of(&meta) == FileIdentity::of(&ours))
            })
        })
    {
        rustix::fs::unlinkat(&directory.file, &lock_name, rustix::fs::AtFlags::empty())?;
    }
    directory.file.sync_all()
}

fn entry_directory(entry: &RecoveryEntry) -> io::Result<Directory> {
    if recovery_file_name(entry_name(entry)?) != Some((entry.id.as_str(), entry.kind)) {
        return Err(invalid("Invalid recovery identity"));
    }
    let directory = Directory::open(
        entry
            .path
            .parent()
            .ok_or_else(|| invalid("Invalid recovery path"))?,
    )?;
    if directory.identity != entry.directory_identity {
        return Err(invalid("Recovery directory changed"));
    }
    Ok(directory)
}
fn lock_is_live(directory: &Directory, id: &str) -> io::Result<bool> {
    let lock = match directory.open_file(&format!("{id}.lock"), u64::MAX) {
        Ok(lock) => lock,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    match rustix::fs::flock(&lock, FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => {
            // A probe's close-only release could stay held by a spawned
            // child's inherited descriptor; unlock so the next probe reads
            // the true state.
            let _ = rustix::fs::flock(&lock, FlockOperation::Unlock);
            Ok(false)
        }
        Err(rustix::io::Errno::WOULDBLOCK) => Ok(true),
        Err(error) => Err(error.into()),
    }
}
fn entry_name(entry: &RecoveryEntry) -> io::Result<&str> {
    entry
        .path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| invalid("Invalid recovery filename"))
}
fn recovery_file_name(name: &str) -> Option<(&str, RecoveryEntryKind)> {
    let (id, suffix) = name.split_once('.')?;
    if id.len() != 36 || uuid::Uuid::parse_str(id).is_err() {
        return None;
    }
    let kind = match suffix {
        "json" => RecoveryEntryKind::Snapshot,
        "lock" => RecoveryEntryKind::Reservation,
        _ => {
            let temporary = suffix.strip_suffix(".tmp")?;
            if temporary.len() != 36 || uuid::Uuid::parse_str(temporary).is_err() {
                return None;
            }
            RecoveryEntryKind::Staged
        }
    };
    Some((id, kind))
}
fn ensure_capacity(directory: &Directory) -> io::Result<()> {
    let mut instances = HashSet::new();
    for (count, entry) in fs::read_dir(&directory.path)?.enumerate() {
        if count >= MAX_DIRECTORY_ENTRIES {
            return Err(invalid("Too many recovery files to open another journal"));
        }
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if let Some((id, _)) = recovery_file_name(name) {
            instances.insert(id.to_owned());
        }
        if instances.len() >= MAX_RECOVERY_ENTRIES {
            return Err(invalid(
                "Recovery storage has 128 journals; recover or discard an inactive draft before opening another",
            ));
        }
    }
    directory.check()
}
fn take_string(object: &mut serde_json::Map<String, Value>, key: &str) -> io::Result<String> {
    object
        .remove(key)
        .and_then(|value| value.as_str().map(str::to_owned))
        .ok_or_else(|| invalid("Invalid input snapshot"))
}
fn parse_input(value: Value) -> io::Result<RecoveryInput> {
    let mut object = value
        .as_object()
        .cloned()
        .ok_or_else(|| invalid("Invalid context draft"))?;
    let context = take_string(&mut object, "context")?;
    let text = take_string(&mut object, "text")?;
    let attachments = serde_json::from_value(
        object
            .remove("attachments")
            .ok_or_else(|| invalid("Invalid attachments"))?,
    )
    .map_err(|_| invalid("Invalid attachments"))?;
    let uncertain_pending = object
        .remove("uncertain_pending")
        .and_then(|value| value.as_bool())
        .ok_or_else(|| invalid("Invalid pending status"))?;
    let mut optional_id = |key| -> io::Result<Option<String>> {
        match object.remove(key) {
            Some(Value::Null) => Ok(None),
            Some(Value::String(value)) if xcb_core::Id::new(value.clone()).is_ok() => {
                Ok(Some(value))
            }
            _ => Err(invalid("Invalid request identity")),
        }
    };
    let operation = optional_id("operation")?;
    let task = optional_id("task")?;
    if !object.is_empty() {
        return Err(invalid("Unknown context draft field"));
    }
    Ok(RecoveryInput {
        context,
        text,
        attachments,
        uncertain_pending,
        operation,
        task,
    })
}
fn validate_snapshot(snapshot: &RecoverySnapshot) -> io::Result<()> {
    if snapshot.text.len() > MAX_INPUT
        || snapshot.context.is_empty()
        || snapshot.context.len() > 4096
        || snapshot.context.chars().any(char::is_control)
        || snapshot.history.len() > MAX_HISTORY
        || snapshot.history.iter().any(|text| text.len() > MAX_INPUT)
        || snapshot.history.iter().map(String::len).sum::<usize>() > MAX_HISTORY_BYTES
        || snapshot.attachments.len() > 8
        || snapshot
            .attachments
            .iter()
            .any(|item| item.validate().is_err())
        || snapshot.other_inputs.len() > MAX_OTHER_INPUTS
        || snapshot
            .task
            .as_ref()
            .is_some_and(|id| xcb_core::Id::new(id.clone()).is_err())
        || snapshot
            .operation
            .as_ref()
            .is_some_and(|id| xcb_core::Id::new(id.clone()).is_err())
        || snapshot.other_inputs.iter().any(|input| {
            input.text.len() > MAX_INPUT
                || input.context.is_empty()
                || input.context.len() > 4096
                || input.context.chars().any(char::is_control)
                || input.attachments.len() > 8
                || input
                    .attachments
                    .iter()
                    .any(|item| item.validate().is_err())
                || [&input.operation, &input.task]
                    .into_iter()
                    .flatten()
                    .any(|id| xcb_core::Id::new(id.clone()).is_err())
        })
    {
        return Err(invalid(
            "Input snapshot exceeds its limits or contains invalid metadata",
        ));
    }
    Ok(())
}
fn ensure_disk_capacity(directory: &Directory, incoming: u64) -> io::Result<()> {
    let mut bytes = 0_u64;
    for (count, entry) in fs::read_dir(&directory.path)?.enumerate() {
        if count >= MAX_DIRECTORY_ENTRIES {
            return Err(invalid("Too many recovery files to save input"));
        }
        let meta = entry?.metadata()?;
        bytes = bytes.saturating_add(meta.len());
    }
    // Atomic replacement briefly retains both old and staged snapshots.
    if bytes.saturating_add(incoming) > MAX_DIRECTORY_BYTES {
        return Err(invalid(
            "Recovery storage needs room for an atomic save within 128 MiB; recover or discard inactive drafts first",
        ));
    }
    Ok(())
}
fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

struct Directory {
    path: PathBuf,
    file: File,
    identity: (u64, u64),
}
impl Directory {
    fn admission(&self) -> io::Result<HeldLock> {
        let lock = match self.create(".admission.lock") {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                self.open_file(".admission.lock", 0)?
            }
            Err(error) => return Err(error),
        };
        rustix::fs::flock(&lock, FlockOperation::LockExclusive)?;
        Ok(HeldLock(lock))
    }
    fn open(path: &Path) -> io::Result<Self> {
        let before = fs::symlink_metadata(path)?;
        if !before.is_dir()
            || before.mode() & 0o077 != 0
            || before.uid() != rustix::process::geteuid().as_raw()
        {
            return Err(invalid(
                "Recovery directory must be private and owned by this user",
            ));
        }
        let file = File::from(rustix::fs::open(
            path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )?);
        let opened = file.metadata()?;
        if (before.dev(), before.ino()) != (opened.dev(), opened.ino()) {
            return Err(invalid("Recovery directory changed"));
        }
        Ok(Self {
            path: path.canonicalize()?,
            file,
            identity: (opened.dev(), opened.ino()),
        })
    }
    fn check(&self) -> io::Result<()> {
        let meta = fs::symlink_metadata(&self.path)?;
        if !meta.is_dir()
            || (meta.dev(), meta.ino()) != self.identity
            || meta.mode() & 0o077 != 0
            || meta.uid() != rustix::process::geteuid().as_raw()
        {
            return Err(invalid("Recovery directory changed"));
        }
        Ok(())
    }
    fn create(&self, name: &str) -> io::Result<File> {
        self.check()?;
        Ok(File::from(rustix::fs::openat(
            &self.file,
            name,
            OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )?))
    }
    fn open_file(&self, name: &str, max: u64) -> io::Result<File> {
        self.check()?;
        let file = File::from(rustix::fs::openat(
            &self.file,
            name,
            OFlags::RDONLY | OFlags::NONBLOCK | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )?);
        let meta = file.metadata()?;
        if !meta.is_file()
            || meta.uid() != rustix::process::geteuid().as_raw()
            || meta.mode() & 0o077 != 0
            || meta.nlink() != 1
            || meta.len() > max
        {
            return Err(invalid(
                "Recovery file must be a private regular file within its size limit",
            ));
        }
        Ok(file)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    struct TestDirectory(PathBuf);
    impl TestDirectory {
        fn new() -> Self {
            Self(std::env::temp_dir().join(format!("xcb-recovery-test-{}", uuid::Uuid::new_v4())))
        }
    }
    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn snapshot(text: &str) -> RecoverySnapshot {
        RecoverySnapshot {
            text: text.into(),
            history: vec!["a complete\nold prompt".into()],
            context: "workspace/session".into(),
            ..Default::default()
        }
    }
    #[test]
    fn concurrent_terminals_keep_independent_private_snapshots() {
        let dir = TestDirectory::new();
        let mut first = RecoveryJournal::new(&dir.0).unwrap();
        let mut second = RecoveryJournal::new(&dir.0).unwrap();
        first.save(&snapshot("first")).unwrap();
        second.save(&snapshot("second")).unwrap();
        let entries = candidates(&dir.0).unwrap();
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|entry| entry.live));
        assert!(entries.iter().all(|entry| remove(entry).is_err()));
        assert_eq!(
            fs::metadata(first.path()).unwrap().permissions().mode() & 0o777,
            0o600
        );
        drop(first);
        let entry = candidates(&dir.0)
            .unwrap()
            .into_iter()
            .find(|entry| !entry.live)
            .unwrap();
        assert_eq!(read(&entry).unwrap().text, "first");
        remove(&entry).unwrap();
        assert_eq!(candidates(&dir.0).unwrap().len(), 1);
        assert_eq!(
            read(&candidates(&dir.0).unwrap()[0]).unwrap().text,
            "second"
        );
    }
    #[test]
    fn stale_and_oversized_snapshots_fail_without_losing_previous_draft() {
        let dir = TestDirectory::new();
        let mut journal = RecoveryJournal::new(&dir.0).unwrap();
        journal.save(&snapshot("before")).unwrap();
        let old = candidates(&dir.0).unwrap().remove(0);
        journal.save(&snapshot("after")).unwrap();
        assert!(read(&old).is_err());
        assert!(journal.save(&snapshot(&"x".repeat(MAX_INPUT + 1))).is_err());
        assert_eq!(read(&candidates(&dir.0).unwrap()[0]).unwrap().text, "after");
        let mut pending = snapshot("");
        pending.uncertain_pending = true;
        journal.save(&pending).unwrap();
        let restored = read(&candidates(&dir.0).unwrap()[0]).unwrap();
        assert!(restored.uncertain_pending);
        assert_eq!(restored.history, pending.history);
    }
    #[test]
    fn permissive_directory_and_symlink_snapshot_are_rejected() {
        let dir = TestDirectory::new();
        fs::create_dir(&dir.0).unwrap();
        fs::set_permissions(&dir.0, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(RecoveryJournal::new(&dir.0).is_err());
        fs::set_permissions(&dir.0, fs::Permissions::from_mode(0o700)).unwrap();
        let mut journal = RecoveryJournal::new(&dir.0).unwrap();
        journal.save(&snapshot("safe")).unwrap();
        let entry = candidates(&dir.0).unwrap().remove(0);
        fs::remove_file(journal.path()).unwrap();
        std::os::unix::fs::symlink("/etc/passwd", journal.path()).unwrap();
        assert!(read(&entry).is_err());
        assert!(journal.save(&snapshot("new")).is_err());
    }
    #[test]
    fn admission_cap_reserves_unsaved_live_instances_without_deleting_drafts() {
        let dir = TestDirectory::new();
        let mut journals = Vec::new();
        for _ in 0..MAX_RECOVERY_ENTRIES {
            journals.push(RecoveryJournal::new(&dir.0).unwrap());
        }
        journals[0].save(&snapshot("keep this draft")).unwrap();
        assert!(RecoveryJournal::new(&dir.0).is_err());
        assert_eq!(
            read(&candidates(&dir.0).unwrap()[0]).unwrap().text,
            "keep this draft"
        );
        journals.pop();
        let replacement = RecoveryJournal::new(&dir.0).unwrap();
        assert!(RecoveryJournal::new(&dir.0).is_err());
        drop(replacement);
    }
    #[test]
    fn directory_byte_cap_preserves_last_complete_snapshot() {
        let dir = TestDirectory::new();
        let mut journal = RecoveryJournal::new(&dir.0).unwrap();
        journal.save(&snapshot("last complete draft")).unwrap();
        let sparse = File::create(dir.0.join("capacity-fixture")).unwrap();
        let current_bytes = fs::metadata(journal.path()).unwrap().len();
        sparse
            .set_len(MAX_DIRECTORY_BYTES - current_bytes - 1)
            .unwrap();
        // Replacement would fit after rename; its required temporary copy does not.
        assert!(journal.save(&snapshot("next complete draft")).is_err());
        assert_eq!(
            read(&candidates(&dir.0).unwrap()[0]).unwrap().text,
            "last complete draft"
        );
    }
    #[test]
    fn simultaneous_discard_never_leaves_an_orphan_reservation() {
        let dir = TestDirectory::new();
        let mut journal = RecoveryJournal::new(&dir.0).unwrap();
        journal.save(&snapshot("explicitly discarded")).unwrap();
        drop(journal);
        let entry = candidates(&dir.0).unwrap().remove(0);
        let barrier = std::sync::Barrier::new(2);
        let outcomes = std::thread::scope(|scope| {
            let first = scope.spawn(|| {
                barrier.wait();
                remove(&entry).is_ok()
            });
            let second = scope.spawn(|| {
                barrier.wait();
                remove(&entry).is_ok()
            });
            [first.join().unwrap(), second.join().unwrap()]
        });
        assert_eq!(outcomes.iter().filter(|success| **success).count(), 1);
        assert!(!entry.path.with_extension("lock").exists());
        assert!(candidates(&dir.0).unwrap().is_empty());
    }
    #[test]
    fn interrupted_staging_is_visible_readable_and_discarded_only_explicitly() {
        let dir = TestDirectory::new();
        let mut journal = RecoveryJournal::new(&dir.0).unwrap();
        journal
            .save(&snapshot("staged draft must survive"))
            .unwrap();
        let id = journal.id().to_owned();
        let published = journal.path();
        drop(journal);
        let staged = dir.0.join(format!("{id}.{}.tmp", uuid::Uuid::new_v4()));
        fs::rename(&published, &staged).unwrap();
        let directory = Directory::open(&dir.0).unwrap();
        drop(directory.create(&format!("{id}.lock")).unwrap());
        let entries = candidates(&dir.0).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].kind, RecoveryEntryKind::Staged);
        assert!(!entries[0].live);
        assert_eq!(read(&entries[0]).unwrap().text, "staged draft must survive");
        assert!(staged.exists());
        remove(&entries[0]).unwrap();
        assert!(!staged.exists());
        assert!(candidates(&dir.0).unwrap().is_empty());
    }
    #[test]
    fn orphan_reservations_are_visible_and_explicit_discard_reopens_capacity() {
        let dir = TestDirectory::new();
        let temporary = RecoveryJournal::new(&dir.0).unwrap();
        drop(temporary);
        let directory = Directory::open(&dir.0).unwrap();
        for _ in 0..MAX_RECOVERY_ENTRIES {
            drop(
                directory
                    .create(&format!("{}.lock", uuid::Uuid::new_v4()))
                    .unwrap(),
            );
        }
        assert!(RecoveryJournal::new(&dir.0).is_err());
        let entries = candidates(&dir.0).unwrap();
        assert_eq!(entries.len(), MAX_RECOVERY_ENTRIES);
        assert!(
            entries
                .iter()
                .all(|entry| !entry.live && entry.kind == RecoveryEntryKind::Reservation)
        );
        assert!(read(&entries[0]).is_err());
        remove(&entries[0]).unwrap();
        assert!(RecoveryJournal::new(&dir.0).is_ok());
    }
    #[test]
    fn incomplete_staging_and_live_reservations_are_never_silently_removed() {
        let dir = TestDirectory::new();
        let active = RecoveryJournal::new(&dir.0).unwrap();
        let live = candidates(&dir.0).unwrap().remove(0);
        assert!(live.live);
        assert!(remove(&live).is_err());
        let directory = Directory::open(&dir.0).unwrap();
        let name = format!("{}.{}.tmp", uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let mut file = directory.create(&name).unwrap();
        file.write_all(b"{\"text\":\"partial draft").unwrap();
        file.sync_all().unwrap();
        drop(file);
        let staged = candidates(&dir.0)
            .unwrap()
            .into_iter()
            .find(|entry| entry.kind == RecoveryEntryKind::Staged)
            .unwrap();
        assert!(read(&staged).is_err());
        assert_eq!(
            fs::read(staged.path()).unwrap(),
            b"{\"text\":\"partial draft"
        );
        remove(&staged).unwrap();
        assert_eq!(candidates(&dir.0).unwrap().len(), 1);
        drop(active);
    }
}
