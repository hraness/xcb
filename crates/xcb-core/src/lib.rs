pub mod models;
pub mod panes;
pub mod policy;
pub mod reflex;
pub mod session;
pub mod ui;
pub mod usage;

use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};

pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_JSON_BYTES: usize = 1024 * 1024;
pub const MAX_TEXT_BYTES: usize = 256 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid {0}")]
    Invalid(&'static str),
    #[error("{0} limit exceeded")]
    Limit(&'static str),
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Id(String);

impl Id {
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if value.is_empty()
            || value.len() > 160
            || !value.as_bytes()[0].is_ascii_alphanumeric()
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.[]".contains(&byte))
        {
            return Err(Error::Invalid("identifier"));
        }
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Id {
    type Error = Error;
    fn try_from(value: String) -> Result<Self> {
        Self::new(value)
    }
}
impl From<Id> for String {
    fn from(value: Id) -> Self {
        value.0
    }
}
impl fmt::Display for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl FromStr for Id {
    type Err = Error;
    fn from_str(value: &str) -> Result<Self> {
        Self::new(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    Claude,
    Codex,
    /// Retained only so older state files can still be read. New routes and
    /// commands reject Devin; xcb currently focuses on Claude and Codex.
    Devin,
}

impl Provider {
    /// All provider tags understood by the state-file decoder, including the
    /// retired Devin tag for backwards-compatible reads.
    pub const ALL: [Self; 3] = [Self::Devin, Self::Claude, Self::Codex];
    /// Providers that xcb can discover, authenticate, route, and run.
    pub const SUPPORTED: [Self; 2] = [Self::Claude, Self::Codex];
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Devin => "devin",
        }
    }
}
impl fmt::Display for Provider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
impl FromStr for Provider {
    type Err = Error;
    fn from_str(value: &str) -> Result<Self> {
        Self::SUPPORTED
            .into_iter()
            .find(|provider| provider.as_str() == value)
            .ok_or(Error::Invalid("provider"))
    }
}

pub fn bounded_text(value: &str, max: usize) -> Result<()> {
    if value.len() > max {
        return Err(Error::Limit("text"));
    }
    if value.chars().any(|ch| {
        (ch.is_control() && ch != '\n' && ch != '\t')
            || matches!(ch, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
    }) {
        return Err(Error::Invalid("text controls"));
    }
    Ok(())
}

pub fn label(value: &str, max: usize) -> Result<()> {
    bounded_text(value, max)?;
    if value.trim().is_empty() || value.contains(['\n', '\t']) {
        return Err(Error::Invalid("label"));
    }
    Ok(())
}

pub fn display_text(value: &str, max: usize) -> String {
    let mut out = String::new();
    for ch in value.chars() {
        if out.len() + ch.len_utf8() > max {
            break;
        }
        if (!ch.is_control() || ch == '\n' || ch == '\t')
            && !matches!(ch, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        {
            out.push(ch);
        }
    }
    out
}

/// ASCII lowercase hex of any length — the shape `hex::encode` produces.
pub fn hex_lower(value: &str) -> bool {
    value
        .bytes()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Exactly 64 lowercase ASCII hex digits — the fixed SHA-256 digest shape
/// stored records, revisions, pins and evidence fields all use.
pub fn hex64(value: &str) -> bool {
    value.len() == 64 && hex_lower(value)
}

/// 64 ASCII hex digits in either case. Some wire inputs and older state
/// fields were admitted before the lowercase-only digest rule; new writers
/// always produce `hex64`.
pub fn hex64_any(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Bounded control-free path text — the shared prefix of every path grammar.
pub fn bounded_path(value: &str) -> bool {
    !value.is_empty() && value.len() <= 4096 && !value.chars().any(char::is_control)
}

/// `value` split into ordinary relative segments: bounded control-free text
/// with no empty, `.`, or `..` parts. The spelling `"."` is not a relative
/// path here; callers that accept it check it themselves.
pub fn relative_parts(value: &str) -> Option<Vec<&str>> {
    if !bounded_path(value) {
        return None;
    }
    let parts: Vec<&str> = value.split('/').collect();
    if parts
        .iter()
        .any(|part| part.is_empty() || *part == "." || *part == "..")
    {
        return None;
    }
    Some(parts)
}

/// `value` is a bounded ordinary relative path; `"."` is not accepted.
pub fn relative_path(value: &str) -> bool {
    relative_parts(value).is_some()
}

/// This user's home directory as the environment names it: `HOME` on Unix,
/// `USERPROFILE` on Windows. Unset means unknown; nothing falls back to the
/// password database.
pub fn home_dir() -> Option<std::path::PathBuf> {
    #[cfg(unix)]
    let home = std::env::var_os("HOME");
    #[cfg(windows)]
    let home = std::env::var_os("USERPROFILE");
    home.map(std::path::PathBuf::from)
}

/// `std::fs::canonicalize`, in the spelling the rest of xcb compares paths
/// in. On Unix it is exactly that call. On Windows the result drops the
/// `\\?\` verbatim prefix (`\\?\C:\x` becomes `C:\x`, `\\?\UNC\s\x`
/// becomes `\\s\x`) whenever the plain spelling canonicalizes back to the
/// same verbatim path, so `path.canonical()? == path` holds for a canonical
/// `C:\...` path just as it does for `/...` on Unix.
pub fn canonical(path: impl AsRef<std::path::Path>) -> std::io::Result<std::path::PathBuf> {
    let resolved = std::fs::canonicalize(path)?;
    #[cfg(windows)]
    {
        let text = resolved.to_string_lossy();
        let plain = if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
            Some(format!(r"\\{rest}"))
        } else {
            text.strip_prefix(r"\\?\")
                .filter(|rest| {
                    let bytes = rest.as_bytes();
                    bytes.len() >= 3
                        && bytes[0].is_ascii_alphabetic()
                        && bytes[1] == b':'
                        && bytes[2] == b'\\'
                })
                .map(str::to_owned)
        };
        if let Some(plain) = plain.map(std::path::PathBuf::from)
            && std::fs::canonicalize(&plain).is_ok_and(|again| again == resolved)
        {
            return Ok(plain);
        }
    }
    Ok(resolved)
}

/// Absolute path made only of the root and ordinary components — no `.`,
/// `..`, or platform prefix segments. Symlinks are not resolved here.
pub fn absolute_clean(path: &std::path::Path) -> bool {
    use std::path::Component;
    path.is_absolute()
        && path.components().all(|part| {
            matches!(
                part,
                // A drive or UNC prefix only ever appears on Windows.
                Component::Prefix(_) | Component::RootDir | Component::Normal(_)
            )
        })
}

/// Exact inode identity for custody and freshness proofs, read from live
/// metadata only. A change to any tracked field is a different file: an inode
/// replacement changes `ino`; an in-place write, permission change or relink
/// changes `mode`, `links`, `size`, `mtime`, or the unforgeable status-change
/// time `ctime`. It is evidence for a comparison, never a substitute for
/// re-proving the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileIdentity {
    pub dev: u64,
    pub ino: u64,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub links: u64,
    pub size: u64,
    pub mtime: (i64, i64),
    pub ctime: (i64, i64),
}

impl FileIdentity {
    /// The identity of an open file or directory (`fstat`).
    pub fn of_file(file: &std::fs::File) -> std::io::Result<Self> {
        #[cfg(unix)]
        {
            Ok(Self::of(&file.metadata()?))
        }
        #[cfg(windows)]
        {
            Ok(Self::of_facts(&xcb_platform::file_facts(file)?))
        }
    }

    /// The identity of `path` itself, never a symlink's target (`lstat`).
    pub fn of_path(path: &std::path::Path) -> std::io::Result<Self> {
        #[cfg(unix)]
        {
            Ok(Self::of(&std::fs::symlink_metadata(path)?))
        }
        #[cfg(windows)]
        {
            Ok(Self::of_facts(&xcb_platform::path_facts(path)?))
        }
    }

    /// Windows `Metadata` has no stable file index, so identity comes from
    /// a handle: volume serial, file index, link count, owner, DACL, and the
    /// last-write and change times. `mode` carries the kind and whether the
    /// DACL is private (`0o600`/`0o700`) or not (`0o644`); `uid` is 0 when
    /// this user owns the object.
    #[cfg(windows)]
    fn of_facts(facts: &xcb_platform::Facts) -> Self {
        let kind = match facts.kind {
            xcb_platform::Kind::File => 0o100_000,
            xcb_platform::Kind::Directory => 0o040_000,
            xcb_platform::Kind::ReparsePoint => 0o120_000,
        };
        let permissions = match (facts.private, facts.kind) {
            (true, xcb_platform::Kind::Directory) => 0o700,
            (true, _) => 0o600,
            (false, _) => 0o644,
        };
        let time = |ticks: i64| {
            (
                ticks.div_euclid(10_000_000),
                ticks.rem_euclid(10_000_000) * 100,
            )
        };
        Self {
            dev: facts.volume,
            ino: facts.index,
            mode: kind | permissions,
            uid: u32::from(!facts.owned),
            gid: 0,
            links: facts.links,
            size: facts.len,
            mtime: time(facts.written),
            ctime: time(facts.changed),
        }
    }

    #[cfg(unix)]
    pub fn of(metadata: &std::fs::Metadata) -> Self {
        use std::os::unix::fs::MetadataExt;
        Self {
            dev: metadata.dev(),
            ino: metadata.ino(),
            mode: metadata.mode(),
            uid: metadata.uid(),
            gid: metadata.gid(),
            links: metadata.nlink(),
            size: metadata.len(),
            mtime: (metadata.mtime(), metadata.mtime_nsec()),
            ctime: (metadata.ctime(), metadata.ctime_nsec()),
        }
    }
}
