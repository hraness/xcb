pub mod git_snapshot;
pub mod snapshot;

use crate::{Error, Result, coordination, digest};
use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags, RenameFlags};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs::File,
    io::{Read, Write},
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Component, Path, PathBuf},
};
use xcb_core::{MAX_TEXT_BYTES, policy::EffectState};

#[derive(Debug, Serialize)]
pub struct ReadResult {
    pub text: String,
    pub revision: String,
}
#[derive(Debug, Serialize)]
pub struct Entry {
    pub name: String,
    pub kind: String,
}

pub struct Workspace {
    root: PathBuf,
    directory: File,
    coordination_root: PathBuf,
}

fn io(error: rustix::io::Errno) -> Error {
    std::io::Error::from(error).into()
}
fn components(path: &str) -> Result<Vec<&std::ffi::OsStr>> {
    if path.is_empty() || path.len() > 4096 || path.chars().any(char::is_control) {
        return Err(xcb_core::Error::Invalid("workspace path").into());
    }
    Path::new(path)
        .components()
        .map(|component| match component {
            Component::Normal(name) => Ok(name),
            _ => Err(xcb_core::Error::Invalid("relative workspace path").into()),
        })
        .collect()
}
fn regular(file: &File) -> Result<()> {
    let meta = file.metadata()?;
    if !meta.is_file() || meta.nlink() != 1 || meta.len() > MAX_TEXT_BYTES as u64 {
        return Err(xcb_core::Error::Invalid("workspace file").into());
    }
    Ok(())
}
fn file_at(parent: &File, name: &std::ffi::OsStr) -> Result<File> {
    let file = File::from(
        rustix::fs::openat(
            parent,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(io)?,
    );
    regular(&file)?;
    Ok(file)
}
fn revision_file_at(parent: &File, name: &std::ffi::OsStr, expected: &str) -> Result<File> {
    if expected.len() != 64
        || !expected
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(xcb_core::Error::Invalid("workspace revision").into());
    }
    let mut file = file_at(parent, name)?;
    let mut bytes = Vec::new();
    (&mut file)
        .take(MAX_TEXT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    regular(&file)?;
    if bytes.len() > MAX_TEXT_BYTES {
        return Err(xcb_core::Error::Limit("workspace file").into());
    }
    if digest(&bytes) != expected {
        return Err(Error::Conflict(
            "workspace revision changed; read the current file first",
        ));
    }
    Ok(file)
}

fn read_at(parent: &File, name: &std::ffi::OsStr) -> Result<ReadResult> {
    let file = file_at(parent, name)?;
    let mut bytes = Vec::new();
    file.take(MAX_TEXT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_TEXT_BYTES {
        return Err(xcb_core::Error::Limit("workspace file").into());
    }
    let revision = digest(&bytes);
    let text =
        String::from_utf8(bytes).map_err(|_| xcb_core::Error::Invalid("UTF-8 workspace file"))?;
    Ok(ReadResult { text, revision })
}

impl Workspace {
    pub fn open(root: &Path) -> Result<Self> {
        Self::open_with_coordination(root, &coordination::default_root()?)
    }
    pub fn open_with_coordination(root: &Path, coordination_root: &Path) -> Result<Self> {
        if !root.is_absolute() || root.canonicalize()? != root || !coordination_root.is_absolute() {
            return Err(Error::PrivateState);
        }
        let root = root.canonicalize()?;
        if coordination_root.starts_with(&root) || root.starts_with(coordination_root) {
            return Err(Error::Conflict(
                "write coordination must be outside the workspace",
            ));
        }
        let fd = rustix::fs::open(
            &root,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(io)?;
        Ok(Self {
            root,
            directory: File::from(fd),
            coordination_root: coordination_root.to_owned(),
        })
    }
    fn check_root(&self) -> Result<()> {
        let opened = self.directory.metadata()?;
        let named = std::fs::symlink_metadata(&self.root)?;
        if !named.is_dir() || opened.dev() != named.dev() || opened.ino() != named.ino() {
            return Err(Error::Conflict("workspace directory changed"));
        }
        Ok(())
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    fn directory_at(&self, parts: &[&std::ffi::OsStr]) -> Result<File> {
        let mut fd = self.directory.try_clone()?;
        for part in parts {
            fd = File::from(
                rustix::fs::openat(
                    &fd,
                    *part,
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                )
                .map_err(io)?,
            );
        }
        Ok(fd)
    }
    fn parent<'a>(&self, path: &'a str) -> Result<(File, &'a std::ffi::OsStr)> {
        let parts = components(path)?;
        let name = *parts.last().ok_or(xcb_core::Error::Invalid("file path"))?;
        Ok((self.directory_at(&parts[..parts.len() - 1])?, name))
    }
    pub fn read(&self, path: &str) -> Result<ReadResult> {
        let (parent, name) = self.parent(path)?;
        read_at(&parent, name)
    }
    pub fn write(&self, path: &str, text: &str, expected: Option<&str>) -> Result<String> {
        self.write_observed(path, text, expected, &mut EffectState::None)
    }
    fn write_observed(
        &self,
        path: &str,
        text: &str,
        expected: Option<&str>,
        effects: &mut EffectState,
    ) -> Result<String> {
        if text.len() > MAX_TEXT_BYTES {
            return Err(xcb_core::Error::Limit("workspace write").into());
        }
        let (parent, name) = self.parent(path)?;
        self.check_root()?;
        let _lock = coordination::WriteLock::acquire(&self.root, &self.coordination_root)?;
        self.check_root()?;
        let check = || -> Result<()> {
            match read_at(&parent, name) {
                Ok(current) if expected == Some(current.revision.as_str()) => Ok(()),
                Err(Error::Io(error))
                    if error.kind() == std::io::ErrorKind::NotFound && expected.is_none() =>
                {
                    Ok(())
                }
                Ok(_) => Err(Error::Conflict(
                    "workspace revision changed; read the current file first",
                )),
                Err(error) => Err(error),
            }
        };
        check()?;
        let mode = if expected.is_some() {
            file_at(&parent, name)?.metadata()?.mode() & 0o777
        } else {
            0o600
        };
        let temp = format!(".xcb-{}", uuid::Uuid::new_v4().simple());
        let mut file = File::from(
            rustix::fs::openat(
                &parent,
                temp.as_str(),
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::RUSR | Mode::WUSR,
            )
            .map_err(io)?,
        );
        // Until publication, a rejected write has no lasting effect if its
        // staging file is removed. Once publication is attempted, an error
        // (including directory fsync failure) requires reconciliation.
        let mut publication_attempted = false;
        *effects = EffectState::Uncertain;
        let result = (|| {
            file.write_all(text.as_bytes())?;
            file.set_permissions(std::fs::Permissions::from_mode(mode))?;
            file.sync_all()?;
            check()?;
            self.check_root()?;
            publication_attempted = true;
            if expected.is_none() {
                rustix::fs::linkat(&parent, temp.as_str(), &parent, name, AtFlags::empty())
                    .map_err(io)?;
                rustix::fs::unlinkat(&parent, temp.as_str(), AtFlags::empty()).map_err(io)?;
            } else {
                rustix::fs::renameat(&parent, temp.as_str(), &parent, name).map_err(io)?;
            }
            parent.sync_all()?;
            *effects = EffectState::Settled;
            Ok(digest(text))
        })();
        if result.is_err()
            && rustix::fs::unlinkat(&parent, temp.as_str(), AtFlags::empty()).is_ok()
            && !publication_attempted
            && parent.sync_all().is_ok()
        {
            *effects = EffectState::None;
        }
        result
    }
    pub fn mkdir(&self, path: &str, parents: bool) -> Result<usize> {
        self.mkdir_observed(path, parents, &mut EffectState::None)
    }
    fn mkdir_observed(
        &self,
        path: &str,
        parents: bool,
        effects: &mut EffectState,
    ) -> Result<usize> {
        let parts = components(path)?;
        if parts.is_empty() || parts.len() > 64 {
            return Err(xcb_core::Error::Limit("workspace directory depth").into());
        }
        self.check_root()?;
        let _lock = coordination::WriteLock::acquire(&self.root, &self.coordination_root)?;
        self.check_root()?;
        let mut directory = self.directory.try_clone()?;
        let mut created = 0;
        for (index, part) in parts.iter().enumerate() {
            let final_component = index + 1 == parts.len();
            if parents || final_component {
                self.check_root()?;
                match rustix::fs::mkdirat(&directory, *part, Mode::RUSR | Mode::WUSR | Mode::XUSR) {
                    Ok(()) => {
                        // A created directory must be reconciled if either sync fails.
                        *effects = EffectState::Uncertain;
                        directory.sync_all()?;
                        let child = File::from(
                            rustix::fs::openat(
                                &directory,
                                *part,
                                OFlags::RDONLY
                                    | OFlags::DIRECTORY
                                    | OFlags::NOFOLLOW
                                    | OFlags::CLOEXEC,
                                Mode::empty(),
                            )
                            .map_err(io)?,
                        );
                        child.sync_all()?;
                        directory = child;
                        created += 1;
                        *effects = EffectState::Settled;
                        continue;
                    }
                    Err(rustix::io::Errno::EXIST) if parents => (),
                    Err(error) => return Err(io(error)),
                }
            }
            // Existing components, including the final one with parents=true,
            // must be real directories; a symlink is never followed.
            directory = File::from(
                rustix::fs::openat(
                    &directory,
                    *part,
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                )
                .map_err(io)?,
            );
        }
        Ok(created)
    }
    pub fn remove(&self, path: &str, expected: &str) -> Result<()> {
        self.remove_observed(path, expected, &mut EffectState::None)
    }
    fn remove_observed(&self, path: &str, expected: &str, effects: &mut EffectState) -> Result<()> {
        // Resolve parents while holding the same coordination transaction used
        // by writes and renames, so cooperating mutations cannot move them.
        components(path)?;
        self.check_root()?;
        let _lock = coordination::WriteLock::acquire(&self.root, &self.coordination_root)?;
        self.check_root()?;
        let (parent, name) = self.parent(path)?;
        let _file = revision_file_at(&parent, name, expected)?;
        self.check_root()?;
        rustix::fs::unlinkat(&parent, name, AtFlags::empty()).map_err(io)?;
        *effects = EffectState::Uncertain;
        parent.sync_all()?;
        *effects = EffectState::Settled;
        Ok(())
    }
    pub fn rename(&self, from: &str, to: &str, expected: &str) -> Result<String> {
        self.rename_observed(from, to, expected, &mut EffectState::None)
    }
    fn rename_observed(
        &self,
        from: &str,
        to: &str,
        expected: &str,
        effects: &mut EffectState,
    ) -> Result<String> {
        components(from)?;
        components(to)?;
        self.check_root()?;
        let _lock = coordination::WriteLock::acquire(&self.root, &self.coordination_root)?;
        self.check_root()?;
        let (source, source_name) = self.parent(from)?;
        let (destination, destination_name) = self.parent(to)?;
        let file = revision_file_at(&source, source_name, expected)?;
        // Some kernels accept renaming a path to itself even with NOREPLACE.
        // Reject every existing destination, including the source and symlinks.
        match rustix::fs::statat(&destination, destination_name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(_) => {
                return Err(Error::Conflict(
                    "workspace rename destination already exists",
                ));
            }
            Err(rustix::io::Errno::NOENT) => (),
            Err(error) => return Err(io(error)),
        }
        // NOREPLACE provides the no-clobber guarantee even when an unrelated
        // editor creates the destination after the revision check.
        file.sync_all()?;
        self.check_root()?;
        rustix::fs::renameat_with(
            &source,
            source_name,
            &destination,
            destination_name,
            RenameFlags::NOREPLACE,
        )
        .map_err(io)?;
        *effects = EffectState::Uncertain;
        destination.sync_all()?;
        source.sync_all()?;
        *effects = EffectState::Settled;
        Ok(expected.to_owned())
    }
    pub fn list(&self, path: &str) -> Result<Vec<Entry>> {
        let fd = if path == "." || path.is_empty() {
            self.directory.try_clone()?
        } else {
            self.directory_at(&components(path)?)?
        };
        let mut entries = Vec::new();
        for item in Dir::read_from(&fd).map_err(io)? {
            let item = item.map_err(io)?;
            let name = item.file_name().to_string_lossy().into_owned();
            if name == "."
                || name == ".."
                || name.chars().any(char::is_control)
                || item.file_type() == FileType::Symlink
            {
                continue;
            }
            if entries.len() >= 512 {
                return Err(xcb_core::Error::Limit("directory entries").into());
            }
            entries.push(Entry {
                name,
                kind: if item.file_type() == FileType::Directory {
                    "directory"
                } else {
                    "file"
                }
                .to_owned(),
            });
        }
        entries.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(entries)
    }
    pub fn search(&self, path: &str, query: &str) -> Result<Value> {
        xcb_core::label(query, 256)?;
        let mut pending = vec![path.to_owned()];
        let mut matches = Vec::new();
        let mut visited = 0;
        let mut scanned = 0;
        let mut truncated = false;
        while let Some(directory) = pending.pop() {
            if visited >= 128 {
                truncated = true;
                break;
            }
            visited += 1;
            for entry in self.list(&directory)? {
                let relative = if directory == "." || directory.is_empty() {
                    entry.name.clone()
                } else {
                    format!("{directory}/{}", entry.name)
                };
                if entry.kind == "directory" {
                    if !matches!(entry.name.as_str(), ".git" | "node_modules" | "target")
                        && pending.len() < 128
                    {
                        pending.push(relative);
                    }
                } else {
                    scanned += 1;
                    if scanned > 512 {
                        truncated = true;
                        break;
                    }
                    let Ok(content) = self.read(&relative) else {
                        continue;
                    };
                    for (index, line) in content.text.lines().enumerate() {
                        if line.contains(query) {
                            matches.push(json!({"path":relative,"line":index + 1,"text":xcb_core::display_text(line, 512)}));
                            if matches.len() >= 64 {
                                truncated = true;
                                break;
                            }
                        }
                    }
                }
                if truncated {
                    break;
                }
            }
            if truncated {
                break;
            }
        }
        Ok(json!({"matches":matches,"truncated":truncated}))
    }
    pub fn call(&self, name: &str, input: &Value) -> Result<Value> {
        self.call_observed(name, input).0
    }
    /// Report effects separately from tool success: a stale revision or bad
    /// argument is a settled rejection, whereas a failed publication may
    /// have affected the workspace and must retain account custody.
    pub fn call_observed(&self, name: &str, input: &Value) -> (Result<Value>, EffectState) {
        let mut effects = EffectState::None;
        let result = self.call_inner(name, input, &mut effects);
        (result, effects)
    }
    fn call_inner(&self, name: &str, input: &Value, effects: &mut EffectState) -> Result<Value> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct PathArgs {
            path: String,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct WriteArgs {
            path: String,
            text: String,
            expected_revision: Option<String>,
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct SearchArgs {
            path: String,
            query: String,
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct MkdirArgs {
            path: String,
            parents: bool,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct RemoveArgs {
            path: String,
            expected_revision: String,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct RenameArgs {
            from: String,
            to: String,
            expected_revision: String,
        }
        match name {
            "workspace_mkdir" => {
                let args: MkdirArgs = serde_json::from_value(input.clone())?;
                Ok(json!({"created": self.mkdir_observed(&args.path, args.parents, effects)?}))
            }
            "workspace_remove" => {
                let args: RemoveArgs = serde_json::from_value(input.clone())?;
                self.remove_observed(&args.path, &args.expected_revision, effects)?;
                Ok(json!({"removed": true}))
            }
            "workspace_rename" => {
                let args: RenameArgs = serde_json::from_value(input.clone())?;
                Ok(
                    json!({"revision": self.rename_observed(&args.from, &args.to, &args.expected_revision, effects)?}),
                )
            }
            "workspace_read" => {
                let args: PathArgs = serde_json::from_value(input.clone())?;
                Ok(serde_json::to_value(self.read(&args.path)?)?)
            }
            "workspace_list" => {
                let args: PathArgs = serde_json::from_value(input.clone())?;
                Ok(json!({"entries":self.list(&args.path)?}))
            }
            "workspace_write" => {
                let args: WriteArgs = serde_json::from_value(input.clone())?;
                Ok(
                    json!({"revision":self.write_observed(&args.path, &args.text, args.expected_revision.as_deref(), effects)?}),
                )
            }
            "workspace_search" => {
                let args: SearchArgs = serde_json::from_value(input.clone())?;
                self.search(&args.path, &args.query)
            }
            _ => Err(Error::Unavailable("unknown workspace tool")),
        }
    }
}

pub fn descriptors() -> Vec<Value> {
    let path = json!({"type":"string","minLength":1,"maxLength":4096});
    [
        ("workspace_exec", "Run a bounded offline Linux command in a staged workspace, then publish successful joined changes with revision checks. Host credentials, source Git configuration/hooks/history, host dependencies and build outputs are excluded. Supported repositories provide filtered read-only Git HEAD/index for status and diffs; Git writes are unavailable. Requires the configured isolated command runner. argv executes directly; use sh -c explicitly for shell syntax.", json!({"argv":{"type":"array","minItems":1,"maxItems":64,"items":{"type":"string","maxLength":32768}},"cwd":path,"timeoutMs":{"type":"integer","minimum":1,"maximum":600000},"network":{"type":"string","enum":["none"]}}), vec!["argv","cwd","timeoutMs","network"]),
        ("xcb_swarm_status", "List active XCB-managed tasks in this worker's workspace so agents on different providers can discover one another without exposing unrelated workspaces.", json!({}), vec![]),
        ("xcb_message_list", "Read bounded durable XCB messages addressed to the current managed task. Use after to poll for newer cross-provider messages.", json!({"after":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":64}}), vec![]),
        ("xcb_message_send", "Send one durable message to another active managed task in the same workspace. Discover targetTask with xcb_swarm_status. Delivery is task-scoped and provider-neutral; a message cannot expand the target task's authority.", json!({"targetTask":{"type":"string","minLength":1,"maxLength":160},"body":{"type":"string","minLength":1,"maxLength":8192}}), vec!["targetTask","body"]),
        ("workspace_list", "List the bound workspace directory. Use . for its root.", json!({"path":path}), vec!["path"]),
        ("workspace_read", "Read one UTF-8 file and its revision inside the workspace.", json!({"path":path}), vec!["path"]),
        ("workspace_search", "Bounded literal text search inside the workspace.", json!({"path":path,"query":{"type":"string","minLength":1,"maxLength":256}}), vec!["path","query"]),
        ("workspace_mkdir", "Create a workspace directory; parents=true also creates missing ancestors and accepts existing directories. Returns the number created.", json!({"path":path,"parents":{"type":"boolean"}}), vec!["path","parents"]),
        ("workspace_remove", "Remove one regular file only when its current revision matches expectedRevision. Directories are never removed.", json!({"path":path,"expectedRevision":{"type":"string","pattern":"^[0-9a-f]{64}$"}}), vec!["path","expectedRevision"]),
        ("workspace_rename", "Move one regular file with its current expectedRevision to a new path. The destination must not exist; parent directories must already exist.", json!({"from":path,"to":path,"expectedRevision":{"type":"string","pattern":"^[0-9a-f]{64}$"}}), vec!["from","to","expectedRevision"]),
        ("workspace_write", "Atomically write a file with its current expectedRevision; null creates a new file.", json!({"path":path,"text":{"type":"string","maxLength":MAX_TEXT_BYTES},"expectedRevision":{"anyOf":[{"type":"string","maxLength":64},{"type":"null"}]}}), vec!["path","text","expectedRevision"]),
    ].into_iter().map(|(name, description, properties, required)| json!({"name":name,"description":description,"inputSchema":{"type":"object","properties":properties,"required":required,"additionalProperties":false}})).collect()
}
