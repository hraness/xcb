//! Bounded command snapshots and revision-checked publication. Guest paths
//! never select a host root; every operation is rooted in Workspace descriptors.
use super::{Workspace, components, git_snapshot, io};
use crate::{Error, Result, coordination, digest, private};
use base64::{Engine, engine::general_purpose::STANDARD};
use rustix::fs::{Dir, FileType, Mode, OFlags};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{Read, Write},
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::Path,
};
use xcb_core::policy::EffectState;

pub const SNAPSHOT_FILE_LIMIT: usize = 2 * 1024 * 1024;
pub const SNAPSHOT_BYTE_LIMIT: usize = 64 * 1024 * 1024;
pub const SNAPSHOT_ENTRY_LIMIT: usize = 8192;
pub const CHANGE_BYTE_LIMIT: usize = 16 * 1024 * 1024;
const DEPTH_LIMIT: usize = 64;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SnapshotFile {
    pub path: String,
    pub base64: String,
    pub sha256: String,
    pub executable: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SnapshotDocument {
    pub version: u32,
    pub workspace_id: String,
    pub files: Vec<SnapshotFile>,
    pub directories: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git: Option<git_snapshot::GitSnapshot>,
}
pub struct CommandSnapshot {
    pub document: SnapshotDocument,
    pub excluded: Vec<String>,
    pub git_unavailable: Option<&'static str>,
    originals: BTreeMap<String, (String, bool)>,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommandChange {
    pub path: String,
    pub base64: Option<String>,
    pub sha256: Option<String>,
    pub executable: bool,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommandChanges {
    pub version: u32,
    pub workspace_id: String,
    pub changes: Vec<CommandChange>,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandPublication {
    pub written: usize,
    pub removed: usize,
    pub directories_created: usize,
}

/// Dependencies, build products, repository controls and conventional secret
/// files are not inputs or publication targets for an isolated command.
pub fn command_excluded(path: &str) -> bool {
    path.split('/').any(|name| {
        matches!(
            name,
            ".git"
                | "node_modules"
                | "target"
                | "dist"
                | "build"
                | ".next"
                | ".venv"
                | "venv"
                | "__pycache__"
                | ".cache"
                | ".ssh"
                | ".aws"
                | ".gnupg"
                | ".codex"
                | ".claude"
                | ".devin"
                | ".npmrc"
                | ".pypirc"
                | ".netrc"
                | ".DS_Store"
        ) || name == ".env"
            || (name.starts_with(".env.")
                && !matches!(name, ".env.example" | ".env.sample" | ".env.template"))
            || name.starts_with(".xcb-")
    })
}
fn stamp(metadata: &std::fs::Metadata) -> (u64, u64, u64, u32, i64, i64, i64, i64) {
    (
        metadata.dev(),
        metadata.ino(),
        metadata.len(),
        metadata.mode(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec(),
    )
}
fn read_binary(parent: &File, name: &std::ffi::OsStr) -> Result<(Vec<u8>, u32)> {
    let mut file = File::from(
        rustix::fs::openat(
            parent,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(io)?,
    );
    let before = file.metadata()?;
    if !before.is_file() || before.nlink() != 1 || before.len() > SNAPSHOT_FILE_LIMIT as u64 {
        return Err(Error::Unavailable(
            "command snapshot requires bounded regular single-link files",
        ));
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(SNAPSHOT_FILE_LIMIT as u64 + 1)
        .read_to_end(&mut bytes)?;
    let after = file.metadata()?;
    let named =
        rustix::fs::statat(parent, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW).map_err(io)?;
    let opened = rustix::fs::fstat(&file).map_err(io)?;
    if bytes.len() > SNAPSHOT_FILE_LIMIT
        || bytes.len() as u64 != before.len()
        || stamp(&before) != stamp(&after)
        || after.nlink() != 1
        || named.st_dev != opened.st_dev
        || named.st_ino != opened.st_ino
    {
        return Err(Error::Conflict(
            "workspace file changed during command snapshot",
        ));
    }
    Ok((bytes, before.mode() & 0o777))
}
fn walk(
    directory: &File,
    prefix: &str,
    document: &mut SnapshotDocument,
    excluded: &mut Vec<String>,
    bytes: &mut usize,
    visited: &mut usize,
    depth: usize,
) -> Result<()> {
    if depth > DEPTH_LIMIT {
        return Err(Error::Unavailable("command snapshot depth limit"));
    }
    let before = directory.metadata()?;
    let mut entries = Dir::read_from(directory).map_err(io)?;
    let mut names = Vec::new();
    for entry in &mut entries {
        let entry = entry.map_err(io)?;
        let name = entry
            .file_name()
            .to_str()
            .map_err(|_| Error::Unavailable("command snapshot requires UTF-8 file names"))?;
        if name == "." || name == ".." {
            continue;
        }
        *visited += 1;
        if *visited > SNAPSHOT_ENTRY_LIMIT {
            return Err(Error::Unavailable("command snapshot entry limit"));
        }
        names.push(name.to_owned());
    }
    names.sort();
    for name in names {
        let path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        if components(&path)?.len() > DEPTH_LIMIT {
            return Err(Error::Unavailable("command snapshot depth limit"));
        }
        if command_excluded(&path) {
            if excluded.len() < 256 {
                excluded.push(path);
            }
            continue;
        }
        if document.files.len() + document.directories.len() >= SNAPSHOT_ENTRY_LIMIT {
            return Err(Error::Unavailable("command snapshot entry limit"));
        }
        let stat = rustix::fs::statat(
            directory,
            name.as_str(),
            rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
        )
        .map_err(io)?;
        match FileType::from_raw_mode(stat.st_mode) {
            FileType::Directory => {
                let child = File::from(
                    rustix::fs::openat(
                        directory,
                        name.as_str(),
                        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                        Mode::empty(),
                    )
                    .map_err(io)?,
                );
                let opened = rustix::fs::fstat(&child).map_err(io)?;
                if opened.st_dev != stat.st_dev || opened.st_ino != stat.st_ino {
                    return Err(Error::Conflict(
                        "workspace directory changed during snapshot",
                    ));
                }
                document.directories.push(path.clone());
                walk(&child, &path, document, excluded, bytes, visited, depth + 1)?;
            }
            FileType::RegularFile => {
                let (data, mode) = read_binary(directory, std::ffi::OsStr::new(&name))?;
                *bytes = bytes
                    .checked_add(data.len())
                    .ok_or(Error::Unavailable("command snapshot byte limit"))?;
                if *bytes > SNAPSHOT_BYTE_LIMIT {
                    return Err(Error::Unavailable("command snapshot byte limit"));
                }
                document.files.push(SnapshotFile {
                    path,
                    base64: STANDARD.encode(&data),
                    sha256: digest(&data),
                    executable: mode & 0o111 != 0,
                });
            }
            _ => {
                return Err(Error::Unavailable(
                    "command snapshot refuses symlinks and special files",
                ));
            }
        }
    }
    if stamp(&before) != stamp(&directory.metadata()?) {
        return Err(Error::Conflict(
            "workspace directory changed during snapshot",
        ));
    }
    Ok(())
}
impl Workspace {
    pub fn command_snapshot(&self) -> Result<CommandSnapshot> {
        self.check_root()?;
        let _lock = coordination::WriteLock::acquire(&self.root, &self.coordination_root)?;
        self.check_root()?;
        let mut document = SnapshotDocument {
            version: 1,
            workspace_id: digest(self.root.to_str().ok_or(Error::PrivateState)?),
            files: vec![],
            directories: vec![],
            git: None,
        };
        let mut excluded = vec![];
        let mut bytes = 0;
        let mut visited = 0;
        walk(
            &self.directory,
            "",
            &mut document,
            &mut excluded,
            &mut bytes,
            &mut visited,
            0,
        )?;
        self.check_root()?;
        // Failure to capture optional Git metadata never widens authority or
        // prevents ordinary offline tests. The worker receives no raw or
        // partially captured Git data, and tool output explains its absence.
        let mut git_unavailable = None;
        match git_snapshot::capture(&self.directory, &self.root, &self.coordination_root) {
            Ok(git) => document.git = git,
            Err(_) => {
                git_unavailable = Some(
                    "Git inspection unavailable: metadata is unsupported, changed, over its limit, or this linked worktree lacks a trusted association. No Git metadata was passed to the command.",
                )
            }
        }
        if document.git.is_some() && serde_json::to_vec(&document)?.len() > 96 * 1024 * 1024 {
            document.git = None;
            git_unavailable = Some(
                "Git inspection unavailable: the combined snapshot exceeds its encoding limit. No Git metadata was passed to the command.",
            );
        }
        self.check_root()?;
        let originals = document
            .files
            .iter()
            .map(|file| (file.path.clone(), (file.sha256.clone(), file.executable)))
            .collect();
        Ok(CommandSnapshot {
            document,
            excluded,
            git_unavailable,
            originals,
        })
    }
    pub fn publish_command_changes(
        &self,
        snapshot: &CommandSnapshot,
        changes: CommandChanges,
    ) -> (Result<CommandPublication>, EffectState) {
        let mut effects = EffectState::None;
        let result = self.publish_command_inner(snapshot, changes, &mut effects);
        (result, effects)
    }
    fn publish_command_inner(
        &self,
        snapshot: &CommandSnapshot,
        changes: CommandChanges,
        effects: &mut EffectState,
    ) -> Result<CommandPublication> {
        if changes.version != 1
            || changes.workspace_id != snapshot.document.workspace_id
            || changes.workspace_id != digest(self.root.to_str().ok_or(Error::PrivateState)?)
            || changes.changes.len() > 512
        {
            return Err(Error::Unavailable(
                "command change identity or count invalid",
            ));
        }
        let mut unique = BTreeSet::new();
        let mut total = 0usize;
        let mut decoded = Vec::new();
        for change in changes.changes {
            if components(&change.path)?.len() > DEPTH_LIMIT {
                return Err(Error::Unavailable("command change depth limit"));
            }
            if command_excluded(&change.path) || !unique.insert(change.path.clone()) {
                return Err(Error::Unavailable(
                    "command change path is excluded or duplicated",
                ));
            }
            let data = match (&change.base64, &change.sha256) {
                (Some(encoded), Some(hash))
                    if encoded.len() <= SNAPSHOT_FILE_LIMIT.div_ceil(3) * 4 =>
                {
                    let bytes = STANDARD
                        .decode(encoded)
                        .map_err(|_| Error::Unavailable("command change encoding invalid"))?;
                    if bytes.len() > SNAPSHOT_FILE_LIMIT || digest(&bytes) != *hash {
                        return Err(Error::Unavailable(
                            "command change digest or length invalid",
                        ));
                    }
                    total = total
                        .checked_add(bytes.len())
                        .ok_or(Error::Unavailable("command change byte limit"))?;
                    if total > CHANGE_BYTE_LIMIT {
                        return Err(Error::Unavailable("command change byte limit"));
                    }
                    Some(bytes)
                }
                (None, None) if snapshot.originals.contains_key(&change.path) => None,
                _ => return Err(Error::Unavailable("command change shape invalid")),
            };
            decoded.push((change, data));
        }
        self.check_root()?;
        let _lock = coordination::WriteLock::acquire(&self.root, &self.coordination_root)?;
        self.check_root()?;
        // Validate every changed source before the first host effect. A later
        // non-cooperating edit is checked again immediately before publication.
        for (change, _) in &decoded {
            self.command_current(snapshot, &change.path)?;
        }
        let mut result = CommandPublication {
            written: 0,
            removed: 0,
            directories_created: 0,
        };
        for (change, data) in decoded {
            let parts = components(&change.path)?;
            let name = parts
                .last()
                .ok_or(Error::Unavailable("command change path"))?;
            let mut parent = self.directory.try_clone()?;
            for component in &parts[..parts.len() - 1] {
                match rustix::fs::openat(
                    &parent,
                    *component,
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                ) {
                    Ok(fd) => parent = File::from(fd),
                    Err(rustix::io::Errno::NOENT) if data.is_some() => {
                        *effects = EffectState::Uncertain;
                        rustix::fs::mkdirat(&parent, *component, Mode::RWXU).map_err(io)?;
                        parent.sync_all()?;
                        parent = File::from(
                            rustix::fs::openat(
                                &parent,
                                *component,
                                OFlags::RDONLY
                                    | OFlags::DIRECTORY
                                    | OFlags::NOFOLLOW
                                    | OFlags::CLOEXEC,
                                Mode::empty(),
                            )
                            .map_err(io)?,
                        );
                        result.directories_created += 1;
                    }
                    Err(error) => return Err(io(error)),
                }
            }
            self.check_root()?;
            self.command_parent_current(&change.path, &parent)?;
            let old_mode = command_current_at(snapshot, &change.path, &parent, name)?;
            if let Some(data) = data {
                let temp = format!(".xcb-command-{}", uuid::Uuid::new_v4().simple());
                *effects = EffectState::Uncertain;
                let mut file = File::from(
                    rustix::fs::openat(
                        &parent,
                        temp.as_str(),
                        OFlags::WRONLY
                            | OFlags::CREATE
                            | OFlags::EXCL
                            | OFlags::NOFOLLOW
                            | OFlags::CLOEXEC,
                        Mode::RUSR | Mode::WUSR,
                    )
                    .map_err(io)?,
                );
                let mode = if change.executable {
                    old_mode.unwrap_or(0o600) | 0o100
                } else {
                    old_mode.unwrap_or(0o600) & !0o111
                };
                *effects = EffectState::Uncertain;
                let mut publication = false;
                let written = (|| -> Result<()> {
                    file.write_all(&data)?;
                    file.set_permissions(std::fs::Permissions::from_mode(mode))?;
                    file.sync_all()?;
                    command_current_at(snapshot, &change.path, &parent, name)?;
                    self.command_parent_current(&change.path, &parent)?;
                    self.check_root()?;
                    publication = true;
                    if old_mode.is_some() {
                        rustix::fs::renameat(&parent, temp.as_str(), &parent, *name).map_err(io)?;
                    } else {
                        rustix::fs::linkat(
                            &parent,
                            temp.as_str(),
                            &parent,
                            *name,
                            rustix::fs::AtFlags::empty(),
                        )
                        .map_err(io)?;
                        rustix::fs::unlinkat(&parent, temp.as_str(), rustix::fs::AtFlags::empty())
                            .map_err(io)?;
                    }
                    parent.sync_all()?;
                    self.command_parent_current(&change.path, &parent)?;
                    Ok(())
                })();
                if written.is_err()
                    && !publication
                    && rustix::fs::unlinkat(&parent, temp.as_str(), rustix::fs::AtFlags::empty())
                        .is_ok()
                    && parent.sync_all().is_ok()
                    && result.written == 0
                    && result.removed == 0
                    && result.directories_created == 0
                {
                    *effects = EffectState::None;
                }
                written?;
                result.written += 1;
            } else {
                *effects = EffectState::Uncertain;
                command_current_at(snapshot, &change.path, &parent, name)?;
                self.command_parent_current(&change.path, &parent)?;
                rustix::fs::unlinkat(&parent, *name, rustix::fs::AtFlags::empty()).map_err(io)?;
                parent.sync_all()?;
                self.command_parent_current(&change.path, &parent)?;
                result.removed += 1;
            }
        }
        self.check_root()?;
        if result.written + result.removed + result.directories_created > 0 {
            *effects = EffectState::Settled;
        }
        Ok(result)
    }
    fn command_parent_current(&self, path: &str, retained: &File) -> Result<()> {
        self.check_root()?;
        let (named, _) = self.parent(path)?;
        let named = named.metadata()?;
        let retained = retained.metadata()?;
        if named.dev() != retained.dev() || named.ino() != retained.ino() {
            return Err(Error::Conflict(
                "workspace parent changed while command ran",
            ));
        }
        Ok(())
    }
    fn command_current(&self, snapshot: &CommandSnapshot, path: &str) -> Result<Option<u32>> {
        match self.parent(path) {
            Ok((parent, name)) => command_current_at(snapshot, path, &parent, name),
            Err(Error::Io(error))
                if error.kind() == std::io::ErrorKind::NotFound
                    && !snapshot.originals.contains_key(path) =>
            {
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }
}
fn command_current_at(
    snapshot: &CommandSnapshot,
    path: &str,
    parent: &File,
    name: &std::ffi::OsStr,
) -> Result<Option<u32>> {
    match (snapshot.originals.get(path), read_binary(parent, name)) {
        (Some((expected, executable)), Ok((bytes, mode)))
            if digest(&bytes) == *expected && (mode & 0o111 != 0) == *executable =>
        {
            Ok(Some(mode))
        }
        (None, Err(Error::Io(error))) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        (None, Ok(_)) | (Some(_), Ok(_)) => Err(Error::Conflict(
            "workspace changed while command ran; staged output retained",
        )),
        (Some(_), Err(Error::Io(error))) if error.kind() == std::io::ErrorKind::NotFound => Err(
            Error::Conflict("workspace changed while command ran; staged output retained"),
        ),
        (_, Err(error)) => Err(error),
    }
}

impl CommandSnapshot {
    pub fn save(&self, path: &Path) -> Result<String> {
        let bytes = serde_json::to_vec(&self.document)?;
        if bytes.len() > 96 * 1024 * 1024 {
            return Err(Error::Unavailable("command snapshot encoding limit"));
        }
        private::create(path, &bytes)?;
        Ok(digest(bytes))
    }
}

#[cfg(test)]
mod tests;
