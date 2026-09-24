//! External editing owns only its private scratch directory; callers own terminal restoration.
use crate::composer::MAX_INPUT;
use rustix::fs::{Mode, OFlags};
use std::{
    env, fmt,
    fs::{self, DirBuilder, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::PathBuf,
    process::Command,
};

#[derive(Debug)]
pub enum EditorError {
    NotConfigured,
    InvalidCommand,
    InputTooLarge,
    Failed,
    InvalidOutput,
    Io(io::Error),
}
impl fmt::Display for EditorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NotConfigured => "Set VISUAL or EDITOR to open an external editor",
            Self::InvalidCommand => "The editor command has invalid quoting or arguments",
            Self::InputTooLarge => "The draft exceeds 256 KiB",
            Self::Failed => "The editor did not finish successfully; the draft is unchanged",
            Self::InvalidOutput => {
                "Editor output must be a private UTF-8 file under 256 KiB; the draft is unchanged"
            }
            Self::Io(_) => {
                "The editor could not open or read its temporary file; the draft is unchanged"
            }
        })
    }
}
impl std::error::Error for EditorError {}
impl From<io::Error> for EditorError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

pub fn edit(text: &str) -> Result<String, EditorError> {
    for name in ["VISUAL", "EDITOR"] {
        if let Some(value) = env::var_os(name) {
            let command = value.to_str().ok_or(EditorError::InvalidCommand)?;
            if !command.trim().is_empty() {
                return edit_with_command(command, text);
            }
        }
    }
    Err(EditorError::NotConfigured)
}

/// Parse quoted argv and append the scratch filename. No shell is implicitly invoked.
/// A rejected edit never modifies the caller's original draft.
pub fn edit_with_command(command: &str, text: &str) -> Result<String, EditorError> {
    if text.len() > MAX_INPUT {
        return Err(EditorError::InputTooLarge);
    }
    let args = parse_command(command)?;
    let scratch = Scratch::new()?;
    let path = scratch.directory.join("prompt.txt");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    file.write_all(text.as_bytes())?;
    file.sync_all()?;
    drop(file);
    let status = Command::new(&args[0])
        .args(&args[1..])
        .arg(&path)
        .status()
        .map_err(|_| EditorError::Failed)?;
    if !status.success() {
        return Err(EditorError::Failed);
    }
    let directory_metadata = fs::symlink_metadata(&scratch.directory)?;
    if !directory_metadata.is_dir()
        || (directory_metadata.dev(), directory_metadata.ino()) != scratch.identity
    {
        return Err(EditorError::InvalidOutput);
    }
    let fd = rustix::fs::openat(
        &scratch.handle,
        "prompt.txt",
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| EditorError::InvalidOutput)?;
    let file = File::from(fd);
    let opened = file.metadata()?;
    if !valid_output(&opened) {
        return Err(EditorError::InvalidOutput);
    }
    let mut bytes = Vec::new();
    (&file).take(MAX_INPUT as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > MAX_INPUT
        || xcb_core::FileIdentity::of(&file.metadata()?) != xcb_core::FileIdentity::of(&opened)
    {
        return Err(EditorError::InvalidOutput);
    }
    String::from_utf8(bytes).map_err(|_| EditorError::InvalidOutput)
}

fn valid_output(metadata: &fs::Metadata) -> bool {
    metadata.is_file()
        && metadata.uid() == rustix::process::geteuid().as_raw()
        && metadata.mode() & 0o077 == 0
        && metadata.nlink() == 1
        && metadata.len() <= MAX_INPUT as u64
}

struct Scratch {
    directory: PathBuf,
    identity: (u64, u64),
    handle: File,
}
impl Scratch {
    fn new() -> io::Result<Self> {
        let directory = env::temp_dir().join(format!("xcb-editor-{}", uuid::Uuid::new_v4()));
        DirBuilder::new().mode(0o700).create(&directory)?;
        let metadata = fs::symlink_metadata(&directory)?;
        let handle = File::from(rustix::fs::open(
            &directory,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )?);
        Ok(Self {
            directory,
            identity: (metadata.dev(), metadata.ino()),
            handle,
        })
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        if fs::symlink_metadata(&self.directory)
            .is_ok_and(|meta| meta.is_dir() && (meta.dev(), meta.ino()) == self.identity)
        {
            // Never recursively delete editor-created backups or follow a replacement directory.
            let _ = fs::remove_file(self.directory.join("prompt.txt"));
            let _ = fs::remove_dir(&self.directory);
        }
    }
}

fn parse_command(command: &str) -> Result<Vec<String>, EditorError> {
    if command.len() > 8192 || command.chars().any(|ch| ch.is_control() && ch != '\t') {
        return Err(EditorError::InvalidCommand);
    }
    let mut args = Vec::new();
    let mut argument = String::new();
    let mut quote = None;
    let mut started = false;
    let mut chars = command.chars();
    while let Some(ch) = chars.next() {
        match (quote, ch) {
            (Some('\''), '\'') | (Some('"'), '"') => quote = None,
            (Some('\''), _) => argument.push(ch),
            (_, '\\') => {
                let escaped = chars.next().ok_or(EditorError::InvalidCommand)?;
                // In double quotes, preserve a backslash before an ordinary character.
                if quote == Some('"') && !matches!(escaped, '"' | '\\' | '$' | '`') {
                    argument.push('\\');
                }
                argument.push(escaped);
                started = true;
            }
            (None, '\'' | '"') => {
                quote = Some(ch);
                started = true;
            }
            (None, ch) if ch.is_whitespace() => {
                if started {
                    args.push(std::mem::take(&mut argument));
                    started = false;
                    if args.len() > 64 {
                        return Err(EditorError::InvalidCommand);
                    }
                }
            }
            _ => {
                argument.push(ch);
                started = true;
            }
        }
    }
    if quote.is_some() {
        return Err(EditorError::InvalidCommand);
    }
    if started {
        args.push(argument);
    }
    if args.is_empty() || args[0].is_empty() || args.len() > 64 {
        return Err(EditorError::InvalidCommand);
    }
    Ok(args)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quoted_argv_does_not_expand_shell_syntax() {
        assert_eq!(
            parse_command("'editor path' --wait \"$HOME `whoami`\" '*.txt'").unwrap(),
            ["editor path", "--wait", "$HOME `whoami`", "*.txt"]
        );
        assert!(parse_command("editor 'bad").is_err());
    }
    #[test]
    fn round_trip_and_failure_keep_original_owned_by_caller() {
        let original = "draft λ\n";
        assert_eq!(
            edit_with_command("/usr/bin/true", original).unwrap(),
            original
        );
        assert!(matches!(
            edit_with_command("/usr/bin/false", original),
            Err(EditorError::Failed)
        ));
        assert_eq!(original, "draft λ\n");
    }
    #[test]
    fn editor_replacements_are_bounded_regular_utf8_files() {
        assert!(
            matches!(edit_with_command("/bin/sh -c 'printf edited > \"$1\"' sh", "draft"), Ok(text) if text == "edited")
        );
        assert!(edit_with_command("/bin/sh -c 'printf \"\\377\" > \"$1\"' sh", "draft").is_err());
        assert!(
            edit_with_command(
                "/bin/sh -c 'rm \"$1\"; ln -s /etc/passwd \"$1\"' sh",
                "draft"
            )
            .is_err()
        );
        assert!(
            edit_with_command(
                "/bin/sh -c 'dd if=/dev/zero of=\"$1\" bs=1024 count=257 2>/dev/null' sh",
                "draft"
            )
            .is_err()
        );
    }
}
