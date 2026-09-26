use crate::{Error, Result, digest, private};
use rustix::process::{Pid, Signal, kill_process_group, test_kill_process_group};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::{
        fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    task::JoinHandle,
};
use xcb_core::{MAX_JSON_BYTES, Provider};

pub fn environment(home: &Path) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("HOME".into(), home.to_string_lossy().into_owned()),
        (
            "PATH".into(),
            "/usr/bin:/bin:/usr/local/bin:/opt/homebrew/bin".into(),
        ),
        ("LANG".into(), "en_US.UTF-8".into()),
        ("NO_COLOR".into(), "1".into()),
        (
            "XDG_CONFIG_HOME".into(),
            home.join(".config").to_string_lossy().into_owned(),
        ),
        (
            "XDG_DATA_HOME".into(),
            home.join(".local/share").to_string_lossy().into_owned(),
        ),
        (
            "XDG_CACHE_HOME".into(),
            home.join(".cache").to_string_lossy().into_owned(),
        ),
        (
            "TMPDIR".into(),
            home.join("tmp").to_string_lossy().into_owned(),
        ),
    ])
}

fn executable_file(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(
            (rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::NONBLOCK
                | rustix::fs::OFlags::CLOEXEC)
                .bits() as i32,
        )
        .open(path)?;
    let meta = file.metadata()?;
    // Each rule fails on its own so the operator learns what to fix.
    if !meta.is_file() {
        return Err(Error::Unavailable("executable is not a regular file"));
    }
    if ![0, rustix::process::getuid().as_raw()].contains(&meta.uid()) {
        return Err(Error::Unavailable(
            "executable is not owned by this user or root",
        ));
    }
    if meta.mode() & 0o7000 != 0 {
        return Err(Error::Unavailable(
            "executable has setuid, setgid, or sticky bits",
        ));
    }
    if meta.mode() & 0o022 != 0 {
        return Err(Error::Unavailable(
            "executable is group- or world-writable; run xcb doctor to repair its mode",
        ));
    }
    if meta.mode() & 0o111 == 0 {
        return Err(Error::Unavailable("executable is not executable"));
    }
    if meta.len() == 0 || meta.len() > 512 * 1024 * 1024 {
        return Err(Error::Unavailable("executable size is invalid"));
    }
    Ok(file)
}

fn repair_executable_mode(path: &Path) -> Result<bool> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(
            (rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::NONBLOCK
                | rustix::fs::OFlags::CLOEXEC)
                .bits() as i32,
        )
        .open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file()
        || meta.uid() != rustix::process::getuid().as_raw()
        || meta.nlink() != 1
        || meta.mode() & 0o7000 != 0
        || meta.mode() & 0o111 == 0
        || meta.mode() & 0o022 == 0
        || meta.len() == 0
        || meta.len() > 512 * 1024 * 1024
    {
        return Ok(false);
    }
    // Tighten writable bits without exposing a private executable to more users.
    file.set_permissions(fs::Permissions::from_mode(meta.mode() & 0o777 & !0o022))?;
    Ok(true)
}

fn wrapper_file(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(
            (rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::NONBLOCK
                | rustix::fs::OFlags::CLOEXEC)
                .bits() as i32,
        )
        .open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file()
        || ![0, rustix::process::getuid().as_raw()].contains(&meta.uid())
        || meta.mode() & 0o022 != 0
        || meta.mode() & 0o111 == 0
        || (meta.mode() & 0o7000 != 0 && meta.uid() != 0)
        || meta.len() == 0
        || meta.len() > 8 * 1024 * 1024
    {
        return Err(Error::Unavailable(
            "sandbox wrapper ownership, permissions, or size is invalid",
        ));
    }
    Ok(file)
}

fn digest_file(mut file: File, limit: u64) -> Result<String> {
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut size = 0u64;
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        size += count as u64;
        if size > limit {
            return Err(Error::Unavailable("executable size changed"));
        }
        hash.update(&buffer[..count]);
    }
    Ok(hex::encode(hash.finalize()))
}

/// The exact file identity a verified digest is bound to, read from the same
/// descriptor the digest is computed on — any difference re-digests. The
/// identity is a cache key, never a substitute for the checks
/// `executable_file` runs on every call.
type FileIdentity = xcb_core::FileIdentity;

/// Process-wide verified digests keyed by canonical executable path. A route
/// decision loads every provider's pin and each turn re-verifies the host and
/// provider binaries, which re-read and re-hashed up to 512 MiB per call; the
/// identity-bound cache makes a repeated verification a metadata read while
/// remaining provably equivalent to re-hashing the exact installed bytes.
const VERIFIED_DIGEST_LIMIT: usize = 16;

fn verified_digests() -> &'static std::sync::Mutex<BTreeMap<PathBuf, (FileIdentity, String)>> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<BTreeMap<PathBuf, (FileIdentity, String)>>> =
        std::sync::OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// Full executable digests actually performed per canonical path, so
/// launch-path tests can observe cache hits while running in parallel.
#[cfg(test)]
static EXECUTABLE_DIGESTS: std::sync::Mutex<BTreeMap<PathBuf, usize>> =
    std::sync::Mutex::new(BTreeMap::new());

#[cfg(test)]
fn digested_executables(path: &Path) -> usize {
    EXECUTABLE_DIGESTS
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .get(path)
        .copied()
        .unwrap_or(0)
}

pub fn executable_digest(path: &Path) -> Result<String> {
    let file = executable_file(path)?;
    // fstat of the open descriptor: the identity below names the inode the
    // digest is computed from, never a re-resolved path.
    let identity = FileIdentity::of(&file.metadata()?);
    let key = path.canonicalize().unwrap_or_else(|_| path.to_owned());
    {
        let cache = verified_digests()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some((known, sha256)) = cache.get(&key)
            && *known == identity
        {
            return Ok(sha256.clone());
        }
    }
    let sha256 = digest_file(file, 512 * 1024 * 1024)?;
    #[cfg(test)]
    EXECUTABLE_DIGESTS
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .entry(key.clone())
        .and_modify(|count| *count += 1)
        .or_insert(1);
    {
        let mut cache = verified_digests()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        while cache.len() >= VERIFIED_DIGEST_LIMIT {
            cache.pop_first();
        }
        cache.insert(key, (identity, sha256.clone()));
    }
    Ok(sha256)
}

/// `clonefile(2)` is atomic: the name either holds the complete copy-on-write
/// clone or is absent, so a failure never leaves a partial snapshot and the
/// stream copy below can still create the name itself. `CLONE_NOFOLLOW`
/// matches the `O_NOFOLLOW` custody of the source descriptor.
#[cfg(target_os = "macos")]
fn clone_file(source: &File, path: &Path) -> bool {
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return false;
    };
    let Ok(directory) = rustix::fs::openat(
        rustix::fs::CWD,
        parent,
        rustix::fs::OFlags::DIRECTORY | rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    ) else {
        return false;
    };
    rustix::fs::fclonefileat(
        source,
        &directory,
        std::path::Path::new(name),
        rustix::fs::CloneFlags::NOFOLLOW,
    )
    .is_ok()
}

/// `FICLONE` clones extents atomically on copy-on-write filesystems;
/// `copy_file_range` keeps the copy inside the kernel everywhere else. Any
/// error or short progress falls back to the stream copy on the same
/// descriptors.
#[cfg(all(
    target_os = "linux",
    not(any(target_arch = "sparc", target_arch = "sparc64"))
))]
fn clone_into(source: &File, target: &File) -> bool {
    if rustix::fs::ioctl_ficlone(target, source).is_ok() {
        return true;
    }
    let Ok(size) = source.metadata().map(|meta| meta.len()) else {
        return false;
    };
    let mut off_in = 0u64;
    let mut off_out = 0u64;
    while off_in < size {
        let remaining = (size - off_in).min(i32::MAX as u64) as usize;
        match rustix::fs::copy_file_range(
            source,
            Some(&mut off_in),
            target,
            Some(&mut off_out),
            remaining,
        ) {
            // The kernel advances both offsets; zero progress is EOF or a
            // stall, so the caller falls back to the stream copy.
            Ok(0) => return false,
            Ok(_) => (),
            Err(_) => return false,
        }
    }
    true
}

/// Write the opened executable to `path` with the least work the filesystem
/// allows, falling back to the bounded stream copy. The caller still digests
/// the result: a clone carries a new inode, so the pinned-byte proof must be
/// repeated against the snapshot itself.
fn snapshot_executable(source: &File, path: &Path) -> Result<()> {
    #[cfg(target_os = "macos")]
    if clone_file(source, path) {
        fs::set_permissions(path, fs::Permissions::from_mode(0o500))?;
        return Ok(());
    }
    let mut target = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o500)
        .custom_flags((rustix::fs::OFlags::CLOEXEC).bits() as i32)
        .open(path)?;
    #[cfg(all(
        target_os = "linux",
        not(any(target_arch = "sparc", target_arch = "sparc64"))
    ))]
    if clone_into(source, &target) {
        target.sync_all()?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o500))?;
        return Ok(());
    }
    std::io::copy(&mut source.take(512 * 1024 * 1024 + 1), &mut target)?;
    target.flush()?;
    target.sync_all()?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o500))?;
    Ok(())
}

/// Captured before a long-lived terminal can observe an in-place xcb update.
/// Pins identify this process's starting implementation, never replacement bytes
/// installed later at the same executable pathname.
struct HostExecutable {
    path: PathBuf,
    sha256: String,
}
impl HostExecutable {
    fn capture(path: PathBuf) -> Result<Self> {
        let path = path.canonicalize()?;
        let sha256 = executable_digest(&path)?;
        Ok(Self { path, sha256 })
    }
    fn verify(&self) -> Result<()> {
        if executable_digest(&self.path).ok().as_deref() != Some(self.sha256.as_str()) {
            return Err(Error::Unavailable(
                "xcb was replaced while this process was running; restart xcb and run xcb doctor",
            ));
        }
        Ok(())
    }
    fn verify_pin(&self, expected: &str) -> Result<()> {
        self.verify()?;
        if expected != self.sha256 {
            return Err(Error::Unavailable("runtime changed; run xcb doctor again"));
        }
        Ok(())
    }
}

fn host_executable() -> Result<&'static HostExecutable> {
    static HOST: std::sync::OnceLock<std::result::Result<HostExecutable, ()>> =
        std::sync::OnceLock::new();
    HOST.get_or_init(|| {
        std::env::current_exe()
            .map_err(Error::from)
            .and_then(HostExecutable::capture)
            .map_err(|_| ())
    })
    .as_ref()
    .map_err(|_| Error::Unavailable("could not bind the running xcb executable"))
}

/// The verified image captured for this process, never newly adopted bytes
/// installed over its executable path while it was running.
pub(crate) fn host_identity() -> Result<(PathBuf, String)> {
    let host = host_executable()?;
    host.verify()?;
    Ok((host.path.clone(), host.sha256.clone()))
}

/// Call at host startup, before accepting commands or waiting for input.
/// Store::open also captures this identity for embedded runtime consumers.
pub fn initialize_host() -> Result<()> {
    host_executable().map(|_| ())
}

pub fn wrapper_digest(path: &Path) -> Result<String> {
    digest_file(wrapper_file(path)?, 8 * 1024 * 1024)
}

pub fn discover(provider: Provider, explicit: Option<&Path>) -> Result<PathBuf> {
    let override_name = format!("XCB_{}", provider.as_str().to_uppercase());
    if let Some(path) = explicit
        .map(Path::to_owned)
        .or_else(|| std::env::var_os(override_name).map(PathBuf::from))
    {
        if !path.is_absolute() {
            return Err(Error::Unavailable("provider path must be absolute"));
        }
        let path = path.canonicalize()?;
        if executable_file(&path).is_err() && !repair_executable_mode(&path)? {
            return Err(Error::Unavailable(
                "executable ownership, permissions, or size is invalid",
            ));
        }
        executable_file(&path)?;
        return Ok(path);
    }
    for directory in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).take(128)
    {
        let candidate = directory.join(provider.as_str());
        if let Ok(path) = candidate.canonicalize()
            && (executable_file(&path).is_ok()
                || (repair_executable_mode(&path).unwrap_or(false)
                    && executable_file(&path).is_ok()))
        {
            return Ok(path);
        }
    }
    Err(Error::Unavailable(
        "provider binary not found; specify its XCB provider path",
    ))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pin {
    pub provider: Provider,
    pub executable: PathBuf,
    pub sha256: String,
    pub version: String,
    pub host_sha256: String,
    pub observed_at_ms: u64,
}
impl Pin {
    pub fn verify(&self) -> Result<()> {
        host_executable()?.verify_pin(&self.host_sha256)?;
        self.verify_bytes()
    }
    /// Digest check only: the bytes at the pinned path still match the pin.
    fn verify_bytes(&self) -> Result<()> {
        // Package managers reinstall the same bytes with group- and
        // world-writable modes (bun's global install does). Tighten the mode
        // of an executable we own before judging it, exactly as discovery
        // does; the digest below still binds the bytes to the pin.
        if executable_file(&self.executable).is_err() {
            repair_executable_mode(&self.executable)?;
        }
        if self.executable.canonicalize()? != self.executable
            || executable_digest(&self.executable)? != self.sha256
        {
            return Err(Error::Unavailable("runtime changed; run xcb doctor again"));
        }
        Ok(())
    }
    pub fn load(root: &Path, provider: Provider) -> Result<Self> {
        let mut pin: Self = serde_json::from_slice(&private::read(
            &root.join("providers").join(format!("{provider}.json")),
            16 * 1024,
        )?)?;
        if pin.provider != provider {
            return Err(Error::Unavailable("provider pin mismatch"));
        }
        match pin.verify() {
            Ok(()) => {}
            // Identical provider bytes under a replaced xcb binary drift only
            // the host binding — a metadata rebind, so heal it instead of
            // forcing `xcb doctor` after every upgrade.
            Err(error) if pin.verify_bytes().is_ok() => {
                let host = host_executable()?;
                host.verify()?;
                pin.host_sha256 = host.sha256.clone();
                pin.observed_at_ms = crate::now_ms();
                pin.save(root).map_err(|_| error)?;
            }
            Err(error) => return Err(error),
        }
        // Migrate pins written before executables were custodied: adopt the
        // pinned bytes into private state so a provider upgrade can no
        // longer move them out from under the pin.
        if custody_dir(root).is_ok_and(|dir| !pin.executable.starts_with(&dir)) {
            let _ = pin.save(root);
        }
        Ok(pin)
    }
    /// Custody the pinned executable bytes into private state, then record
    /// the pin pointing at the custodied copy. A provider auto-update at the
    /// original path can no longer break the pin.
    pub fn save(&mut self, root: &Path) -> Result<()> {
        self.executable = custody_executable(root, self.provider, &self.executable, &self.sha256)?;
        let directory = private::directory(&root.join("providers"))?;
        let path = directory.join(format!("{}.json", self.provider));
        let bytes = serde_json::to_vec_pretty(self)?;
        match private::read(&path, 16 * 1024) {
            Ok(old) => private::replace(&path, &bytes, &digest(old)),
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                private::create(&path, &bytes)
            }
            Err(error) => Err(error),
        }
    }
    /// Pin the current host relay to immutable launch-owned bytes, just as we
    /// pin the provider. An in-place xcb upgrade cannot change an active relay.
    #[cfg(target_os = "macos")]
    pub(crate) fn host_snapshot(&self, directory: &Path) -> Result<PathBuf> {
        self.verify()?;
        let source_path = std::env::current_exe()?.canonicalize()?;
        let path = directory.join("xcb-helper");
        snapshot_executable(&executable_file(&source_path)?, &path)?;
        if executable_digest(&path)? != self.host_sha256 {
            return Err(Error::Unavailable("host relay snapshot changed"));
        }
        Ok(path)
    }
    pub fn snapshot(&self, directory: &Path) -> Result<PathBuf> {
        self.verify()?;
        let path = directory.join("provider");
        snapshot_executable(&executable_file(&self.executable)?, &path)?;
        if executable_digest(&path)? != self.sha256 {
            return Err(Error::Unavailable("executable snapshot changed"));
        }
        Ok(path)
    }
}

fn parse_version(provider: Provider, output: &str) -> Result<&str> {
    let version = match provider {
        Provider::Claude => output.strip_suffix(" (Claude Code)").unwrap_or(output),
        Provider::Devin => output
            .strip_prefix("devin ")
            .and_then(|text| text.split_once(" (").map(|pair| pair.0))
            .ok_or(Error::Protocol("Devin version"))?,
        Provider::Codex => output
            .strip_prefix("codex-cli ")
            .ok_or(Error::Protocol("Codex version"))?,
    };
    // Metadata discovery accepts official prerelease Codex builds. This does
    // not admit task execution: exact provider qualification remains separate.
    let stable = r"^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$";
    let codex = r"^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$";
    let pattern = if provider == Provider::Codex {
        codex
    } else {
        stable
    };
    if version.len() > 64
        || !regex::Regex::new(pattern)
            .expect("static version grammar")
            .is_match(version)
    {
        return Err(Error::Protocol("version shape"));
    }
    Ok(version)
}

pub async fn inspect(provider: Provider, explicit: Option<&Path>, home: &Path) -> Result<Pin> {
    let host = host_executable()?;
    host.verify()?;
    let executable = discover(provider, explicit)?;
    let sha256 = executable_digest(&executable)?;
    let mut command = Command::new(&executable);
    command
        .arg("--version")
        .env_clear()
        .envs(environment(home))
        .current_dir(home);
    let bytes = capture(command, 1024, Duration::from_secs(10)).await?;
    let output = std::str::from_utf8(&bytes)
        .map_err(|_| Error::Protocol("version encoding"))?
        .trim();
    let version = parse_version(provider, output)?;
    if executable_digest(&executable)? != sha256 {
        return Err(Error::Unavailable("runtime changed during inspection"));
    }
    host.verify()?;
    Ok(Pin {
        provider,
        executable,
        sha256,
        version: version.to_owned(),
        host_sha256: host.sha256.clone(),
        observed_at_ms: crate::now_ms(),
    })
}

/// Pinned executables live under `providers/bin`, named `<provider>-<sha256>`,
/// so the pin binds private bytes an auto-update cannot replace.
fn custody_dir(root: &Path) -> Result<PathBuf> {
    private::directory(&root.join("providers").join("bin"))
}

/// Copy the admitted executable into private custody (idempotent for the same
/// digest) and retire stale copies of the same provider. Returns the
/// custodied path; the caller rewrites its pin to point at it.
fn custody_executable(
    root: &Path,
    provider: Provider,
    source: &Path,
    sha256: &str,
) -> Result<PathBuf> {
    let dir = custody_dir(root)?;
    if source.starts_with(&dir) {
        return Ok(source.to_owned());
    }
    let name = format!("{provider}-{sha256}");
    let target = dir.join(&name);
    match executable_digest(&target) {
        Ok(known) if known == sha256 => {}
        _ => {
            if target.is_file() {
                fs::remove_file(&target)?;
            }
            let input = executable_file(source)?;
            let mut bytes = Vec::new();
            input.take(512 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
            if hex::encode(Sha256::digest(&bytes)) != sha256 {
                return Err(Error::Unavailable(
                    "provider executable changed while pinning",
                ));
            }
            match private::create(&target, &bytes) {
                Ok(()) => {}
                Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
            fs::set_permissions(&target, fs::Permissions::from_mode(0o700))?;
            if executable_digest(&target)? != sha256 {
                let _ = fs::remove_file(&target);
                return Err(Error::Unavailable("custodied executable digest mismatch"));
            }
        }
    }
    if let Ok(entries) = fs::read_dir(&dir) {
        let prefix = format!("{provider}-");
        for entry in entries.flatten() {
            let stale = entry.file_name();
            if stale
                .to_str()
                .is_some_and(|file| file.starts_with(&prefix) && file != name)
            {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
    Ok(target)
}

/// Outcome of one refresh pass over a pinned provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefreshOutcome {
    /// The pinned build is still the discovered one.
    Kept,
    /// A newly discovered build passed admission and became the pin.
    Adopted,
    /// The discovered build is inspectable but not yet catalog-admitted —
    /// the pinned build keeps routing while the catalog catches up.
    PendingCatalog,
    /// A newly discovered build failed inspection or was denied; the
    /// previous pin stands and this build is not re-inspected every pass.
    Rejected,
    /// No pin exists — `xcb doctor` owns first admission.
    Unpinned,
}

#[derive(Debug, Clone)]
pub struct ProviderRefresh {
    pub provider: Provider,
    pub outcome: RefreshOutcome,
    pub detail: Option<String>,
}

/// Re-inspect the discovered provider binary and adopt it only when it is an
/// admitted build. Runs at daemon boot, hourly in the supervisor, and before
/// interactive launches: floor-admitted providers (Claude) track upstream
/// updates without breaking routes, while exact-artifact providers (Codex,
/// Devin) can only ever re-adopt the qualified build.
pub async fn refresh_provider(
    root: &Path,
    provider: Provider,
    explicit: Option<&Path>,
    home: &Path,
) -> ProviderRefresh {
    let outcome = private::directory(home)
        .and_then(|_| private::directory(&home.join("tmp")))
        .map(|_| ());
    let outcome = match outcome {
        Ok(()) => refresh_provider_inner(root, provider, explicit, home).await,
        Err(error) => Err(error),
    };
    let (outcome, detail) = match outcome {
        Ok((outcome, detail)) => (outcome, detail),
        Err(error) => (RefreshOutcome::Rejected, Some(error.to_string())),
    };
    ProviderRefresh {
        provider,
        outcome,
        detail,
    }
}

async fn refresh_provider_inner(
    root: &Path,
    provider: Provider,
    explicit: Option<&Path>,
    home: &Path,
) -> Result<(RefreshOutcome, Option<String>)> {
    let record = root.join("providers").join(format!("{provider}.json"));
    if !record.is_file() {
        return Ok((RefreshOutcome::Unpinned, None));
    }
    let pin = Pin::load(root, provider).ok();
    let executable = discover(provider, explicit)?;
    let discovered_sha = executable_digest(&executable)?;
    if let Some(pin) = &pin
        && pin.sha256 == discovered_sha
    {
        return Ok((RefreshOutcome::Kept, None));
    }
    let marker = record.with_extension("rejected");
    if let Some(rejected) = read_rejected(&marker)
        && rejected.sha256 == discovered_sha
        && !crate::catalog::listed(root, &discovered_sha)
    {
        // The catalog still does not admit a build seen before. Inspection
        // failures stay terminal while the bytes are unchanged; a denied
        // digest is rejected only while the catalog still denies it.
        return Ok(
            if crate::catalog::denied(root, &discovered_sha)
                || rejected.reason == RejectedReason::Inspection
            {
                (RefreshOutcome::Rejected, None)
            } else {
                (
                    RefreshOutcome::PendingCatalog,
                    Some(pending_detail(&rejected)),
                )
            },
        );
    }
    match inspect(provider, explicit, home).await {
        Ok(mut fresh) if crate::runner::provider_admitted(root, &fresh) => {
            fresh.save(root)?;
            let _ = fs::remove_file(&marker);
            Ok((RefreshOutcome::Adopted, None))
        }
        Ok(fresh) => {
            let denied = crate::catalog::denied(root, &discovered_sha);
            let reason = if denied {
                RejectedReason::Denied
            } else {
                RejectedReason::Unadmitted
            };
            write_rejected(&marker, &discovered_sha, Some(&fresh.version), reason)?;
            if denied {
                Err(Error::Unavailable(
                    "provider build is denied by the reviewed-builds catalog",
                ))
            } else {
                Ok((
                    RefreshOutcome::PendingCatalog,
                    Some(format!("{} awaiting catalog admission", fresh.version)),
                ))
            }
        }
        Err(error) => {
            write_rejected(&marker, &discovered_sha, None, RejectedReason::Inspection)?;
            Err(error)
        }
    }
}

fn pending_detail(rejected: &Rejected) -> String {
    match &rejected.provider_version {
        Some(version) => format!("{version} awaiting catalog admission"),
        None => "discovered build awaiting catalog admission".to_owned(),
    }
}

/// A discovered build the refresh pass marked as awaiting catalog
/// admission. Doctor and status surfaces report it without re-inspecting;
/// inspection failures and denied builds are not pending.
pub struct PendingBuild {
    pub version: Option<String>,
    pub sha256: String,
}
pub fn pending_build(root: &Path, provider: Provider) -> Option<PendingBuild> {
    let marker = root
        .join("providers")
        .join(format!("{provider}.json"))
        .with_extension("rejected");
    let rejected = read_rejected(&marker)?;
    if rejected.reason != RejectedReason::Unadmitted {
        return None;
    }
    Some(PendingBuild {
        version: rejected.provider_version,
        sha256: rejected.sha256,
    })
}

/// A build the refresh pass already judged: pending builds wait on the
/// catalog; inspection failures and catalog-denied builds are terminal
/// until the bytes change.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Rejected {
    version: u32,
    sha256: String,
    #[serde(default)]
    provider_version: Option<String>,
    reason: RejectedReason,
    observed_at_ms: u64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
enum RejectedReason {
    Unadmitted,
    Inspection,
    Denied,
}

fn read_rejected(marker: &Path) -> Option<Rejected> {
    let bytes = private::read(marker, 1024).ok()?;
    if let Ok(rejected) = serde_json::from_slice::<Rejected>(&bytes)
        && rejected.version == 1
    {
        return Some(rejected);
    }
    // Markers written before the catalog flow store the raw digest bytes;
    // treat them as unadmitted builds awaiting review.
    std::str::from_utf8(&bytes)
        .ok()
        .map(str::trim)
        .filter(|sha256| sha256.len() == 64)
        .map(|sha256| Rejected {
            version: 1,
            sha256: sha256.to_owned(),
            provider_version: None,
            reason: RejectedReason::Unadmitted,
            observed_at_ms: 0,
        })
}

fn write_rejected(
    marker: &Path,
    sha256: &str,
    provider_version: Option<&str>,
    reason: RejectedReason,
) -> Result<()> {
    let record = Rejected {
        version: 1,
        sha256: sha256.to_owned(),
        provider_version: provider_version.map(str::to_owned),
        reason,
        observed_at_ms: crate::now_ms(),
    };
    let bytes = serde_json::to_vec_pretty(&record)?;
    match private::read(marker, 1024) {
        Ok(old) => private::replace(marker, &bytes, &digest(old)),
        Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            private::create(marker, &bytes)
        }
        Err(error) => Err(error),
    }
}

/// Bytes a provider may write to stderr before the host reports the excess.
/// Volume never fails a join: a chatty child must not turn a proven kill,
/// reap and group-absence check into an unsettled run.
pub const STDERR_NOTICE_BYTES: u64 = 1024 * 1024;

/// Outcome of draining one child stream to EOF. `complete` is false only for
/// a real read failure, never for volume.
#[derive(Debug, Clone, Copy, Default)]
struct Drained {
    bytes: u64,
    complete: bool,
}

pub struct StreamProcess {
    pub(crate) stdin: Option<ChildStdin>,
    pub(crate) stdout: BufReader<ChildStdout>,
    child: Child,
    group: Option<Pid>,
    stderr: JoinHandle<Drained>,
    stderr_bytes: Option<u64>,
    exit_status: Option<std::process::ExitStatus>,
    frame_buffer: Vec<u8>,
}
impl StreamProcess {
    pub fn spawn(mut command: Command) -> Result<Self> {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        command.as_std_mut().process_group(0);
        let mut child = command.spawn().map_err(Error::LaunchNotStarted)?;
        let pid = child
            .id()
            .filter(|pid| *pid > 1)
            .and_then(|pid| i32::try_from(pid).ok())
            .and_then(Pid::from_raw)
            .ok_or(Error::Protocol("child process identity"))?;
        let stdin = child.stdin.take().ok_or(Error::Protocol("child stdin"))?;
        let stdout = BufReader::new(child.stdout.take().ok_or(Error::Protocol("child stdout"))?);
        let stderr = child.stderr.take().ok_or(Error::Protocol("child stderr"))?;
        let stderr = tokio::spawn(drain_to_eof(stderr));
        Ok(Self {
            stdin: Some(stdin),
            stdout,
            child,
            group: Some(pid),
            stderr,
            stderr_bytes: None,
            exit_status: None,
            frame_buffer: Vec::new(),
        })
    }
    pub fn pid(&self) -> u32 {
        self.group
            .expect("owned process group")
            .as_raw_nonzero()
            .get() as u32
    }
    pub async fn send(&mut self, value: &serde_json::Value) -> Result<()> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or(Error::Protocol("provider input closed"))?;
        crate::wire_helpers::write_frame(stdin, value, 16 * 1024 * 1024, "input frame limit").await
    }
    pub async fn frame(&mut self) -> Result<Option<Vec<u8>>> {
        self.frame_bounded(MAX_JSON_BYTES).await
    }
    /// Some providers echo accepted image inputs in notifications. Permit a
    /// separately bounded wire envelope without widening tool/text limits.
    pub async fn frame_bounded(&mut self, max: usize) -> Result<Option<Vec<u8>>> {
        if max == 0 || max > 16 * 1024 * 1024 {
            return Err(Error::Protocol("invalid frame bound"));
        }
        if self.frame_buffer.len() > max {
            return Err(Error::Protocol("output frame limit"));
        }
        // The retained buffer survives cancellation: an adapter may select
        // ACP stdout against an independent MCP callback channel.
        crate::wire_helpers::frame(
            &mut self.stdout,
            &mut self.frame_buffer,
            max,
            "output frame limit",
            "incomplete final frame",
        )
        .await
    }
    fn signal(&self) {
        if let Some(group) = self.group {
            let _ = kill_process_group(group, Signal::KILL);
        }
    }
    /// Total stderr bytes the child wrote when that exceeded
    /// `STDERR_NOTICE_BYTES`; known only after `join`. The bytes themselves
    /// are never retained: provider stderr can carry credentials or paths.
    pub fn stderr_overflow(&self) -> Option<u64> {
        self.stderr_bytes
            .filter(|bytes| *bytes > STDERR_NOTICE_BYTES)
    }
    /// The status observed when `join`/`join_graceful` reaped the child —
    /// how callers prove a graceful settle exited on its own rather than
    /// under SIGKILL.
    pub fn exit_status(&self) -> Option<std::process::ExitStatus> {
        self.exit_status
    }
    pub async fn join(&mut self) -> bool {
        self.signal();
        self.reap().await
    }
    /// The graceful settle cancellation asks for: close stdin first, give the
    /// provider `grace` to exit on its own (after any interruption frame the
    /// caller already sent), then kill the group exactly as `join` does. The
    /// reaped-streams and group-absence proof is unchanged either way.
    pub async fn join_graceful(&mut self, grace: Duration) -> bool {
        // Dropping stdin is what sends EOF: shutdown on a child pipe is a
        // no-op, so a provider that polls its input sees a real close.
        self.stdin.take();
        if tokio::time::timeout(grace, self.child.wait())
            .await
            .is_err()
        {
            self.signal();
        }
        self.reap().await
    }
    async fn reap(&mut self) -> bool {
        let Some(group) = self.group.take() else {
            return false;
        };
        let joined = tokio::time::timeout(Duration::from_secs(5), async {
            self.stdin.take();
            let (exit, stdout, stderr) = tokio::join!(
                self.child.wait(),
                drain_to_eof(&mut self.stdout),
                &mut self.stderr
            );
            let stderr = stderr.unwrap_or_default();
            self.stderr_bytes = Some(stderr.bytes);
            self.exit_status = exit.as_ref().ok().copied();
            exit.is_ok() && stdout.complete && stderr.complete
        })
        .await
        .unwrap_or(false);
        joined && group_absent(group).await
    }
}
impl Drop for StreamProcess {
    fn drop(&mut self) {
        self.signal();
        self.stderr.abort();
    }
}

async fn group_absent(group: Pid) -> bool {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if test_kill_process_group(group) == Err(rustix::io::Errno::SRCH) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or(false)
}

pub fn prove_process_group_absent(pid: u32) -> Result<()> {
    let group = i32::try_from(pid)
        .ok()
        .and_then(Pid::from_raw)
        .ok_or(Error::Unavailable("process group id is invalid"))?;
    match test_kill_process_group(group) {
        Ok(()) => Err(Error::Conflict("process group is still present")),
        Err(rustix::io::Errno::SRCH) => Ok(()),
        Err(_) => Err(Error::Unavailable("process group probe failed")),
    }
}

/// Read a stream until EOF, counting but never retaining bytes. Only a read
/// failure leaves `complete` false; the caller decides what volume means.
async fn drain_to_eof(mut source: impl AsyncRead + Unpin) -> Drained {
    let mut buffer = [0u8; 8192];
    let mut drained = Drained::default();
    loop {
        match source.read(&mut buffer).await {
            Ok(0) => {
                drained.complete = true;
                return drained;
            }
            Ok(read) => drained.bytes = drained.bytes.saturating_add(read as u64),
            Err(_) => return drained,
        }
    }
}

/// Bounded drain for one-shot captures whose whole output budget is fixed.
async fn drain(mut source: impl AsyncRead + Unpin, max: usize) -> Result<()> {
    let mut buffer = [0u8; 8192];
    let mut count = 0;
    loop {
        let read = source.read(&mut buffer).await?;
        if read == 0 {
            return Ok(());
        }
        count += read;
        if count > max {
            return Err(Error::Protocol("stream output limit"));
        }
    }
}

pub async fn capture_with_input(
    mut command: Command,
    input: &[u8],
    max: usize,
    deadline: Duration,
) -> Result<Vec<u8>> {
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command.as_std_mut().process_group(0);
    let mut child = command.spawn()?;
    let group = child
        .id()
        .filter(|pid| *pid > 1)
        .and_then(|pid| i32::try_from(pid).ok())
        .and_then(Pid::from_raw)
        .ok_or(Error::Protocol("child process identity"))?;
    let mut stdin = child.stdin.take().ok_or(Error::Protocol("child stdin"))?;
    let mut stdout = child.stdout.take().ok_or(Error::Protocol("child stdout"))?;
    let stderr = child.stderr.take().ok_or(Error::Protocol("child stderr"))?;
    let result = tokio::time::timeout(deadline, async {
        stdin.write_all(input).await?;
        stdin.shutdown().await?;
        drop(stdin);
        let output = async {
            let mut bytes = Vec::new();
            (&mut stdout)
                .take(max as u64 + 1)
                .read_to_end(&mut bytes)
                .await?;
            if bytes.len() > max {
                return Err(Error::Protocol("command output limit"));
            }
            Ok(bytes)
        };
        let (bytes, _) = tokio::try_join!(output, drain(stderr, 1024 * 1024))?;
        Ok::<_, Error>(bytes)
    })
    .await;
    let timed_out = result.is_err();
    if timed_out {
        let _ = kill_process_group(group, Signal::KILL);
    }
    let status = match tokio::time::timeout(Duration::from_secs(5), child.wait()).await {
        Ok(status) => status?,
        Err(_) => {
            let _ = kill_process_group(group, Signal::KILL);
            let _ = child.wait().await;
            return Err(Error::Unavailable("child did not join"));
        }
    };
    if !group_absent(group).await {
        return Err(Error::Unavailable("process group did not join"));
    }
    if timed_out {
        return Err(Error::Unavailable("command timed out"));
    }
    if !status.success() {
        return Err(Error::Unavailable("command failed"));
    }
    result.expect("timeout handled")
}

/// No Debug or Serialize: successful capture may contain a reusable credential.
/// A Joined error carries independent process/pipe cleanup proof; arbitrary
/// capture errors must never be treated as that proof.
pub(crate) enum CaptureOutcome {
    NeverStarted(Error),
    Joined(Result<zeroize::Zeroizing<Vec<u8>>>),
    Unproven,
}

struct CaptureGroup(Option<Pid>);
impl Drop for CaptureGroup {
    fn drop(&mut self) {
        if let Some(group) = self.0 {
            let _ = kill_process_group(group, Signal::KILL);
        }
    }
}

/// Capture one credential-bearing host login under caller-owned durable custody.
/// `started` runs immediately after spawn, before the first await. Cancellation
/// is signalled, never implemented by dropping the cleanup future. Drop only
/// attempts a stop; the caller must retain its durable lease in that case.
pub(crate) async fn capture_supervised(
    mut command: Command,
    max: usize,
    deadline: Duration,
    mut cancel: tokio::sync::watch::Receiver<bool>,
    started: impl FnOnce(u32) -> Result<()>,
) -> CaptureOutcome {
    if max == 0 || max > 64 * 1024 || deadline.is_zero() || deadline > Duration::from_secs(600) {
        return CaptureOutcome::NeverStarted(Error::Unavailable(
            "invalid supervised capture bounds",
        ));
    }
    if *cancel.borrow() {
        return CaptureOutcome::NeverStarted(Error::Unavailable("sign-in cancelled before launch"));
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command.as_std_mut().process_group(0);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => return CaptureOutcome::NeverStarted(Error::LaunchNotStarted(error)),
    };
    let Some(pid) = child.id().filter(|pid| *pid > 1) else {
        return CaptureOutcome::Unproven;
    };
    let Some(group) = i32::try_from(pid).ok().and_then(Pid::from_raw) else {
        return CaptureOutcome::Unproven;
    };
    let mut custody = CaptureGroup(Some(group));
    let recorded = started(pid);
    let Some(mut stdout) = child.stdout.take() else {
        return CaptureOutcome::Unproven;
    };
    let Some(mut stderr) = child.stderr.take() else {
        return CaptureOutcome::Unproven;
    };
    let result = match recorded {
        Err(error) => Err(error),
        Ok(()) => {
            let execution = async {
                let output = async {
                    let mut bytes = zeroize::Zeroizing::new(Vec::new());
                    (&mut stdout)
                        .take(max as u64 + 1)
                        .read_to_end(&mut bytes)
                        .await?;
                    if bytes.len() > max {
                        return Err(Error::Protocol("login output limit"));
                    }
                    Ok(bytes)
                };
                let (bytes, _) = tokio::try_join!(output, drain(&mut stderr, 1024 * 1024))?;
                Ok(bytes)
            };
            tokio::select! {
                biased;
                _ = async { if !*cancel.borrow() { let _ = cancel.changed().await; } } =>
                    Err(Error::Unavailable("sign-in cancelled")),
                result = tokio::time::timeout(deadline, execution) =>
                    result.unwrap_or(Err(Error::Unavailable("sign-in timed out"))),
            }
        }
    };
    // Error/timeout/cancellation stops the still-unreaped owned group. With
    // complete streams, allow the leader to exit naturally so closing stdout
    // immediately before exit cannot turn a successful login into SIGKILL.
    if result.is_err() {
        if child.id() == Some(pid) {
            let _ = kill_process_group(group, Signal::KILL);
        }
        custody.0 = None;
    }
    let cleanup = tokio::time::timeout(Duration::from_secs(5), async {
        let exit = async {
            let status = child.wait().await;
            // No await between reaping and disarming: never signal a recycled
            // process-group number from the future's Drop or later cleanup.
            custody.0 = None;
            status
        };
        tokio::join!(exit, drain_to_eof(&mut stdout), drain_to_eof(&mut stderr))
    })
    .await;
    let Ok((Ok(status), stdout, stderr)) = cleanup else {
        return CaptureOutcome::Unproven;
    };
    if !stdout.complete || !stderr.complete {
        return CaptureOutcome::Unproven;
    }
    if !group_absent(group).await {
        return CaptureOutcome::Unproven;
    }
    if !status.success() && result.is_ok() {
        return CaptureOutcome::Joined(Err(Error::Unavailable("sign-in did not complete")));
    }
    CaptureOutcome::Joined(result)
}

pub async fn capture(mut command: Command, max: usize, deadline: Duration) -> Result<Vec<u8>> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command.as_std_mut().process_group(0);
    let mut child = command.spawn()?;
    let group = child
        .id()
        .filter(|pid| *pid > 1)
        .and_then(|pid| i32::try_from(pid).ok())
        .and_then(Pid::from_raw)
        .ok_or(Error::Protocol("child process identity"))?;
    let mut stdout = child.stdout.take().ok_or(Error::Protocol("stdout"))?;
    let stderr = child.stderr.take().ok_or(Error::Protocol("stderr"))?;
    let result = match tokio::time::timeout(deadline, async {
        let output = async {
            let mut bytes = Vec::new();
            (&mut stdout)
                .take(max as u64 + 1)
                .read_to_end(&mut bytes)
                .await?;
            if bytes.len() > max {
                return Err(Error::Protocol("command output limit"));
            }
            Ok(bytes)
        };
        let (bytes, _) = tokio::try_join!(output, drain(stderr, 1024 * 1024))?;
        Ok::<_, Error>(bytes)
    })
    .await
    {
        Ok(result) => result,
        Err(_) => Err(Error::Unavailable("provider command timed out")),
    };
    // EOF on the pipes only proves the descriptors closed; the leader can
    // still be a moment from its own clean exit, and a group sweep landing in
    // that window rewrites the exit as a signal. Only a failed or timed-out
    // read stops the group before the leader's status is known — the same
    // distinction capture_with_input and capture_supervised already make.
    if result.is_err() {
        let _ = kill_process_group(group, Signal::KILL);
    }
    let status = match tokio::time::timeout(Duration::from_secs(5), child.wait()).await {
        Ok(status) => status?,
        Err(_) => {
            let _ = kill_process_group(group, Signal::KILL);
            let _ = child.wait().await;
            return Err(Error::Unavailable("child did not join"));
        }
    };
    if !group_absent(group).await {
        return Err(Error::Unavailable("process group did not join"));
    }
    if !status.success() {
        return Err(Error::Unavailable("provider command failed"));
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustix::io::{FdFlags, fcntl_getfd};
    use std::{io::Write, os::unix::fs::PermissionsExt};

    fn write_executable(directory: &std::path::Path, mode: u32) -> PathBuf {
        let path = directory.join("provider");
        let mut file = fs::File::create(&path).unwrap();
        file.write_all(b"#!/bin/sh\necho ok\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
        path
    }

    #[tokio::test]
    async fn supervised_capture_distinguishes_no_start_and_joined_output() {
        let root = tempfile::tempdir().unwrap();
        let (_sender, cancel) = tokio::sync::watch::channel(false);
        let missing = capture_supervised(
            Command::new(root.path().join("missing")),
            1024,
            Duration::from_secs(1),
            cancel.clone(),
            |_| panic!("missing executable started"),
        )
        .await;
        assert!(matches!(
            missing,
            CaptureOutcome::NeverStarted(Error::LaunchNotStarted(_))
        ));
        for (script, success) in [
            ("printf synthetic-login-output", true),
            ("printf secret-not-success; exit 7", false),
            ("printf output-too-long", false),
        ] {
            let mut command = Command::new("/bin/sh");
            command.args(["-c", script]);
            let mut pid = 0;
            let outcome = capture_supervised(
                command,
                if script == "printf output-too-long" {
                    1
                } else {
                    1024
                },
                Duration::from_secs(1),
                cancel.clone(),
                |started| {
                    pid = started;
                    Ok(())
                },
            )
            .await;
            match outcome {
                CaptureOutcome::Joined(Ok(bytes)) if success => {
                    assert_eq!(&**bytes, b"synthetic-login-output")
                }
                CaptureOutcome::Joined(Err(_)) if !success => (),
                _ => panic!("capture did not preserve join and result distinction"),
            }
            assert!(prove_process_group_absent(pid).is_ok());
        }
    }

    #[tokio::test]
    async fn supervised_capture_cancel_deadline_and_drop_stop_owned_groups() {
        for mode in ["cancel", "deadline", "drop"] {
            let mut command = Command::new("/bin/sh");
            // Do not introduce an orphan-reaping dependency into the joined
            // cancellation/deadline/drop fixture.
            command.args(["-c", "exec sleep 30"]);
            let (sender, cancel) = tokio::sync::watch::channel(false);
            let (ready, pid) = tokio::sync::oneshot::channel();
            let task = tokio::spawn(capture_supervised(
                command,
                1024,
                if mode == "deadline" {
                    Duration::from_millis(25)
                } else {
                    Duration::from_secs(30)
                },
                cancel,
                |pid| {
                    let _ = ready.send(pid);
                    Ok(())
                },
            ));
            let pid = tokio::time::timeout(Duration::from_secs(5), pid)
                .await
                .unwrap()
                .unwrap();
            if mode == "drop" {
                task.abort();
                match task.await {
                    Err(error) => assert!(error.is_cancelled()),
                    Ok(_) => panic!("capture was not dropped"),
                }
                tokio::time::timeout(Duration::from_secs(5), async {
                    while prove_process_group_absent(pid).is_err() {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await
                .unwrap();
            } else {
                if mode == "cancel" {
                    sender.send(true).unwrap();
                }
                let outcome = tokio::time::timeout(Duration::from_secs(15), task)
                    .await
                    .unwrap()
                    .unwrap();
                assert!(matches!(outcome, CaptureOutcome::Joined(Err(_))));
                assert!(prove_process_group_absent(pid).is_ok());
            }
        }
    }

    #[test]
    fn provider_version_metadata_accepts_official_codex_prereleases() {
        assert_eq!(
            parse_version(Provider::Codex, "codex-cli 0.155.0-alpha.2.6").unwrap(),
            "0.155.0-alpha.2.6"
        );
        assert_eq!(
            parse_version(Provider::Claude, "2.1.274 (Claude Code)").unwrap(),
            "2.1.274"
        );
        assert_eq!(
            parse_version(Provider::Devin, "devin 3000.10.31 (b98cc431)").unwrap(),
            "3000.10.31"
        );
        for text in [
            "codex-cli 1..3",
            "codex-cli 1.2.3-",
            "codex-cli 1.2.3\ninjected",
            "codex-cli 01.2.3",
        ] {
            assert!(parse_version(Provider::Codex, text).is_err());
        }
        assert!(parse_version(Provider::Claude, "2.1.274-beta").is_err());
    }

    #[test]
    fn host_identity_rejects_a_replacement_even_when_its_new_pin_matches_disk() {
        let directory = tempfile::tempdir().unwrap();
        let path = write_executable(directory.path(), 0o755);
        let running = HostExecutable::capture(path.clone()).unwrap();
        running.verify_pin(&running.sha256).unwrap();
        let replacement = directory.path().join("replacement");
        fs::write(&replacement, b"new implementation").unwrap();
        fs::set_permissions(&replacement, fs::Permissions::from_mode(0o755)).unwrap();
        fs::rename(&replacement, &path).unwrap();
        let new_digest = executable_digest(&path).unwrap();
        assert_ne!(new_digest, running.sha256);
        assert!(running.verify_pin(&new_digest).is_err());
        assert!(running.verify_pin(&running.sha256).is_err());
        let restarted = HostExecutable::capture(path.clone()).unwrap();
        restarted.verify_pin(&new_digest).unwrap();
        // An installer that overwrites instead of renaming is rejected too.
        fs::write(&path, b"third implementation").unwrap();
        assert!(
            restarted
                .verify_pin(&executable_digest(&path).unwrap())
                .is_err()
        );
    }

    #[test]
    fn discovery_repairs_only_owned_single_link_ordinary_executables() {
        let directory = tempfile::tempdir().unwrap();
        let path = write_executable(directory.path(), 0o777);
        let canonical = path.canonicalize().unwrap();
        assert_eq!(
            discover(Provider::Claude, Some(&canonical)).unwrap(),
            canonical
        );
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o7777, 0o755);
        for (mode, tightened) in [(0o702, 0o700), (0o720, 0o700), (0o775, 0o755)] {
            fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
            assert!(repair_executable_mode(&path).unwrap());
            assert_eq!(fs::metadata(&path).unwrap().mode() & 0o7777, tightened);
        }
        for mode in [0o666, 0o4777, 0o2777, 0o1777] {
            fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
            assert!(!repair_executable_mode(&path).unwrap());
            assert_eq!(fs::metadata(&path).unwrap().mode() & 0o7777, mode);
        }
        fs::set_permissions(&path, fs::Permissions::from_mode(0o777)).unwrap();
        fs::hard_link(&path, directory.path().join("other")).unwrap();
        assert!(!repair_executable_mode(&path).unwrap());
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o7777, 0o777);
    }

    #[test]
    fn failed_os_spawn_proves_that_no_child_started() {
        let directory = tempfile::tempdir().unwrap();
        let command = Command::new(directory.path().join("absent-provider"));
        assert!(matches!(
            StreamProcess::spawn(command),
            Err(Error::LaunchNotStarted(_))
        ));
    }

    #[test]
    fn admitted_executable_descriptor_has_cloexec() {
        let directory = tempfile::tempdir().unwrap();
        let path = write_executable(directory.path(), 0o500);
        let file = executable_file(&path).unwrap();
        let flags = fcntl_getfd(&file).unwrap();
        assert!(flags.contains(FdFlags::CLOEXEC));
    }

    #[test]
    fn executable_checks_name_the_failed_rule() {
        let directory = tempfile::tempdir().unwrap();
        let path = write_executable(directory.path(), 0o777);
        match executable_file(&path) {
            Err(Error::Unavailable(message)) => {
                assert!(message.contains("world-writable"), "{message}")
            }
            other => panic!("expected a permissions error, got {other:?}"),
        }
        let path = write_executable(directory.path(), 0o600);
        match executable_file(&path) {
            Err(Error::Unavailable(message)) => assert_eq!(message, "executable is not executable"),
            other => panic!("expected a mode error, got {other:?}"),
        }
    }

    /// Package managers reinstall the same bytes with writable modes. A pin
    /// whose bytes still match tightens the mode instead of failing until the
    /// operator reruns doctor; bytes that changed still fail.
    #[test]
    fn pin_verify_repairs_an_owned_world_writable_executable() {
        let directory = tempfile::tempdir().unwrap();
        let path = write_executable(directory.path(), 0o777)
            .canonicalize()
            .unwrap();
        let sha256 = digest_file(fs::File::open(&path).unwrap(), 1 << 20).unwrap();
        let (_, host_sha256) = host_identity().unwrap();
        let pin = Pin {
            provider: Provider::Claude,
            executable: path.clone(),
            sha256,
            version: "2.1.282".into(),
            host_sha256,
            observed_at_ms: 0,
        };
        pin.verify().unwrap();
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o755);
        // Changed bytes are not adopted, writable or not.
        fs::write(&path, b"#!/bin/sh\necho changed\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o777)).unwrap();
        assert!(pin.verify().is_err());
    }

    #[test]
    fn setuid_setgid_and_sticky_executables_are_rejected() {
        let directory = tempfile::tempdir().unwrap();
        for mode in [0o4755, 0o2755, 0o6755, 0o1755] {
            let path = write_executable(directory.path(), mode);
            assert!(
                executable_file(&path).is_err(),
                "mode 0o{mode:o} should be rejected",
            );
        }
    }

    #[tokio::test]
    async fn absent_process_group_is_proven_by_esrch() {
        let mut command = Command::new("/bin/echo");
        command.arg("done").env_clear();
        command.as_std_mut().process_group(0);
        let child = command.spawn().unwrap();
        let pid = child.id().unwrap();
        child.wait_with_output().await.unwrap();
        assert!(prove_process_group_absent(pid).is_ok());
    }

    #[tokio::test]
    async fn present_process_group_is_not_proven_absent() {
        let mut command = Command::new("/bin/sleep");
        command.arg("60").env_clear();
        command.as_std_mut().process_group(0);
        let mut child = command.spawn().unwrap();
        let pid = child.id().unwrap();
        let proof = prove_process_group_absent(pid);
        child.kill().await.unwrap();
        child.wait().await.unwrap();
        assert!(proof.is_err());
    }

    #[test]
    fn zero_process_group_id_is_rejected_as_absence_proof() {
        assert!(prove_process_group_absent(0).is_err());
    }

    /// A pinned executable digest that bypasses the verified-digest cache, so
    /// tests can observe the first `Pin::load` re-hash directly.
    fn uncached_digest(path: &Path) -> String {
        digest_file(executable_file(path).unwrap(), 512 * 1024 * 1024).unwrap()
    }

    fn codex_pin(root: &Path, executable: &Path, sha256: String) -> Pin {
        let (_, host_sha256) = host_identity().unwrap();
        let mut pin = Pin {
            provider: Provider::Codex,
            executable: executable.to_owned(),
            sha256,
            version: "0.0.0".into(),
            host_sha256,
            observed_at_ms: crate::now_ms(),
        };
        pin.save(root).unwrap();
        pin
    }

    #[test]
    fn repeated_pin_loads_verify_against_one_executable_digest() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let executable = write_executable(&root, 0o755).canonicalize().unwrap();
        host_identity().unwrap();
        let pin = codex_pin(&root, &executable, uncached_digest(&executable));
        let digested = digested_executables(&pin.executable);
        for _ in 0..3 {
            Pin::load(&root, Provider::Codex).unwrap();
        }
        // Each load re-verifies through the inode-bound digest cache: at most
        // one digest per load. The shared 16-entry cache may evict the
        // custodied copy between loads when sibling tests run in parallel, so
        // three loads can digest up to three times.
        assert!(digested_executables(&pin.executable) - digested <= 3);
    }

    #[test]
    fn touched_and_resized_executables_are_digested_again() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let executable = write_executable(&root, 0o755).canonicalize().unwrap();
        host_identity().unwrap();
        let pin = codex_pin(&root, &executable, uncached_digest(&executable));
        Pin::load(&root, Provider::Codex).unwrap();
        let digested = digested_executables(&pin.executable);
        // A pure metadata touch still re-hashes; identical bytes pass again.
        File::options()
            .write(true)
            .open(&pin.executable)
            .unwrap()
            .set_modified(std::time::SystemTime::now() - Duration::from_secs(60))
            .unwrap();
        Pin::load(&root, Provider::Codex).unwrap();
        assert_eq!(digested_executables(&pin.executable) - digested, 1);
        // A size change re-hashes and the changed bytes fail verification.
        fs::write(&pin.executable, b"#!/bin/sh\necho changed\n").unwrap();
        fs::set_permissions(&pin.executable, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(Pin::load(&root, Provider::Codex).is_err());
        assert_eq!(digested_executables(&pin.executable) - digested, 2);
    }

    #[test]
    fn a_replaced_inode_with_the_same_size_is_digested_again() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let executable = write_executable(&root, 0o755).canonicalize().unwrap();
        host_identity().unwrap();
        let pin = codex_pin(&root, &executable, uncached_digest(&executable));
        Pin::load(&root, Provider::Codex).unwrap();
        let digested = digested_executables(&pin.executable);
        // Same size, different inode and bytes: a stale path or size cache
        // would return the old digest; verification must fail closed.
        let replacement = root.join("replacement");
        fs::write(&replacement, b"#!/bin/sh\necho no\n").unwrap();
        fs::set_permissions(&replacement, fs::Permissions::from_mode(0o755)).unwrap();
        fs::rename(&replacement, &pin.executable).unwrap();
        assert!(Pin::load(&root, Provider::Codex).is_err());
        assert_eq!(digested_executables(&pin.executable) - digested, 1);
    }

    fn claude_pin(root: &Path, executable: &Path, version: &str) -> Pin {
        let (_, host_sha256) = host_identity().unwrap();
        let mut pin = Pin {
            provider: Provider::Claude,
            executable: executable.to_owned(),
            sha256: uncached_digest(executable),
            version: version.into(),
            host_sha256,
            observed_at_ms: crate::now_ms(),
        };
        pin.save(root).unwrap();
        pin
    }

    /// The failure this whole change exists to prevent: a provider upgrade
    /// replaces the discovered binary, and the pin must keep pointing at the
    /// admitted bytes rather than the moved path.
    #[test]
    fn a_pinned_provider_update_cannot_move_the_custodied_executable() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let executable = write_executable(&root, 0o755).canonicalize().unwrap();
        let pin = claude_pin(&root, &executable, "2.1.300");
        let custody = pin.executable.clone();
        assert!(custody.starts_with(root.join("providers").join("bin")));
        assert_eq!(fs::metadata(&custody).unwrap().mode() & 0o777, 0o700);
        // The provider's own path is replaced — the pinned copy does not move.
        fs::write(&executable, b"#!/bin/sh\necho newer\n").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        let loaded = Pin::load(&root, Provider::Claude).unwrap();
        assert_eq!(loaded.executable, custody);
        // Stale custody copies for the same provider are retired on save.
        assert_eq!(
            fs::read_dir(root.join("providers").join("bin"))
                .unwrap()
                .count(),
            1
        );
    }

    /// A pin recorded by an older xcb binds its host hash; identical provider
    /// bytes prove nothing about the artifact changed, so the binding heals
    /// instead of demanding `xcb doctor` after every upgrade.
    #[test]
    fn load_heals_a_pin_bound_to_a_replaced_host_binary() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let executable = write_executable(&root, 0o755).canonicalize().unwrap();
        let pin = claude_pin(&root, &executable, "2.1.300");
        let record = root.join("providers").join("claude.json");
        let stale = "0".repeat(64);
        let bytes = fs::read_to_string(&record)
            .unwrap()
            .replace(&pin.host_sha256, &stale);
        fs::write(&record, bytes).unwrap();
        let healed = Pin::load(&root, Provider::Claude).unwrap();
        let (_, host_sha256) = host_identity().unwrap();
        assert_eq!(healed.host_sha256, host_sha256);
        // A drifted provider digest still refuses to heal.
        fs::write(
            &record,
            fs::read_to_string(&record)
                .unwrap()
                .replace(&host_sha256, &stale),
        )
        .unwrap();
        fs::write(&pin.executable, b"#!/bin/sh\necho tampered\n").unwrap();
        fs::set_permissions(&pin.executable, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(Pin::load(&root, Provider::Claude).is_err());
    }

    /// Pins written before custody point at the live provider path; loading
    /// one migrates the admitted bytes into private custody so subsequent
    /// provider upgrades cannot break it.
    #[test]
    fn load_migrates_a_live_path_pin_into_custody() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let executable = write_executable(&root, 0o755).canonicalize().unwrap();
        let (_, host_sha256) = host_identity().unwrap();
        let legacy = Pin {
            provider: Provider::Claude,
            executable: executable.clone(),
            sha256: uncached_digest(&executable),
            version: "2.1.300".into(),
            host_sha256,
            observed_at_ms: 0,
        };
        private::directory(&root.join("providers")).unwrap();
        private::create(
            &root.join("providers").join("claude.json"),
            serde_json::to_vec_pretty(&legacy).unwrap().as_slice(),
        )
        .unwrap();
        let migrated = Pin::load(&root, Provider::Claude).unwrap();
        assert!(migrated.executable.starts_with(root.join("providers/bin")));
        fs::write(&executable, b"#!/bin/sh\necho newer\n").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(Pin::load(&root, Provider::Claude).is_ok());
    }

    fn version_script(directory: &Path, version: &str) -> PathBuf {
        let path = directory.join("provider");
        fs::write(&path, format!("#!/bin/sh\necho {version}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        path.canonicalize().unwrap()
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn refresh_adopts_an_admitted_update_and_keeps_routes_on_the_pin() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let home = private::directory(&root.join("home")).unwrap();
        let executable = version_script(&root, "2.1.300");
        let pin = claude_pin(&root, &executable, "2.1.300");
        let report = refresh_provider(&root, Provider::Claude, Some(&executable), &home).await;
        assert_eq!(report.outcome, RefreshOutcome::Kept);
        // An upstream update at the same path: admitted, so the pin follows it.
        fs::write(&executable, b"#!/bin/sh\necho 2.1.301\n").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let report = refresh_provider(&root, Provider::Claude, Some(&executable), &home).await;
        assert_eq!(report.outcome, RefreshOutcome::Adopted);
        let loaded = Pin::load(&root, Provider::Claude).unwrap();
        assert_eq!(loaded.version, "2.1.301");
        assert_ne!(loaded.executable, pin.executable);
        // The retired custody copy was collected with the adoption.
        assert!(!pin.executable.exists());
    }

    /// Exact-artifact adoption without an xcb release: a pending Codex
    /// build adopts on the first pass after the catalog lists its
    /// `(version, digest)` pair.
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn a_pending_codex_build_adopts_once_the_catalog_lists_it() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let home = private::directory(&root.join("home")).unwrap();
        let executable = version_script(&root, "codex-cli 0.156.1");
        let pin = codex_pin(&root, &executable, uncached_digest(&executable));
        fs::write(&executable, b"#!/bin/sh\necho codex-cli 0.157.1\n").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let report = refresh_provider(&root, Provider::Codex, Some(&executable), &home).await;
        assert_eq!(report.outcome, RefreshOutcome::PendingCatalog);
        let report = refresh_provider(&root, Provider::Codex, Some(&executable), &home).await;
        assert_eq!(report.outcome, RefreshOutcome::PendingCatalog);
        // The catalog publication lists the pending pair.
        let sha = uncached_digest(&executable);
        let catalog = serde_json::json!({
            "version": 1,
            "codex": [{"version": "0.157.1", "sha256": sha,
                "platform": "darwin-aarch64"}],
        });
        private::create(
            &root.join("providers").join("catalog.json"),
            catalog.to_string().as_bytes(),
        )
        .unwrap();
        let report = refresh_provider(&root, Provider::Codex, Some(&executable), &home).await;
        assert_eq!(report.outcome, RefreshOutcome::Adopted);
        let loaded = Pin::load(&root, Provider::Codex).unwrap();
        assert_eq!(loaded.version, "0.157.1");
        assert_eq!(loaded.sha256, sha);
        assert_ne!(loaded.executable, pin.executable);
        assert!(!root.join("providers").join("codex.rejected").exists());
    }

    /// A build the host can inspect but no admission path accepts waits on
    /// the reviewed-builds catalog rather than being rejected outright: the
    /// pin keeps routing, the digest is remembered, and a later catalog
    /// publication adopts it.
    #[tokio::test]
    async fn refresh_parks_an_unadmitted_build_for_the_catalog() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let home = private::directory(&root.join("home")).unwrap();
        let executable = version_script(&root, "2.1.300");
        let pin = claude_pin(&root, &executable, "2.1.300");
        fs::write(&executable, b"#!/bin/sh\necho 9.9.9\n").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let report = refresh_provider(&root, Provider::Claude, Some(&executable), &home).await;
        assert_eq!(report.outcome, RefreshOutcome::PendingCatalog);
        assert_eq!(
            report.detail.as_deref(),
            Some("9.9.9 awaiting catalog admission")
        );
        // The previous pin is untouched and still resolves.
        let loaded = Pin::load(&root, Provider::Claude).unwrap();
        assert_eq!(loaded.executable, pin.executable);
        // The marker records the pending decision: a second pass skips
        // inspection and reports the stored version.
        let marker = root.join("providers").join("claude.rejected");
        let marker: serde_json::Value =
            serde_json::from_slice(&fs::read(&marker).unwrap()).unwrap();
        assert_eq!(marker["sha256"], uncached_digest(&executable));
        assert_eq!(marker["providerVersion"], "9.9.9");
        assert_eq!(marker["reason"], "unadmitted");
        let report = refresh_provider(&root, Provider::Claude, Some(&executable), &home).await;
        assert_eq!(report.outcome, RefreshOutcome::PendingCatalog);
        fs::remove_file(&executable).unwrap();
        let report = refresh_provider(&root, Provider::Claude, Some(&executable), &home).await;
        assert_eq!(report.outcome, RefreshOutcome::Rejected);
    }

    /// A build that could not even be inspected stays rejected while its
    /// bytes stand: the catalog must never be consulted for it.
    #[tokio::test]
    async fn an_inspection_failure_stays_rejected_until_the_bytes_change() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let home = private::directory(&root.join("home")).unwrap();
        let executable = version_script(&root, "2.1.300");
        claude_pin(&root, &executable, "2.1.300");
        // A binary whose --version output never parses cannot be qualified.
        fs::write(&executable, b"#!/bin/sh\necho not-a-version\n").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let report = refresh_provider(&root, Provider::Claude, Some(&executable), &home).await;
        assert_eq!(report.outcome, RefreshOutcome::Rejected);
        let marker = root.join("providers").join("claude.rejected");
        let marker: serde_json::Value =
            serde_json::from_slice(&fs::read(&marker).unwrap()).unwrap();
        assert_eq!(marker["reason"], "inspection");
        // Even listing the digest would not help: the marker-hit arm only
        // reconsiders for catalog-listed digests.
        let report = refresh_provider(&root, Provider::Claude, Some(&executable), &home).await;
        assert_eq!(report.outcome, RefreshOutcome::Rejected);
        assert_eq!(report.detail, None);
    }

    #[tokio::test]
    async fn a_catalog_denied_build_is_rejected_outright() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let home = private::directory(&root.join("home")).unwrap();
        let executable = version_script(&root, "2.1.300");
        claude_pin(&root, &executable, "2.1.300");
        fs::write(&executable, b"#!/bin/sh\necho 9.9.9\n").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let sha = uncached_digest(&executable);
        let catalog = serde_json::json!({
            "version": 1,
            "claude": [], "codex": [], "devin": [],
            "deny": {"claude": [sha]}
        });
        private::create(
            &root.join("providers").join("catalog.json"),
            catalog.to_string().as_bytes(),
        )
        .unwrap();
        let report = refresh_provider(&root, Provider::Claude, Some(&executable), &home).await;
        assert_eq!(report.outcome, RefreshOutcome::Rejected);
        assert!(report.detail.unwrap().contains("denied"));
        // The denial is remembered; a second pass stays rejected.
        let report = refresh_provider(&root, Provider::Claude, Some(&executable), &home).await;
        assert_eq!(report.outcome, RefreshOutcome::Rejected);
    }

    /// Markers written before the catalog flow stored the raw digest bytes;
    /// they read as unadmitted builds awaiting review.
    #[tokio::test]
    async fn a_raw_digest_marker_is_read_as_pending() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let home = private::directory(&root.join("home")).unwrap();
        let executable = version_script(&root, "2.1.300");
        claude_pin(&root, &executable, "2.1.300");
        fs::write(&executable, b"#!/bin/sh\necho 9.9.9\n").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        private::create(
            &root.join("providers").join("claude.rejected"),
            uncached_digest(&executable).as_bytes(),
        )
        .unwrap();
        let report = refresh_provider(&root, Provider::Claude, Some(&executable), &home).await;
        assert_eq!(report.outcome, RefreshOutcome::PendingCatalog);
        assert_eq!(
            report.detail.as_deref(),
            Some("discovered build awaiting catalog admission")
        );
    }

    #[tokio::test]
    async fn refresh_leaves_unpinned_providers_to_doctor() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let home = private::directory(&root.join("home")).unwrap();
        let executable = version_script(&root, "2.1.300");
        let report = refresh_provider(&root, Provider::Claude, Some(&executable), &home).await;
        assert_eq!(report.outcome, RefreshOutcome::Unpinned);
        assert!(!root.join("providers").join("claude.json").exists());
    }

    #[test]
    fn snapshot_clones_then_proves_the_pinned_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let executable = write_executable(&root, 0o755).canonicalize().unwrap();
        let pin = codex_pin(&root, &executable, uncached_digest(&executable));
        let launch = private::directory(&root.join("launch")).unwrap();
        let snapshot = pin.snapshot(&launch).unwrap();
        assert_eq!(executable_digest(&snapshot).unwrap(), pin.sha256);
        assert_eq!(
            fs::metadata(&snapshot).unwrap().permissions().mode() & 0o7777,
            0o500
        );
        // The snapshot is a distinct inode: on clone-capable filesystems this
        // also proves the clone path produced launch-owned bytes.
        assert_ne!(
            fs::metadata(&snapshot).unwrap().ino(),
            fs::metadata(&pin.executable).unwrap().ino()
        );
        assert!(pin.snapshot(&launch).is_err());
    }

    /// Launch-path timing on a synthetic 150 MiB provider executable that
    /// embeds the Codex catalog fixture. Fixtures live under `XCB_BENCH_DIR`
    /// (default: the system temp directory). Run with
    /// `cargo test -p xcb-runtime launch_path_benchmark --locked -- --ignored --nocapture`.
    #[test]
    #[ignore = "launch-path benchmark; run explicitly with --ignored --nocapture"]
    fn launch_path_benchmark() {
        use std::time::Instant;
        let base = std::env::var_os("XCB_BENCH_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        fs::create_dir_all(&base).unwrap();
        let directory = tempfile::tempdir_in(&base).unwrap();
        let root = directory.path().canonicalize().unwrap();
        let executable = root.join("codex");
        {
            // 150 MiB of xorshift filler with the pretty-printed catalog
            // fixture embedded once, NUL-terminated, a third of the way in.
            let mut file = std::io::BufWriter::new(fs::File::create(&executable).unwrap());
            let mut state = 0x9E37_79B9_7F4A_7C15u64;
            let mut block = [0u8; 64 * 1024];
            let total = 150 * 1024 * 1024usize;
            let catalog_at = total / 3;
            let mut written = 0usize;
            let mut fixture =
                serde_json::to_vec_pretty(&crate::codex::fixture_catalog_source()).unwrap();
            fixture.push(0);
            while written < total {
                for chunk in block.chunks_mut(8) {
                    state ^= state << 13;
                    state ^= state >> 7;
                    state ^= state << 17;
                    chunk.copy_from_slice(&state.to_le_bytes()[..chunk.len()]);
                }
                if written == catalog_at {
                    file.write_all(&fixture).unwrap();
                }
                file.write_all(&block).unwrap();
                written += block.len();
            }
            file.flush().unwrap();
        }
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        let sha256 = executable_digest(&executable).unwrap();
        let (_, host_sha256) = host_identity().unwrap();
        let mut pin = Pin {
            provider: Provider::Codex,
            executable: executable.clone(),
            sha256: sha256.clone(),
            version: crate::codex::VERSION.into(),
            host_sha256,
            observed_at_ms: crate::now_ms(),
        };
        pin.save(&root).unwrap();
        let size = fs::metadata(&executable).unwrap().len();
        eprintln!(
            "benchmark executable: {} bytes at {}",
            size,
            executable.display()
        );
        for round in 1..=3 {
            let started = Instant::now();
            let loaded = Pin::load(&root, Provider::Codex).unwrap();
            assert_eq!(loaded.sha256, sha256);
            eprintln!("Pin::load #{round}: {:?}", started.elapsed());
        }
        for round in 1..=2 {
            let launch = private::directory(&root.join(format!("launch-{round}"))).unwrap();
            let started = Instant::now();
            let snapshot = pin.snapshot(&launch).unwrap();
            eprintln!("Pin::snapshot #{round}: {:?}", started.elapsed());
            assert_eq!(fs::metadata(&snapshot).unwrap().len(), size);
        }
        for round in 1..=3 {
            let started = Instant::now();
            let catalog =
                crate::codex::static_catalog_bound(&root, &pin, Some("gpt-6-astra"), &sha256)
                    .unwrap();
            eprintln!("static_catalog #{round}: {:?}", started.elapsed());
            assert!(catalog.admission.models.contains("gpt-6-astra"));
        }
    }
    #[tokio::test]
    async fn interrupted_frame_read_preserves_the_partial_json_prefix() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "printf '{\"ok\":'; read -r next; printf 'true}\\n'"]);
        let mut process = StreamProcess::spawn(command).unwrap();
        let started = tokio::time::Instant::now();
        loop {
            assert!(
                tokio::time::timeout(Duration::from_millis(10), process.frame())
                    .await
                    .is_err()
            );
            if !process.frame_buffer.is_empty() {
                break;
            }
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "fixture never wrote its prefix"
            );
        }
        process
            .send(&serde_json::json!({"continue":true}))
            .await
            .unwrap();
        let frame = tokio::time::timeout(Duration::from_secs(5), process.frame())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&frame).unwrap(),
            serde_json::json!({"ok":true})
        );
        assert!(process.join().await);
    }
    #[tokio::test]
    async fn stderr_volume_never_leaves_a_proven_join_unsettled() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "head -c 2000000 /dev/zero >&2; echo '{}'"]);
        let mut process = StreamProcess::spawn(command).unwrap();
        let frame = tokio::time::timeout(Duration::from_secs(10), process.frame())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(frame, b"{}\n");
        assert!(process.stderr_overflow().is_none(), "unknown before join");
        assert!(process.join().await);
        assert_eq!(process.stderr_overflow(), Some(2_000_000));

        let mut command = Command::new("/bin/sh");
        command.args(["-c", "echo quiet >&2; echo '{}'"]);
        let mut process = StreamProcess::spawn(command).unwrap();
        assert!(process.frame().await.unwrap().is_some());
        assert!(process.join().await);
        assert!(process.stderr_overflow().is_none());
    }
}
