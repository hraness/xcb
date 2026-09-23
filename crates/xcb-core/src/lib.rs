pub mod models;
pub mod panes;
pub mod policy;
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
    Devin,
}

impl Provider {
    pub const ALL: [Self; 3] = [Self::Devin, Self::Claude, Self::Codex];
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
        Self::ALL
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

/// Absolute path made only of the root and ordinary components — no `.`,
/// `..`, or platform prefix segments. Symlinks are not resolved here.
pub fn absolute_clean(path: &std::path::Path) -> bool {
    use std::path::Component;
    path.is_absolute()
        && path
            .components()
            .all(|part| matches!(part, Component::RootDir | Component::Normal(_)))
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
