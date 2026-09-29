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
    process::{Command, Stdio},
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

#[derive(Debug, Serialize)]
pub struct UpgradeResult {
    pub version: u32,
    pub previous: String,
    pub current: String,
    pub changed: bool,
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
    /// Written by every installer since the manifest existed; older
    /// manifests fall back to `<prefix>/bin/xcb`.
    #[serde(rename = "binaryPath", default)]
    binary_path: Option<PathBuf>,
}

/// What `install-native.sh` recorded about one global install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallRecord {
    /// The `install.json` this record came from.
    pub manifest: PathBuf,
    pub prefix: PathBuf,
    /// The installer copy `xcb upgrade` runs.
    pub helper: PathBuf,
    /// The installed `xcb` binary.
    pub binary: PathBuf,
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

/// The release archive's extension: a zip on Windows, a gzipped tar elsewhere.
const ARCHIVE: &str = if cfg!(windows) { "zip" } else { "tar.gz" };

/// The installer `xcb upgrade` delegates to, as the reinstall guidance names it.
macro_rules! reinstall {
    ($message:literal) => {
        if cfg!(windows) {
            concat!($message, "; reinstall with scripts/install.ps1")
        } else {
            concat!($message, "; reinstall with scripts/install-native.sh")
        }
    };
    ($message:literal, $rest:literal) => {
        if cfg!(windows) {
            concat!($message, "; reinstall with scripts/install.ps1", $rest)
        } else {
            concat!(
                $message,
                "; reinstall with scripts/install-native.sh",
                $rest
            )
        }
    };
}

/// Where an installer puts the binary and itself under its prefix.
fn install_layout(prefix: &Path) -> (PathBuf, PathBuf) {
    let share = prefix.join("share").join("xcb");
    if cfg!(windows) {
        (
            prefix.join("bin").join("xcb.exe"),
            share.join("install.ps1"),
        )
    } else {
        (prefix.join("bin/xcb"), share.join("install-native.sh"))
    }
}

/// The release platform this build installs, such as `linux-aarch64`: the
/// `<os>-<arch>` part of `xcb-<version>-<os>-<arch>.tar.gz` (`.zip` on
/// Windows).
pub fn platform() -> String {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        value => value,
    };
    format!("{os}-{}", std::env::consts::ARCH)
}

fn platform_asset(version: &str) -> String {
    format!("xcb-{version}-{}.{ARCHIVE}", platform())
}

/// The version of a published, stable release entry, whatever it carries.
fn stable_version(value: &Value) -> Option<String> {
    if value.get("draft")?.as_bool()? || value.get("prerelease")?.as_bool()? {
        return None;
    }
    let version = value
        .get("tag_name")?
        .as_str()?
        .strip_prefix('v')?
        .to_owned();
    version_tuple(&version)?;
    Some(version)
}

/// The refusal for a release that exists but carries no archive for this
/// host, so an update never reports it as a network or install failure.
fn no_platform_build(version: Option<&str>) -> Error {
    let platform = platform();
    let message = match version {
        Some(version) => format!("xcb {version} has no release build for {platform}"),
        None => format!("no xcb release has a build for {platform} yet"),
    };
    Error::guided(
        format!("{message}; build from source to update this install"),
        "https://xcb.sh/install#source",
    )
}

fn release_from_value(value: &Value) -> Option<Release> {
    let version = stable_version(value)?;
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

fn fetch_release_metadata(url: &str) -> Result<Value> {
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
            url,
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
    serde_json::from_slice(&output.stdout).map_err(|_| {
        Error::Io(std::io::Error::other(
            "GitHub returned invalid release metadata",
        ))
    })
}

/// The newest stable release carrying this platform's archive, and the
/// newest stable release of any kind (to tell "no release" from "no build
/// for this platform").
#[derive(Debug, Default)]
struct Latest {
    release: Option<Release>,
    newest: Option<String>,
}

fn latest_from_list(value: &Value) -> Result<Latest> {
    let releases = value.as_array().ok_or_else(|| {
        Error::Io(std::io::Error::other(
            "GitHub returned an invalid release list",
        ))
    })?;
    let order = |version: &String| version_tuple(version).unwrap_or((0, 0, 0));
    Ok(Latest {
        release: releases
            .iter()
            .filter_map(release_from_value)
            .max_by_key(|release| order(&release.version)),
        newest: releases.iter().filter_map(stable_version).max_by_key(order),
    })
}

fn requested_release_with(
    requested: &str,
    fetch: impl FnOnce(&str) -> Result<Value>,
) -> Result<Option<Release>> {
    let version = requested.strip_prefix('v').unwrap_or(requested);
    let canonical =
        version_tuple(version).map(|(major, minor, patch)| format!("{major}.{minor}.{patch}"));
    if canonical.as_deref() != Some(version) {
        return Err(Error::guided(
            "use an exact release version, such as 0.10.1",
            "xcb upgrade --help",
        ));
    }
    let url = format!("https://api.github.com/repos/hraness/xcb/releases/tags/v{version}");
    let value = fetch(&url)?;
    if stable_version(&value).as_deref() != Some(version) {
        return Ok(None);
    }
    release_from_value(&value)
        .map(Some)
        .ok_or_else(|| no_platform_build(Some(version)))
}

fn latest() -> Result<Latest> {
    latest_from_list(&fetch_release_metadata(API_URL)?)
}

pub fn status(root: &Path, current: &str) -> Result<Status> {
    let state = load(root)?;
    let latest = latest()?.release;
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
    let Latest {
        release: latest,
        newest,
    } = latest()?;
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
            (None, _) if newest.is_some() => eprintln!(
                "xcb: no release has a build for {} yet; build from source to update.",
                platform()
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

/// The first install manifest found: the state root's (the default state
/// root is the installer's share directory), then the one beside the
/// running binary.
fn find_manifest(root: &Path) -> Result<Option<(PathBuf, InstallManifest)>> {
    let mut paths = vec![root.join("install.json")];
    if let Ok(binary) = std::env::current_exe()
        && let Some(prefix) = binary.parent().and_then(Path::parent)
    {
        paths.push(prefix.join("share").join("xcb").join("install.json"));
    }
    for path in paths {
        match private::read(&path, 16 * 1024) {
            Ok(bytes) => {
                let manifest = serde_json::from_slice(&bytes).map_err(|_| {
                    Error::Unavailable(reinstall!("global install metadata is invalid"))
                })?;
                return Ok(Some((path, manifest)));
            }
            Err(Error::Io(io)) if io.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(None)
}

fn manifest(root: &Path) -> Result<InstallManifest> {
    find_manifest(root)?
        .map(|(_, manifest)| manifest)
        .ok_or(Error::Unavailable(reinstall!(
            "global install metadata is missing",
            " to enable xcb upgrade"
        )))
}

/// The install `install-native.sh` recorded, if any. Paths must be the
/// installer's own layout (`<prefix>/bin/xcb`, `<prefix>/share/xcb/...`);
/// anything else is refused rather than trusted for removal.
pub fn install_record(root: &Path) -> Result<Option<InstallRecord>> {
    let Some((manifest, recorded)) = find_manifest(root)? else {
        return Ok(None);
    };
    let prefix = recorded.prefix;
    let (expected_binary, expected_helper) = install_layout(&prefix);
    let binary = recorded
        .binary_path
        .unwrap_or_else(|| expected_binary.clone());
    let plain = |path: &Path| {
        path.is_absolute()
            && !path.components().any(|component| {
                matches!(
                    component,
                    std::path::Component::ParentDir | std::path::Component::CurDir
                )
            })
    };
    if !plain(&prefix) || binary != expected_binary || recorded.helper_path != expected_helper {
        return Err(Error::Unavailable(
            "the install record names paths outside its install prefix; remove xcb by hand",
        ));
    }
    Ok(Some(InstallRecord {
        manifest,
        prefix,
        helper: recorded.helper_path,
        binary,
    }))
}

/// Refuse to install an older release unless the caller asked for it.
pub fn check_downgrade(current: &str, requested: &str, allowed: bool) -> Result<()> {
    let requested = requested.strip_prefix('v').unwrap_or(requested);
    match (version_tuple(requested), version_tuple(current)) {
        (Some(wanted), Some(running)) if wanted < running && !allowed => Err(Error::guided(
            format!(
                "xcb {requested} is older than the installed {current}; installing it would downgrade xcb"
            ),
            format!("xcb upgrade {requested} --allow-downgrade"),
        )),
        _ => Ok(()),
    }
}

pub fn upgrade(
    root: &Path,
    current: &str,
    requested: Option<&str>,
    quiet: bool,
    allow_downgrade: bool,
) -> Result<UpgradeResult> {
    if let Some(requested) = requested {
        check_downgrade(current, requested, allow_downgrade)?;
    }
    let release = match requested {
        Some(requested) => {
            requested_release_with(requested, fetch_release_metadata)?.ok_or_else(|| {
                Error::Io(std::io::Error::other(
                    "no verified native xcb release matches that version",
                ))
            })?
        }
        None => {
            let latest = latest()?;
            match latest.release {
                Some(release) => release,
                None if latest.newest.is_some() => return Err(no_platform_build(None)),
                None => {
                    return Err(Error::Io(std::io::Error::other(
                        "no verified native xcb release is published yet",
                    )));
                }
            }
        }
    };
    if requested.is_none() && version_tuple(&release.version) <= version_tuple(current) {
        if !quiet {
            eprintln!("xcb: current {current} is already up to date.");
        }
        return Ok(UpgradeResult {
            version: 1,
            previous: current.to_owned(),
            current: current.to_owned(),
            changed: false,
        });
    }
    let install = manifest(root)?;
    let metadata = std::fs::symlink_metadata(&install.helper_path)
        .map_err(|_| Error::Unavailable(reinstall!("the recorded xcb installer is missing")))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(Error::PrivateState);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(Error::Unavailable(reinstall!(
                "the recorded xcb installer is not executable"
            )));
        }
    }
    #[cfg(unix)]
    let mut command = Command::new(&install.helper_path);
    // The installer renames the running xcb.exe aside, which Windows allows,
    // and moves the new binary into its place.
    #[cfg(windows)]
    let mut command = {
        if install.helper_path.extension() != Some(std::ffi::OsStr::new("ps1")) {
            return Err(Error::Unavailable(reinstall!(
                "the recorded xcb installer is not install.ps1"
            )));
        }
        let mut command = Command::new("powershell.exe");
        command
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-File",
            ])
            .arg(&install.helper_path);
        command
    };
    command
        .env("XCB_VERSION", &release.version)
        .env("XCB_INSTALL_PREFIX", &install.prefix)
        .env("XCB_ADD_PATH", "no");
    // JSON callers own stdout. Keep installer diagnostics on stderr without
    // buffering an unbounded download/build log in memory.
    if quiet {
        command.stdout(Stdio::from(std::io::stderr()));
    }
    let status = command.status()?;
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
    Ok(UpgradeResult {
        version: 1,
        previous: current.to_owned(),
        current: release.version,
        changed: true,
    })
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

const SCHEDULER_LABEL: &str = "dev.hraness.xcb.update";
/// The systemd user units on Linux: `xcb-update.timer` starts
/// `xcb-update.service`.
const SYSTEMD_UNIT: &str = "xcb-update";
/// The first line of each systemd unit xcb writes. A unit without it is
/// someone else's and is never replaced or removed.
const SYSTEMD_MARKER: &str = "# Written by xcb update enable; xcb update disable removes it.";
const SYSTEMD: bool = cfg!(target_os = "linux");

/// Platforms with a built-in daily update check.
const SCHEDULER_PLATFORM: bool = cfg!(any(target_os = "macos", target_os = "linux"));

/// Whether xcb can install the daily update check here: a LaunchAgent on
/// macOS, or a systemd user timer on Linux when a systemd user manager
/// answers (a container or a host without systemd has none).
pub fn scheduler_supported() -> bool {
    if SYSTEMD {
        crate::systemd::user_manager()
    } else {
        SCHEDULER_PLATFORM
    }
}

/// Where the daily update check lives under `home`: the LaunchAgent plist
/// on macOS, the timer unit on Linux.
pub fn scheduler_path(home: &Path) -> PathBuf {
    if SYSTEMD {
        crate::systemd::unit_dir(home).join(format!("{SYSTEMD_UNIT}.timer"))
    } else {
        home.join("Library/LaunchAgents")
            .join(format!("{SCHEDULER_LABEL}.plist"))
    }
}

/// Every file of the daily update check under `home`.
fn scheduler_files(home: &Path) -> Vec<PathBuf> {
    let mut files = vec![scheduler_path(home)];
    if SYSTEMD {
        files.push(crate::systemd::unit_dir(home).join(format!("{SYSTEMD_UNIT}.service")));
    }
    files
}

fn launchd_domain() -> String {
    // launchd exists only on macOS; the scheduler never runs elsewhere.
    #[cfg(unix)]
    let uid = rustix::process::getuid().as_raw();
    #[cfg(not(unix))]
    let uid = 0;
    format!("gui/{uid}")
}

/// Remove the daily update check under `home` when it is there, and report
/// whether it was. A missing check is not an error, so this works on every
/// platform; it never touches a file it did not write.
pub fn remove_scheduler(home: &Path) -> Result<bool> {
    let removed = remove_scheduler_with(home, |plist| {
        // The agent may already be unloaded; removing the file is what
        // keeps it from loading at the next login.
        if SYSTEMD {
            crate::systemd::systemctl(&["disable", "--now", &format!("{SYSTEMD_UNIT}.timer")]);
            return;
        }
        let _ = Command::new("/bin/launchctl")
            .args(["bootout", &launchd_domain(), &plist.to_string_lossy()])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    })?;
    if removed && SYSTEMD {
        crate::systemd::systemctl(&["daemon-reload"]);
    }
    Ok(removed)
}

/// [`remove_scheduler`] with the unload step supplied, so tests never reach
/// the user's real launchd session or systemd user manager.
pub fn remove_scheduler_with(home: &Path, unload: impl FnOnce(&Path)) -> Result<bool> {
    let mut found = Vec::new();
    for file in scheduler_files(home) {
        match std::fs::symlink_metadata(&file) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
            Ok(meta) if meta.file_type().is_symlink() || !meta.is_file() => {
                return Err(Error::PrivateState);
            }
            Ok(_) => {}
        }
        if SYSTEMD && !written_by_xcb(&file)? {
            return Err(Error::Conflict(
                "a systemd unit named xcb-update was not written by xcb; it was left in place",
            ));
        }
        found.push(file);
    }
    let Some(first) = found.first() else {
        return Ok(false);
    };
    if SCHEDULER_PLATFORM {
        unload(first);
    }
    for file in &found {
        std::fs::remove_file(file)?;
    }
    Ok(true)
}

fn written_by_xcb(unit: &Path) -> Result<bool> {
    crate::systemd::has_marker(unit, SYSTEMD_MARKER)
}

/// The systemd service and timer for the daily update check. The timer runs
/// the check five minutes after the user manager starts and then once a
/// day; the check itself skips a run within a day of the last one.
fn systemd_units(binary: &Path, home: &Path) -> Result<(String, String)> {
    use crate::systemd::{environment, exec_arg};
    let service = format!(
        "{SYSTEMD_MARKER}\n\
         [Unit]\n\
         Description=xcb daily update check\n\
         \n\
         [Service]\n\
         Type=oneshot\n\
         ExecStart={} update daemon --quiet\n\
         {}\n\
         StandardOutput=null\n\
         StandardError=null\n",
        exec_arg(binary)?,
        environment("HOME", home)?
    );
    let timer = format!(
        "{SYSTEMD_MARKER}\n\
         [Unit]\n\
         Description=xcb daily update check\n\
         \n\
         [Timer]\n\
         OnStartupSec=5min\n\
         OnUnitActiveSec=1d\n\
         \n\
         [Install]\n\
         WantedBy=timers.target\n"
    );
    Ok((service, timer))
}

/// Write one file of the daily update check, replacing only a file xcb
/// wrote.
fn write_unit(path: &Path, body: &str) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
        Ok(meta) if meta.file_type().is_symlink() || !meta.is_file() => {
            return Err(Error::PrivateState);
        }
        Ok(_) if !written_by_xcb(path)? => {
            return Err(Error::Conflict(
                "a systemd unit named xcb-update was not written by xcb; it was left in place",
            ));
        }
        Ok(_) => {}
    }
    let parent = path.parent().ok_or(Error::PrivateState)?;
    let mut staged = tempfile::NamedTempFile::new_in(parent)?;
    std::io::Write::write_all(&mut staged, body.as_bytes())?;
    staged.as_file().sync_all()?;
    staged
        .persist(path)
        .map_err(|error| Error::Io(error.error))?;
    Ok(())
}

fn install_systemd_scheduler(binary: &Path, home: &Path) -> Result<()> {
    for directory in crate::systemd::unit_dir_chain(home) {
        crate::habitat_service::private_directory(&directory)?;
    }
    let (service, timer) = systemd_units(binary, home)?;
    let files = scheduler_files(home);
    // Check both before writing either, so a foreign unit changes nothing.
    for file in &files {
        if std::fs::symlink_metadata(file).is_ok() && !written_by_xcb(file)? {
            return Err(Error::Conflict(
                "a systemd unit named xcb-update was not written by xcb; it was left in place",
            ));
        }
    }
    write_unit(&files[1], &service)?;
    write_unit(&files[0], &timer)?;
    if !crate::systemd::systemctl(&["daemon-reload"])
        || !crate::systemd::systemctl(&["enable", "--now", &format!("{SYSTEMD_UNIT}.timer")])
    {
        return Err(Error::Unavailable(
            "the update timer files are written but systemd could not enable them; run xcb update enable again from a login session with a systemd user manager (systemctl --user)",
        ));
    }
    Ok(())
}

/// Install the per-user daily update check. It invokes the already-installed
/// binary once a day; it never runs a shell or follows a project-local
/// setting.
pub fn install_scheduler(binary: &Path) -> Result<()> {
    if !scheduler_supported() {
        return Err(Error::Unavailable(
            "the daily update check needs macOS or a systemd user manager; otherwise, run xcb update daemon once a day from your own scheduler",
        ));
    }
    let home = std::env::var_os("HOME").ok_or(Error::PrivateState)?;
    let home = PathBuf::from(home);
    if SYSTEMD {
        return install_systemd_scheduler(binary, &home);
    }
    let agents = home.join("Library/LaunchAgents");
    let plist = scheduler_path(&home);
    let label = SCHEDULER_LABEL;
    let domain = launchd_domain();
    let bootout = || {
        let _ = Command::new("/bin/launchctl")
            .args(["bootout", &domain, &plist.to_string_lossy()])
            .status();
    };
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
    let status = Command::new("/bin/launchctl")
        .args(["bootstrap", &domain, &plist.to_string_lossy()])
        .status()?;
    if !status.success() {
        return Err(Error::Io(std::io::Error::other(
            "launchctl could not load the xcb update scheduler",
        )));
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn explicit_versions_use_the_exact_tag_and_validate_its_assets() {
        let version = "0.4.0";
        let binary = platform_asset(version);
        let value = serde_json::json!({
            "tag_name": "v0.4.0", "draft": false, "prerelease": false,
            "assets": [{"name": binary}, {"name": format!("{binary}.sha256")}]
        });
        let selected = requested_release_with("v0.4.0", |url| {
            assert_eq!(
                url,
                "https://api.github.com/repos/hraness/xcb/releases/tags/v0.4.0"
            );
            Ok(value.clone())
        })
        .unwrap()
        .unwrap();
        assert_eq!(selected.version, version);
        assert!(
            requested_release_with("0.4.1", |_| Ok(value.clone()))
                .unwrap()
                .is_none()
        );
        // A release without this platform's archive and checksum is refused
        // by name instead of looking like a missing release.
        let mut missing_checksum = value.clone();
        missing_checksum["assets"].as_array_mut().unwrap().pop();
        let refusal = requested_release_with(version, |_| Ok(missing_checksum))
            .unwrap_err()
            .to_string();
        assert!(
            refusal.contains(&format!(
                "xcb 0.4.0 has no release build for {}",
                platform()
            )),
            "{refusal}"
        );
        assert!(
            refusal.contains("https://xcb.sh/install#source"),
            "{refusal}"
        );
        for invalid in [
            "latest",
            "01.2.3",
            "1.2",
            "1.2.3-beta",
            "../latest",
            "1.2.3?x=y",
        ] {
            assert!(
                requested_release_with(invalid, |_| panic!("must not fetch invalid tag")).is_err()
            );
        }
    }

    #[test]
    fn downgrades_need_an_explicit_flag() {
        let error = check_downgrade("0.9.1", "v0.8.0", false).unwrap_err();
        assert!(
            matches!(&error, Error::Guided { next: Some(next), .. } if next == "xcb upgrade 0.8.0 --allow-downgrade"),
            "{error}"
        );
        assert!(error.to_string().contains("older than the installed 0.9.1"));
        assert!(check_downgrade("0.9.1", "0.9.0", true).is_ok());
        // Reinstalling the same version or moving forward needs no flag;
        // an unparseable version is left to the release lookup.
        for requested in ["0.9.1", "0.10.0", "v1.0.0", "latest"] {
            assert!(
                check_downgrade("0.9.1", requested, false).is_ok(),
                "{requested}"
            );
        }
    }

    #[test]
    fn removing_the_scheduler_touches_only_the_agent_it_finds() {
        let home = tempfile::tempdir().unwrap();
        let mut unloaded = false;
        assert!(!remove_scheduler_with(home.path(), |_| unloaded = true).unwrap());
        assert!(!unloaded, "nothing to unload without the agent file");
        let plist = scheduler_path(home.path());
        std::fs::create_dir_all(plist.parent().unwrap()).unwrap();
        let (service, timer) = systemd_units(Path::new("/bin/xcb"), home.path()).unwrap();
        std::fs::write(&plist, &timer).unwrap();
        let files = scheduler_files(home.path());
        if SYSTEMD {
            std::fs::write(&files[1], &service).unwrap();
        }
        let mut seen = None;
        assert!(remove_scheduler_with(home.path(), |path| seen = Some(path.to_owned())).unwrap());
        assert!(files.iter().all(|file| !file.exists()));
        assert_eq!(seen.is_some(), SCHEDULER_PLATFORM);
        // Something else in its place is refused and left alone.
        let target = home.path().join("elsewhere.plist");
        std::fs::write(&target, "keep").unwrap();
        std::os::unix::fs::symlink(&target, &plist).unwrap();
        assert!(remove_scheduler_with(home.path(), |_| panic!("must not unload")).is_err());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "keep");
    }

    #[test]
    fn systemd_update_units_run_the_binary_daily_and_leave_foreign_units_alone() {
        let home = tempfile::tempdir().unwrap();
        let (service, timer) =
            systemd_units(Path::new("/opt/x%b/xcb"), Path::new("/home/me")).unwrap();
        for unit in [&service, &timer] {
            assert!(unit.starts_with(&format!("{SYSTEMD_MARKER}\n")));
        }
        assert!(service.contains("ExecStart=\"/opt/x%%b/xcb\" update daemon --quiet\n"));
        assert!(service.contains("Environment=\"HOME=/home/me\"\n"));
        assert!(service.contains("Type=oneshot\n"));
        assert!(timer.contains("OnUnitActiveSec=1d\n"));
        assert!(timer.contains("WantedBy=timers.target\n"));
        assert!(systemd_units(Path::new("/bin/xcb\nExecStartPre=/bin/sh"), home.path()).is_err());

        // A unit xcb did not write is neither replaced nor removed.
        let unit = home.path().join("xcb-update.timer");
        std::fs::write(&unit, "[Timer]\nOnCalendar=hourly\n").unwrap();
        assert!(!written_by_xcb(&unit).unwrap());
        assert!(matches!(write_unit(&unit, &timer), Err(Error::Conflict(_))));
        assert_eq!(
            std::fs::read_to_string(&unit).unwrap(),
            "[Timer]\nOnCalendar=hourly\n"
        );
        std::fs::write(&unit, &timer).unwrap();
        assert!(written_by_xcb(&unit).unwrap());
        write_unit(&unit, &timer.replace("5min", "6min")).unwrap();
        assert!(std::fs::read_to_string(&unit).unwrap().contains("6min"));
        if SYSTEMD {
            let installed = scheduler_path(home.path());
            std::fs::create_dir_all(installed.parent().unwrap()).unwrap();
            std::fs::write(&installed, "[Timer]\n").unwrap();
            assert!(matches!(
                remove_scheduler_with(home.path(), |_| panic!("must not unload")),
                Err(Error::Conflict(_))
            ));
            assert!(installed.exists());
        }
    }

    fn write_manifest(root: &Path, body: serde_json::Value) {
        use std::os::unix::fs::PermissionsExt;
        let path = root.join("install.json");
        std::fs::write(&path, body.to_string()).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    #[test]
    fn install_records_must_use_the_installer_layout() {
        let root = tempfile::tempdir().unwrap();
        let root = xcb_core::canonical(root.path()).unwrap();
        assert_eq!(install_record(&root).unwrap(), None);
        let prefix = root.join("prefix");
        write_manifest(
            &root,
            serde_json::json!({
                "version": 1,
                "installMethod": "release",
                "prefix": prefix,
                "helperPath": prefix.join("share/xcb/install-native.sh"),
                "binaryPath": prefix.join("bin/xcb"),
            }),
        );
        assert_eq!(
            install_record(&root).unwrap(),
            Some(InstallRecord {
                manifest: root.join("install.json"),
                prefix: prefix.clone(),
                helper: prefix.join("share/xcb/install-native.sh"),
                binary: prefix.join("bin/xcb"),
            })
        );
        // An older manifest without binaryPath means <prefix>/bin/xcb.
        write_manifest(
            &root,
            serde_json::json!({"prefix": prefix, "helperPath": prefix.join("share/xcb/install-native.sh")}),
        );
        assert_eq!(
            install_record(&root).unwrap().unwrap().binary,
            prefix.join("bin/xcb")
        );
        // A record pointing anywhere else is never trusted for removal.
        for binary in ["/usr/bin/xcb", "/etc/passwd"] {
            write_manifest(
                &root,
                serde_json::json!({"prefix": prefix, "helperPath": prefix.join("share/xcb/install-native.sh"), "binaryPath": binary}),
            );
            assert!(install_record(&root).is_err(), "{binary}");
        }
        write_manifest(
            &root,
            serde_json::json!({"prefix": "relative", "helperPath": "relative/share/xcb/install-native.sh"}),
        );
        assert!(install_record(&root).is_err());
    }

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
    fn platform_names_match_the_release_archives() {
        let expected = match (std::env::consts::OS, std::env::consts::ARCH) {
            ("macos", "aarch64") => "darwin-aarch64",
            ("linux", "x86_64") => "linux-x86_64",
            ("linux", "aarch64") => "linux-aarch64",
            // Hosts without a release archive keep Rust's own names.
            (os, arch) => &format!("{os}-{arch}"),
        };
        assert_eq!(platform(), expected);
        assert_eq!(
            platform_asset("0.5.0"),
            format!("xcb-0.5.0-{expected}.tar.gz")
        );
    }

    #[test]
    fn latest_tells_a_missing_platform_build_from_no_release() {
        let entry = |tag: &str, assets: &[String]| {
            serde_json::json!({
                "tag_name": tag, "draft": false, "prerelease": false,
                "assets": assets.iter().map(|name| serde_json::json!({"name": name})).collect::<Vec<_>>(),
            })
        };
        let pair = |version: &str| {
            let asset = platform_asset(version);
            vec![format!("{asset}.sha256"), asset]
        };
        let foreign = vec![
            "xcb-0.7.0-plan9-mips.tar.gz".to_owned(),
            "xcb-0.7.0-plan9-mips.tar.gz.sha256".to_owned(),
        ];
        // The newest release lacks this platform: the newest one with it wins.
        let latest = latest_from_list(&serde_json::json!([
            entry("v0.7.0", &foreign),
            entry("v0.6.0", &pair("0.6.0")),
            entry("v0.5.0", &pair("0.5.0")),
        ]))
        .unwrap();
        assert_eq!(latest.release.unwrap().version, "0.6.0");
        assert_eq!(latest.newest.as_deref(), Some("0.7.0"));
        // Releases exist, none for this platform.
        let latest = latest_from_list(&serde_json::json!([entry("v0.7.0", &foreign)])).unwrap();
        assert!(latest.release.is_none());
        assert_eq!(latest.newest.as_deref(), Some("0.7.0"));
        // Drafts and prereleases count as neither.
        let latest = latest_from_list(&serde_json::json!([
            {"tag_name": "v0.8.0", "draft": true, "prerelease": false, "assets": []},
            {"tag_name": "v0.9.0", "draft": false, "prerelease": true, "assets": []},
        ]))
        .unwrap();
        assert!(latest.release.is_none() && latest.newest.is_none());
        assert!(latest_from_list(&serde_json::json!({})).is_err());
        assert!(no_platform_build(None).to_string().contains(&format!(
            "no xcb release has a build for {} yet",
            platform()
        )));
    }

    #[test]
    fn versions_are_strict_stable_semver() {
        assert_eq!(version_tuple("v1.2.3"), Some((1, 2, 3)));
        assert!(version_tuple("1.2").is_none());
        assert!(version_tuple("1.2.3-beta").is_none());
    }
}
