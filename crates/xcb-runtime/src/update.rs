//! Native xcb update discovery and installation.
//!
//! Updates are deliberately release based.  The updater never follows a
//! moving branch or replaces a package-managed binary.  It asks GitHub for
//! immutable release metadata, then delegates the verified archive download
//! and atomic swap to the installer recorded by `install-native.sh`.

use crate::{Error, Result, now_ms, private};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    process::Command,
};

const API_URL: &str = "https://api.github.com/repos/hraness/xcb/releases?per_page=20";
const MAX_RESPONSE: usize = 2 * 1024 * 1024;
const CHECK_INTERVAL_MS: u64 = 24 * 60 * 60 * 1000;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Policy {
    #[default]
    Notify,
    Auto,
    Disable,
}

impl std::fmt::Display for Policy {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        output.write_str(match self {
            Self::Notify => "notify",
            Self::Auto => "auto",
            Self::Disable => "disable",
        })
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct State {
    pub policy: Policy,
    pub last_check_ms: u64,
    pub available_version: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Status {
    pub version: u32,
    pub current: String,
    pub latest: Option<String>,
    pub asset: Option<String>,
    pub policy: Policy,
    pub checked_at_ms: u64,
    pub release_available: bool,
}

#[derive(Debug, Clone)]
struct Release {
    version: String,
    asset: String,
}

#[derive(Debug, Deserialize)]
struct InstallManifest {
    #[serde(rename = "helperPath")]
    helper_path: PathBuf,
    #[serde(rename = "prefix")]
    prefix: PathBuf,
}

fn state_path(root: &Path) -> PathBuf {
    root.join("update.json")
}

pub fn load(root: &Path) -> Result<State> {
    match private::read(&state_path(root), 16 * 1024) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|_| Error::Unavailable("update state is incompatible with this xcb build")),
        Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(State::default())
        }
        Err(error) => Err(error),
    }
}

fn save(root: &Path, state: &State) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(state)?;
    let path = state_path(root);
    match private::read(&path, 16 * 1024) {
        Ok(previous) => private::replace(&path, &bytes, &crate::digest(previous)),
        Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            private::create(&path, &bytes)
        }
        Err(error) => Err(error),
    }
}

fn version_tuple(version: &str) -> Option<(u64, u64, u64)> {
    let mut parts = version.strip_prefix('v').unwrap_or(version).split('.');
    let tuple = (
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    );
    parts.next().is_none().then_some(tuple)
}

fn platform_asset(version: &str) -> String {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        value => value,
    };
    let arch = std::env::consts::ARCH;
    format!("xcb-{version}-{os}-{arch}.tar.gz")
}

fn release_from_value(value: &Value) -> Option<Release> {
    if value.get("draft")?.as_bool()? || value.get("prerelease")?.as_bool()? {
        return None;
    }
    let version = value
        .get("tag_name")?
        .as_str()?
        .strip_prefix('v')?
        .to_owned();
    version_tuple(&version)?;
    let asset = platform_asset(&version);
    value
        .get("assets")?
        .as_array()?
        .iter()
        .find(|item| item.get("name").and_then(Value::as_str) == Some(asset.as_str()))?;
    value.get("assets")?.as_array()?.iter().find(|item| {
        item.get("name").and_then(Value::as_str) == Some(format!("{asset}.sha256").as_str())
    })?;
    Some(Release { version, asset })
}

fn fetch_releases() -> Result<Vec<Release>> {
    let output = Command::new("curl")
        .args([
            "--fail",
            "--silent",
            "--show-error",
            "--location",
            "--max-time",
            "8",
            "--connect-timeout",
            "3",
            "--proto",
            "=https",
            "--user-agent",
            concat!("xcb-update/", env!("CARGO_PKG_VERSION")),
            API_URL,
        ])
        .output()
        .map_err(|error| {
            Error::Io(std::io::Error::new(
                error.kind(),
                "xcb update needs curl on PATH",
            ))
        })?;
    if !output.status.success() {
        return Err(Error::Io(std::io::Error::other(
            "GitHub release lookup failed; retry xcb update check later",
        )));
    }
    if output.stdout.len() > MAX_RESPONSE {
        return Err(Error::Io(std::io::Error::other(
            "GitHub release response exceeded the xcb update limit",
        )));
    }
    let value: Value = serde_json::from_slice(&output.stdout).map_err(|_| {
        Error::Io(std::io::Error::other(
            "GitHub returned invalid release metadata",
        ))
    })?;
    let releases = value.as_array().ok_or_else(|| {
        Error::Io(std::io::Error::other(
            "GitHub returned an invalid release list",
        ))
    })?;
    Ok(releases.iter().filter_map(release_from_value).collect())
}

fn latest_release() -> Result<Option<Release>> {
    Ok(fetch_releases()?
        .into_iter()
        .max_by_key(|release| version_tuple(&release.version).unwrap_or((0, 0, 0))))
}

pub fn status(root: &Path, current: &str) -> Result<Status> {
    let state = load(root)?;
    let latest = latest_release()?;
    let latest_version = latest.as_ref().map(|release| release.version.clone());
    let release_available = latest
        .as_ref()
        .is_some_and(|release| version_tuple(&release.version) > version_tuple(current));
    Ok(Status {
        version: 1,
        current: current.to_owned(),
        latest: latest_version,
        asset: latest.as_ref().map(|release| release.asset.clone()),
        policy: state.policy,
        checked_at_ms: now_ms(),
        release_available,
    })
}

pub fn check(root: &Path, current: &str, quiet: bool) -> Result<Status> {
    let mut state = load(root)?;
    let latest = latest_release()?;
    let latest_version = latest.as_ref().map(|release| release.version.clone());
    state.last_check_ms = now_ms();
    state.available_version = latest_version.clone();
    save(root, &state)?;
    let release_available = latest
        .as_ref()
        .is_some_and(|release| version_tuple(&release.version) > version_tuple(current));
    let result = Status {
        version: 1,
        current: current.to_owned(),
        latest: latest_version,
        asset: latest.map(|release| release.asset),
        policy: state.policy,
        checked_at_ms: state.last_check_ms,
        release_available,
    };
    if !quiet {
        match (&result.latest, result.release_available) {
            (Some(version), true) => eprintln!(
                "xcb: xcb {version} is available (current {current}); run xcb upgrade to install it."
            ),
            (Some(version), false) => eprintln!(
                "xcb: current {current} is up to date (latest verified release {version})."
            ),
            (None, _) => eprintln!(
                "xcb: no verified native release is published yet; source install remains current."
            ),
        }
    }
    Ok(result)
}

pub fn set_policy(root: &Path, policy: Policy) -> Result<State> {
    let mut state = load(root)?;
    state.policy = policy;
    save(root, &state)?;
    Ok(state)
}

pub fn should_check(root: &Path) -> Result<bool> {
    let state = load(root)?;
    Ok(state.policy != Policy::Disable
        && now_ms().saturating_sub(state.last_check_ms) >= CHECK_INTERVAL_MS)
}

fn manifest(root: &Path) -> Result<InstallManifest> {
    let mut paths = vec![root.join("install.json")];
    if let Ok(binary) = std::env::current_exe()
        && let Some(prefix) = binary.parent().and_then(Path::parent)
    {
        paths.push(prefix.join("share/xcb/install.json"));
    }
    for path in paths {
        match private::read(&path, 16 * 1024) {
            Ok(bytes) => return serde_json::from_slice(&bytes).map_err(|_| {
                Error::Unavailable(
                    "global install metadata is invalid; reinstall with scripts/install-native.sh",
                )
            }),
            Err(Error::Io(io)) if io.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        }
    }
    Err(Error::Unavailable(
        "global install metadata is missing; reinstall with scripts/install-native.sh to enable xcb upgrade",
    ))
}

pub fn upgrade(root: &Path, current: &str, requested: Option<&str>, quiet: bool) -> Result<i32> {
    let releases = fetch_releases()?;
    let release = match requested {
        Some(requested) => releases
            .into_iter()
            .find(|release| release.version == requested.strip_prefix('v').unwrap_or(requested)),
        None => releases
            .into_iter()
            .max_by_key(|release| version_tuple(&release.version).unwrap_or((0, 0, 0))),
    }
    .ok_or_else(|| {
        Error::Io(std::io::Error::other(
            "no verified native xcb release matches that version and this platform",
        ))
    })?;
    if requested.is_none() && version_tuple(&release.version) <= version_tuple(current) {
        if !quiet {
            eprintln!("xcb: current {current} is already up to date.");
        }
        return Ok(0);
    }
    let install = manifest(root)?;
    let metadata = std::fs::symlink_metadata(&install.helper_path).map_err(|_| {
        Error::Unavailable(
            "the recorded xcb installer is missing; reinstall with scripts/install-native.sh",
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(Error::PrivateState);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(Error::Unavailable(
                "the recorded xcb installer is not executable; reinstall with scripts/install-native.sh",
            ));
        }
    }
    let status = Command::new(&install.helper_path)
        .env("XCB_VERSION", &release.version)
        .env("XCB_INSTALL_PREFIX", &install.prefix)
        .env("XCB_ADD_PATH", "no")
        .status()?;
    if !status.success() {
        return Err(Error::Io(std::io::Error::other(format!(
            "xcb installer exited with {status}"
        ))));
    }
    if !quiet {
        eprintln!(
            "xcb: upgraded to {}; restart open terminals and run xcb doctor.",
            release.version
        );
    }
    Ok(0)
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Install or remove the per-user macOS scheduler. The scheduler invokes the
/// already-installed binary once a day; it never runs a shell or follows a
/// project-local setting.
pub fn configure_scheduler(binary: &Path, enabled: bool) -> Result<()> {
    if std::env::consts::OS != "macos" {
        return Err(Error::Unavailable(
            "automatic scheduling is currently supported on macOS only; use xcb update check from your user timer on Linux",
        ));
    }
    let home = std::env::var_os("HOME").ok_or(Error::PrivateState)?;
    let home = PathBuf::from(home);
    let agents = home.join("Library/LaunchAgents");
    let plist = agents.join("dev.hraness.xcb.update.plist");
    let label = "dev.hraness.xcb.update";
    let uid = Command::new("id").arg("-u").output().map_err(|_| {
        Error::Unavailable("could not determine the current user for the xcb scheduler")
    })?;
    let uid = String::from_utf8(uid.stdout)
        .map_err(|_| Error::Unavailable("current user id was not UTF-8"))?
        .trim()
        .to_owned();
    let domain = format!("gui/{uid}");
    let bootout = || {
        let _ = Command::new("launchctl")
            .args(["bootout", &domain, &plist.to_string_lossy()])
            .status();
    };
    if !enabled {
        bootout();
        match std::fs::symlink_metadata(&plist) {
            Ok(meta) if meta.file_type().is_symlink() || !meta.is_file() => {
                return Err(Error::PrivateState);
            }
            Ok(_) => std::fs::remove_file(&plist)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
        return Ok(());
    }
    std::fs::create_dir_all(&agents)?;
    if std::fs::symlink_metadata(&agents)?.file_type().is_symlink() {
        return Err(Error::PrivateState);
    }
    if let Ok(meta) = std::fs::symlink_metadata(&plist)
        && (meta.file_type().is_symlink() || !meta.is_file())
    {
        return Err(Error::PrivateState);
    }
    let binary = binary.to_str().ok_or(Error::PrivateState)?;
    let home = home.to_str().ok_or(Error::PrivateState)?;
    let body = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict><key>Label</key><string>{label}</string><key>ProgramArguments</key><array><string>{}</string><string>update</string><string>daemon</string><string>--quiet</string></array><key>EnvironmentVariables</key><dict><key>HOME</key><string>{}</string></dict><key>RunAtLoad</key><true/><key>StartInterval</key><integer>86400</integer><key>ProcessType</key><string>Background</string><key>StandardOutPath</key><string>/dev/null</string><key>StandardErrorPath</key><string>/dev/null</string></dict></plist>\n",
        xml_escape(binary),
        xml_escape(home)
    );
    let temp = agents.join(format!(
        ".dev.hraness.xcb.update.{}.tmp",
        std::process::id()
    ));
    std::fs::write(&temp, body.as_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&temp, &plist)?;
    bootout();
    let status = Command::new("launchctl")
        .args(["bootstrap", &domain, &plist.to_string_lossy()])
        .status()?;
    if !status.success() {
        return Err(Error::Io(std::io::Error::other(
            "launchctl could not load the xcb update scheduler",
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_selection_requires_platform_binary_and_checksum() {
        let version = "0.5.0";
        let binary = platform_asset(version);
        let value = serde_json::json!({
            "tag_name": "v0.5.0", "draft": false, "prerelease": false,
            "assets": [{"name": binary}, {"name": format!("{binary}.sha256")}]
        });
        assert_eq!(release_from_value(&value).unwrap().version, version);
        let missing =
            serde_json::json!({"tag_name":"v0.5.0","draft":false,"prerelease":false,"assets":[]});
        assert!(release_from_value(&missing).is_none());
    }

    #[test]
    fn versions_are_strict_stable_semver() {
        assert_eq!(version_tuple("v1.2.3"), Some((1, 2, 3)));
        assert!(version_tuple("1.2").is_none());
        assert!(version_tuple("1.2.3-beta").is_none());
    }
}
