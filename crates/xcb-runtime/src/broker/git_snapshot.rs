//! Raw Git inputs for the trusted guest projector, never for the command worker.
//! The caller holds the workspace coordination lock. No Git process runs here.
use super::io;
use crate::{Error, Result, digest, private};
use base64::{Engine, engine::general_purpose::STANDARD};
use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::File,
    io::Read,
    os::unix::fs::MetadataExt,
    path::{Component, Path, PathBuf},
};

pub const GIT_BYTE_LIMIT: usize = 32 * 1024 * 1024;
pub const GIT_FILE_LIMIT: usize = 2 * 1024 * 1024;
pub const GIT_ENTRY_LIMIT: usize = 4096;
const DEPTH_LIMIT: usize = 64;

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GitSnapshotFile {
    pub path: String,
    pub base64: String,
    pub sha256: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GitSnapshot {
    pub version: u32,
    pub head_object_id: Option<String>,
    pub files: Vec<GitSnapshotFile>,
}
impl std::fmt::Debug for GitSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitSnapshot")
            .field("version", &self.version)
            .field("file_count", &self.files.len())
            .finish_non_exhaustive()
    }
}

/// Trusted host registration, never inferred from a workspace-controlled gitfile.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GitAssociation {
    pub version: u32,
    pub workspace: PathBuf,
    pub git_dir: PathBuf,
    pub common_dir: PathBuf,
}

/// Inode identity guard for the source files a snapshot depends on; a change
/// to any tracked field means the source moved under the read.
type Stamp = xcb_core::FileIdentity;
fn stamp(m: &std::fs::Metadata) -> Stamp {
    Stamp::of(m)
}
fn unsupported() -> Error {
    Error::Unavailable(
        "Git inspection requires a bounded ordinary SHA-1 repository without split/sparse indexes, submodules, or alternate object stores",
    )
}
fn changed() -> Error {
    Error::Conflict("Git source changed during command snapshot; retry after Git finishes")
}
fn path_parts(path: &str) -> Result<Vec<&str>> {
    xcb_core::relative_parts(path)
        .filter(|parts| parts.len() <= DEPTH_LIMIT)
        .ok_or_else(unsupported)
}
fn open_dir(parent: &File, name: &str) -> Result<File> {
    Ok(File::from(
        rustix::fs::openat(
            parent,
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(io)?,
    ))
}
fn physical(path: &Path) -> Result<()> {
    if !xcb_core::absolute_clean(path) || path.canonicalize()? != path {
        return Err(Error::PrivateState);
    }
    Ok(())
}
fn physical_dir(path: &Path) -> Result<File> {
    physical(path)?;
    let mut fd = File::open("/")?;
    for component in path.components() {
        if let Component::Normal(name) = component {
            fd = open_dir(&fd, name.to_str().ok_or(Error::PrivateState)?)?;
        }
    }
    Ok(fd)
}
struct Tree {
    root: File,
    path: PathBuf,
    owner: u32,
    guards: Vec<(String, Stamp, bool)>,
}
impl Tree {
    fn new(root: File, path: PathBuf, owner: u32) -> Result<Self> {
        let metadata = root.metadata()?;
        if !metadata.is_dir() || metadata.uid() != owner {
            return Err(unsupported());
        }
        Ok(Self {
            root,
            path,
            owner,
            guards: vec![(String::new(), stamp(&metadata), true)],
        })
    }
    fn parent(&self, path: &str) -> Result<(File, String)> {
        let parts = path_parts(path)?;
        let mut fd = self.root.try_clone()?;
        for name in &parts[..parts.len() - 1] {
            fd = open_dir(&fd, name)?;
        }
        Ok((fd, parts[parts.len() - 1].into()))
    }
    fn kind(&self, path: &str) -> Result<Option<FileType>> {
        let (parent, name) = self.parent(path)?;
        match rustix::fs::statat(parent, name.as_str(), AtFlags::SYMLINK_NOFOLLOW) {
            Ok(s) => Ok(Some(FileType::from_raw_mode(s.st_mode))),
            Err(rustix::io::Errno::NOENT) => Ok(None),
            Err(error) => Err(io(error)),
        }
    }
    fn read(&mut self, path: &str, limit: usize) -> Result<Vec<u8>> {
        let (parent, name) = self.parent(path)?;
        let mut fd = File::from(
            rustix::fs::openat(
                &parent,
                name.as_str(),
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(io)?,
        );
        let before = fd.metadata()?;
        if !before.is_file()
            || before.nlink() != 1
            || before.uid() != self.owner
            || before.len() > limit as u64
        {
            return Err(unsupported());
        }
        let mut bytes = Vec::new();
        (&mut fd).take(limit as u64 + 1).read_to_end(&mut bytes)?;
        let named =
            rustix::fs::statat(parent, name.as_str(), AtFlags::SYMLINK_NOFOLLOW).map_err(io)?;
        let opened = rustix::fs::fstat(&fd).map_err(io)?;
        if bytes.len() as u64 != before.len()
            || stamp(&fd.metadata()?) != stamp(&before)
            || named.st_dev != opened.st_dev
            || named.st_ino != opened.st_ino
        {
            return Err(changed());
        }
        self.guards.push((path.into(), stamp(&before), false));
        Ok(bytes)
    }
    fn list(&mut self, path: &str, budget: &mut Budget) -> Result<Vec<(String, FileType)>> {
        let directory = if path.is_empty() {
            self.root.try_clone()?
        } else {
            let (parent, name) = self.parent(path)?;
            open_dir(&parent, &name)?
        };
        let before = directory.metadata()?;
        if before.uid() != self.owner {
            return Err(unsupported());
        }
        let mut output: Vec<(String, FileType)> = Vec::new();
        for entry in Dir::read_from(&directory).map_err(io)? {
            let entry = entry.map_err(io)?;
            let name = entry.file_name().to_str().map_err(|_| unsupported())?;
            if name == "." || name == ".." {
                continue;
            }
            budget.visit()?;
            path_parts(name)?;
            let stat =
                rustix::fs::statat(&directory, name, AtFlags::SYMLINK_NOFOLLOW).map_err(io)?;
            output.push((name.into(), FileType::from_raw_mode(stat.st_mode)));
        }
        if stamp(&before) != stamp(&directory.metadata()?) {
            return Err(changed());
        }
        self.guards.push((path.into(), stamp(&before), true));
        output.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(output)
    }
    fn verify(&self) -> Result<()> {
        physical(&self.path)?;
        if stamp(&std::fs::symlink_metadata(&self.path)?) != self.guards[0].1 {
            return Err(changed());
        }
        for (path, expected, directory) in &self.guards {
            let fd = if path.is_empty() {
                self.root.try_clone()?
            } else {
                let (parent, name) = self.parent(path)?;
                if *directory {
                    open_dir(&parent, &name)?
                } else {
                    File::from(
                        rustix::fs::openat(
                            parent,
                            name.as_str(),
                            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
                            Mode::empty(),
                        )
                        .map_err(io)?,
                    )
                }
            };
            if stamp(&fd.metadata()?) != *expected {
                return Err(changed());
            }
        }
        Ok(())
    }
}
#[derive(Default)]
struct Budget {
    entries: usize,
    bytes: usize,
}
impl Budget {
    fn visit(&mut self) -> Result<()> {
        self.entries += 1;
        if self.entries > GIT_ENTRY_LIMIT {
            return Err(Error::Unavailable("Git inspection entry limit"));
        }
        Ok(())
    }
    fn add(&mut self, bytes: usize) -> Result<()> {
        self.bytes = self.bytes.checked_add(bytes).ok_or_else(unsupported)?;
        if self.bytes > GIT_BYTE_LIMIT {
            return Err(Error::Unavailable("Git inspection byte limit"));
        }
        Ok(())
    }
}
fn hex40(value: &str) -> bool {
    value.len() == 40 && xcb_core::hex_lower(value) && value.bytes().any(|b| b != b'0')
}
fn hex_name(value: &str, length: usize) -> bool {
    value.len() == length && xcb_core::hex_lower(value)
}
fn reference(value: &str) -> Result<()> {
    let parts = path_parts(value)?;
    if !value.starts_with("refs/")
        || parts.iter().any(|part| {
            part.starts_with('.')
                || part.ends_with('.')
                || part.ends_with(".lock")
                || part.contains("..")
        })
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_/ .".contains(&b) && b != b' ')
    {
        return Err(unsupported());
    }
    Ok(())
}
fn line(bytes: &[u8]) -> Result<&str> {
    let value = std::str::from_utf8(bytes).map_err(|_| unsupported())?;
    let value = value.strip_suffix('\n').unwrap_or(value);
    if value.is_empty() || value.len() > 4096 || value.chars().any(char::is_control) {
        return Err(unsupported());
    }
    Ok(value)
}
fn lexical(base: &Path, value: &str) -> Result<PathBuf> {
    if !xcb_core::bounded_path(value) {
        return Err(unsupported());
    }
    let mut path = if Path::new(value).is_absolute() {
        PathBuf::from("/")
    } else {
        base.to_owned()
    };
    for part in Path::new(value).components() {
        match part {
            Component::RootDir | Component::CurDir => (),
            Component::Normal(name) => path.push(name),
            Component::ParentDir => {
                if !path.pop() {
                    return Err(unsupported());
                }
            }
            _ => return Err(unsupported()),
        }
    }
    Ok(path)
}
fn selected(
    tree: &mut Tree,
    path: &str,
    budget: &mut Budget,
    data: &mut BTreeMap<String, Vec<u8>>,
) -> Result<()> {
    let bytes = tree.read(path, GIT_FILE_LIMIT)?;
    budget.add(bytes.len())?;
    if data.insert(path.into(), bytes).is_some() {
        return Err(unsupported());
    }
    Ok(())
}
fn refs(
    tree: &mut Tree,
    prefix: &str,
    budget: &mut Budget,
    data: &mut BTreeMap<String, Vec<u8>>,
) -> Result<()> {
    if path_parts(prefix)?.len() > DEPTH_LIMIT {
        return Err(unsupported());
    }
    for (name, kind) in tree.list(prefix, budget)? {
        let path = format!("{prefix}/{name}");
        reference(&path)?;
        match kind {
            FileType::Directory => refs(tree, &path, budget, data)?,
            FileType::RegularFile => {
                selected(tree, &path, budget, data)?;
                if !hex40(line(&data[&path])?) {
                    return Err(unsupported());
                }
            }
            _ => return Err(unsupported()),
        }
    }
    Ok(())
}
fn objects(
    tree: &mut Tree,
    budget: &mut Budget,
    data: &mut BTreeMap<String, Vec<u8>>,
) -> Result<()> {
    for (name, kind) in tree.list("objects", budget)? {
        let prefix = format!("objects/{name}");
        if name == "info" {
            if kind != FileType::Directory {
                return Err(unsupported());
            }
            // Listing pins the directory so an alternate cannot appear mid-capture.
            if tree
                .list(&prefix, budget)?
                .iter()
                .any(|(name, _)| matches!(name.as_str(), "alternates" | "http-alternates"))
            {
                return Err(unsupported());
            }
        } else if name == "pack" {
            if kind != FileType::Directory {
                return Err(unsupported());
            }
            for (pack, kind) in tree.list(&prefix, budget)? {
                if pack.ends_with(".promisor") {
                    return Err(unsupported());
                }
                let Some((stem, extension)) = pack.rsplit_once('.') else {
                    continue;
                };
                if !matches!(extension, "pack" | "idx") {
                    continue;
                }
                if kind != FileType::RegularFile
                    || !stem.strip_prefix("pack-").is_some_and(|s| hex_name(s, 40))
                {
                    return Err(unsupported());
                }
                selected(tree, &format!("{prefix}/{pack}"), budget, data)?;
            }
        } else if hex_name(&name, 2) {
            if kind != FileType::Directory {
                return Err(unsupported());
            }
            for (object, kind) in tree.list(&prefix, budget)? {
                if kind != FileType::RegularFile || !hex_name(&object, 38) {
                    return Err(unsupported());
                }
                selected(tree, &format!("{prefix}/{object}"), budget, data)?;
            }
        } else {
            return Err(unsupported());
        }
    }
    Ok(())
}
fn index(bytes: &[u8]) -> Result<()> {
    // Git verifies the SHA-1 checksum in the trusted projector. This parser
    // admits only layouts whose filtering preserves staged/unstaged semantics.
    if bytes.len() < 32 || &bytes[..4] != b"DIRC" {
        return Err(unsupported());
    }
    let number = |slice: &[u8]| u32::from_be_bytes(slice.try_into().expect("checked length"));
    let version = number(&bytes[4..8]);
    if !matches!(version, 2 | 3) {
        return Err(unsupported());
    }
    let count = number(&bytes[8..12]) as usize;
    if count > GIT_ENTRY_LIMIT {
        return Err(unsupported());
    }
    let end = bytes.len() - 20;
    let mut offset = 12;
    for _ in 0..count {
        let start = offset;
        if end.saturating_sub(offset) < 63 {
            return Err(unsupported());
        }
        let mode = number(&bytes[offset + 24..offset + 28]);
        let flags = u16::from_be_bytes(
            bytes[offset + 60..offset + 62]
                .try_into()
                .expect("checked length"),
        );
        if !matches!(mode, 0o100644 | 0o100755) || flags & 0xf000 != 0 {
            return Err(unsupported());
        }
        offset += 62;
        let length = bytes[offset..end]
            .iter()
            .position(|b| *b == 0)
            .ok_or_else(unsupported)?;
        let path =
            std::str::from_utf8(&bytes[offset..offset + length]).map_err(|_| unsupported())?;
        let parts = path_parts(path)?;
        if parts.contains(&".git") || usize::from(flags & 0x0fff) != length.min(0x0fff) {
            return Err(unsupported());
        }
        offset = start + (62 + length + 1).div_ceil(8) * 8;
        if offset > end || bytes[start + 62 + length..offset].iter().any(|b| *b != 0) {
            return Err(unsupported());
        }
    }
    while offset < end {
        if end - offset < 8
            || !bytes[offset..offset + 4].iter().all(u8::is_ascii_uppercase)
            || &bytes[offset..offset + 4] == b"FSMN"
        {
            return Err(unsupported());
        }
        let length = number(&bytes[offset + 4..offset + 8]) as usize;
        offset = offset
            .checked_add(8)
            .and_then(|n| n.checked_add(length))
            .ok_or_else(unsupported)?;
        if offset > end {
            return Err(unsupported());
        }
    }
    Ok(())
}
fn head_id(data: &BTreeMap<String, Vec<u8>>) -> Result<Option<String>> {
    let head = line(data.get("HEAD").ok_or_else(unsupported)?)?;
    if hex40(head) {
        return Ok(Some(head.into()));
    }
    let target = head.strip_prefix("ref: ").ok_or_else(unsupported)?;
    reference(target)?;
    if !target.starts_with("refs/heads/") {
        return Err(unsupported());
    }
    let mut packed = BTreeMap::new();
    if let Some(bytes) = data.get("packed-refs") {
        for row in std::str::from_utf8(bytes)
            .map_err(|_| unsupported())?
            .lines()
        {
            if row.starts_with("# pack-refs with:") {
                continue;
            }
            if let Some(peeled) = row.strip_prefix('^') {
                if !hex40(peeled) {
                    return Err(unsupported());
                }
                continue;
            }
            let (object, name) = row.split_once(' ').ok_or_else(unsupported)?;
            reference(name)?;
            if !hex40(object) || packed.insert(name, object).is_some() {
                return Err(unsupported());
            }
        }
    }
    if let Some(bytes) = data.get(target) {
        let object = line(bytes)?;
        if !hex40(object) {
            return Err(unsupported());
        }
        Ok(Some(object.into()))
    } else {
        Ok(packed.get(target).map(|s| (*s).into()))
    }
}

/// Called under the same coordination lock as the normal file snapshot.
/// Never expose this result in tool output or mount it in the worker namespace.
pub(crate) fn capture(
    directory: &File,
    workspace: &Path,
    coordination_root: &Path,
) -> Result<Option<GitSnapshot>> {
    let owner = directory.metadata()?.uid();
    let mut work = Tree::new(directory.try_clone()?, workspace.into(), owner)?;
    let kind = work.kind(".git")?;
    if kind.is_none() {
        work.verify()?;
        return Ok(None);
    }
    let mut association_pin = None;
    let (mut git, mut common) = match kind {
        Some(FileType::Directory) => {
            let file = open_dir(directory, ".git")?;
            (
                Tree::new(file.try_clone()?, workspace.join(".git"), owner)?,
                Tree::new(file, workspace.join(".git"), owner)?,
            )
        }
        Some(FileType::RegularFile) => {
            let binding = coordination_root.join("git-associations").join(format!(
                "{}.json",
                digest(workspace.to_str().ok_or(Error::PrivateState)?)
            ));
            let parent = binding.parent().ok_or(Error::PrivateState)?;
            if !parent.exists() || !binding.exists() {
                return Err(Error::Unavailable(
                    "linked-worktree Git inspection requires an explicit trusted host association",
                ));
            }
            private::check_directory(parent)?;
            let binding_stamp = stamp(&std::fs::symlink_metadata(&binding)?);
            let binding_bytes = private::read(&binding, 16 * 1024)?;
            if stamp(&std::fs::symlink_metadata(&binding)?) != binding_stamp {
                return Err(changed());
            }
            let association: GitAssociation = serde_json::from_slice(&binding_bytes)?;
            association_pin = Some((binding, binding_stamp, binding_bytes));
            if association.version != 1
                || association.workspace != workspace
                || association.git_dir == association.common_dir
                || association.git_dir.parent().and_then(Path::parent)
                    != Some(association.common_dir.as_path())
                || association.git_dir.parent().and_then(Path::file_name)
                    != Some(std::ffi::OsStr::new("worktrees"))
            {
                return Err(Error::PrivateState);
            }
            let pointer = work.read(".git", 4096)?;
            if lexical(
                workspace,
                line(&pointer)?
                    .strip_prefix("gitdir: ")
                    .ok_or_else(unsupported)?,
            )? != association.git_dir
            {
                return Err(Error::PrivateState);
            }
            let mut git = Tree::new(
                physical_dir(&association.git_dir)?,
                association.git_dir.clone(),
                owner,
            )?;
            let back = git.read("gitdir", 4096)?;
            let common_pointer = git.read("commondir", 4096)?;
            if lexical(&association.git_dir, line(&back)?)? != workspace.join(".git")
                || lexical(&association.git_dir, line(&common_pointer)?)? != association.common_dir
            {
                return Err(Error::PrivateState);
            }
            let common = Tree::new(
                physical_dir(&association.common_dir)?,
                association.common_dir,
                owner,
            )?;
            (git, common)
        }
        _ => return Err(unsupported()),
    };
    let mut budget = Budget::default();
    // Visit but never read unselected controls; reject unsupported indirections.
    for (name, _) in git.list("", &mut budget)? {
        if name.starts_with("sharedindex.") || matches!(name.as_str(), "shallow" | "modules") {
            return Err(unsupported());
        }
        if name == "commondir" && git.path == common.path {
            return Err(unsupported());
        }
    }
    if git.path != common.path {
        for (name, _) in common.list("", &mut budget)? {
            if name.starts_with("sharedindex.")
                || matches!(name.as_str(), "shallow" | "modules" | "commondir")
            {
                return Err(unsupported());
            }
        }
    }
    let mut data = BTreeMap::new();
    selected(&mut git, "HEAD", &mut budget, &mut data)?;
    if git.kind("index")?.is_some() {
        selected(&mut git, "index", &mut budget, &mut data)?;
        index(&data["index"])?;
    }
    if common.kind("packed-refs")?.is_some() {
        selected(&mut common, "packed-refs", &mut budget, &mut data)?;
    }
    if let Some(kind) = common.kind("refs")? {
        if kind != FileType::Directory {
            return Err(unsupported());
        }
        common.list("refs", &mut budget)?;
        if let Some(kind) = common.kind("refs/heads")? {
            if kind != FileType::Directory {
                return Err(unsupported());
            }
            refs(&mut common, "refs/heads", &mut budget, &mut data)?;
        }
    }
    objects(&mut common, &mut budget, &mut data)?;
    let head_object_id = head_id(&data)?;
    work.verify()?;
    git.verify()?;
    common.verify()?;
    if let Some((binding, expected, bytes)) = association_pin {
        private::check_directory(binding.parent().ok_or(Error::PrivateState)?)?;
        if private::read(&binding, 16 * 1024)? != bytes
            || stamp(&std::fs::symlink_metadata(binding)?) != expected
        {
            return Err(changed());
        }
    }
    Ok(Some(GitSnapshot {
        version: 1,
        head_object_id,
        files: data
            .into_iter()
            .map(|(path, bytes)| GitSnapshotFile {
                path,
                sha256: digest(&bytes),
                base64: STANDARD.encode(bytes),
            })
            .collect(),
    }))
}

#[cfg(test)]
mod tests;
