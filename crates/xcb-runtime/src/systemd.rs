//! systemd user units, the Linux counterpart of the macOS LaunchAgents.
//!
//! xcb writes its units to `~/.config/systemd/user`, the folder the user
//! manager searches by default, and drives them with `systemctl --user`.
//! Every value xcb puts in a unit is data: arguments are quoted, systemd's
//! `%` specifiers and `$` expansion are escaped, and control characters are
//! refused, so no path can add a directive or change a command.
use crate::{Error, Result};
use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

/// The user unit folder under `home`.
pub fn unit_dir(home: &Path) -> PathBuf {
    home.join(".config/systemd/user")
}

/// The folders between `home` and [`unit_dir`], outermost first, so callers
/// can check each one before creating the next.
pub(crate) fn unit_dir_chain(home: &Path) -> [PathBuf; 3] {
    [
        home.join(".config"),
        home.join(".config/systemd"),
        unit_dir(home),
    ]
}

fn text(value: &OsStr) -> Result<&str> {
    let value = value.to_str().ok_or(Error::PrivateState)?;
    if value.chars().any(char::is_control) {
        return Err(Error::PrivateState);
    }
    Ok(value)
}

/// One `ExecStart=` argument: double-quoted, with `\`, `"`, `%` and `$`
/// escaped.
pub(crate) fn exec_arg(value: impl AsRef<OsStr>) -> Result<String> {
    let value = text(value.as_ref())?;
    Ok(format!(
        "\"{}\"",
        value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('%', "%%")
            .replace('$', "$$")
    ))
}

/// One `Environment=` assignment. systemd does not expand `$` there.
pub(crate) fn environment(name: &str, value: impl AsRef<OsStr>) -> Result<String> {
    let value = text(value.as_ref())?;
    Ok(format!(
        "Environment=\"{name}={}\"",
        value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('%', "%%")
    ))
}

/// A path used verbatim after a directive such as `StandardOutput=append:`.
pub(crate) fn path(value: &Path) -> Result<String> {
    let value = text(value.as_os_str())?;
    if !value.starts_with('/') || value != value.trim() {
        return Err(Error::PrivateState);
    }
    Ok(value.replace('%', "%%"))
}

/// Whether the unit file at `path` begins with `marker` as its own line.
/// The file is opened without following a symlink, and a large file is
/// never read past the marker.
pub(crate) fn has_marker(path: &Path, marker: &str) -> Result<bool> {
    use std::{io::Read, os::unix::fs::OpenOptionsExt};
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags((rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32)
        .open(path)?;
    let expected = format!("{marker}\n");
    let mut head = Vec::new();
    file.take(expected.len() as u64).read_to_end(&mut head)?;
    Ok(head == expected.as_bytes())
}

/// Run `systemctl --user <args>` quietly and report whether it succeeded.
/// A missing `systemctl` or user manager counts as failure.
pub(crate) fn systemctl(args: &[&str]) -> bool {
    Command::new("systemctl")
        .arg("--user")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Whether systemd keeps this user's services running while they are logged
/// out (`loginctl enable-linger`). `None` when the user name is unknown.
/// Without lingering, a supervisor on a headless host stops at logout and
/// starts again only at the next login.
pub fn lingering() -> Option<bool> {
    let user = std::env::var_os("USER").or_else(|| std::env::var_os("LOGNAME"))?;
    let user = user.to_str()?;
    if user.is_empty() || user.contains('/') {
        return None;
    }
    Some(Path::new("/var/lib/systemd/linger").join(user).exists())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_are_quoted_data() {
        assert_eq!(
            exec_arg("/home/a b/%h/$HOME/\"q\"\\").unwrap(),
            r#""/home/a b/%%h/$$HOME/\"q\"\\""#
        );
        assert_eq!(
            environment("HOME", "/home/%u/$x").unwrap(),
            r#"Environment="HOME=/home/%%u/$x""#
        );
        assert_eq!(
            path(Path::new("/home/a b/%n.log")).unwrap(),
            "/home/a b/%%n.log"
        );
        for bad in ["/home/a\nExecStart=/bin/sh", "/home/\u{7f}", "/tab\there"] {
            assert!(exec_arg(bad).is_err(), "{bad:?}");
            assert!(environment("HOME", bad).is_err(), "{bad:?}");
            assert!(path(Path::new(bad)).is_err(), "{bad:?}");
        }
        assert!(path(Path::new("relative.log")).is_err());
        assert!(path(Path::new("/trailing ")).is_err());
    }

    #[test]
    fn the_unit_folder_is_the_default_user_search_path() {
        let home = Path::new("/home/me");
        assert_eq!(unit_dir(home), Path::new("/home/me/.config/systemd/user"));
        assert_eq!(unit_dir_chain(home)[2], unit_dir(home));
        assert!(
            unit_dir_chain(home)
                .windows(2)
                .all(|pair| pair[1].starts_with(&pair[0]))
        );
    }
}
