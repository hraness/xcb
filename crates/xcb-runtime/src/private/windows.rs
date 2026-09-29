//! The Windows backend of the private-state contract.
//!
//! The contract is the Unix one with Windows primitives (see `xcb-platform`):
//! a private object is owned by this user, has a DACL that grants no one
//! else, is never a reparse point, and a file has exactly one name. New
//! directories are created with a protected owner-only DACL that every file
//! created inside inherits. Publication stages a file beside the target and
//! commits it with `CreateHardLinkW` (no-clobber create) or a replacing
//! rename.

use crate::{Error, Result};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use xcb_platform::{Facts, Kind};

pub fn default_root() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("XCB_STATE") {
        return Ok(PathBuf::from(path));
    }
    let local = xcb_platform::local_app_data().ok_or(Error::PrivateState)?;
    Ok(local.join("xcb"))
}

fn clean(path: &Path) -> bool {
    path.is_absolute()
        && !path
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
}

/// No ancestor of `path` may be a reparse point: the Windows counterpart of
/// requiring a canonical parent (`realpath(parent) == parent`).
fn ancestors_are_real(path: &Path) -> Result<()> {
    use std::os::windows::fs::MetadataExt;
    const REPARSE_POINT: u32 = 0x400;
    for ancestor in path.ancestors().skip(1) {
        if ancestor.parent().is_none() {
            break;
        }
        let metadata = fs::symlink_metadata(ancestor)?;
        if metadata.file_attributes() & REPARSE_POINT != 0 || !metadata.is_dir() {
            return Err(Error::PrivateState);
        }
    }
    Ok(())
}

pub fn directory(path: &Path) -> Result<PathBuf> {
    if !clean(path) {
        return Err(Error::PrivateState);
    }
    match fs::symlink_metadata(path) {
        Ok(_) => (),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // Every created component gets the owner-only DACL, not just the
            // leaf, like the Unix recursive 0700 creation.
            xcb_platform::create_private_directory_all(path)?;
        }
        Err(error) => return Err(error.into()),
    }
    check_directory(path)
}

pub fn check_directory(path: &Path) -> Result<PathBuf> {
    if !clean(path) || path.parent().is_none() {
        return Err(Error::PrivateState);
    }
    ancestors_are_real(path)?;
    let facts = xcb_platform::path_facts(path)?;
    if !facts.is_private_directory() {
        return Err(Error::PrivateState);
    }
    Ok(path.to_owned())
}

fn file_contract(facts: &Facts, max: u64) -> Result<()> {
    if !facts.is_private_file() {
        return Err(Error::PrivateState);
    }
    if facts.len > max {
        return Err(Error::PrivateState);
    }
    Ok(())
}

pub fn check_file(file: &File, max: u64) -> Result<()> {
    file_contract(&xcb_platform::file_facts(file)?, max)
}

fn open_read(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    xcb_platform::no_follow(&mut options);
    options.open(path)
}

pub fn open_file(path: &Path, max: u64) -> Result<File> {
    // A racing replace can remove the name between open and the handle
    // query: the handle then names an object with no surviving link.
    // Re-resolve the path, like the Unix backend.
    for _ in 0..4 {
        let file = open_read(path)?;
        let facts = xcb_platform::file_facts(&file)?;
        if facts.links == 0 {
            continue;
        }
        if facts.kind == Kind::ReparsePoint {
            return Err(Error::PrivateState);
        }
        file_contract(&facts, max)?;
        return Ok(file);
    }
    Err(Error::PrivateState)
}

pub fn open_file_maybe_vanished(path: &Path, max: u64) -> Result<Option<File>> {
    let file = match open_read(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let facts = xcb_platform::file_facts(&file)?;
    if facts.links == 0 {
        return Ok(None);
    }
    file_contract(&facts, max)?;
    Ok(Some(file))
}

pub(crate) fn same_file(path: &Path, file: &File) -> Result<()> {
    let opened = xcb_platform::file_facts(file)?;
    let named = xcb_platform::path_facts(path)?;
    if named.kind != Kind::File || !opened.same_object(&named) {
        return Err(Error::Conflict("file identity changed"));
    }
    Ok(())
}

/// The publication-name grammar shared with the Unix custody crate.
fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    name.len() <= 127
        && chars
            .next()
            .is_some_and(|first| first.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn publish_target(path: &Path) -> Result<(PathBuf, &str)> {
    let parent = check_directory(path.parent().ok_or(Error::PrivateState)?)?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| valid_name(name))
        .ok_or_else(|| Error::from(xcb_core::Error::Invalid("file name")))?;
    Ok((parent, name))
}

static STAGED: AtomicU64 = AtomicU64::new(0);

/// Write `bytes` to a new staging file beside the target. The file inherits
/// the private directory's owner-only DACL.
fn stage(parent: &Path, name: &str, bytes: &[u8]) -> Result<PathBuf> {
    let staged = parent.join(format!(
        ".publish-{name}-{}-{}",
        std::process::id(),
        STAGED.fetch_add(1, Ordering::Relaxed)
    ));
    let written = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staged)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        check_file(&file, u64::MAX)
    })();
    if let Err(error) = written {
        let _ = fs::remove_file(&staged);
        return Err(error);
    }
    Ok(staged)
}

pub fn create(path: &Path, bytes: &[u8]) -> Result<()> {
    let (parent, name) = publish_target(path)?;
    let staged = stage(&parent, name, bytes)?;
    // CreateHardLinkW never replaces an existing name: the existence check
    // and the commit are one step.
    let linked = fs::hard_link(&staged, path);
    let _ = fs::remove_file(&staged);
    match linked {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            Err(Error::Io(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("{} already exists", path.display()),
            )))
        }
        Err(error) => Err(error.into()),
    }
}

/// Read a locked file through its own handle. Windows byte-range locks are
/// mandatory, so a second handle in this process cannot read it.
fn read_locked(file: &File, max: usize) -> Result<Vec<u8>> {
    let mut handle = file;
    handle.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    handle.take(max as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > max {
        return Err(xcb_core::Error::Limit("private file").into());
    }
    Ok(bytes)
}

pub(crate) fn replace_guarded(
    path: &Path,
    bytes: &[u8],
    expected: &str,
    check: impl Fn() -> Result<()>,
) -> Result<()> {
    let (parent, name) = publish_target(path)?;
    let current_file = open_file(path, 1024 * 1024)?;
    super::lock(&current_file)?;
    let current_file = super::ExclusiveLock::held(current_file);
    same_file(path, &current_file)?;
    if crate::digest(read_locked(&current_file, 1024 * 1024)?) != expected {
        return Err(Error::Conflict("file revision changed"));
    }
    check()?;
    let staged = stage(&parent, name, bytes)?;
    // The commit guard: the same checks immediately before the rename.
    let verdict = (|| -> Result<()> {
        check()?;
        same_file(path, &current_file)?;
        if crate::digest(read_locked(&current_file, 1024 * 1024)?) != expected {
            return Err(Error::Conflict("file revision changed"));
        }
        Ok(())
    })();
    if let Err(error) = verdict {
        let _ = fs::remove_file(&staged);
        return Err(error);
    }
    // std's rename replaces the target with POSIX semantics where the
    // filesystem supports it, so the open, locked old file does not block it.
    if let Err(error) = fs::rename(&staged, path) {
        let _ = fs::remove_file(&staged);
        return Err(error.into());
    }
    Ok(())
}
