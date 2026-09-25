//! Offline Linux commands in a separately owned VM. SSH exit is not worker
//! custody: only the trusted guest's exact cgroup/stream receipt can join it.
use crate::{Error, Result, broker::snapshot::VerifiedSnapshot, digest, private, process};
use base64::Engine;
use serde::{Deserialize, Serialize};
use std::{
    path::{Component, Path, PathBuf},
    time::Duration,
};
use tokio::sync::watch;
use xcb_core::Id;

const PUBLIC_CACHE: &[u8] = include_bytes!("command/public_cache.py");
const GIT_PROJECTION: &[u8] = include_bytes!("command/git_projection.py");
const GIT_PROJECTION_TESTS: &[u8] = include_bytes!("command/test_git_projection.py");
const GUEST: &[u8] = include_bytes!("command/guest.py");
const POLICY: &[u8] = include_bytes!("command/lima.yaml");
const QUALIFIER: &[u8] = include_bytes!("command/test_live.py");
const MAX_SNAPSHOT: usize = 96 * 1024 * 1024;
const MAX_CHANGES: usize = 24 * 1024 * 1024;
const MAX_RESPONSE: usize = 36 * 1024 * 1024;
const MAX_WORKSPACE: usize = 2 * 1024 * 1024 * 1024;

fn canonical(value: &impl Serialize) -> Result<Vec<u8>> {
    let mut value = serde_json::to_value(value)?;
    // Workspace dependencies enable preserve_order; wire hashes still require
    // recursive lexical ordering, matching Python's sort_keys encoder.
    value.sort_all_objects();
    Ok(serde_json::to_vec(&value)?)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommandRequest {
    pub argv: Vec<String>,
    pub cwd: String,
    pub timeout_ms: u32,
    pub network: CommandNetwork,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CommandNetwork {
    None,
}

fn relative(value: &str) -> bool {
    value == "."
        || (xcb_core::relative_path(value)
            && Path::new(value)
                .components()
                .all(|part| matches!(part, Component::Normal(_)))
            && value.split('/').next() != Some(".git"))
}

impl CommandRequest {
    pub fn validate(&self) -> Result<()> {
        if self.argv.is_empty()
            || self.argv.len() > 64
            || self.argv[0].is_empty()
            || self.argv.iter().any(|arg| arg.contains('\0'))
            || self.argv.iter().map(String::len).sum::<usize>() > 32768
            || !relative(&self.cwd)
            || self.timeout_ms == 0
            || self.timeout_ms > 600000
        {
            return Err(Error::Unavailable("invalid bounded offline command"));
        }
        Ok(())
    }
}

#[derive(Clone)]
pub struct CommandInput {
    pub command_id: Id,
    pub run_id: Id,
    pub workspace_id: String,
    pub snapshot_path: PathBuf,
    /// The snapshot bytes already encoded and verified by the caller; the
    /// backend compares digests instead of re-reading the snapshot file.
    pub snapshot: VerifiedSnapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommandCustody {
    pub version: u32,
    pub command_id: Id,
    pub run_id: Id,
    pub workspace_id: String,
    pub snapshot_sha256: String,
    pub request_sha256: String,
    pub backend_sha256: String,
    pub boot_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommandOutput {
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
    pub cancelled: bool,
    pub truncated: bool,
    pub changes_path: Option<PathBuf>,
    pub changes_sha256: Option<String>,
    #[serde(default)]
    pub cleanup_pending: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommandOutcome {
    pub custody: CommandCustody,
    pub joined: bool,
    pub output: Option<CommandOutput>,
    pub error: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Manifest {
    version: u32,
    lima_executable: PathBuf,
    lima_sha256: String,
    guest_sha256: String,
    public_cache_sha256: String,
    git_projection_sha256: String,
    git_projection_tests_sha256: String,
    policy_sha256: String,
    boot_id: String,
    bwrap_sha256: String,
    tool_versions: Vec<String>,
    tool_digests: std::collections::BTreeMap<String, ToolIdentity>,
    bounds: serde_json::Value,
    qualification: QualificationBinding,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct QualificationBinding {
    suite_sha256: String,
    environment_sha256: String,
    evidence_sha256: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct QualificationEvidence {
    version: u32,
    environment_sha256: String,
    suite_sha256: String,
    cases: Vec<QualificationCase>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct QualificationCase {
    name: String,
    result_sha256: String,
    response: String,
}

const REQUIRED_CASES: &[&str] = &[
    "file-edit-and-python",
    "uid-filesystem-network-and-userns",
    "detached-setsid-closed-stdio",
    "deadline-kills-descendants",
    "output-overflow-joins",
    "pre-cancel-never-executes",
    "offline-language-toolchains",
    "peer-work-and-control-denied",
    "git-projection-unit-semantics",
    "readonly-filtered-git-inspection",
    "public-cache-isolation-and-key-binding",
    "offline-cargo-bun-cache-usage",
];

fn qualification(root: &Path, manifest: &Manifest) -> Result<()> {
    let binding = &manifest.qualification;
    let mut environment = serde_json::to_value(manifest)?;
    environment
        .as_object_mut()
        .ok_or(Error::Protocol("command manifest"))?
        .remove("qualification");
    if binding.suite_sha256 != digest(QUALIFIER)
        || binding.environment_sha256 != digest(canonical(&environment)?)
        || !hash(&binding.evidence_sha256)
    {
        return Err(Error::Unavailable("command boundary qualification changed"));
    }
    let bytes = private::read(
        &root.join(format!("qualification-{}.json", binding.evidence_sha256)),
        2 * 1024 * 1024,
    )?;
    let evidence: QualificationEvidence = serde_json::from_slice(&bytes)?;
    if digest(&bytes) != binding.evidence_sha256
        || evidence.version != 1
        || evidence.environment_sha256 != binding.environment_sha256
        || evidence.suite_sha256 != binding.suite_sha256
        || evidence.cases.len() != REQUIRED_CASES.len()
    {
        return Err(Error::Unavailable("command boundary evidence mismatch"));
    }
    let mut commands = std::collections::HashSet::new();
    for (case, name) in evidence.cases.iter().zip(REQUIRED_CASES) {
        let result: GuestResult = serde_json::from_str(&case.response)?;
        if case.name != *name
            || digest(case.response.as_bytes()) != case.result_sha256
            || result.version != 1
            || !result.joined
            || result.error.is_some()
            || result.custody.backend_sha256 != binding.environment_sha256
            || result.custody.boot_id != manifest.boot_id
            || !identifier(&result.custody.command_id)
            || !commands.insert(result.custody.command_id.clone())
            || result.unstarted
            || !guest_join_shape(&result, &result.custody)
        {
            return Err(Error::Unavailable("command boundary case failed"));
        }
        let good = match *name {
            "file-edit-and-python" => result.exit_code == Some(0) && result.stdout == "success\n",
            "uid-filesystem-network-and-userns" => {
                result.exit_code == Some(0) && result.stdout == "all-denied\n"
            }
            "detached-setsid-closed-stdio" => result.exit_code == Some(0) && !result.timed_out,
            "deadline-kills-descendants" => result.timed_out && result.exit_code != Some(0),
            "output-overflow-joins" => result.truncated && result.stdout.len() <= 256 * 1024,
            "pre-cancel-never-executes" => result.cancelled && result.exit_code.is_none(),
            "offline-language-toolchains" => {
                result.exit_code == Some(0) && result.stdout == "42\n43\n44\n"
            }
            "peer-work-and-control-denied" => {
                result.exit_code == Some(0) && result.stdout == "peer-denied\n"
            }
            "git-projection-unit-semantics" => {
                result.exit_code == Some(0) && result.stdout == "git-tests-passed\n"
            }
            "readonly-filtered-git-inspection" => {
                result.exit_code == Some(0) && result.stdout == "git-projected-readonly\n"
            }
            "public-cache-isolation-and-key-binding" => {
                result.exit_code == Some(0) && result.stdout == "cache-isolation-key-binding\n"
            }
            "offline-cargo-bun-cache-usage" => {
                result.exit_code == Some(0) && result.stdout == "offline-cargo-bun-passed\n"
            }
            _ => false,
        };
        if !good {
            return Err(Error::Unavailable("command boundary assertion failed"));
        }
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Inspection {
    version: u32,
    boot_id: String,
    agent_sha256: String,
    public_cache_sha256: String,
    git_projection_sha256: String,
    bwrap_sha256: String,
    tool_digests: std::collections::BTreeMap<String, ToolIdentity>,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ToolIdentity {
    path: String,
    sha256: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GuestResult {
    version: u32,
    custody: CommandCustody,
    joined: bool,
    cgroup: Option<Cgroup>,
    #[serde(default)]
    unstarted: bool,
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
    timed_out: bool,
    cancelled: bool,
    truncated: bool,
    error: Option<String>,
    changes_base64: Option<String>,
    changes_sha256: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Cgroup {
    path: String,
    dev: u64,
    ino: u64,
}

fn guest_join_shape(result: &GuestResult, custody: &CommandCustody) -> bool {
    if result.unstarted {
        result.joined
            && result.cgroup.is_none()
            && result.exit_code.is_none()
            && result.stdout.is_empty()
            && result.stderr.is_empty()
            && !result.timed_out
            && !result.cancelled
            && !result.truncated
            && result.changes_base64.is_none()
            && result.changes_sha256.is_none()
            && result.error.as_deref() == Some("command did not start: guest preparation failed")
    } else {
        result.cgroup.as_ref().is_some_and(|cgroup| {
            cgroup.dev != 0
                && cgroup.ino != 0
                && cgroup.path
                    == format!(
                        "/sys/fs/cgroup/system.slice/xcb-command-{}.service/worker",
                        custody.command_id
                    )
        })
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GuestBinding {
    version: u32,
    custody: CommandCustody,
    result_sha256: String,
}

#[derive(Clone)]
pub struct CommandBackend {
    root: PathBuf,
    manifest: Manifest,
    identity: String,
}

const PENDING: &str = "pending";
const PENDING_MARKER: &[u8] = b"{\"pending\":true}\n";
/// Written last by a complete marker migration. A pending directory without
/// it may be a crashed partial migration and is rebuilt by merging, never by
/// deleting markers a live command may have created.
const PENDING_READY: &str = "pending-ready";
const PENDING_LIMIT: usize = 16384;
const LEGACY_SCAN_LIMIT: usize = 16384;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JobState {
    /// No start record: the command never reached the guest.
    Unstarted,
    /// A receipt with matching custody proves the guest command joined.
    Joined,
    Unjoined,
}
/// Verify one retained job exactly as admission always has: a started job
/// counts as joined only with a joined receipt bound to its custody.
fn job_joined(job: &Path) -> Result<JobState> {
    private::check_directory(job)?;
    match std::fs::symlink_metadata(job.join("started.json")) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(JobState::Unstarted);
        }
        Err(error) => return Err(error.into()),
        Ok(_) => (),
    }
    let custody: CommandCustody =
        serde_json::from_slice(&private::read(&job.join("custody.json"), 8192)?)?;
    for name in ["outcome.json", "recovered.json"] {
        match private::read(&job.join(name), 2 * 1024 * 1024) {
            Ok(bytes) => {
                let outcome: CommandOutcome = serde_json::from_slice(&bytes)?;
                if outcome.custody == custody && outcome.joined {
                    return Ok(JobState::Joined);
                }
            }
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error),
        }
    }
    Ok(JobState::Unjoined)
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PruneReport {
    pub candidates: Vec<String>,
    pub archived: usize,
    pub retained_unjoined: usize,
    pub retained_cleanup_pending: usize,
    pub retained_recent: usize,
}

fn hash(value: &str) -> bool {
    xcb_core::hex64(value)
}
fn identifier(value: &Id) -> bool {
    let text = value.as_str();
    text.len() <= 80
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
}

impl CommandBackend {
    /// Recover an already durable joined result across an explicit backend
    /// refresh. This is read-only and never infers exit from a new VM, a missing
    /// process or the current manifest. Unjoined work still needs its old guest.
    pub fn recover_recorded_join(
        root: &Path,
        custody: &CommandCustody,
    ) -> Result<Option<CommandOutcome>> {
        let root = private::check_directory(root)?;
        if custody.version != 1
            || !identifier(&custody.command_id)
            || !identifier(&custody.run_id)
            || !hash(&custody.workspace_id)
            || !hash(&custody.snapshot_sha256)
            || !hash(&custody.request_sha256)
            || !hash(&custody.backend_sha256)
        {
            return Err(Error::Conflict("invalid recorded command authority"));
        }
        let job = private::check_directory(&root.join("jobs").join(custody.command_id.as_str()))?;
        let recorded: CommandCustody =
            serde_json::from_slice(&private::read(&job.join("custody.json"), 8192)?)?;
        if recorded != *custody {
            return Err(Error::Conflict("recorded command custody changed"));
        }
        for name in ["recovered.json", "outcome.json"] {
            let bytes = match private::read(&job.join(name), 2 * 1024 * 1024) {
                Ok(bytes) => bytes,
                Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            let outcome: CommandOutcome = serde_json::from_slice(&bytes)?;
            if outcome.custody != *custody {
                return Err(Error::Conflict("recorded joined outcome changed"));
            }
            if !outcome.joined {
                continue;
            }
            let Some(output) = outcome.output.as_ref() else {
                // A finished host preflight is a positive durable no-start
                // record. Absence alone is never accepted as exit evidence.
                if matches!(std::fs::symlink_metadata(job.join("started.json")),Err(error) if error.kind()==std::io::ErrorKind::NotFound)
                    && outcome
                        .error
                        .as_deref()
                        .is_some_and(|error| error.starts_with("command did not start:"))
                {
                    #[derive(Deserialize)]
                    #[serde(deny_unknown_fields)]
                    struct Owner {
                        pid: u32,
                        instance: Id,
                    }
                    let owner: Owner =
                        serde_json::from_slice(&private::read(&job.join("owner.json"), 8192)?)?;
                    if owner.pid == 0 || !identifier(&owner.instance) {
                        return Err(Error::Conflict("invalid no-start owner"));
                    }
                    return Ok(Some(outcome));
                }
                let raw = private::read(&job.join("guest-result.json"), MAX_RESPONSE)?;
                let binding: GuestBinding = serde_json::from_slice(&private::read(
                    &job.join("guest-result-binding.json"),
                    8192,
                )?)?;
                let guest: GuestResult = serde_json::from_slice(&raw)?;
                if binding.version == 1
                    && binding.custody == *custody
                    && binding.result_sha256 == digest(&raw)
                    && guest.version == 1
                    && guest.custody == *custody
                    && guest.unstarted
                    && guest_join_shape(&guest, custody)
                    && guest.error == outcome.error
                {
                    return Ok(Some(outcome));
                }
                return Err(Error::Conflict("recorded no-start proof is incomplete"));
            };
            let raw = private::read(&job.join("guest-result.json"), MAX_RESPONSE)?;
            let binding: GuestBinding = serde_json::from_slice(&private::read(
                &job.join("guest-result-binding.json"),
                8192,
            )?)?;
            if binding.version != 1
                || binding.custody != *custody
                || binding.result_sha256 != digest(&raw)
            {
                return Err(Error::Conflict("recorded guest result digest changed"));
            }
            let guest: GuestResult = serde_json::from_slice(&raw)?;
            if guest.version != 1
                || guest.custody != *custody
                || !guest.joined
                || guest.unstarted
                || !guest_join_shape(&guest, custody)
                || guest.stdout.len() + guest.stderr.len() > 3 * 256 * 1024
                || guest.exit_code != output.exit_code
                || guest.stdout != output.stdout
                || guest.stderr != output.stderr
                || guest.timed_out != output.timed_out
                || guest.cancelled != output.cancelled
                || guest.truncated != output.truncated
                || guest.error != outcome.error
                || guest.changes_sha256 != output.changes_sha256
            {
                return Err(Error::Conflict(
                    "recorded guest join does not match host outcome",
                ));
            }
            match (
                &guest.changes_base64,
                &guest.changes_sha256,
                &output.changes_path,
            ) {
                (Some(encoded), Some(expected), Some(path)) => {
                    let changes = base64::engine::general_purpose::STANDARD
                        .decode(encoded)
                        .map_err(|_| Error::Protocol("recorded changes encoding"))?;
                    if changes.len() > MAX_CHANGES
                        || !hash(expected)
                        || digest(&changes) != *expected
                        || *path != job.join("changes.json")
                        || private::read(path, MAX_CHANGES)? != changes
                    {
                        return Err(Error::Conflict("recorded command changes custody changed"));
                    }
                }
                (None, None, None) => (),
                _ => return Err(Error::Conflict("incomplete recorded command changes")),
            }
            return Ok(Some(outcome));
        }
        Ok(None)
    }

    fn admission(&self) -> Result<private::ExclusiveLock> {
        let path = self.root.join("admission.lock");
        match private::create(&path, b"command-admission-v1\n") {
            Ok(()) => (),
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(error) => return Err(error),
        }
        let file = private::open_file(&path, 64)?;
        file.try_lock().map_err(|_| {
            Error::Unavailable("command runner is busy; retry after its current command joins")
        })?;
        private::same_file(&path, &file)?;
        let file = private::ExclusiveLock::held(file);
        // Only pending markers are scanned, so admission cost follows the
        // number of unjoined commands, not the retained job history. Each
        // named job is still verified against its exact receipts.
        let pending = self.pending_directory()?;
        for (count, entry) in std::fs::read_dir(&pending)?.enumerate() {
            if count >= PENDING_LIMIT {
                return Err(Error::Unavailable("command pending marker limit"));
            }
            let marker = entry?;
            if !marker.file_type()?.is_file() {
                return Err(Error::PrivateState);
            }
            let name = marker.file_name();
            let Some(name) = name.to_str().filter(|name| !name.is_empty()) else {
                return Err(Error::PrivateState);
            };
            if name == PENDING_READY {
                continue;
            }
            match job_joined(&self.root.join("jobs").join(name))? {
                // A crash between the submission records, or between a joined
                // receipt and the marker unlink, leaves a stale marker; the
                // durable receipts are the truth, so it is unlinked here.
                JobState::Unstarted | JobState::Joined => self.clear_pending_name(name)?,
                JobState::Unjoined => {
                    return Err(Error::Unavailable(
                        "command runner has an unjoined command; recover its custody before retrying",
                    ));
                }
            }
        }
        Ok(file)
    }

    /// The pending-marker directory. A root created before markers existed
    /// is migrated once: every retained job is scanned the old way and
    /// unjoined jobs receive markers, so the legacy refusal is preserved.
    /// The ready record is written last, after every marker; a pending
    /// directory without it is rebuilt by merging, so a crash mid-migration
    /// or a racing mark cannot lose an unjoined command's marker.
    fn pending_directory(&self) -> Result<PathBuf> {
        let jobs = self.root.join("jobs");
        let pending = match std::fs::symlink_metadata(jobs.join(PENDING)) {
            Ok(_) => {
                let pending = private::check_directory(&jobs.join(PENDING))?;
                if pending.join(PENDING_READY).exists() {
                    return Ok(pending);
                }
                pending
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                private::directory(&jobs.join(PENDING))?
            }
            Err(error) => return Err(error.into()),
        };
        let mut unjoined = Vec::new();
        for (count, entry) in std::fs::read_dir(&jobs)?.enumerate() {
            if count >= LEGACY_SCAN_LIMIT {
                return Err(Error::Unavailable("command receipt retention limit"));
            }
            let job = entry?.path();
            if job.file_name().and_then(|name| name.to_str()) == Some(PENDING) {
                continue;
            }
            private::check_directory(&job)?;
            if job_joined(&job)? == JobState::Unjoined {
                let name = job
                    .file_name()
                    .and_then(|name| name.to_str())
                    .ok_or(Error::PrivateState)?;
                unjoined.push(name.to_owned());
            }
        }
        let create_marker = |name: &str| -> Result<()> {
            match private::create(&pending.join(name), PENDING_MARKER) {
                Ok(()) => Ok(()),
                Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    Ok(())
                }
                Err(error) => Err(error),
            }
        };
        for name in unjoined {
            create_marker(&name)?;
        }
        create_marker(PENDING_READY)?;
        std::fs::File::open(&pending)?.sync_all()?;
        Ok(pending)
    }
    fn mark_pending(&self, command: &Id) -> Result<()> {
        let pending = self.pending_directory()?;
        match private::create(&pending.join(command.as_str()), PENDING_MARKER) {
            Ok(()) => Ok(()),
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
            Err(error) => Err(error),
        }
    }
    fn clear_pending(&self, command: &Id) -> Result<()> {
        self.clear_pending_name(command.as_str())
    }
    fn clear_pending_name(&self, name: &str) -> Result<()> {
        let pending = self.pending_directory()?;
        match std::fs::remove_file(pending.join(name)) {
            Ok(()) => (),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        }
        std::fs::File::open(&pending)?.sync_all()?;
        Ok(())
    }

    /// Move joined, acknowledged jobs whose newest receipt is older than
    /// `before_ms` into `jobs-archive/`. Jobs with a pending marker, without
    /// a joined receipt (including never-started ones), or whose guest
    /// scratch cleanup is still pending are retained. Nothing is deleted.
    pub fn prune_joined_jobs(root: &Path, before_ms: u64, apply: bool) -> Result<PruneReport> {
        let root = private::check_directory(root)?;
        let marker = private::read(&root.join("xcb-owner.json"), 256)?;
        if marker != b"{\"owner\":\"xcb-command-v1\"}\n" {
            return Err(Error::Unavailable("command root is not owned by xcb"));
        }
        let jobs = private::check_directory(&root.join("jobs"))?;
        let admission = root.join("admission.lock");
        let lock = private::open_file(&admission, 64)?;
        lock.try_lock().map_err(|_| {
            Error::Unavailable("command runner is busy; retry after its current command joins")
        })?;
        let _lock = private::ExclusiveLock::held(lock);
        let pending = jobs.join(PENDING);
        let mut report = PruneReport::default();
        let mut archive = None;
        for entry in std::fs::read_dir(&jobs)? {
            let job = entry?.path();
            let Some(name) = job.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if name == PENDING || !std::fs::symlink_metadata(&job)?.is_dir() {
                continue;
            }
            if pending.join(name).exists() || job_joined(&job)? != JobState::Joined {
                report.retained_unjoined += 1;
                continue;
            }
            let started = job.join("started.json").exists();
            if started && !job.join("acknowledged.json").exists() {
                report.retained_cleanup_pending += 1;
                continue;
            }
            let newest = ["recovered.json", "outcome.json"]
                .into_iter()
                .filter_map(|receipt| std::fs::symlink_metadata(job.join(receipt)).ok())
                .filter_map(|metadata| metadata.modified().ok())
                .filter_map(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|age| age.as_millis().min(u128::from(u64::MAX)) as u64)
                .max()
                .unwrap_or(u64::MAX);
            if newest >= before_ms {
                report.retained_recent += 1;
                continue;
            }
            report.candidates.push(name.to_owned());
            if apply {
                let archive = match &archive {
                    Some(archive) => archive,
                    None => archive.insert(private::directory(&root.join("jobs-archive"))?),
                };
                let destination = archive.join(name);
                if destination.exists() {
                    return Err(Error::Conflict("archived command job already exists"));
                }
                std::fs::rename(&job, &destination)?;
                report.archived += 1;
            }
        }
        if apply {
            std::fs::File::open(&jobs)?.sync_all()?;
        }
        Ok(report)
    }

    fn unstarted(
        &self,
        job: &Path,
        custody: CommandCustody,
        error: Error,
    ) -> Result<CommandOutcome> {
        match std::fs::symlink_metadata(job.join("started.json")) {
            Err(missing) if missing.kind() == std::io::ErrorKind::NotFound => (),
            _ => return Err(Error::Conflict("command submission state is uncertain")),
        }
        let outcome = CommandOutcome {
            custody,
            joined: true,
            output: None,
            error: Some(format!("command did not start: {error}")),
        };
        private::create(&job.join("outcome.json"), &serde_json::to_vec(&outcome)?)?;
        Ok(outcome)
    }

    pub fn load(root: &Path) -> Result<Self> {
        let root = private::check_directory(root)?;
        let bytes = private::read(&root.join("backend.json"), 16384)?;
        let manifest: Manifest = serde_json::from_slice(&bytes)?;
        let bounds = serde_json::json!({"snapshotBytes":MAX_SNAPSHOT,"workspaceBytes":MAX_WORKSPACE,
            "outputBytes":256*1024,"changesBytes":MAX_CHANGES,"maximumTimeoutMs":600000,
            "maximumTasks":256,"workerMemoryBytes":1536*1024*1024,"cacheBytes":1024*1024*1024,"network":"none"});
        if manifest.version != 1
            || manifest.guest_sha256 != digest(GUEST)
            || manifest.public_cache_sha256 != digest(PUBLIC_CACHE)
            || manifest.git_projection_sha256 != digest(GIT_PROJECTION)
            || manifest.git_projection_tests_sha256 != digest(GIT_PROJECTION_TESTS)
            || manifest.policy_sha256 != digest(POLICY)
            || manifest.bounds != bounds
            || manifest.tool_versions.len() != 4
            || manifest
                .tool_digests
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>()
                != ["bun", "cargo", "cc", "git", "node", "python", "rustc", "sh"]
            || manifest
                .tool_digests
                .values()
                .any(|tool| !hash(&tool.sha256) || !tool.path.starts_with('/'))
            || manifest.boot_id.len() != 36
            || !hash(&manifest.bwrap_sha256)
            || process::executable_digest(&manifest.lima_executable)? != manifest.lima_sha256
        {
            return Err(Error::Unavailable(
                "command backend identity changed; requalification required",
            ));
        }
        qualification(&root, &manifest)?;
        for name in ["lima", "home", "cache", "jobs"] {
            private::check_directory(&root.join(name))?;
        }
        Ok(Self {
            root,
            manifest,
            identity: digest(bytes),
        })
    }
    pub fn identity(&self) -> &str {
        &self.identity
    }

    fn transport(&self, operation: &str, command: Option<&Id>) -> tokio::process::Command {
        let mut child = tokio::process::Command::new(&self.manifest.lima_executable);
        child
            .env_clear()
            .env("HOME", self.root.join("home"))
            .env("LIMA_HOME", self.root.join("lima"))
            .env("XDG_CACHE_HOME", self.root.join("cache"))
            .env("SSH", "/usr/bin/ssh")
            .env("PATH", "/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin")
            .env("LANG", "en_US.UTF-8")
            .args([
                "shell",
                "--workdir",
                "/",
                "worker",
                "sudo",
                "-n",
                "/usr/bin/python3",
                "/usr/local/lib/xcb-command/guest.py",
                operation,
            ]);
        if let Some(command) = command {
            child.arg(command.as_str());
        }
        child
    }

    async fn inspect(&self) -> Result<()> {
        let bytes = process::capture_with_input(
            self.transport("inspect", None),
            &[],
            16384,
            Duration::from_secs(20),
        )
        .await?;
        let seen: Inspection = serde_json::from_slice(&bytes)?;
        if seen.version != 1
            || seen.boot_id != self.manifest.boot_id
            || seen.agent_sha256 != self.manifest.guest_sha256
            || seen.public_cache_sha256 != self.manifest.public_cache_sha256
            || seen.git_projection_sha256 != self.manifest.git_projection_sha256
            || seen.bwrap_sha256 != self.manifest.bwrap_sha256
            || seen.tool_digests != self.manifest.tool_digests
        {
            return Err(Error::Unavailable(
                "command guest changed or restarted; requalification required",
            ));
        }
        Ok(())
    }

    /// Call before starting a provider tool effect. The returned receipt must
    /// also be bound by the caller's durable run/tool receipt before execute.
    pub fn prepare(
        &self,
        input: &CommandInput,
        request: &CommandRequest,
    ) -> Result<CommandCustody> {
        request.validate()?;
        if !identifier(&input.command_id)
            || !identifier(&input.run_id)
            || !hash(&input.workspace_id)
            || !hash(input.snapshot.sha256())
            || input.snapshot.bytes().len() > MAX_SNAPSHOT
        {
            return Err(Error::Unavailable("invalid command authority"));
        }
        // Value serializes object keys in the same canonical lexical order as
        // the guest, avoiding a dependency on struct declaration order.
        let request_bytes = canonical(request)?;
        let custody = CommandCustody {
            version: 1,
            command_id: input.command_id.clone(),
            run_id: input.run_id.clone(),
            workspace_id: input.workspace_id.clone(),
            snapshot_sha256: input.snapshot.sha256().to_owned(),
            request_sha256: digest(request_bytes),
            backend_sha256: self.identity.clone(),
            boot_id: self.manifest.boot_id.clone(),
        };
        let job = self.root.join("jobs").join(input.command_id.as_str());
        if job.exists() {
            return Err(Error::Conflict("command ID already has custody"));
        }
        private::directory(&job)?;
        private::create(&job.join("custody.json"), &serde_json::to_vec(&custody)?)?;
        Ok(custody)
    }

    /// An independent owner always completes cleanup and saves the result.
    /// Dropping the caller future requests cancellation but never proves join.
    pub async fn execute(
        &self,
        input: CommandInput,
        request: CommandRequest,
        custody: CommandCustody,
        cancel: watch::Receiver<bool>,
    ) -> Result<CommandOutcome> {
        let (drop_cancel, dropped) = watch::channel(false);
        struct CancelOnDrop(watch::Sender<bool>);
        impl Drop for CancelOnDrop {
            fn drop(&mut self) {
                let _ = self.0.send(true);
            }
        }
        let guard = CancelOnDrop(drop_cancel);
        let backend = self.clone();
        let task = tokio::spawn(async move {
            backend
                .execute_owned(input, request, custody, cancel, dropped)
                .await
        });
        let result = task
            .await
            .map_err(|_| Error::Unavailable("command owner failed; custody retained"))?;
        drop(guard);
        result
    }

    async fn execute_owned(
        &self,
        input: CommandInput,
        request: CommandRequest,
        custody: CommandCustody,
        mut cancel: watch::Receiver<bool>,
        mut dropped: watch::Receiver<bool>,
    ) -> Result<CommandOutcome> {
        let job = self.root.join("jobs").join(custody.command_id.as_str());
        let recorded: CommandCustody =
            serde_json::from_slice(&private::read(&job.join("custody.json"), 8192)?)?;
        if recorded != custody
            || custody.backend_sha256 != self.identity
            || custody.command_id != input.command_id
            || custody.run_id != input.run_id
            || custody.workspace_id != input.workspace_id
            || custody.snapshot_sha256 != input.snapshot.sha256()
            || custody.boot_id != self.manifest.boot_id
        {
            return Err(Error::Conflict("command authority changed"));
        }
        // One durable owner can submit this command. A duplicate executor
        // cannot manufacture a no-child receipt while the original is active.
        private::create(
            &job.join("owner.json"),
            &serde_json::to_vec(&serde_json::json!({
                "pid":std::process::id(),"instance":crate::new_id("command_owner")
            }))?,
        )?;
        let _admission = match self.admission() {
            Ok(guard) => guard,
            Err(error) => return self.unstarted(&job, custody, error),
        };
        let prepared: Result<Vec<u8>> = async {
            request.validate()?;
            if digest(canonical(&request)?) != custody.request_sha256 {
                return Err(Error::Conflict("command request changed"));
            }
            if process::executable_digest(&self.manifest.lima_executable)?
                != self.manifest.lima_sha256
            {
                return Err(Error::Conflict("command transport executable changed"));
            }
            self.inspect().await?;
            // The bytes were verified when encoded; the custody digest binds
            // them without another read of the snapshot file.
            if input.snapshot.sha256() != custody.snapshot_sha256 {
                return Err(Error::Conflict("command snapshot changed"));
            }
            Ok(serde_json::to_vec(
                &serde_json::json!({"custody":custody,"request":request,
                "snapshotBase64":base64::engine::general_purpose::STANDARD.encode(input.snapshot.bytes())}),
            )?)
        }
        .await;
        let envelope = match prepared {
            Ok(envelope) => envelope,
            Err(error) => return self.unstarted(&job, custody, error),
        };
        // The pending marker and the start record are one submission step:
        // admission scans only pending markers, so the marker must exist
        // before guest work can begin and stays until a joined receipt.
        self.mark_pending(&custody.command_id)?;
        private::create(&job.join("started.json"), b"{\"started\":true}")?;
        let run = process::capture_with_input(
            self.transport("run", None),
            &envelope,
            MAX_RESPONSE,
            Duration::from_millis(u64::from(request.timeout_ms) + 220000),
        );
        tokio::pin!(run);
        let mut cancellation_sent = false;
        let mut channel_closed = false;
        let result = loop {
            if !cancellation_sent && (channel_closed || *cancel.borrow() || *dropped.borrow()) {
                cancellation_sent = true;
                let _ = process::capture_with_input(
                    self.transport("cancel", Some(&custody.command_id)),
                    &[],
                    4096,
                    Duration::from_secs(15),
                )
                .await;
            }
            tokio::select! {
                result = &mut run => break result,
                change = cancel.changed(), if !cancellation_sent => { channel_closed |= change.is_err(); },
                change = dropped.changed(), if !cancellation_sent => { channel_closed |= change.is_err(); },
            }
        };
        let custody_id = custody.command_id.clone();
        let mut outcome = match result {
            Ok(bytes) => match self.accept(&custody, &bytes) {
                Ok(value) => value,
                Err(_) => CommandOutcome {
                    custody,
                    joined: false,
                    output: None,
                    error: Some("invalid command join receipt; custody retained".into()),
                },
            },
            Err(_) => CommandOutcome {
                custody,
                joined: false,
                output: None,
                error: Some("command transport did not prove completion; custody retained".into()),
            },
        };
        private::create(&job.join("outcome.json"), &serde_json::to_vec(&outcome)?)?;
        if outcome.joined {
            self.clear_pending(&custody_id)?;
            // Resource reclamation is allowed only after the joined result,
            // decoded changes and host outcome are independently durable.
            self.acknowledge_or_record_pending(&mut outcome, "outcome.json")
                .await;
        }
        Ok(outcome)
    }

    fn accept(&self, custody: &CommandCustody, bytes: &[u8]) -> Result<CommandOutcome> {
        let result: GuestResult = serde_json::from_slice(bytes)?;
        if result.version != 1
            || result.custody != *custody
            || !guest_join_shape(&result, custody)
            || result.stdout.len() + result.stderr.len() > 3 * 256 * 1024
        {
            return Err(Error::Protocol("command receipt binding"));
        }
        let changes_path = match (&result.changes_base64, &result.changes_sha256) {
            (Some(encoded), Some(expected)) if result.joined => {
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(encoded)
                    .map_err(|_| Error::Protocol("command changes encoding"))?;
                if bytes.len() > MAX_CHANGES || !hash(expected) || digest(&bytes) != *expected {
                    return Err(Error::Protocol("command changes digest/size"));
                }
                let path = self
                    .root
                    .join("jobs")
                    .join(custody.command_id.as_str())
                    .join("changes.json");
                match private::read(&path, MAX_CHANGES) {
                    Ok(existing) if existing == bytes => (),
                    Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                        private::create(&path, &bytes)?
                    }
                    _ => return Err(Error::Conflict("command changes already differ")),
                }
                Some(path)
            }
            (None, None) => None,
            _ => return Err(Error::Protocol("command changes custody")),
        };
        let raw_path = self
            .root
            .join("jobs")
            .join(custody.command_id.as_str())
            .join("guest-result.json");
        match private::read(&raw_path, MAX_RESPONSE) {
            Ok(existing) if existing == bytes => (),
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                private::create(&raw_path, bytes)?
            }
            _ => return Err(Error::Conflict("command guest receipt changed")),
        }
        let binding = serde_json::to_vec(&GuestBinding {
            version: 1,
            custody: custody.clone(),
            result_sha256: digest(bytes),
        })?;
        let binding_path = raw_path.with_file_name("guest-result-binding.json");
        match private::read(&binding_path, 8192) {
            Ok(existing) if existing == binding => (),
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                private::create(&binding_path, &binding)?
            }
            _ => return Err(Error::Conflict("guest result binding changed")),
        }
        Ok(CommandOutcome {
            custody: custody.clone(),
            joined: result.joined,
            error: result.error,
            output: if result.unstarted {
                None
            } else {
                Some(CommandOutput {
                    exit_code: result.exit_code,
                    stdout: result.stdout,
                    stderr: result.stderr,
                    timed_out: result.timed_out,
                    cancelled: result.cancelled,
                    truncated: result.truncated,
                    changes_path,
                    changes_sha256: result.changes_sha256,
                    cleanup_pending: false,
                })
            },
        })
    }

    async fn acknowledge(&self, custody: &CommandCustody, receipt_name: &str) -> Result<()> {
        let job = self.root.join("jobs").join(custody.command_id.as_str());
        let receipt = private::read(&job.join(receipt_name), 2 * 1024 * 1024)?;
        let outcome: CommandOutcome = serde_json::from_slice(&receipt)?;
        if outcome.custody != *custody || !outcome.joined {
            return Err(Error::Conflict("cannot acknowledge unjoined command"));
        }
        let raw = private::read(&job.join("guest-result.json"), MAX_RESPONSE)?;
        let request = serde_json::to_vec(
            &serde_json::json!({"version":1,"custody":custody,"resultSha256":digest(&raw),"hostReceiptSha256":digest(receipt)}),
        )?;
        let response = process::capture_with_input(
            self.transport("ack", Some(&custody.command_id)),
            &request,
            4096,
            Duration::from_secs(20),
        )
        .await?;
        if serde_json::from_slice::<serde_json::Value>(&response)?
            != serde_json::json!({"acknowledged":true})
        {
            return Err(Error::Protocol("command cleanup acknowledgment"));
        }
        let path = job.join("acknowledged.json");
        match private::read(&path, 16384) {
            Ok(existing) if existing == request => (),
            Ok(_) => (), // Recovery may bind the same guest result to its newer durable receipt.
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                private::create(&path, &request)?
            }
            Err(error) => return Err(error),
        }
        Ok(())
    }

    async fn acknowledge_or_record_pending(&self, outcome: &mut CommandOutcome, receipt: &str) {
        if self.acknowledge(&outcome.custody, receipt).await.is_err() {
            if let Some(output) = outcome.output.as_mut() {
                output.cleanup_pending = true;
            }
            let job = self
                .root
                .join("jobs")
                .join(outcome.custody.command_id.as_str());
            let diagnostic = serde_json::json!({"version":1,"custody":outcome.custody,"reason":"joined scratch cleanup pending"});
            if let Ok(bytes) = serde_json::to_vec(&diagnostic) {
                let _ = private::create(&job.join("cleanup-pending.json"), &bytes);
            }
        }
    }

    /// Read exact durable guest evidence; never restarts a command or publishes
    /// pending workspace bytes. Caller still verifies its original run owner.
    pub async fn recover(&self, custody: &CommandCustody) -> Result<CommandOutcome> {
        if custody.backend_sha256 != self.identity
            || custody.boot_id != self.manifest.boot_id
            || !identifier(&custody.command_id)
        {
            return Err(Error::Conflict("command recovery identity changed"));
        }
        let recorded: CommandCustody = serde_json::from_slice(&private::read(
            &self
                .root
                .join("jobs")
                .join(custody.command_id.as_str())
                .join("custody.json"),
            8192,
        )?)?;
        if recorded != *custody {
            return Err(Error::Conflict("command custody changed"));
        }
        let job = self.root.join("jobs").join(custody.command_id.as_str());
        if matches!(std::fs::symlink_metadata(job.join("started.json")), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
        {
            let outcome: CommandOutcome =
                serde_json::from_slice(&private::read(&job.join("outcome.json"), 16384)?)?;
            if outcome.custody == *custody && outcome.joined && outcome.output.is_none() {
                return Ok(outcome);
            }
            return Err(Error::Conflict("unstarted command receipt is invalid"));
        }
        if process::executable_digest(&self.manifest.lima_executable)? != self.manifest.lima_sha256
        {
            return Err(Error::Conflict("command transport executable changed"));
        }
        self.inspect().await?;
        let bytes = process::capture_with_input(
            self.transport("status", Some(&custody.command_id)),
            &[],
            MAX_RESPONSE,
            Duration::from_secs(20),
        )
        .await?;
        let mut outcome = self.accept(custody, &bytes)?;
        if outcome.joined {
            let path = job.join("recovered.json");
            let encoded = serde_json::to_vec(&outcome)?;
            match private::read(&path, 2 * 1024 * 1024) {
                Ok(existing) if existing == encoded => (),
                Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                    private::create(&path, &encoded)?
                }
                _ => return Err(Error::Conflict("command recovery receipt changed")),
            }
            self.clear_pending(&custody.command_id)?;
            self.acknowledge_or_record_pending(&mut outcome, "recovered.json")
                .await;
        }
        Ok(outcome)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, CommandBackend) {
        let temporary = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temporary.path())
            .unwrap()
            .join("private");
        private::directory(&root).unwrap();
        private::directory(&root.join("jobs")).unwrap();
        let manifest = Manifest {
            version: 1,
            lima_executable: PathBuf::from("/missing-synthetic-transport"),
            lima_sha256: "a".repeat(64),
            guest_sha256: digest(GUEST),
            public_cache_sha256: digest(PUBLIC_CACHE),
            git_projection_sha256: digest(GIT_PROJECTION),
            git_projection_tests_sha256: digest(GIT_PROJECTION_TESTS),
            policy_sha256: digest(POLICY),
            boot_id: "00000000-0000-0000-0000-000000000000".into(),
            bwrap_sha256: "b".repeat(64),
            tool_versions: vec!["synthetic".into(); 4],
            tool_digests: std::collections::BTreeMap::new(),
            bounds: serde_json::json!({}),
            qualification: QualificationBinding {
                suite_sha256: digest(QUALIFIER),
                environment_sha256: "c".repeat(64),
                evidence_sha256: "d".repeat(64),
            },
        };
        (
            temporary,
            CommandBackend {
                root,
                manifest,
                identity: "e".repeat(64),
            },
        )
    }
    fn prepared(backend: &CommandBackend) -> (CommandInput, CommandRequest, CommandCustody) {
        let command_id = crate::new_id("cmd");
        let snapshot_path = backend.root.join(format!("snapshot-{command_id}.json"));
        let snapshot = br#"{"version":1,"workspaceId":"synthetic","files":[],"directories":[]}"#;
        private::create(&snapshot_path, snapshot).unwrap();
        let input = CommandInput {
            command_id,
            run_id: crate::new_id("run"),
            workspace_id: "a".repeat(64),
            snapshot_path,
            snapshot: VerifiedSnapshot::new(snapshot.to_vec()),
        };
        let request = CommandRequest {
            argv: vec!["true".into()],
            cwd: ".".into(),
            timeout_ms: 1000,
            network: CommandNetwork::None,
        };
        let custody = backend.prepare(&input, &request).unwrap();
        (input, request, custody)
    }

    #[test]
    fn command_admission_lock_and_pending_recovery_are_cross_process() {
        const CHILD: &str = "XCB_TEST_COMMAND_LOCK_ROOT";
        if let Some(path) = std::env::var_os(CHILD) {
            let file = private::open_file(&PathBuf::from(path).join("admission.lock"), 64).unwrap();
            assert!(matches!(
                file.try_lock(),
                Err(std::fs::TryLockError::WouldBlock)
            ));
            return;
        }
        let (_temporary, backend) = fixture();
        let guard = backend.admission().unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "command::tests::command_admission_lock_and_pending_recovery_are_cross_process",
            ])
            .env(CHILD, &backend.root)
            .status()
            .unwrap();
        assert!(status.success());
        assert!(backend.admission().is_err());
        drop(guard);
        let (_, _, custody) = prepared(&backend);
        let job = backend.root.join("jobs").join(custody.command_id.as_str());
        // Submission creates the pending marker and the start record as one
        // step; fabrication mirrors that so admission sees the unjoined job.
        backend.mark_pending(&custody.command_id).unwrap();
        private::create(&job.join("started.json"), b"{}").unwrap();
        let mut outcome = CommandOutcome {
            custody,
            joined: false,
            output: None,
            error: Some("synthetic transport loss".into()),
        };
        private::create(
            &job.join("outcome.json"),
            &serde_json::to_vec(&outcome).unwrap(),
        )
        .unwrap();
        assert!(backend.admission().is_err());
        outcome.joined = true;
        private::create(
            &job.join("recovered.json"),
            &serde_json::to_vec(&outcome).unwrap(),
        )
        .unwrap();
        assert!(backend.admission().is_ok());
        assert!(
            !serde_json::from_slice::<CommandOutcome>(
                &private::read(&job.join("outcome.json"), 8192).unwrap()
            )
            .unwrap()
            .joined
        );
    }

    #[tokio::test]
    async fn command_definite_preflight_failure_is_recoverable_and_never_resubmitted() {
        let (_temporary, backend) = fixture();
        let (input, request, custody) = prepared(&backend);
        let (_sender, cancel) = watch::channel(false);
        let outcome = backend
            .execute(
                input.clone(),
                request.clone(),
                custody.clone(),
                cancel.clone(),
            )
            .await
            .unwrap();
        assert!(outcome.joined && outcome.output.is_none());
        let job = backend.root.join("jobs").join(custody.command_id.as_str());
        assert!(!job.join("started.json").exists());
        assert!(backend.recover(&custody).await.unwrap().joined);
        assert!(
            CommandBackend::recover_recorded_join(&backend.root, &custody)
                .unwrap()
                .unwrap()
                .joined
        );
        let mut upgraded = backend.clone();
        upgraded.identity = "f".repeat(64);
        assert!(upgraded.recover(&custody).await.is_err());
        assert!(
            CommandBackend::recover_recorded_join(&upgraded.root, &custody)
                .unwrap()
                .unwrap()
                .output
                .is_none()
        );
        assert!(
            backend
                .execute(input, request, custody, cancel)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn command_recorded_join_survives_upgrade_and_cleanup_failure_without_losing_proof() {
        let (_temporary, backend) = fixture();
        let (_, _, custody) = prepared(&backend);
        let job = backend.root.join("jobs").join(custody.command_id.as_str());
        private::create(&job.join("started.json"), b"{}").unwrap();
        let changes = serde_json::to_vec(
            &serde_json::json!({"version":1,"workspaceId":custody.workspace_id,"changes":[]}),
        )
        .unwrap();
        let raw=serde_json::to_vec(&serde_json::json!({"version":1,"custody":custody,"joined":true,"unstarted":false,
            "cgroup":{"path":format!("/sys/fs/cgroup/system.slice/xcb-command-{}.service/worker",custody.command_id),"dev":1,"ino":2},
            "exitCode":0,"stdout":"ok","stderr":"","timedOut":false,"cancelled":false,"truncated":false,"error":null,
            "changesBase64":base64::engine::general_purpose::STANDARD.encode(&changes),"changesSha256":digest(&changes)})).unwrap();
        let mut outcome = backend.accept(&custody, &raw).unwrap();
        private::create(
            &job.join("outcome.json"),
            &serde_json::to_vec(&outcome).unwrap(),
        )
        .unwrap();
        backend
            .acknowledge_or_record_pending(&mut outcome, "outcome.json")
            .await;
        assert!(
            outcome.joined
                && outcome.error.is_none()
                && outcome.output.as_ref().unwrap().cleanup_pending
        );
        assert!(job.join("cleanup-pending.json").exists());
        assert!(
            CommandBackend::recover_recorded_join(&backend.root, &custody)
                .unwrap()
                .unwrap()
                .joined
        );
        let mut upgraded = backend.clone();
        upgraded.identity = "f".repeat(64);
        upgraded.manifest.boot_id = "different-boot".into();
        assert!(
            CommandBackend::recover_recorded_join(&upgraded.root, &custody)
                .unwrap()
                .unwrap()
                .joined
        );
        assert!(upgraded.recover(&custody).await.is_err());
        std::fs::write(job.join("changes.json"), b"changed").unwrap();
        assert!(CommandBackend::recover_recorded_join(&backend.root, &custody).is_err());
        std::fs::write(job.join("changes.json"), &changes).unwrap();
        let mut missing = serde_json::from_slice::<serde_json::Value>(&raw).unwrap();
        missing["cgroup"]["ino"] = serde_json::json!(3);
        std::fs::write(
            job.join("guest-result.json"),
            serde_json::to_vec(&missing).unwrap(),
        )
        .unwrap();
        assert!(CommandBackend::recover_recorded_join(&backend.root, &custody).is_err());
    }

    #[test]
    fn command_qualification_requires_exact_complete_hashed_evidence() {
        let (_temporary, mut backend) = fixture();
        let mut environment = serde_json::to_value(&backend.manifest).unwrap();
        environment.as_object_mut().unwrap().remove("qualification");
        let environment_sha = digest(canonical(&environment).unwrap());
        backend.manifest.qualification.environment_sha256 = environment_sha.clone();
        assert!(qualification(&backend.root, &backend.manifest).is_err());
        let mut cases = Vec::new();
        for (i, name) in REQUIRED_CASES.iter().enumerate() {
            let command_id = format!("cmd_{i}");
            let (exit, stdout) = match *name {
                "file-edit-and-python" => (Some(0), "success\n"),
                "uid-filesystem-network-and-userns" => (Some(0), "all-denied\n"),
                "offline-language-toolchains" => (Some(0), "42\n43\n44\n"),
                "peer-work-and-control-denied" => (Some(0), "peer-denied\n"),
                "git-projection-unit-semantics" => (Some(0), "git-tests-passed\n"),
                "readonly-filtered-git-inspection" => (Some(0), "git-projected-readonly\n"),
                "public-cache-isolation-and-key-binding" => {
                    (Some(0), "cache-isolation-key-binding\n")
                }
                "offline-cargo-bun-cache-usage" => (Some(0), "offline-cargo-bun-passed\n"),
                "deadline-kills-descendants" => (Some(-9), ""),
                "pre-cancel-never-executes" => (None, ""),
                _ => (Some(0), ""),
            };
            let response=serde_json::to_string(&serde_json::json!({
                "version":1,"custody":{"version":1,"commandId":command_id,"runId":"run_test","workspaceId":"a".repeat(64),"snapshotSha256":"b".repeat(64),"requestSha256":"c".repeat(64),"backendSha256":environment_sha,"bootId":backend.manifest.boot_id},
                "joined":true,"cgroup":{"path":format!("/sys/fs/cgroup/system.slice/xcb-command-{command_id}.service/worker"),"dev":1,"ino":2},
                "exitCode":exit,"stdout":stdout,"stderr":"","timedOut":*name=="deadline-kills-descendants","cancelled":*name=="pre-cancel-never-executes","truncated":*name=="output-overflow-joins","error":null,"changesBase64":null,"changesSha256":null
            })).unwrap();
            cases.push(serde_json::json!({"name":name,"resultSha256":digest(response.as_bytes()),"response":response}));
        }
        let evidence = serde_json::json!({"version":1,"environmentSha256":environment_sha,"suiteSha256":digest(QUALIFIER),"cases":cases});
        let publish = |backend: &mut CommandBackend, value: &serde_json::Value| {
            let bytes = serde_json::to_vec(value).unwrap();
            backend.manifest.qualification.evidence_sha256 = digest(&bytes);
            private::create(
                &backend
                    .root
                    .join(format!("qualification-{}.json", digest(&bytes))),
                &bytes,
            )
            .unwrap();
        };
        publish(&mut backend, &evidence);
        qualification(&backend.root, &backend.manifest).unwrap();
        let mut incomplete = evidence.clone();
        incomplete["cases"].as_array_mut().unwrap().pop();
        publish(&mut backend, &incomplete);
        assert!(qualification(&backend.root, &backend.manifest).is_err());
        let mut changed = evidence.clone();
        changed["cases"][0]["response"] = serde_json::json!("{}");
        publish(&mut backend, &changed);
        assert!(qualification(&backend.root, &backend.manifest).is_err());
        backend.manifest.qualification.suite_sha256 = "0".repeat(64);
        assert!(qualification(&backend.root, &backend.manifest).is_err());
    }

    /// Requires an explicitly provisioned, qualified task-owned VM. Run only
    /// under the mac-native scheduler; never inferred from ambient state.
    #[tokio::test]
    #[ignore = "explicit XCB_TEST_COMMAND_ROOT and isolated VM required"]
    async fn command_live_dropped_future_keeps_owner_and_busy_admission_is_unstarted() {
        let root =
            PathBuf::from(std::env::var_os("XCB_TEST_COMMAND_ROOT").expect("explicit VM root"));
        let backend = CommandBackend::load(&root).unwrap();
        let workspace_id = digest(b"synthetic dropped-future workspace");
        let command_id = crate::new_id("cmd");
        let snapshot=serde_json::to_vec(&serde_json::json!({"version":1,"workspaceId":workspace_id,"files":[],"directories":[]})).unwrap();
        let snapshot_path = root.join(format!("snapshot-{command_id}.json"));
        private::create(&snapshot_path, &snapshot).unwrap();
        let input = CommandInput {
            command_id,
            run_id: crate::new_id("run"),
            workspace_id,
            snapshot_path,
            snapshot: VerifiedSnapshot::new(snapshot),
        };
        let request = CommandRequest {
            argv: vec![
                "python3".into(),
                "-c".into(),
                "import os,time; os.fork(); time.sleep(30)".into(),
            ],
            cwd: ".".into(),
            timeout_ms: 10000,
            network: CommandNetwork::None,
        };
        let custody = backend.prepare(&input, &request).unwrap();
        let (_sender, cancel) = watch::channel(false);
        let executing = backend.clone();
        let task_custody = custody.clone();
        let task = tokio::spawn(async move {
            executing
                .execute(input, request, task_custody, cancel)
                .await
        });
        let job = root.join("jobs").join(custody.command_id.as_str());
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        while !job.join("started.json").exists() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "command did not begin submission"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let (second, request, second_custody) = prepared(&backend);
        let (_sender2, cancel2) = watch::channel(false);
        let busy = backend
            .execute(second, request, second_custody.clone(), cancel2)
            .await
            .unwrap();
        assert!(busy.joined && busy.output.is_none());
        assert!(busy.error.as_deref().unwrap().contains("busy"));
        assert!(backend.recover(&second_custody).await.unwrap().joined);
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        while !job.join("outcome.json").exists() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "detached command owner lost custody"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let outcome: CommandOutcome = serde_json::from_slice(
            &private::read(&job.join("outcome.json"), 2 * 1024 * 1024).unwrap(),
        )
        .unwrap();
        assert!(outcome.joined, "{:?}", outcome.error);
        assert!(outcome.output.as_ref().unwrap().cancelled);
        assert!(backend.recover(&custody).await.unwrap().joined);
        assert!(job.join("recovered.json").exists());
        assert!(backend.admission().is_ok());
    }
    #[test]
    fn command_request_is_closed_offline_and_bounded() {
        let good = serde_json::json!({"argv":["sh","-c","echo ok"],"cwd":".","timeoutMs":1000,"network":"none"});
        serde_json::from_value::<CommandRequest>(good.clone())
            .unwrap()
            .validate()
            .unwrap();
        for (field, value) in [
            ("network", serde_json::json!("all")),
            ("env", serde_json::json!({})),
        ] {
            let mut bad = good.clone();
            bad[field] = value;
            assert!(serde_json::from_value::<CommandRequest>(bad).is_err());
        }
        for path in ["/tmp", "../peer", "a/../b", ".git", "a//b", "a/./b"] {
            let mut bad: CommandRequest = serde_json::from_value(good.clone()).unwrap();
            bad.cwd = path.into();
            assert!(bad.validate().is_err(), "{path}");
        }
    }
    #[test]
    fn command_digest_and_transport_ids_are_strict() {
        assert!(hash(&"a".repeat(64)));
        assert!(!hash(&"A".repeat(64)));
        assert!(identifier(&Id::new("cmd_123").unwrap()));
        assert!(!identifier(&Id::new("cmd[123]").unwrap()));
        let request = CommandRequest {
            argv: vec!["é".into()],
            cwd: ".".into(),
            timeout_ms: 1000,
            network: CommandNetwork::None,
        };
        assert_eq!(
            canonical(&request).unwrap(),
            "{\"argv\":[\"é\"],\"cwd\":\".\",\"network\":\"none\",\"timeoutMs\":1000}".as_bytes()
        );
    }

    /// Fabricate one retained job with a start record and a durable outcome,
    /// exactly what `job_joined` verifies. Returns its command id.
    fn synthetic_finished_job(backend: &CommandBackend, joined: bool) -> String {
        let custody = CommandCustody {
            version: 1,
            command_id: crate::new_id("cmd"),
            run_id: crate::new_id("run"),
            workspace_id: "a".repeat(64),
            snapshot_sha256: "b".repeat(64),
            request_sha256: "c".repeat(64),
            backend_sha256: backend.identity.clone(),
            boot_id: backend.manifest.boot_id.clone(),
        };
        let job = backend.root.join("jobs").join(custody.command_id.as_str());
        private::directory(&job).unwrap();
        private::create(
            &job.join("custody.json"),
            &serde_json::to_vec(&custody).unwrap(),
        )
        .unwrap();
        private::create(&job.join("started.json"), b"{\"started\":true}").unwrap();
        let outcome = CommandOutcome {
            custody,
            joined,
            output: None,
            error: (!joined).then(|| "synthetic transport loss".to_owned()),
        };
        private::create(
            &job.join("outcome.json"),
            &serde_json::to_vec(&outcome).unwrap(),
        )
        .unwrap();
        outcome.custody.command_id.as_str().to_owned()
    }

    #[test]
    fn admission_scans_pending_markers_not_retained_job_history() {
        let (_temporary, backend) = fixture();
        let jobs = backend.root.join("jobs");
        let pending = jobs.join(PENDING);
        let mut joined = Vec::new();
        for _ in 0..96 {
            joined.push(synthetic_finished_job(&backend, true));
        }
        let unjoined = synthetic_finished_job(&backend, false);
        // A root without markers is migrated once: the unjoined job receives
        // its marker and still refuses admission, while joined jobs do not.
        assert!(!pending.exists());
        assert!(backend.admission().is_err());
        assert!(pending.join(&unjoined).exists());
        assert!(pending.join(PENDING_READY).exists());
        assert!(backend.admission().is_err());
        // The joined receipt is the truth: fabricating it clears the stale
        // marker during the next pending-only scan.
        let job = jobs.join(&unjoined);
        let custody: CommandCustody =
            serde_json::from_slice(&private::read(&job.join("custody.json"), 8192).unwrap())
                .unwrap();
        let outcome = CommandOutcome {
            custody,
            joined: true,
            output: None,
            error: None,
        };
        private::create(
            &job.join("recovered.json"),
            &serde_json::to_vec(&outcome).unwrap(),
        )
        .unwrap();
        drop(backend.admission().unwrap());
        assert!(!pending.join(&unjoined).exists());
        // Once migrated, admission never opens retained job dirs: a corrupt
        // job outside pending does not even get read, proving the scan is
        // proportional to pending markers, not job history.
        let corrupt = jobs.join("cmd_corrupt");
        private::directory(&corrupt).unwrap();
        private::create(&corrupt.join("custody.json"), b"not json").unwrap();
        private::create(&corrupt.join("started.json"), b"{}").unwrap();
        drop(backend.admission().unwrap());
        // A stale marker for a joined job is unlinked during the scan.
        private::create(&pending.join(&joined[0]), PENDING_MARKER).unwrap();
        drop(backend.admission().unwrap());
        assert!(!pending.join(&joined[0]).exists());
    }

    #[test]
    fn prune_archives_only_old_joined_acknowledged_jobs() {
        let (_temporary, backend) = fixture();
        let jobs = backend.root.join("jobs");
        private::create(
            &backend.root.join("xcb-owner.json"),
            b"{\"owner\":\"xcb-command-v1\"}\n",
        )
        .unwrap();
        drop(backend.admission().unwrap());
        let archive_candidate = synthetic_finished_job(&backend, true);
        let job = jobs.join(&archive_candidate);
        private::create(&job.join("acknowledged.json"), b"{}").unwrap();
        let cleanup_pending = synthetic_finished_job(&backend, true);
        let recent = synthetic_finished_job(&backend, true);
        private::create(&jobs.join(&recent).join("acknowledged.json"), b"{}").unwrap();
        let unjoined = synthetic_finished_job(&backend, false);
        backend.mark_pending(&Id::new(&unjoined).unwrap()).unwrap();
        // Age the two archivable outcomes behind the cutoff; the recent one
        // keeps its fresh receipt time.
        let cutoff_ms = crate::now_ms() - 30 * 86_400_000;
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(40 * 86_400);
        for id in [&archive_candidate, &cleanup_pending] {
            std::fs::File::options()
                .write(true)
                .open(jobs.join(id).join("outcome.json"))
                .unwrap()
                .set_modified(old)
                .unwrap();
        }
        let report = CommandBackend::prune_joined_jobs(&backend.root, cutoff_ms, false).unwrap();
        assert_eq!(report.candidates, [archive_candidate.as_str()]);
        assert_eq!(report.archived, 0);
        assert_eq!(report.retained_unjoined, 1);
        assert_eq!(report.retained_cleanup_pending, 1);
        assert_eq!(report.retained_recent, 1);
        assert!(job.exists());
        let applied = CommandBackend::prune_joined_jobs(&backend.root, cutoff_ms, true).unwrap();
        assert_eq!(applied.archived, 1);
        assert!(!job.exists());
        assert!(
            backend
                .root
                .join("jobs-archive")
                .join(&archive_candidate)
                .exists()
        );
        // Nothing is deleted and nothing unjoined is ever archived.
        let after = CommandBackend::prune_joined_jobs(&backend.root, cutoff_ms, true).unwrap();
        assert!(after.candidates.is_empty());
        assert_eq!(after.retained_unjoined, 1);
        assert!(jobs.join(&unjoined).exists());
        // Archived and joined jobs are never scanned; the unjoined job's
        // live pending marker still refuses admission.
        assert!(backend.admission().is_err());
        assert!(jobs.join(PENDING).join(&unjoined).exists());
    }
}
