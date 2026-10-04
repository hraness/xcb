#[cfg(target_os = "macos")]
use crate::process::environment;
use crate::{
    Error, Result, attachments, auth,
    broker::{self, Workspace},
    claude::{self, Event},
    claude_protocol::ClaudeProtocol,
    config::Config,
    context, digest, egress, judge, new_id, now_ms, private,
    process::{Pin, StreamProcess},
    protocol::{Event as TurnEvent, ImageInput, Prompt, Protocol},
    sandbox,
    store::{PendingQuota, RunRecord, Store, UsageObservation},
};
use base64::Engine;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{process::Command, sync::watch};
use xcb_core::{
    Id, MAX_TEXT_BYTES, Provider,
    models::{Mode, ModelChoice},
    policy::{EffectState, Failure, Terminal, TurnFacts},
    session::{Message, MessageProvenance, Role, Session, State, Subagent, classify},
    usage::{QuotaPoint, VelocitySample},
};

pub enum Progress {
    Text { thinking: bool, text: String },
    Tool(String),
    Subagent(Subagent),
    Notice(String),
}
pub type Observer = Arc<dyn Fn(Progress) + Send + Sync>;
pub use crate::device_login::DeviceLoginPrompt;

/// Diagnostic for a turn refused before its prompt was sent because the
/// session's model left the freshly discovered provider catalog.
pub const STALE_MODEL: &str = "selected model or effort is not in the fresh provider catalog";
/// Diagnostic for a turn refused before its prompt was sent because the
/// provider reported no models at all; the stored catalog is kept.
pub const EMPTY_CATALOG: &str =
    "the provider reported no models at startup; the stored model list was kept";

/// A bounded host-selected explanation, never a raw provider/OS error payload.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(transparent)]
pub struct Diagnostic(String);

impl Diagnostic {
    const MAX_BYTES: usize = 512;

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// A host-authored notice; provider text never enters here.
    pub(crate) fn notice(text: &'static str) -> Self {
        Self::bounded(text.to_owned())
    }

    pub(crate) fn from_error(error: &Error) -> Self {
        let text = match error {
            Error::Protocol(_)
            | Error::Conflict(_)
            | Error::Unavailable(_)
            | Error::CodexRpc { .. }
            | Error::CodexUsage { .. }
            | Error::CodexNotification { .. }
            | Error::CodexNativeTool { .. }
            | Error::DevinRpc { .. }
            | Error::DevinModelChoices { .. }
            | Error::Message(_)
            | Error::Guided { .. } => error.to_string(),
            Error::Core(_) => "invalid host input or local record".into(),
            Error::Io(_) => "local I/O failed".into(),
            Error::LaunchNotStarted(_) => "provider process could not start".into(),
            Error::CleanupUnproven => "provider cleanup is unproven; custody retained".into(),
            Error::AuthUnproven(message) => (*message).into(),
            Error::Database(_) => "local database operation failed".into(),
            Error::Json(_) => "invalid local record".into(),
            Error::PrivateState => "local state failed private-file validation".into(),
        };
        Self::bounded(text)
    }

    fn bounded(text: String) -> Self {
        let mut text: String = text
            .chars()
            .map(|character| {
                if character.is_control() {
                    ' '
                } else {
                    character
                }
            })
            .collect();
        if text.len() > Self::MAX_BYTES {
            let mut end = Self::MAX_BYTES;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            text.truncate(end);
        }
        Self(text)
    }
}

impl<'de> serde::Deserialize<'de> for Diagnostic {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let text = <String as serde::Deserialize>::deserialize(deserializer)?;
        if text.is_empty() || text.len() > Self::MAX_BYTES || text.chars().any(char::is_control) {
            return Err(serde::de::Error::custom("invalid bounded host diagnostic"));
        }
        Ok(Self(text))
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Outcome {
    pub text: String,
    pub facts: TurnFacts,
    pub state: State,
    /// Distinct tool calls the turn admitted. The settle reflex reads it:
    /// a turn that did a lot of work and then stopped is the turn most
    /// often answered with "continue". Not persisted, so the settled-outcome
    /// record keeps the format older builds decode; a turn reconciled after a
    /// restart has no count, and the settle reflex skips it.
    #[serde(skip)]
    pub tool_calls: Option<u32>,
    /// `pending_attention` came from the text classifier alone: the turn
    /// ended with a question or a request to confirm, and no provider
    /// request was denied. Such a turn may still be read by the settle
    /// reflex within its safety envelope; a denied request never is. Not
    /// persisted, so a turn reconciled after a restart reads as a denial.
    #[serde(skip)]
    pub text_attention: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostic: Option<Diagnostic>,
}

impl Outcome {
    /// Whether the settle reflex may read this turn: it ended idle, or with
    /// a question or confirmation request that only the text raised. A
    /// denied provider request, and a step only the user can take
    /// ([`State::NeedsAction`]: sign in, run this yourself), stay with them.
    pub fn askable(&self) -> bool {
        match self.state {
            State::Idle => true,
            State::NeedsAnswer | State::NeedsApproval => self.text_attention,
            _ => false,
        }
    }

    /// A provider request was denied during the turn, so only the user can
    /// supply new permission; the text alone never sets this.
    pub fn denied(&self) -> bool {
        self.facts.pending_attention && !self.text_attention
    }
}

pub fn should_idle_export(pane_generation: bool, facts: &TurnFacts, state: State) -> bool {
    !pane_generation
        && state == State::Idle
        && facts.joined
        && facts.effects != EffectState::Uncertain
        && facts.terminal == Terminal::Completed
}

pub(crate) struct Launch {
    pub(crate) command: Command,
    pub(crate) cwd: PathBuf,
    pub(crate) bridge: Option<egress::EgressBridge>,
    pub(crate) artifacts: LaunchArtifacts,
    pub(crate) prepared_run: Option<RunRecord>,
    pub(crate) codex_credentials: Option<auth::CodexAuthSnapshot>,
}

impl Launch {
    async fn discard_unstarted(&mut self) {
        let joined = close_bridge(self.bridge.take()).await;
        self.artifacts.release_after_join(joined, EffectState::None);
    }
}

pub(crate) async fn close_bridge(bridge: Option<egress::EgressBridge>) -> bool {
    if let Some(bridge) = bridge {
        let receipt = bridge.close().await;
        receipt.listener_closed && receipt.sockets_joined && receipt.socket_removed
    } else {
        true
    }
}

#[cfg(any(all(test, unix), target_os = "linux"))]
async fn discard_failed_preparation(
    artifacts: &mut LaunchArtifacts,
    bridge: egress::EgressBridge,
    error: Error,
) -> Error {
    let joined = close_bridge(Some(bridge)).await;
    artifacts.release_after_join(joined, EffectState::None);
    if joined {
        error
    } else {
        Error::CleanupUnproven
    }
}

fn settle_failed_preparation(store: &Store, run: Option<&RunRecord>, error: Error) -> Error {
    if !error.is_cleanup_unproven()
        && let Some(run) = run
        && let Err(settlement) = store
            .require_settled_tools(run)
            .and_then(|_| store.settle(run, State::Failed, now_ms()))
    {
        return settlement;
    }
    error
}

/// Cancellation can drop an async credential refresh before it returns.
/// Only discard a never-started preparation whose effects are proven settled.
struct PreparationCustody<'a> {
    store: &'a Store,
    run: Option<&'a RunRecord>,
    active: bool,
}

impl Drop for PreparationCustody<'_> {
    fn drop(&mut self) {
        if self.active
            && let Some(run) = self.run
        {
            let _ = self.store.require_settled_tools(run).and_then(|_| {
                let current = self.store.run(&run.id)?.ok_or(Error::CleanupUnproven)?;
                if current.phase != "prepared" || current.pid.is_some() {
                    return Err(Error::CleanupUnproven);
                }
                self.store.settle(run, State::Failed, now_ms())
            });
        }
    }
}

async fn spawn_process(
    store: &Store,
    run: Option<&RunRecord>,
    command: Command,
    artifacts: &mut LaunchArtifacts,
    bridge: Option<egress::EgressBridge>,
    codex_credentials: bool,
) -> Result<(StreamProcess, Option<egress::EgressBridge>)> {
    artifacts.retain_before_launch();
    match StreamProcess::spawn(command) {
        Ok(process) => Ok((process, bridge)),
        Err(error @ Error::LaunchNotStarted(_)) => {
            // This variant proves command.spawn() failed before a child
            // existed. Postspawn errors carry no such proof and stay held.
            if close_bridge(bridge).await {
                if let Some(run) = run {
                    if codex_credentials {
                        auth::discard_unstarted_codex_auth(store, run, true)?;
                    }
                    store.settle(run, State::Failed, now_ms())?;
                }
                artifacts.release_after_join(true, EffectState::None);
            }
            Err(error)
        }
        Err(error) => Err(error),
    }
}

/// Serializes cleanup with the host owner. Its release proves neither that a
/// provider descendant exited nor that its effects settled.
pub(crate) const LAUNCH_OWNER_LOCK: &str = "owner.lock";

/// How long a cancelled provider may exit on its own after its interruption
/// frame and stdin close before the process group is killed.
const CANCEL_GRACE: Duration = Duration::from_millis(1500);

/// Minimum interval between fsync'd observability checkpoints during a turn.
/// Velocity/quota events still arrive at provider pace; the commits do not.
const OBSERVATION_FLUSH_MS: u64 = 2_000;

/// Launch snapshots are disposable only before spawn or after independent
/// process-join evidence and settled effects. Cancellation/drop alone never
/// grants cleanup permission.
pub(crate) struct LaunchArtifacts {
    directory: PathBuf,
    identity: (u64, u64),
    retained: bool,
    /// Declared last so the lock stays held through safe disposal.
    owner: private::ExclusiveLock,
}

#[derive(serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ReclaimableLaunch {
    version: u8,
    path: PathBuf,
    directory: (u64, u64),
    owner: (u64, u64),
}

fn reclamation_receipt(path: &Path) -> PathBuf {
    // A sibling of the launch directory is outside provider-writable scratch.
    path.with_extension("reclaimable.json")
}

fn launch_directory_metadata(path: &Path) -> Result<crate::os::Stamp> {
    // Inspection must not use ensure_private_directory: a path concurrently
    // removed by its owner must stay absent, including during a dry run.
    let metadata = crate::os::lstat(path)?;
    if !metadata.dir || !metadata.owned || !metadata.private || xcb_core::canonical(path)? != path {
        return Err(Error::PrivateState);
    }
    Ok(metadata)
}

fn reclaimable_identity(path: &Path, owner: &std::fs::File) -> Result<ReclaimableLaunch> {
    let directory = launch_directory_metadata(path)?;
    private::same_file(&path.join(LAUNCH_OWNER_LOCK), owner)?;
    let owner = crate::os::fstat(owner)?;
    Ok(ReclaimableLaunch {
        version: 1,
        path: path.to_path_buf(),
        directory: (directory.dev, directory.ino),
        owner: (owner.dev, owner.ino),
    })
}

impl LaunchArtifacts {
    fn record_reclaimable(&self) -> Result<()> {
        if self.retained {
            return Err(Error::CleanupUnproven);
        }
        let identity = reclaimable_identity(&self.directory, &self.owner)?;
        if identity.directory != self.identity {
            return Err(Error::CleanupUnproven);
        }
        private::create(
            &reclamation_receipt(&self.directory),
            &serde_json::to_vec(&identity)?,
        )
    }
}
impl LaunchArtifacts {
    pub(crate) fn create(root: &Path) -> Result<Self> {
        let directory = private::directory(&root.join("runs").join(new_id("launch").as_str()))?;
        let metadata = crate::os::lstat(&directory)?;
        let owner = open_owner_lock(&directory.join(LAUNCH_OWNER_LOCK))?;
        // The directory name is a fresh identifier, so the lock cannot already
        // be held. A refusal here means something else is writing into our
        // private state, which is never routine.
        owner
            .try_lock()
            .map_err(|_| Error::Conflict("launch directory is already owned"))?;
        let owner = private::ExclusiveLock::held(owner);
        Ok(Self {
            directory,
            identity: (metadata.dev, metadata.ino),
            retained: false,
            owner,
        })
    }
    pub(crate) fn path(&self) -> &Path {
        &self.directory
    }
    pub(crate) fn retain_before_launch(&mut self) {
        self.retained = true;
    }
    pub(crate) fn release_after_join(&mut self, joined: bool, effects: EffectState) {
        if joined && effects != EffectState::Uncertain {
            self.retained = false;
        }
    }
}
/// Opens the owner lock without following a symlink and without blocking on a
/// device. The same flags the store uses for its initialization lock.
fn open_owner_lock(path: &Path) -> Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    let file = crate::os::no_follow(crate::os::owner_only(&mut options), true).open(path)?;
    private::check_file(&file, 0)?;
    private::same_file(path, &file)?;
    Ok(file)
}

/// What a sweep of `runs/` found. Bytes are reported so an operator can see
/// what is at stake before being asked to remove anything.
#[derive(Debug, Default)]
pub struct LaunchArtifactSweep {
    /// Proven disposable directories, whether this was a dry run or removal.
    pub reclaimable: usize,
    pub reclaimable_bytes: u64,
    /// Proven disposable directories removed by this sweep.
    pub reclaimed: usize,
    pub reclaimed_bytes: u64,
    /// Directories whose owner lock is held right now. Left untouched.
    pub live: usize,
    /// Missing or mismatched settlement evidence. Always retained.
    pub unprovable: Vec<PathBuf>,
    pub unprovable_bytes: u64,
}

fn directory_bytes(path: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| match entry.file_type() {
            Ok(kind) if kind.is_dir() => directory_bytes(&entry.path()),
            Ok(kind) if kind.is_file() => entry.metadata().map(|data| data.len()).unwrap_or(0),
            _ => 0,
        })
        .sum()
}

/// Reclaims only directories durably marked disposable by their safe Drop
/// path. A free owner lock alone cannot prove provider join or effect settlement.
/// Legacy, interrupted preparation, and uncertain launches remain untouched.
/// `remove = false` performs a read-only inventory.
pub fn reclaim_launch_artifacts(root: &Path, remove: bool) -> Result<LaunchArtifactSweep> {
    let runs = root.join("runs");
    let mut sweep = LaunchArtifactSweep::default();
    let entries = match std::fs::read_dir(&runs) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(sweep),
        Err(error) => return Err(error.into()),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with("launch_"))
        {
            continue;
        }
        // Refuses a symlink or anything not owner-only, so a sweep never
        // follows a planted name out of our private state.
        if launch_directory_metadata(&path).is_err() {
            continue;
        }
        let receipt = private::read(&reclamation_receipt(&path), 1024)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<ReclaimableLaunch>(&bytes).ok());
        if receipt.is_none() {
            // Do not contend with a constructor acquiring its fresh lock.
            // Only a safely finished launch can publish a disposal receipt.
            sweep.unprovable_bytes += directory_bytes(&path);
            sweep.unprovable.push(path);
            continue;
        }
        // Never create a lock during inspection: the launch creator may still
        // be between mkdir and acquiring its own lock.
        let owner = match private::open_file(&path.join(LAUNCH_OWNER_LOCK), 0).ok() {
            Some(owner) => match owner.try_lock() {
                Ok(()) => Some(private::ExclusiveLock::held(owner)),
                Err(std::fs::TryLockError::WouldBlock) => {
                    sweep.live += 1;
                    continue;
                }
                Err(_) => continue,
            },
            None => None,
        };
        let identity = owner
            .as_ref()
            .and_then(|owner| reclaimable_identity(&path, owner).ok());
        let bytes = directory_bytes(&path);
        if identity.is_none() || identity != receipt {
            sweep.unprovable.push(path);
            sweep.unprovable_bytes += bytes;
            continue;
        }
        sweep.reclaimable += 1;
        sweep.reclaimable_bytes += bytes;
        if remove {
            // Revalidate the exact directory and lock immediately before removal.
            if reclaimable_identity(&path, owner.as_ref().expect("verified owner")).ok() != identity
            {
                continue;
            }
            std::fs::remove_dir_all(&path)?;
            let _ = std::fs::remove_file(reclamation_receipt(&path));
            sweep.reclaimed += 1;
            sweep.reclaimed_bytes += bytes;
        }
    }
    sweep.unprovable.sort();
    Ok(sweep)
}

impl Drop for LaunchArtifacts {
    fn drop(&mut self) {
        if self.retained || launch_directory_metadata(&self.directory).is_err() {
            return;
        }
        if crate::os::lstat(&self.directory)
            .is_ok_and(|metadata| (metadata.dev, metadata.ino) == self.identity)
        {
            // Publish outside the provider's writable tree before removal, so
            // an interrupted or failed disposal remains safely reclaimable.
            // Receipt or identity failure retains the evidence.
            if self.record_reclaimable().is_err()
                || !reclaimable_identity(&self.directory, &self.owner)
                    .is_ok_and(|identity| identity.directory == self.identity)
            {
                return;
            }
            if std::fs::remove_dir_all(&self.directory).is_ok() {
                let _ = std::fs::remove_file(reclamation_receipt(&self.directory));
            }
        }
    }
}

#[derive(Default)]
struct Answer {
    complete: String,
    partial: String,
}
impl Answer {
    fn delta(&mut self, text: &str) -> Result<()> {
        if self.partial.len().saturating_add(text.len()) > MAX_TEXT_BYTES {
            return Err(Error::Protocol("answer limit"));
        }
        self.partial.push_str(text);
        Ok(())
    }
    fn completed(&mut self, text: String) -> Result<()> {
        if text.len() > MAX_TEXT_BYTES {
            return Err(Error::Protocol("answer limit"));
        }
        self.complete = text;
        self.partial.clear();
        Ok(())
    }
    fn into_text(self) -> String {
        if self.partial.is_empty() {
            self.complete
        } else {
            self.partial
        }
    }
}

#[cfg_attr(windows, allow(dead_code))]
fn provider_args(model: &ModelChoice, tools: bool) -> Vec<String> {
    let mut args = vec![
        "--print".into(),
        "--input-format".into(),
        "stream-json".into(),
        "--output-format".into(),
        "stream-json".into(),
        "--verbose".into(),
        "--include-partial-messages".into(),
        "--tools".into(),
        "".into(),
        "--permission-mode".into(),
        "auto".into(),
        "--permission-prompt-tool".into(),
        "stdio".into(),
        "--setting-sources".into(),
        "".into(),
        "--strict-mcp-config".into(),
        "--no-session-persistence".into(),
        "--max-turns".into(),
        "32".into(),
    ];
    args.push("--model".into());
    args.push(model.id.as_str().into());
    if let Some(effort) = &model.effort {
        args.push("--effort".into());
        args.push(effort.as_str().into());
    }
    args.push("--settings".into());
    args.push(json!({"disableAllHooks":true,"disableClaudeAiConnectors":true,"autoMemoryEnabled":false,"disableBundledSkills":true,"disableSkillShellExecution":true,"enableWorkflows":false,"workflowKeywordTriggerEnabled":false,"skillOverrides":{"doctor":"off","checkup":"off"},"enabledPlugins":{"agents-md@builtin":false}}).to_string());
    if tools {
        args.push("--allowedTools".into());
        args.push(
            broker::descriptors()
                .iter()
                .map(|tool| format!("mcp__xcb__{}", tool["name"].as_str().expect("tool name")))
                .collect::<Vec<_>>()
                .join(","),
        );
    }
    args
}

/// The bwrap launch plan the Linux `prepare` actually builds, kept
/// platform-neutral so the planner-acceptance regression test can construct
/// it anywhere. The planner owns the executable and forwarder-runtime binds;
/// `read_only` carries only the dynamic-loader/library closure — repeating a
/// path the plan already mounts used to be a fatal "bind target duplicated".
#[cfg(unix)]
pub(crate) fn linux_spec(
    executable: PathBuf,
    runtime: PathBuf,
    scratch: PathBuf,
    policy_path: PathBuf,
    socket: PathBuf,
    env_file: PathBuf,
    read_only: Vec<PathBuf>,
) -> sandbox::BwrapSpec {
    sandbox::BwrapSpec {
        executable,
        scratch,
        account_home: None,
        policy_path,
        read_only,
        egress: sandbox::Egress::Tcp443Dns,
        socket: Some(socket),
        forwarder: Some(sandbox::Forwarder {
            runtime,
            lo_up: None,
            env_file: Some(env_file),
            port: 48123,
        }),
    }
}

#[cfg(target_os = "linux")]
fn child_env(
    home: &Path,
    config: &Path,
    tmp: &Path,
    token: Option<&str>,
) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    env.insert("HOME".into(), home.to_string_lossy().into_owned());
    env.insert("TMPDIR".into(), tmp.to_string_lossy().into_owned());
    env.insert(
        "CLAUDE_CODE_TMPDIR".into(),
        tmp.to_string_lossy().into_owned(),
    );
    env.insert(
        "CLAUDE_CONFIG_DIR".into(),
        config.to_string_lossy().into_owned(),
    );
    env.insert(
        "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC".into(),
        "1".into(),
    );
    env.insert("CLAUDE_CODE_DISABLE_AUTO_MEMORY".into(), "1".into());
    if let Some(token) = token {
        env.insert("CLAUDE_CODE_OAUTH_TOKEN".into(), token.to_owned());
    }
    env
}

#[cfg(target_os = "macos")]
pub(crate) async fn prepare(
    pin: &Pin,
    root: &Path,
    model: &ModelChoice,
    token: Option<&str>,
    tools: bool,
) -> Result<Launch> {
    claude::runtime_admitted_with_catalog(root, pin)?;
    if !sandbox::available() {
        return Err(Error::Unavailable(
            "native OS confinement is not qualified on this platform; no unsandboxed fallback",
        ));
    }
    let artifacts = LaunchArtifacts::create(root)?;
    let directory = &artifacts.directory;
    let executable = pin.snapshot(directory)?;
    let scratch = private::directory(&directory.join("scratch"))?;
    let cwd = private::directory(&scratch.join("work"))?;
    let home = private::directory(&scratch.join("home"))?;
    let config = private::directory(&scratch.join("config"))?;
    let tmp = private::directory(&home.join("tmp"))?;
    let policy = sandbox::seatbelt(&executable, &scratch)?;
    let policy_path = directory.join("sandbox.sb");
    private::create(&policy_path, policy.as_bytes())?;
    let mut env = environment(&home);
    env.insert(
        "CLAUDE_CONFIG_DIR".into(),
        config.to_string_lossy().into_owned(),
    );
    env.insert("TMPDIR".into(), tmp.to_string_lossy().into_owned());
    env.insert(
        "CLAUDE_CODE_TMPDIR".into(),
        tmp.to_string_lossy().into_owned(),
    );
    env.insert(
        "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC".into(),
        "1".into(),
    );
    env.insert("CLAUDE_CODE_DISABLE_AUTO_MEMORY".into(), "1".into());
    if let Some(token) = token {
        env.insert("CLAUDE_CODE_OAUTH_TOKEN".into(), token.to_owned());
    }
    let mut command = Command::new("/usr/bin/sandbox-exec");
    command.arg("-f").arg(policy_path).arg(executable);
    command.args(provider_args(model, tools));
    command.env_clear().envs(env).current_dir(&cwd);
    Ok(Launch {
        command,
        cwd,
        bridge: None,
        artifacts,
        prepared_run: None,
        codex_credentials: None,
    })
}

#[cfg(target_os = "linux")]
pub(crate) async fn prepare(
    pin: &Pin,
    root: &Path,
    model: &ModelChoice,
    token: Option<&str>,
    tools: bool,
) -> Result<Launch> {
    claude::runtime_admitted_with_catalog(root, pin)?;
    let status = sandbox::linux_sandbox(root);
    if !status.qualified {
        return Err(Error::Unavailable(
            "native OS confinement is not qualified on this platform; no unsandboxed fallback",
        ));
    }
    let bwrap = status
        .candidate
        .as_deref()
        .and_then(|path| sandbox::BwrapPin::admit(path).ok())
        .ok_or(Error::Unavailable("bwrap not admitted"))?;
    let mut artifacts = LaunchArtifacts::create(root)?;
    let directory = &artifacts.directory;
    let executable = pin.snapshot(directory)?;
    let scratch = private::directory(&directory.join("scratch"))?;
    let cwd = private::directory(&scratch.join("work"))?;
    let home = private::directory(&scratch.join("home"))?;
    let config = private::directory(&scratch.join("config"))?;
    let tmp = private::directory(&home.join("tmp"))?;
    let env_file = egress::write_forwarder_env(&scratch, &child_env(&home, &config, &tmp, token))?;
    let socket_dir = private::directory(&directory.join("egress"))?;
    let socket = socket_dir.join("egress.sock");
    let xcb = xcb_core::canonical(std::env::current_exe()?)?;
    // The planner mounts the executable and the forwarder runtime itself;
    // read_only carries only the shared-library closure the dynamic loader
    // needs — provider snapshot and runtime alike.
    let mut read_only = sandbox::shared_library_closure(&executable)?;
    read_only.extend(sandbox::shared_library_closure(&xcb)?);
    let policy_path = directory.join("sandbox.json");
    artifacts.retain_before_launch();
    let bridge =
        match egress::EgressBridge::start(egress::EgressBridgeOptions::new(socket.clone())).await {
            Ok(bridge) => bridge,
            Err(error) => {
                // start cannot fail after installing its accept task.
                artifacts.release_after_join(true, EffectState::None);
                return Err(error);
            }
        };
    let spec = linux_spec(
        executable,
        xcb,
        scratch,
        policy_path,
        socket,
        env_file,
        read_only,
    );
    let wrapper_env = BTreeMap::from([("PATH".into(), "/usr/bin:/bin".into())]);
    let planned = (|| -> Result<Command> {
        let launch = sandbox::bwrap_launch(
            &bwrap,
            &spec,
            &provider_args(model, tools),
            &wrapper_env,
            &cwd,
        )?;
        private::create(&spec.policy_path, launch.policy.as_bytes())?;
        let mut command = Command::new(&bwrap.executable);
        command
            .args(launch.args)
            .env_clear()
            .envs(launch.env)
            .current_dir(&cwd);
        Ok(command)
    })();
    let command = match planned {
        Ok(command) => command,
        Err(error) => {
            return Err(discard_failed_preparation(&mut artifacts, bridge, error).await);
        }
    };
    Ok(Launch {
        command,
        cwd,
        bridge: Some(bridge),
        artifacts,
        prepared_run: None,
        codex_credentials: None,
    })
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(crate) async fn prepare(
    _pin: &Pin,
    _root: &Path,
    _model: &ModelChoice,
    _token: Option<&str>,
    _tools: bool,
) -> Result<Launch> {
    Err(Error::Unavailable(
        "native OS confinement is not supported on this platform",
    ))
}

#[cfg(target_os = "macos")]
pub(crate) fn prepare_codex(
    store: &Store,
    pin: &Pin,
    model: &ModelChoice,
    tools: bool,
    metadata_only: bool,
    run: Option<&RunRecord>,
) -> Result<(Launch, crate::codex::CodexProtocol)> {
    prepare_codex_native(store, pin, model, tools, metadata_only, run, None)
}

#[cfg(target_os = "macos")]
#[allow(clippy::too_many_arguments)]
fn prepare_codex_native(
    store: &Store,
    pin: &Pin,
    model: &ModelChoice,
    tools: bool,
    metadata_only: bool,
    run: Option<&RunRecord>,
    proxy: Option<&crate::native_mcp::NativeMcpProxy>,
) -> Result<(Launch, crate::codex::CodexProtocol)> {
    use crate::codex::{self, CodexOptions, CodexProtocol};
    codex::runtime_admitted_with_catalog(store.root(), pin)?;
    if !sandbox::available() {
        return Err(Error::Unavailable(
            "Codex requires qualified native OS confinement",
        ));
    }
    let artifacts = LaunchArtifacts::create(store.root())?;
    let directory = &artifacts.directory;
    let executable = pin.snapshot(directory)?;
    let scratch = private::directory(&directory.join("scratch"))?;
    let cwd = private::directory(&scratch.join("work"))?;
    let home = private::directory(&scratch.join("home"))?;
    let profile = private::directory(&scratch.join("profile"))?;
    let tmp = private::directory(&home.join("tmp"))?;
    let catalog_builder = if proxy.is_some() {
        codex::static_catalog_with_native
    } else {
        codex::static_catalog
    };
    let catalog = catalog_builder(
        store.root(),
        pin,
        (!metadata_only).then_some(model.id.as_str()),
    )?;
    let catalog_path = directory.join("models.json");
    private::create(&catalog_path, &catalog.bytes)?;
    let config_path = profile.join("config.toml");
    let native_helper = proxy
        .map(|_| {
            let (source, digest) = crate::process::host_identity()?;
            crate::process::snapshot_pinned_executable(&source, &digest, directory)
        })
        .transpose()?;
    let native_mcp = proxy
        .zip(native_helper.as_deref())
        .map(|(proxy, helper)| proxy.configuration(helper))
        .transpose()?;
    private::create(
        &config_path,
        codex::configuration_with_native(&catalog_path, native_mcp.as_ref())?.as_bytes(),
    )?;
    let ca_bundle = crate::public_ca::snapshot(directory)?;
    let policy = sandbox::codex_seatbelt_with_native(
        &executable,
        &scratch,
        &profile,
        &config_path,
        &catalog_path,
        &ca_bundle,
        native_helper
            .as_deref()
            .zip(proxy.map(|proxy| proxy.socket_path())),
    )?;
    let policy_path = directory.join("sandbox.sb");
    private::create(&policy_path, policy.as_bytes())?;
    let protocol = CodexProtocol::new(CodexOptions {
        cwd: cwd.clone(),
        account_home: profile.clone(),
        catalog_path,
        model: model.clone(),
        tools,
        metadata_only,
        admission: catalog.admission,
        native_mcp,
    })?;
    let mut env = environment(&home);
    env.insert("PATH".into(), "/usr/bin:/bin:/usr/sbin:/sbin".into());
    env.insert("CODEX_HOME".into(), profile.to_string_lossy().into_owned());
    // This exact public snapshot selects the file-based Rustls root loader; no
    // ambient CA override, user keychain, or trust-service access is inherited.
    env.insert(
        "SSL_CERT_FILE".into(),
        ca_bundle.to_string_lossy().into_owned(),
    );
    env.insert("TMPDIR".into(), tmp.to_string_lossy().into_owned());
    env.insert(
        "CODEX_INTERNAL_APP_SERVER_REMOTE_CONTROL_DISABLED".into(),
        "1".into(),
    );
    let mut command = Command::new("/usr/bin/sandbox-exec");
    command
        .arg("-f")
        .arg(policy_path)
        .arg(executable)
        .args(codex::ARGS)
        .env_clear()
        .envs(env)
        .current_dir(&cwd);
    // Everything before this point is a credential-free launch plan. The
    // caller has already acquired the exact account's run before snapshotting.
    let codex_credentials = run
        .map(|run| auth::snapshot_codex_auth(store, run, &profile))
        .transpose()?;
    Ok((
        Launch {
            command,
            cwd,
            bridge: None,
            artifacts,
            prepared_run: run.cloned(),
            codex_credentials,
        },
        protocol,
    ))
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn prepare_codex(
    _store: &Store,
    _pin: &Pin,
    _model: &ModelChoice,
    _tools: bool,
    _metadata_only: bool,
    _run: Option<&RunRecord>,
) -> Result<(Launch, crate::codex::CodexProtocol)> {
    Err(Error::Unavailable(
        "native Codex OS confinement is not qualified on this platform",
    ))
}

#[cfg(target_os = "macos")]
#[allow(dead_code)]
pub(crate) async fn prepare_devin(
    store: &Store,
    pin: &Pin,
    model: &ModelChoice,
    tools: bool,
    metadata_only: bool,
    run: Option<&RunRecord>,
) -> Result<(Launch, crate::devin::DevinProtocol)> {
    use crate::devin::{self, DevinBridge, DevinOptions, DevinProtocol};
    devin::runtime_admitted_with_catalog(store.root(), pin)?;
    if !sandbox::available() {
        return Err(Error::Unavailable(
            "Devin requires qualified native OS confinement",
        ));
    }
    model.validate()?;
    if model.provider != Provider::Devin || model.mode != Mode::Fixed || model.effort.is_some() {
        return Err(Error::Unavailable("unsupported Devin model mode"));
    }
    let artifacts = LaunchArtifacts::create(store.root())?;
    let directory = &artifacts.directory;
    let executable = pin.snapshot(directory)?;
    let helper = pin.host_snapshot(directory)?;
    let scratch = private::directory(&directory.join("scratch"))?;
    let cwd = private::directory(&scratch.join("work"))?;
    let home = private::directory(&scratch.join("home"))?;
    private::directory(&home.join("tmp"))?;
    let config_directory = private::directory(&home.join(".config/devin"))?;
    let config_path = config_directory.join("config.json");
    let mcp_path = config_directory.join("mcp_config.json");
    private::create(&config_path, &serde_json::to_vec(&devin::configuration())?)?;
    let socket = directory.join("mcp.sock");
    let mut bridge = if tools {
        Some(DevinBridge::bind(&socket)?)
    } else {
        None
    };
    let plan = (|| {
        let mcp = match &bridge {
            Some(bridge) => bridge.configuration(&helper)?,
            None => json!({"mcpServers":{}}),
        };
        private::create(&mcp_path, &serde_json::to_vec(&mcp)?)?;
        let policy = sandbox::devin_seatbelt(
            &executable,
            &helper,
            &scratch,
            &home,
            &config_directory,
            tools.then_some(socket.as_path()),
        )?;
        let policy_path = directory.join("sandbox.sb");
        private::create(&policy_path, policy.as_bytes())?;
        let mut env = environment(&home);
        env.insert("PATH".into(), "/usr/bin:/bin:/usr/sbin:/sbin".into());
        let mut command = Command::new("/usr/bin/sandbox-exec");
        command
            .arg("-f")
            .arg(policy_path)
            .arg(executable)
            .arg("--config")
            .arg(config_path)
            .args(["--permission-mode", "auto", "acp"])
            .env_clear()
            .envs(env)
            .current_dir(&cwd);
        if let Some(run) = run {
            let credential = devin::auth::token(store, &run.account)?;
            command.env("WINDSURF_API_KEY", credential.as_str());
        }
        Ok::<_, Error>(command)
    })();
    let command = match plan {
        Ok(command) => command,
        Err(error) => {
            if let Some(bridge) = &mut bridge {
                bridge.shutdown().await;
            }
            return Err(error);
        }
    };
    let protocol = DevinProtocol::new(
        DevinOptions {
            cwd: cwd.clone(),
            model: model.clone(),
            tools,
            metadata_only,
        },
        bridge,
    )?;
    Ok((
        Launch {
            command,
            cwd,
            bridge: None,
            artifacts,
            prepared_run: run.cloned(),
            codex_credentials: None,
        },
        protocol,
    ))
}

#[cfg(not(target_os = "macos"))]
pub(crate) async fn prepare_devin(
    _store: &Store,
    _pin: &Pin,
    _model: &ModelChoice,
    _tools: bool,
    _metadata_only: bool,
    _run: Option<&RunRecord>,
) -> Result<(Launch, crate::devin::DevinProtocol)> {
    Err(Error::Unavailable(
        "native Devin OS confinement is not qualified on this platform",
    ))
}

#[allow(dead_code)]
async fn probe_devin(store: &Store, pin: &Pin, account: Option<&Id>) -> Result<Vec<ModelChoice>> {
    if account.is_none() {
        return Err(Error::Unavailable(
            "Devin catalog requires a connected xcb account",
        ));
    }
    let model = ModelChoice {
        provider: Provider::Devin,
        id: Id::new("swe-2-high")?,
        label: "Devin catalog".into(),
        mode: Mode::Fixed,
        resolved: None,
        effort: None,
        observed_at_ms: now_ms(),
    };
    let run = account
        .map(|id| store.prepare_probe(id, Some(model.clone()), now_ms()))
        .transpose()?;
    let (mut launch, mut protocol) =
        match prepare_devin(store, pin, &model, false, true, run.as_ref()).await {
            Ok(prepared) => prepared,
            Err(error) => {
                if let Some(run) = &run {
                    store.settle(run, State::Failed, now_ms())?;
                }
                return Err(error);
            }
        };
    let spawned = spawn_process(
        store,
        run.as_ref(),
        launch.command,
        &mut launch.artifacts,
        launch.bridge.take(),
        false,
    )
    .await;
    let (mut process, bridge) = match spawned {
        Ok(process) => process,
        Err(error) => {
            protocol.shutdown().await;
            return Err(error);
        }
    };
    let result = tokio::time::timeout(Duration::from_secs(45), async {
        if let Some(run) = &run {
            store.mark_spawned(run, process.pid())?;
        }
        let models = protocol
            .initialize(&mut process, "Metadata only; do not create or run a task.")
            .await?;
        if models.is_empty() {
            return Err(Error::Unavailable(
                "Devin returned no models; refresh the account credentials",
            ));
        }
        if let Some(account) = account {
            // The provider's own account meter is the GetUserStatus call the
            // CLI's `auth status` makes; the host reads it directly with the
            // stored session token. Metering is best-effort: a failure leaves
            // usage unmeasured instead of failing the account refresh.
            let token = crate::devin::auth::token(store, account)?;
            if let Ok(status) =
                crate::devin::status::user_status(token.as_str(), crate::devin::VERSION, now_ms())
                    .await
            {
                let pool = store.account(account)?.quota_pool;
                let now = now_ms();
                for (name, window) in [
                    ("devin.daily", status.daily),
                    ("devin.weekly", status.weekly),
                ] {
                    let Some(window) = window else { continue };
                    let point = QuotaPoint {
                        pool: pool.clone(),
                        window: Id::new(name)?,
                        // The service reports remaining percentage; the store
                        // records usage as used_percent.
                        used_percent: 100.0 - window.remaining_percent,
                        resets_at_ms: window.resets_at_ms,
                        observed_at_ms: now,
                    };
                    if point.validate().is_ok() {
                        store.record_account_quota(
                            run.as_ref()
                                .ok_or(Error::Conflict("quota probe has no lease"))?,
                            &point,
                        )?;
                    }
                }
                if status.email.is_some() || status.plan_name.is_some() {
                    store.set_account_identity(
                        account,
                        status.email,
                        status.plan_name.map(|plan| format!("Devin {plan}")),
                    )?;
                }
            }
        }
        Ok(models)
    })
    .await
    .unwrap_or(Err(Error::Unavailable("Devin metadata timed out")));
    let process_joined = process.join().await;
    let protocol_joined = protocol.shutdown().await;
    let bridge_joined = close_bridge(bridge).await;
    let joined = process_joined && protocol_joined && bridge_joined;
    if !joined {
        return Err(Error::Unavailable(
            "metadata process stop is unproven; account custody retained",
        ));
    }
    if let Some(run) = &run {
        store.settle(run, State::Idle, now_ms())?;
    }
    launch
        .artifacts
        .release_after_join(joined, EffectState::None);
    result
}

/// Artifact/runtime admission only; each launch still checks configuration,
/// model/tool authority, fresh authentication, and OS confinement. The baked
/// constants are the floor; the reviewed-builds catalog admits newer pairs
/// and can deny any digest outright.
pub fn provider_admitted(root: &Path, pin: &Pin) -> bool {
    if crate::catalog::denied(root, &pin.sha256) {
        return false;
    }
    match pin.provider {
        Provider::Claude => {
            claude::runtime_admitted_with_catalog(root, pin).is_ok()
                && (sandbox::available()
                    || (cfg!(target_os = "linux") && sandbox::linux_sandbox(root).qualified))
        }
        Provider::Codex => {
            cfg!(target_os = "macos")
                && sandbox::available()
                && crate::codex::runtime_admitted_with_catalog(root, pin).is_ok()
        }
        Provider::Devin => false,
    }
}

async fn probe_codex(store: &Store, pin: &Pin, account: Option<&Id>) -> Result<Vec<ModelChoice>> {
    let model = ModelChoice {
        provider: Provider::Codex,
        id: Id::new(crate::codex::QUALIFIED_MODELS[0])?,
        label: "Codex catalog".into(),
        mode: Mode::Fixed,
        resolved: None,
        effort: None,
        observed_at_ms: now_ms(),
    };
    let run = account
        .map(|account| store.prepare_probe(account, Some(model.clone()), now_ms()))
        .transpose()?;
    let (mut launch, mut protocol) =
        match prepare_codex(store, pin, &model, false, true, run.as_ref()) {
            Ok(prepared) => prepared,
            Err(error) => {
                if let Some(run) = &run {
                    auth::discard_unstarted_codex_auth(store, run, true)?;
                    store.settle(run, State::Failed, now_ms())?;
                }
                return Err(error);
            }
        };
    let (mut process, bridge) = spawn_process(
        store,
        run.as_ref(),
        launch.command,
        &mut launch.artifacts,
        launch.bridge.take(),
        launch.codex_credentials.is_some(),
    )
    .await?;
    let result = async {
        if let Some(run) = &run {
            store.mark_spawned(run, process.pid())?;
        }
        let models = protocol
            .initialize(&mut process, "Metadata only; do not create or run a task.")
            .await?;
        if let Some(account) = account {
            let (email, plan) = protocol.account_identity();
            if email.is_some() || plan.is_some() {
                store.set_account_identity(account, email, plan)?;
            }
            for point in protocol
                .read_quotas(&mut process, &store.account(account)?.quota_pool)
                .await?
            {
                store.record_account_quota(
                    run.as_ref()
                        .ok_or(Error::Conflict("quota probe has no lease"))?,
                    &point,
                )?;
            }
        }
        Ok(models)
    }
    .await;
    let process_joined = process.join().await;
    let bridge_joined = close_bridge(bridge).await;
    let joined = process_joined && bridge_joined;
    if !joined {
        return Err(Error::Unavailable(
            "metadata process stop is unproven; account custody retained",
        ));
    }
    if let Some(run) = &run {
        if let Some(credentials) = &launch.codex_credentials {
            auth::persist_codex_auth(store, run, credentials, joined)?;
        }
        store.settle(run, State::Idle, now_ms())?;
    }
    launch
        .artifacts
        .release_after_join(joined, EffectState::None);
    result
}

/// Initialize the registered native connector in an ephemeral Codex thread.
/// Never submits a prompt, performs inference, or calls a browser/computer
/// operation. Uses the normal account lease, confinement, and joined refresh.
#[cfg(target_os = "macos")]
pub async fn inspect_computer_connector(
    store: Arc<Store>,
    account: &Id,
    model: &ModelChoice,
    server: crate::capabilities::CapabilityServer,
    workspace: PathBuf,
) -> Result<crate::native_mcp::NativeInspection> {
    if model.provider != Provider::Codex || store.account(account)?.provider != Provider::Codex {
        return Err(Error::Unavailable(
            "computer connector inspection requires Codex",
        ));
    }
    server.validate()?;
    store.require_authenticated_account(account)?;
    let pin = Pin::load(store.root(), Provider::Codex)?;
    crate::codex::runtime_admitted(&pin)?;
    let run = store.prepare_probe(account, Some(model.clone()), now_ms())?;
    let mut proxy = match crate::native_mcp::NativeMcpProxy::start(
        server,
        store.clone(),
        run.clone(),
        workspace,
    )
    .await
    {
        Ok(proxy) => proxy,
        Err(error) => {
            store.settle(&run, State::Failed, now_ms())?;
            return Err(error);
        }
    };
    let (mut launch, mut protocol) =
        match prepare_codex_native(&store, &pin, model, true, false, Some(&run), Some(&proxy)) {
            Ok(prepared) => prepared,
            Err(error) => {
                let receipt = proxy.shutdown().await;
                if !receipt.joined || receipt.effects != EffectState::None {
                    return Err(Error::Unavailable(
                        "connector inspection stop is unproven; account custody retained",
                    ));
                }
                auth::discard_unstarted_codex_auth(&store, &run, true)?;
                store.settle(&run, State::Failed, now_ms())?;
                return Err(error);
            }
        };
    let spawned = spawn_process(
        &store,
        Some(&run),
        launch.command,
        &mut launch.artifacts,
        launch.bridge.take(),
        launch.codex_credentials.is_some(),
    )
    .await;
    let (mut process, bridge) = match spawned {
        Ok(spawned) => spawned,
        Err(error) => {
            let receipt = proxy.shutdown().await;
            if !receipt.joined || receipt.effects != EffectState::None {
                return Err(Error::Unavailable(
                    "connector inspection stop is unproven; account custody retained",
                ));
            }
            return Err(error);
        }
    };
    protocol.attach_native_proxy(proxy);
    let initialization = async {
        store.mark_spawned(&run, process.pid())?;
        protocol
            .initialize(
                &mut process,
                "Metadata inspection only. No model turn or computer operation is authorized.",
            )
            .await?;
        Ok(())
    }
    .await;
    let diagnostic = protocol.native_diagnostic();
    let process_joined = process.join().await;
    let connector_joined = protocol.shutdown().await;
    let bridge_joined = close_bridge(bridge).await;
    let joined = process_joined && connector_joined && bridge_joined;
    if !joined || protocol.host_effects() != EffectState::None {
        return Err(Error::Unavailable(
            "connector inspection stop is unproven; account custody retained",
        ));
    }
    if let Some(credentials) = &launch.codex_credentials {
        auth::persist_codex_auth(&store, &run, credentials, joined)?;
    }
    store.settle(&run, State::Idle, now_ms())?;
    launch
        .artifacts
        .release_after_join(joined, EffectState::None);
    Ok(crate::native_mcp::NativeInspection {
        diagnostic: diagnostic.ok_or(Error::Protocol("native connector diagnostic absent"))?,
        initialization,
        unhandled_notice_sha256: protocol.unhandled_startup_notice_sha256.clone(),
    })
}

/// Interactive provider sign-in is a host action, separate from a model turn.
/// Device codes are shown by the official CLI; credentials stay in the private
/// launch profile until the process has independently joined.
#[cfg(windows)]
pub async fn login_codex(_store: &Store, _account: &Id, _pin: &Pin) -> Result<()> {
    Err(Error::providers_unsupported())
}

#[cfg(unix)]
pub async fn login_codex(store: &Store, account: &Id, pin: &Pin) -> Result<()> {
    login_codex_inner(store, account, pin, None, None).await
}

#[cfg(windows)]
pub async fn login_codex_with_prompt(
    _store: &Store,
    _account: &Id,
    _pin: &Pin,
    _sender: tokio::sync::mpsc::Sender<DeviceLoginPrompt>,
) -> Result<()> {
    Err(Error::providers_unsupported())
}

#[cfg(unix)]
pub async fn login_codex_with_prompt(
    store: &Store,
    account: &Id,
    pin: &Pin,
    sender: tokio::sync::mpsc::Sender<DeviceLoginPrompt>,
) -> Result<()> {
    login_codex_inner(store, account, pin, Some(sender), None).await
}

/// A command-lifetime cancellation request also covers the preparation phase,
/// before the login's own Unix signal receivers have been installed.
pub async fn login_codex_with_cancel(
    store: &Store,
    account: &Id,
    pin: &Pin,
    cancel: tokio::sync::watch::Receiver<bool>,
) -> Result<()> {
    #[cfg(windows)]
    {
        let _ = (store, account, pin, cancel);
        Err(Error::providers_unsupported())
    }
    #[cfg(unix)]
    {
        login_codex_inner(store, account, pin, None, Some(cancel)).await
    }
}

/// Keep the device prompt and prelaunch cancellation in the caller's command.
pub async fn login_codex_with_prompt_and_cancel(
    store: &Store,
    account: &Id,
    pin: &Pin,
    sender: tokio::sync::mpsc::Sender<DeviceLoginPrompt>,
    cancel: tokio::sync::watch::Receiver<bool>,
) -> Result<()> {
    #[cfg(windows)]
    {
        let _ = (store, account, pin, sender, cancel);
        Err(Error::providers_unsupported())
    }
    #[cfg(unix)]
    {
        login_codex_inner(store, account, pin, Some(sender), Some(cancel)).await
    }
}

#[cfg(unix)]
struct CodexLoginSignals {
    interrupt: tokio::signal::unix::Signal,
    terminate: tokio::signal::unix::Signal,
}

#[cfg(unix)]
impl CodexLoginSignals {
    fn register() -> std::io::Result<Self> {
        use tokio::signal::unix::{SignalKind, signal};
        Ok(Self {
            interrupt: signal(SignalKind::interrupt())?,
            terminate: signal(SignalKind::terminate())?,
        })
    }

    async fn cancelled(&mut self) {
        tokio::select! {
            _ = self.interrupt.recv() => {},
            _ = self.terminate.recv() => {},
        }
    }
}

#[cfg(unix)]
fn settle_unstarted_codex_login(
    store: &Store,
    run: &RunRecord,
    artifacts: &mut LaunchArtifacts,
) -> Result<()> {
    artifacts.retain_before_launch();
    auth::discard_unstarted_codex_auth(store, run, true)?;
    store.settle(run, State::Failed, now_ms())?;
    artifacts.release_after_join(true, EffectState::None);
    Ok(())
}

#[cfg(unix)]
async fn login_codex_inner(
    store: &Store,
    account: &Id,
    pin: &Pin,
    sender: Option<tokio::sync::mpsc::Sender<DeviceLoginPrompt>>,
    mut cancel: Option<tokio::sync::watch::Receiver<bool>>,
) -> Result<()> {
    use rustix::process::{Pid, Signal, kill_process_group};
    use std::{os::fd::AsFd, process::Stdio};
    // Register before launch so a command-wide signal handler cannot consume
    // SIGTERM while the account-owned provider continues running unattended.
    let mut stop = CodexLoginSignals::register()?;
    if cancel
        .as_ref()
        .is_some_and(|cancel| *cancel.borrow() || cancel.has_changed().is_err())
    {
        return Err(Error::Unavailable("Codex sign-in cancelled before launch"));
    }
    crate::codex::runtime_admitted_with_catalog(store.root(), pin)?;
    let status_output = std::io::stderr().as_fd().try_clone_to_owned()?;
    let mut artifacts = LaunchArtifacts::create(store.root())?;
    let snapshot_pin = Pin {
        executable: pin.snapshot(&artifacts.directory)?,
        ..pin.clone()
    };
    let profile = private::directory(&artifacts.directory.join("profile"))?;
    let run = store.prepare_probe(account, None, now_ms())?;
    // Preparation creates recovery metadata referencing this profile. Keep
    // it through every fallible unstarted receipt/account settlement step.
    artifacts.retain_before_launch();
    let mut plan = match auth::prepare_codex_login(store, &run, &snapshot_pin, &profile) {
        Ok(plan) => plan,
        Err(error) if error.is_cleanup_unproven() => {
            artifacts.retain_before_launch();
            return Err(error);
        }
        Err(error) => {
            settle_unstarted_codex_login(store, &run, &mut artifacts)?;
            return Err(error);
        }
    };
    // Keep stdout available for the CLI's final JSON acknowledgement.
    if sender.is_some() {
        // The assisted UI owns Enter; neither browser authorization path
        // gives the provider access to the operator's terminal input.
        plan.command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
    } else {
        plan.command.stdout(Stdio::from(status_output));
    }
    if cancel
        .as_ref()
        .is_some_and(|cancel| *cancel.borrow() || cancel.has_changed().is_err())
    {
        settle_unstarted_codex_login(store, &run, &mut artifacts)?;
        return Err(Error::Unavailable("Codex sign-in cancelled before launch"));
    }
    artifacts.retain_before_launch();
    let mut child = match plan.command.spawn() {
        Ok(child) => child,
        Err(error) => {
            settle_unstarted_codex_login(store, &run, &mut artifacts)?;
            return Err(Error::LaunchNotStarted(error));
        }
    };
    let pid = child
        .id()
        .filter(|pid| *pid > 1)
        .ok_or(Error::Protocol("login process identity"))?;
    struct LoginGroup(Option<Pid>);
    impl Drop for LoginGroup {
        fn drop(&mut self) {
            if let Some(group) = self.0 {
                let _ = kill_process_group(group, Signal::KILL);
            }
        }
    }
    let group = i32::try_from(pid)
        .ok()
        .and_then(Pid::from_raw)
        .ok_or(Error::Protocol("login process group"))?;
    let mut custody = LoginGroup(Some(group));
    let captured = sender.map(|sender| (child.stdout.take(), child.stderr.take(), sender));
    let has_bridge = captured.is_some();
    // Readers are owned by this future, never spawned or detached. Pipe EOF
    // is independent of physical provider exit.
    let bridge = async move {
        match captured {
            Some((Some(stdout), Some(stderr), sender)) => {
                crate::device_login::forward(stdout, stderr, tokio::io::stderr(), sender).await
            }
            Some(_) => Err(std::io::Error::other("Codex sign-in output capture failed")),
            None => Ok(()),
        }
    };
    tokio::pin!(bridge);
    let mut bridge_done = !has_bridge;
    let marked = store.mark_spawned(&run, pid);
    let mut result = if marked.is_ok() {
        let deadline = tokio::time::sleep(Duration::from_secs(600));
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                _ = stop.cancelled() => break Err(Error::Unavailable("Codex sign-in cancelled")),
                _ = async {
                    if let Some(cancel) = cancel.as_mut() {
                        while !*cancel.borrow() {
                            if cancel.changed().await.is_err() { break; }
                        }
                    } else {
                        std::future::pending::<()>().await;
                    }
                } => break Err(Error::Unavailable("Codex sign-in cancelled")),
                _ = &mut deadline => break Err(Error::Unavailable("Codex sign-in timed out")),
                output = &mut bridge, if !bridge_done => {
                    bridge_done = true;
                    if let Err(error) = output { break Err(error.into()); }
                },
                status = child.wait() => break match status {
                    Ok(status) if status.success() => Ok(()),
                    Ok(_) => Err(Error::Unavailable("Codex sign-in did not complete")),
                    Err(error) => Err(error.into()),
                },
            }
        }
    } else {
        marked.map(|_| ())
    };
    // A completed wait has reaped the leader, so its numeric group may be
    // reused. Signal only while Child still owns that unreaped identity, then
    // disarm before any further wait can reap it. Natural exits need absence
    // evidence; lingering descendants keep custody rather than risking a
    // signal to an unrelated recycled process group.
    if child.id() == Some(pid) {
        let _ = kill_process_group(group, Signal::KILL);
    }
    custody.0 = None;
    let joined = tokio::time::timeout(Duration::from_secs(5), async {
        if child.wait().await.is_err() {
            return false;
        }
        loop {
            if crate::process::prove_process_group_absent(pid).is_ok() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or(false);
    if !bridge_done {
        // A bounded output tail cannot hold account cleanup indefinitely.
        if let Ok(Err(error)) = tokio::time::timeout(Duration::from_secs(1), &mut bridge).await
            && result.is_ok()
        {
            result = Err(error.into());
        }
    }
    if !joined {
        return Err(Error::Unavailable(
            "sign-in process stop unproven; account custody retained",
        ));
    }
    custody.0 = None;
    let credential_path = plan.credentials.profile().join("auth.json");
    let has_credential = match std::fs::symlink_metadata(&credential_path) {
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(error.into()),
    };
    if has_credential {
        auth::persist_codex_auth(store, &run, &plan.credentials, joined)?;
        if result.is_ok() {
            store.clear_authentication_failure(&run)?;
        }
    } else {
        // No persistent account credential was handed to the login process.
        // A stopped, unsuccessful device flow may be retried normally.
        store.settle_tool(&run, "xcb_auth_snapshot")?;
    }
    store.settle(
        &run,
        if result.is_ok() && has_credential {
            State::Idle
        } else {
            State::Failed
        },
        now_ms(),
    )?;
    artifacts.release_after_join(joined, EffectState::None);
    result?;
    if !has_credential {
        return Err(Error::Unavailable("Codex sign-in returned no credential"));
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod login_cancellation_tests {
    use super::*;

    #[test]
    fn unstarted_codex_login_keeps_recovery_profile_until_all_settlement_succeeds() {
        use std::os::unix::fs::PermissionsExt;
        for failure in ["none", "receipt", "account"] {
            let directory = tempfile::tempdir().unwrap();
            let base = xcb_core::canonical(directory.path()).unwrap();
            let store = Store::open(&base.join("state")).unwrap();
            let account = store
                .add_account(Provider::Codex, "Synthetic", 1, None)
                .unwrap();
            let run = store.prepare_probe(&account.id, None, now_ms()).unwrap();
            let fixture = private::directory(&base.join("fixture")).unwrap();
            let executable = fixture.join("synthetic-codex-never-launched");
            private::create(&executable, b"#!/bin/sh\nexit 99\n").unwrap();
            std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
            let pin = Pin {
                provider: Provider::Codex,
                sha256: crate::process::executable_digest(&executable).unwrap(),
                executable,
                version: "synthetic-unadmitted".into(),
                host_sha256: crate::process::executable_digest(
                    &xcb_core::canonical(std::env::current_exe().unwrap()).unwrap(),
                )
                .unwrap(),
                observed_at_ms: 1,
            };
            let mut artifacts = LaunchArtifacts::create(store.root()).unwrap();
            let path = artifacts.path().to_owned();
            artifacts.retain_before_launch();
            let profile = private::directory(&path.join("profile")).unwrap();
            // Build the real preparation/recovery record without executing
            // or admitting this deliberately synthetic provider image.
            let _plan = auth::prepare_codex_login(&store, &run, &pin, &profile).unwrap();
            let db = rusqlite::Connection::open(store.root().join("xcb.sqlite")).unwrap();
            match failure {
                "receipt" => db.execute_batch("CREATE TRIGGER fail_unstarted_receipt BEFORE UPDATE OF settled ON tool_effects WHEN OLD.call='xcb_auth_snapshot' BEGIN SELECT RAISE(ABORT,'synthetic receipt failure'); END;").unwrap(),
                "account" => db.execute_batch("CREATE TRIGGER fail_unstarted_account BEFORE UPDATE OF phase ON runs WHEN NEW.phase='settled' BEGIN SELECT RAISE(ABORT,'synthetic account failure'); END;").unwrap(),
                _ => {},
            }
            let result = settle_unstarted_codex_login(&store, &run, &mut artifacts);
            let failed = failure != "none";
            assert_eq!(result.is_err(), failed, "{failure}");
            drop(artifacts);
            assert_eq!(path.exists(), failed, "{failure}");
            assert_eq!(store.unsettled_runs().unwrap().len(), usize::from(failed));
            if failed {
                assert!(profile.is_dir());
                let receipt = store
                    .root()
                    .join("runs")
                    .join(format!("{}.codex-auth-recovery.json", run.id));
                let metadata: Value =
                    serde_json::from_slice(&private::read(&receipt, 64 * 1024).unwrap()).unwrap();
                assert_eq!(metadata["profile"].as_str(), profile.to_str());
                assert!(store.prepare_probe(&account.id, None, now_ms()).is_err());
            }
        }
    }

    #[tokio::test]
    async fn codex_login_prelaunch_cancel_creates_no_provider_artifacts_or_account_run() {
        let directory = tempfile::tempdir().unwrap();
        let base = xcb_core::canonical(directory.path()).unwrap();
        let store = Store::open(&base.join("state")).unwrap();
        let account = store
            .add_account(Provider::Codex, "Synthetic", 1, None)
            .unwrap();
        let pin = Pin {
            provider: Provider::Codex,
            executable: base.join("must-not-be-inspected-or-launched"),
            sha256: "0".repeat(64),
            host_sha256: "0".repeat(64),
            version: "synthetic-unadmitted".into(),
            observed_at_ms: 1,
        };
        let runs_before = store.root().join("runs").exists();
        for closed in [false, true] {
            let (request, cancel) = tokio::sync::watch::channel(!closed);
            let _request = if closed {
                drop(request);
                None
            } else {
                Some(request)
            };
            for assisted in [false, true] {
                let (events, _observed) = tokio::sync::mpsc::channel(1);
                let result = if assisted {
                    login_codex_with_prompt_and_cancel(
                        &store,
                        &account.id,
                        &pin,
                        events,
                        cancel.clone(),
                    )
                    .await
                } else {
                    login_codex_with_cancel(&store, &account.id, &pin, cancel.clone()).await
                };
                assert!(matches!(
                    result,
                    Err(Error::Unavailable("Codex sign-in cancelled before launch"))
                ));
                assert!(store.unsettled_runs().unwrap().is_empty());
                assert_eq!(store.root().join("runs").exists(), runs_before);
            }
        }
    }

    #[tokio::test]
    async fn signal_login_child() {
        if std::env::var_os("XCB_CODEX_LOGIN_SIGNAL_CHILD").is_none() {
            return;
        }
        let mut stop = CodexLoginSignals::register().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let base = xcb_core::canonical(directory.path()).unwrap();
        let store = Store::open(&base.join("state")).unwrap();
        let account = store
            .add_account(Provider::Codex, "Synthetic", 1, None)
            .unwrap();
        let run = store.prepare_probe(&account.id, None, now_ms()).unwrap();
        let mut artifacts = LaunchArtifacts::create(store.root()).unwrap();
        let launch_path = artifacts.path().to_owned();
        artifacts.retain_before_launch();
        let mut command = Command::new("/bin/sh");
        // A failed subprocess harness also lets its synthetic descendants
        // exit on parent loss instead of leaving a long-lived fixture behind.
        command
            .env_clear()
            .env("XCB_TEST_PARENT_PID", std::process::id().to_string())
            .args([
                "-c",
                "while kill -0 \"$XCB_TEST_PARENT_PID\" 2>/dev/null; do /bin/sleep 0.05; done",
            ]);
        let (request, cancel) = tokio::sync::watch::channel(false);
        let login = crate::process::capture_supervised(
            command,
            1024,
            Duration::from_secs(10),
            cancel,
            |pid| {
                use std::io::Write;
                store.mark_spawned(&run, pid)?;
                println!("SIGNAL_LOGIN_READY");
                std::io::stdout().flush()?;
                Ok(())
            },
        );
        tokio::pin!(login);
        let outcome = tokio::select! {
            result = &mut login => panic!("login ended before cancellation: {}", matches!(result, crate::process::CaptureOutcome::Joined(_))),
            _ = stop.cancelled() => {
                request.send(true).unwrap();
                login.await
            },
        };
        assert!(matches!(
            outcome,
            crate::process::CaptureOutcome::Joined(Err(_))
        ));
        let pid = store.unsettled_runs().unwrap()[0].pid.unwrap();
        crate::process::prove_process_group_absent(pid).unwrap();
        store.settle(&run, State::Failed, now_ms()).unwrap();
        artifacts.release_after_join(true, EffectState::None);
        drop(artifacts);
        assert!(store.unsettled_runs().unwrap().is_empty());
        assert!(!launch_path.exists());
        println!("SIGNAL_LOGIN_JOINED");
    }

    #[test]
    fn login_interrupt_and_termination_join_owned_group_before_account_release() {
        use std::path::Path;
        let python = ["/usr/bin/python3", "/usr/local/bin/python3"]
            .into_iter()
            .find(|path| Path::new(path).is_file())
            .expect("Python 3 is required for signal regression tests");
        let script = r#"
import os, select, signal, subprocess, sys, time
for cancel in [signal.SIGINT, signal.SIGTERM]:
    child = subprocess.Popen([sys.argv[1], '--exact', 'runner::login_cancellation_tests::signal_login_child', '--nocapture'], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    data = bytearray()
    try:
        deadline = time.monotonic() + 8
        while b'SIGNAL_LOGIN_READY' not in data and time.monotonic() < deadline:
            if select.select([child.stdout], [], [], .02)[0]:
                chunk = os.read(child.stdout.fileno(), 65536)
                if not chunk:
                    break
                data.extend(chunk)
        assert b'SIGNAL_LOGIN_READY' in data, data
        child.send_signal(cancel)
        stdout, stderr = child.communicate(timeout=8)
        data.extend(stdout)
        assert child.returncode == 0 and b'SIGNAL_LOGIN_JOINED' in data, (cancel, data, stderr)
    finally:
        if child.poll() is None:
            child.kill()
            child.wait(timeout=2)
"#;
        let output = std::process::Command::new(python)
            .args(["-c", script])
            .arg(std::env::current_exe().unwrap())
            .env("XCB_CODEX_LOGIN_SIGNAL_CHILD", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

fn initialize(tools: bool, system: &str) -> Value {
    json!({"type":"control_request","request_id":"xcb_initialize","request":{"subtype":"initialize","sdkMcpServers":if tools { vec!["xcb"] } else { vec![] },"hooks":{},"agents":{},"skills":[],"plugins":[],"systemPrompt":[system],"supportedDialogKinds":[]}})
}

pub(crate) fn mcp_reply(request: &Value, tools: bool, call_result: Option<Value>) -> Result<Value> {
    if request.get("server_name").and_then(Value::as_str) != Some("xcb") || !tools {
        return Err(Error::Protocol("unexpected MCP server"));
    }
    let message = request
        .get("message")
        .ok_or(Error::Protocol("MCP message"))?;
    if message.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(Error::Protocol("MCP version"));
    }
    let result = match message.get("method").and_then(Value::as_str) {
        Some("initialize") => {
            json!({"protocolVersion":message.pointer("/params/protocolVersion").and_then(Value::as_str).ok_or(Error::Protocol("MCP protocol"))?,"capabilities":{"tools":{}},"serverInfo":{"name":"xcb","version":env!("CARGO_PKG_VERSION")}})
        }
        Some("tools/list") => json!({"tools":broker::descriptors()}),
        Some("notifications/initialized") => return Ok(json!({})),
        Some("tools/call") => call_result.ok_or(Error::Protocol("tool call before admission"))?,
        _ => return Err(Error::Protocol("unsupported MCP method")),
    };
    let id = message
        .get("id")
        .filter(|id| id.is_string() || id.is_i64())
        .ok_or(Error::Protocol("MCP request id"))?;
    Ok(json!({"mcp_response":{"jsonrpc":"2.0","id":id,"result":result}}))
}

pub(crate) async fn control(
    process: &mut StreamProcess,
    envelope: &Value,
    response: Value,
) -> Result<()> {
    let id = envelope
        .get("request_id")
        .and_then(Value::as_str)
        .filter(|id| id.len() <= 160)
        .ok_or(Error::Protocol("control id"))?;
    process.send(&json!({"type":"control_response","response":{"subtype":"success","request_id":id,"response":response}})).await
}

pub fn parse_models(value: &Value, now: u64) -> Result<Vec<ModelChoice>> {
    let models = value
        .get("models")
        .and_then(Value::as_array)
        .ok_or(Error::Protocol("model catalog"))?;
    if models.len() > 128 {
        return Err(Error::Protocol("model catalog limit"));
    }
    let mut choices = BTreeMap::new();
    for model in models {
        let id = Id::new(
            model
                .get("value")
                .or_else(|| model.get("resolvedModel"))
                .and_then(Value::as_str)
                .ok_or(Error::Protocol("model identifier"))?,
        )?;
        let resolved = model
            .get("resolvedModel")
            .and_then(Value::as_str)
            .filter(|resolved| *resolved != id.as_str())
            .map(Id::new)
            .transpose()?;
        let name = model
            .get("displayName")
            .and_then(Value::as_str)
            .ok_or(Error::Protocol("model label"))?;
        xcb_core::label(name, 200)?;
        let efforts = match model.get("supportedEffortLevels") {
            None | Some(Value::Null) => vec![None],
            Some(value) => {
                let values = value.as_array().ok_or(Error::Protocol("model efforts"))?;
                if values.len() > 8 {
                    return Err(Error::Protocol("effort limit"));
                }
                values
                    .iter()
                    .map(|effort| {
                        Ok(Some(Id::new(
                            effort.as_str().ok_or(Error::Protocol("effort value"))?,
                        )?))
                    })
                    .collect::<Result<Vec<_>>>()?
            }
        };
        for effort in efforts {
            let choice = ModelChoice {
                provider: Provider::Claude,
                id: id.clone(),
                label: effort
                    .as_ref()
                    .map(|effort| format!("{name} · {effort}"))
                    .unwrap_or_else(|| name.to_owned()),
                mode: Mode::Fixed,
                resolved: resolved.clone(),
                effort,
                observed_at_ms: now,
            };
            choice.validate()?;
            choices.entry(choice.key()).or_insert(choice);
        }
    }
    Ok(choices.into_values().collect())
}

pub fn validate_init(value: &Value, cwd: &Path, model: &ModelChoice, tools: bool) -> Result<()> {
    let mut expected = if tools {
        broker::descriptors()
            .iter()
            .map(|tool| format!("mcp__xcb__{}", tool["name"].as_str().expect("static tool")))
            .collect::<Vec<_>>()
    } else {
        vec![]
    };
    expected.sort();
    let inventory = value
        .get("tools")
        .and_then(Value::as_array)
        .ok_or(Error::Protocol("tool inventory"))?;
    let mut actual = inventory
        .iter()
        .map(|tool| {
            tool.as_str()
                .map(str::to_owned)
                .ok_or(Error::Protocol("tool inventory name"))
        })
        .collect::<Result<Vec<_>>>()?;
    actual.sort();
    let empty = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty)
    };
    let servers = value
        .get("mcp_servers")
        .and_then(Value::as_array)
        .ok_or(Error::Protocol("MCP inventory"))?;
    // Each boundary field fails on its own so the diagnostic names what the
    // runtime changed. Field names only: values never leave the run record.
    if value
        .get("claude_code_version")
        .and_then(Value::as_str)
        .is_none_or(|version| !claude::version_admitted(version))
    {
        return Err(Error::Protocol(
            "effective runtime boundary mismatch: claude_code_version",
        ));
    }
    if value.get("cwd").and_then(Value::as_str) != cwd.to_str() {
        return Err(Error::Protocol("effective runtime boundary mismatch: cwd"));
    }
    let model_mismatch = match value.get("model").and_then(Value::as_str) {
        // Alias choices (`value` ≠ `resolvedModel`, e.g. `default`) are
        // resolved provider-side and can vary by effort — `default/low`
        // was observed serving `claude-sonnet-5` while the catalog says
        // `claude-opus-5[1m]`. The bound intent is the alias itself, so
        // any well-formed reported model is within contract.
        Some(reported) if model.resolved.is_some() => Id::new(reported).is_err(),
        reported => reported != Some(model.id.as_str()),
    };
    if model_mismatch {
        return Err(Error::Protocol(
            "effective runtime boundary mismatch: model",
        ));
    }
    if value.get("apiKeySource").and_then(Value::as_str) != Some("none") {
        return Err(Error::Protocol(
            "effective runtime boundary mismatch: apiKeySource",
        ));
    }
    if value.get("permissionMode").and_then(Value::as_str) != Some("auto") {
        return Err(Error::Protocol(
            "effective runtime boundary mismatch: permissionMode",
        ));
    }
    if actual != expected {
        return Err(Error::Protocol(
            "effective runtime boundary mismatch: tools",
        ));
    }
    if !empty("skills") {
        return Err(Error::Protocol(
            "effective runtime boundary mismatch: skills",
        ));
    }
    // Claude Code 2.1.281 started reporting its builtin `agents-md` plugin
    // here; the launch settings disable it so the effective set stays empty.
    if !empty("plugins") {
        return Err(Error::Protocol(
            "effective runtime boundary mismatch: plugins",
        ));
    }
    if servers.len() != usize::from(tools)
        || servers.iter().any(|server| {
            server.get("name").and_then(Value::as_str) != Some("xcb")
                || server.get("status").and_then(Value::as_str) != Some("connected")
        })
    {
        return Err(Error::Protocol(
            "effective runtime boundary mismatch: mcp_servers",
        ));
    }
    Ok(())
}

pub fn parse_quotas(value: &Value, pool: &Id, now: u64) -> Result<Vec<QuotaPoint>> {
    if value.get("rate_limits_available").and_then(Value::as_bool) != Some(true) {
        return Ok(vec![]);
    }
    let limits = value
        .get("rate_limits")
        .and_then(Value::as_object)
        .ok_or(Error::Protocol("quota windows"))?;
    let mut points = Vec::new();
    for name in [
        "five_hour",
        "seven_day",
        "seven_day_oauth_apps",
        "seven_day_opus",
        "seven_day_sonnet",
    ] {
        let Some(window) = limits.get(name).filter(|window| !window.is_null()) else {
            continue;
        };
        let Some(percent) = window.get("utilization").filter(|value| !value.is_null()) else {
            continue;
        };
        let percent = percent
            .as_f64()
            .filter(|value| value.is_finite() && (0.0..=100.0).contains(value))
            .ok_or(Error::Protocol("quota percentage"))?;
        let Some(reset) = window.get("resets_at").and_then(Value::as_str) else {
            continue;
        };
        let date =
            time::OffsetDateTime::parse(reset, &time::format_description::well_known::Rfc3339)
                .map_err(|_| Error::Protocol("quota reset timestamp"))?;
        let reset = u64::try_from(date.unix_timestamp_nanos() / 1_000_000)
            .map_err(|_| Error::Protocol("quota reset timestamp"))?;
        if reset <= now {
            continue;
        }
        points.push(QuotaPoint {
            pool: pool.clone(),
            window: Id::new(name)?,
            used_percent: percent,
            observed_at_ms: now,
            resets_at_ms: reset,
        });
    }
    Ok(points)
}

pub(crate) async fn handshake(
    process: &mut StreamProcess,
    tools: bool,
    system: &str,
) -> Result<Vec<ModelChoice>> {
    process.send(&initialize(tools, system)).await?;
    tokio::time::timeout(crate::protocol::INIT_DEADLINE, async {
        for _ in 0..256 {
            let frame = process
                .frame()
                .await?
                .ok_or(Error::Protocol("provider ended during initialization"))?;
            match claude::parse_event(&frame)? {
                Event::Control(envelope) => {
                    let request = envelope
                        .get("request")
                        .ok_or(Error::Protocol("control request"))?;
                    if request.get("subtype").and_then(Value::as_str) != Some("mcp_message") {
                        return Err(Error::Protocol("unexpected initialization request"));
                    }
                    let response = mcp_reply(request, tools, None)?;
                    control(process, &envelope, response).await?;
                }
                Event::ControlResponse(value)
                    if value
                        .pointer("/response/request_id")
                        .and_then(Value::as_str)
                        == Some("xcb_initialize") =>
                {
                    if value.pointer("/response/subtype").and_then(Value::as_str) != Some("success")
                    {
                        // The error text is the provider's; only its fixed
                        // classification crosses into host state.
                        let detail = value
                            .pointer("/response/error")
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        return Err(if claude::authentication_cue(detail) {
                            Error::Unavailable(crate::category::AUTHENTICATION)
                        } else {
                            Error::Protocol("initialization failed")
                        });
                    }
                    return parse_models(
                        value
                            .pointer("/response/response")
                            .ok_or(Error::Protocol("initialize response"))?,
                        now_ms(),
                    );
                }
                Event::Result {
                    failure: Some(Failure::Authentication),
                    ..
                } => return Err(Error::Unavailable(crate::category::AUTHENTICATION)),
                Event::Notice => (),
                _ => return Err(Error::Protocol("unexpected frame before initialization")),
            }
        }
        Err(Error::Protocol("initialization frame limit"))
    })
    .await
    .map_err(|_| Error::Unavailable("provider initialization timed out"))?
}

pub async fn probe(store: &Store, pin: &Pin, account: Option<&Id>) -> Result<Vec<ModelChoice>> {
    if pin.provider == Provider::Codex {
        return probe_codex(store, pin, account).await;
    }
    if pin.provider == Provider::Devin {
        return Err(Error::Unavailable("Devin support was removed; use Claude or Codex"));
    }
    let now = now_ms();
    let model = ModelChoice {
        provider: Provider::Claude,
        id: Id::new("claude-fable-5-1")?,
        label: "Fable 5.1".into(),
        mode: Mode::Fixed,
        resolved: None,
        effort: None,
        observed_at_ms: now,
    };
    let run = account
        .map(|id| store.prepare_probe(id, Some(model.clone()), now))
        .transpose()?;
    let mut custody = PreparationCustody {
        store,
        run: run.as_ref(),
        active: true,
    };
    let prepared = async {
        if let Some(run) = &run {
            let (_keepalive, cancel) = watch::channel(false);
            auth::refresh_claude_credentials(store, run, pin, cancel).await?;
        }
        let token = account.map(|id| auth::token(store, id)).transpose()?;
        prepare(
            pin,
            store.root(),
            &model,
            token.as_deref().map(|token| token.as_str()),
            false,
        )
        .await
    }
    .await;
    custody.active = false;
    drop(custody);
    let mut launch = match prepared {
        Ok(launch) => launch,
        Err(error) => return Err(settle_failed_preparation(store, run.as_ref(), error)),
    };
    let (mut process, bridge) = spawn_process(
        store,
        run.as_ref(),
        launch.command,
        &mut launch.artifacts,
        launch.bridge.take(),
        launch.codex_credentials.is_some(),
    )
    .await?;
    let result = async {
        if let Some(run) = &run { store.mark_spawned(run, process.pid())?; }
        let models = handshake(&mut process, false, "Return no messages; this connection is for host metadata queries only.").await?;
        if let Some(account) = account {
            process.send(&json!({"type":"control_request","request_id":"xcb_usage","request":{"subtype":"get_usage","skip_behaviors":true}})).await?;
            let response = tokio::time::timeout(Duration::from_secs(20), async {
                for _ in 0..128 {
                    let bytes = process.frame().await?.ok_or(Error::Protocol("usage connection ended"))?;
                    if let Event::ControlResponse(value) = claude::parse_event(&bytes)?
                        && value.pointer("/response/request_id").and_then(Value::as_str) == Some("xcb_usage") {
                            if value.pointer("/response/subtype").and_then(Value::as_str) != Some("success") { return Err(Error::Protocol("quota query unavailable")); }
                            return value.pointer("/response/response").cloned().ok_or(Error::Protocol("quota response"));
                        }
                }
                Err(Error::Protocol("usage frame limit"))
            }).await.map_err(|_| Error::Unavailable("usage query timed out"))??;
            let pool = store.account(account)?.quota_pool;
            let now = now_ms();
            let mut points = parse_quotas(&response, &pool, now)?;
            if points.is_empty() {
                // `get_usage` resolves only for stored-credential profiles, so
                // env-token launches never serve it. Browser sign-ins fall
                // back to the provider's own usage endpoint; setup tokens
                // lack its scope and stay passively metered.
                if let Ok(Some(usage)) = auth::claude_usage(store, account).await {
                    points = parse_quotas(
                        &json!({"rate_limits_available": true, "rate_limits": usage}),
                        &pool,
                        now,
                    )?;
                }
            }
            for point in points { store.record_account_quota(run.as_ref().ok_or(Error::Conflict("quota probe has no lease"))?, &point)?; }
            // subscription_type is the provider's own plan report; the profile
            // file inside our launch profile may carry the account email.
            let plan = response
                .get("subscription_type")
                .and_then(Value::as_str)
                .filter(|value| {
                    !value.is_empty()
                        && value.len() <= 64
                        && !value.chars().any(char::is_control)
                        && value.trim() == *value
                })
                .map(|value| format!("Claude {value}"));
            let base = launch.artifacts.path();
            let email = auth::claude_profile_email(&[
                &base.join("scratch").join("config"),
                &base.join("scratch").join("home"),
                &base.join("profile"),
            ]);
            if email.is_some() || plan.is_some() {
                store.set_account_identity(account, email, plan)?;
            }
        }
        Ok::<_, Error>(models)
    }.await;
    let process_joined = process.join().await;
    let bridge_joined = close_bridge(bridge).await;
    let joined = process_joined && bridge_joined;
    if !joined {
        return Err(Error::Unavailable(
            "metadata process stop is unproven; account custody retained",
        ));
    }
    if let Some(run) = &run {
        store.settle(run, State::Idle, now_ms())?;
    }
    launch
        .artifacts
        .release_after_join(joined, EffectState::None);
    result
}

fn combine_effects(previous: EffectState, next: EffectState) -> EffectState {
    if previous == EffectState::Uncertain || next == EffectState::Uncertain {
        EffectState::Uncertain
    } else if previous == EffectState::Settled || next == EffectState::Settled {
        EffectState::Settled
    } else {
        EffectState::None
    }
}

fn settle_tool_effects(
    store: &Store,
    run: &RunRecord,
    call: &str,
    call_effects: EffectState,
    effects: &mut EffectState,
) -> Result<()> {
    let previous = *effects;
    // A receipt write can fail after the tool ran. Until that receipt is
    // durable, retain custody even when the filesystem result was known.
    *effects = EffectState::Uncertain;
    if call_effects != EffectState::Uncertain {
        store.settle_tool(run, call)?;
        *effects = if previous == EffectState::Uncertain {
            EffectState::Uncertain
        } else if call_effects == EffectState::Settled {
            EffectState::Settled
        } else {
            previous
        };
    }
    Ok(())
}

/// The managed mailbox bridge is opened once per worker run instead of once
/// per `xcb_*` tool call. A failed open is not remembered: the next call
/// retries, preserving the original per-call error surface.
pub(crate) async fn managed_tool_call(
    store: &Store,
    bridge: &mut Option<crate::managed::ManagedStore>,
    session: &Id,
    call: &str,
    name: &str,
    arguments: &Value,
) -> (Result<Value>, EffectState) {
    if bridge.is_none() {
        match crate::managed::ManagedStore::open(store.root()) {
            Ok(managed) => *bridge = Some(managed),
            // No worker effect occurred if its host mailbox could not be
            // opened. Return a normal tool rejection so its already-written
            // pending receipt is settled and does not strand otherwise
            // proven custody.
            Err(error) => return (Err(error), EffectState::None),
        }
    }
    bridge
        .as_ref()
        .expect("managed bridge opened above")
        .worker_call(store, session, call, name, arguments)
        .await
}

pub struct RunInput {
    pub session: Session,
    pub message: Message,
    pub config: Config,
    pub pane_generation: bool,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CapabilityList {
    server: Option<String>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CapabilityCall {
    server: String,
    tool: String,
    arguments: Value,
}

fn requested_computer_capability(
    arguments: &Value,
) -> Result<(&'static str, xcb_core::session::TaskRequirements)> {
    use xcb_core::session::TaskRequirements;
    if *arguments == json!({"capability":"signed_in_browser"}) {
        Ok((
            "signed_in_browser",
            TaskRequirements {
                signed_in_browser: true,
                ..Default::default()
            },
        ))
    } else if *arguments == json!({"capability":"desktop"}) {
        Ok((
            "desktop",
            TaskRequirements {
                desktop: true,
                ..Default::default()
            },
        ))
    } else {
        Err(Error::Protocol("unsupported task capability"))
    }
}

pub async fn run(
    store: Arc<Store>,
    input: RunInput,
    cancel: watch::Receiver<bool>,
    observer: Observer,
) -> Result<Outcome> {
    let session = &input.session;
    if *cancel.borrow() {
        return Err(Error::Unavailable("cancelled before launch"));
    }
    if session.requirements.signed_in_browser
        && !input.pane_generation
        && !input.config.capabilities.servers.iter().any(|server| {
            server
                .features
                .contains(&crate::capabilities::CapabilityFeature::Browser)
        })
    {
        return Err(Error::Unavailable(
            "signed-in browser tools are not connected; run xcb tools setup-computer or register a browser server with xcb tools add",
        ));
    }
    if !input.pane_generation {
        input
            .config
            .capabilities
            .require_native_task(session.requirements)?;
        #[cfg(not(target_os = "macos"))]
        if session.requirements.desktop || session.requirements.codex_native {
            return Err(Error::Unavailable(
                "native desktop tools require a supported macOS host",
            ));
        }
    }
    let workspace = Workspace::open(Path::new(&session.workspace))?;
    if session.model.provider == Provider::Codex {
        let pin = Pin::load(store.root(), Provider::Codex)?;
        crate::codex::runtime_admitted_with_catalog(store.root(), &pin)?;
        let run = store.prepare_run(&session.id, session.revision, now_ms())?;
        #[cfg(target_os = "macos")]
        let mut native_proxy = if !input.pane_generation {
            match input
                .config
                .capabilities
                .servers
                .iter()
                .find(|server| {
                    server.transport == crate::capabilities::CapabilityTransport::CodexNative
                })
                .cloned()
            {
                Some(server) => match crate::native_mcp::NativeMcpProxy::start(
                    server,
                    store.clone(),
                    run.clone(),
                    workspace.root().to_owned(),
                )
                .await
                {
                    Ok(proxy) => Some(proxy),
                    Err(error) => {
                        store.settle(&run, State::Failed, now_ms())?;
                        return Err(error);
                    }
                },
                None => None,
            }
        } else {
            None
        };
        #[cfg(target_os = "macos")]
        let prepared = prepare_codex_native(
            &store,
            &pin,
            &session.model,
            !input.pane_generation,
            false,
            Some(&run),
            native_proxy.as_ref(),
        );
        #[cfg(not(target_os = "macos"))]
        let prepared = prepare_codex(
            &store,
            &pin,
            &session.model,
            !input.pane_generation,
            false,
            Some(&run),
        );
        let (launch, protocol) = match prepared {
            Ok((launch, protocol)) => {
                #[cfg(target_os = "macos")]
                let protocol = {
                    let mut protocol = protocol;
                    if let Some(proxy) = native_proxy.take() {
                        protocol.attach_native_proxy(proxy);
                    }
                    protocol
                };
                (launch, protocol)
            }
            Err(error) => {
                #[cfg(target_os = "macos")]
                if let Some(proxy) = native_proxy.as_mut() {
                    let receipt = proxy.shutdown().await;
                    if !receipt.joined || receipt.effects == EffectState::Uncertain {
                        return Err(Error::Unavailable(
                            "computer-use connector could not be joined; account custody is retained",
                        ));
                    }
                }
                auth::discard_unstarted_codex_auth(&store, &run, true)?;
                store.settle(&run, State::Failed, now_ms())?;
                return Err(error);
            }
        };
        return run_prepared(store, input, cancel, observer, launch, protocol, workspace).await;
    }
    if session.model.provider == Provider::Devin {
        return Err(Error::Unavailable("Devin support was removed; use Claude or Codex"));
    }
    if session.model.provider != Provider::Claude {
        return Err(Error::Unavailable(
            "native execution for this provider is not yet qualified",
        ));
    }
    let pin = Pin::load(store.root(), Provider::Claude)?;
    let run = store.prepare_run(&session.id, session.revision, now_ms())?;
    let tools = !input.pane_generation;
    let mut custody = PreparationCustody {
        store: &store,
        run: Some(&run),
        active: true,
    };
    let prepared = async {
        auth::refresh_claude_credentials(&store, &run, &pin, cancel.clone()).await?;
        let credential = auth::token(&store, &session.account)?;
        prepare(&pin, store.root(), &session.model, Some(&credential), tools).await
    }
    .await;
    custody.active = false;
    drop(custody);
    let mut launch = match prepared {
        Ok(launch) => launch,
        Err(error) => return Err(settle_failed_preparation(&store, Some(&run), error)),
    };
    launch.prepared_run = Some(run);
    let protocol = ClaudeProtocol::new(tools, launch.cwd.clone(), session.model.clone());
    run_prepared(store, input, cancel, observer, launch, protocol, workspace).await
}

pub(crate) async fn run_prepared<P: Protocol>(
    store: Arc<Store>,
    input: RunInput,
    mut cancel: watch::Receiver<bool>,
    observer: Observer,
    mut launch: Launch,
    mut protocol: P,
    workspace: Workspace,
) -> Result<Outcome> {
    let session = &input.session;
    let tools = !input.pane_generation;
    let prepared = match launch.prepared_run.take() {
        Some(run) => Ok(run),
        None => store.prepare_run(&session.id, session.revision, now_ms()),
    };
    let run = match prepared {
        Ok(run) => run,
        Err(error) => {
            if protocol.shutdown().await {
                launch.discard_unstarted().await;
            } else {
                launch.artifacts.retain_before_launch();
            }
            return Err(error);
        }
    };
    launch.artifacts.retain_before_launch();
    let bridge = launch.bridge.take();
    let mut process = match StreamProcess::spawn(launch.command) {
        Ok(process) => process,
        Err(error) => {
            // A protocol may own an active broker listener before its child
            // starts. Independently join every listener before no-child
            // evidence can release the account or disposable launch files.
            let protocol_joined = protocol.shutdown().await;
            let bridge_joined = close_bridge(bridge).await;
            if matches!(error, Error::LaunchNotStarted(_)) && protocol_joined && bridge_joined {
                if launch.codex_credentials.is_some() {
                    auth::discard_unstarted_codex_auth(&store, &run, true)?;
                }
                store.settle(&run, State::Failed, now_ms())?;
                launch.artifacts.release_after_join(true, EffectState::None);
            }
            return Err(error);
        }
    };
    let spawned = store.mark_spawned(&run, process.pid());
    let mut cancel_execution = cancel.clone();
    let mut effects = EffectState::None;
    let mut pending_attention = false;
    let mut quota_failure = None;
    let mut diagnostic = None;
    let mut answer = Answer::default();
    let mut thinking = String::new();
    let workspace = Arc::new(workspace);
    let mut commands = crate::command_tool::CommandTools::default();
    let mut commands_joined = true;
    let mut capabilities = None;
    let mut capability_handoff = false;
    // Velocity and quota observations are display meters, not custody: they
    // accumulate in memory during the stream and land in one fsync'd commit
    // per flush instead of one transaction per provider event.
    let mut pending_velocity: Vec<VelocitySample> = Vec::new();
    let mut pending_quota: Vec<PendingQuota> = Vec::new();
    let mut last_observation_flush_ms = 0u64;
    // One managed mailbox connection per worker run rather than one open
    // (and migration probe) per `xcb_*` tool call.
    let mut managed_bridge = None;
    // A transcript row and joined failure alone cannot prove delivery:
    // initialization or prompt submission itself may fail before it is sent.
    let mut prompt_submission = Some(false);
    let tool_calls = std::sync::atomic::AtomicU32::new(0);
    let execution = async {
        spawned?;
        if tools {
            capabilities = Some(crate::capabilities::CapabilityManager::new(
                input.config.capabilities.clone(),
                store.clone(),
                run.clone(),
                workspace.root().to_owned(),
            )?);
        }
        let baseline = store
            .velocities(&session.id, 0)?
            .last()
            .map(|point| point.output_tokens)
            .unwrap_or(0);
        let models = protocol.initialize(&mut process, "You are xcb (Excalibur), a local coding assistant. Use the declared workspace tools for project files and xcb_tools_list/xcb_tools_call for the host's enabled tools, including browser and computer use. Tools remain subject to the user's authorization and the host's approval controls. Before using an existing signed-in browser session, declare signed_in_browser with xcb_require_capability. For native desktop application control beyond browser-page tools, declare desktop. These requirements survive retries and hand the same task to Codex after this provider stops cleanly, without changing the user's scope or permissions. Ordinary public-page browsing, Playwright verification, and developing desktop software do not require that handoff. workspace_exec runs bounded offline Linux commands in an isolated staged workspace; host secrets, host dependency trees and build products are excluded. Supported repositories provide filtered read-only Git HEAD/index for status and diffs; source Git configuration, hooks, history and Git writes are unavailable. Use gitInspectionAvailable and gitUnavailable in the command result to check support. Only successful joined commands publish revision-checked changes. Native provider shell or arbitrary host paths are unavailable. Managed workers can use xcb_swarm_status, xcb_message_list and xcb_message_send for durable cross-provider coordination inside this workspace. Use xcb_backlog_list/get/add/update/complete to inspect, propose or report already-completed deferred work for your project. xcb_memory_recent supplies bounded recent work summaries; xcb_memory_search retrieves cited historical knowledge from the explicitly bound Wordcell vault. Only the host can admit proposed follow-ups under a user-delegated project grant. Proposals never expand that grant or release work themselves. Always end with a concise work summary, checks and remaining blockers; the harness records it in work history. Recent summaries are historical reports and must be revalidated before relying on changing facts. Direct sessions have no managed mailbox or backlog. Keep file revisions and use expectedRevision when writing. Never claim effects you did not perform. Ask for human input when it is necessary.").await?;
        // An empty catalog is not evidence that every model was withdrawn
        // (metadata probes refuse it too), so keep the stored catalog.
        if models.is_empty() {
            return Err(Error::Unavailable(EMPTY_CATALOG));
        }
        // Retain fresh discovery even when a cached selection has disappeared.
        // The failed turn still cannot start or silently choose another model.
        // The list is this account's own: another account's plan may differ.
        if protocol.refreshes_catalog() {
            store.set_account_models(&session.account, &models)?;
        }
        if !models.iter().any(|choice| {
            choice.id == session.model.id
                && (session.model.effort.is_none() || choice.effort == session.model.effort)
        }) {
            return Err(Error::Unavailable(STALE_MODEL));
        }
        let (email, plan) = protocol.account_identity();
        if email.is_some() || plan.is_some() {
            store.set_account_identity(&session.account, email, plan)?;
        }
        let history = store
            .messages(&session.id, 512)?
            .into_iter()
            .filter(|message| message.id != input.message.id)
            .collect::<Vec<_>>();
        let context_judge = if input.pane_generation {
            None
        } else {
            match judge::resolve(store.root(), &input.config.extensions.judge) {
                Ok(judge) => judge,
                Err(error) => {
                    observer(Progress::Notice(format!(
                        "Judge compaction unavailable ({error}); using deterministic Gobstopper"
                    )));
                    None
                }
            }
        };
        let projection = match context::project(
            session,
            &history,
            &input.message.text,
            &input.config.extensions.gobstopper,
            context_judge.as_deref(),
        )
        .await
        {
            Ok(projection) => projection,
            Err(error) if context_judge.is_some() => {
                observer(Progress::Notice(format!(
                    "Judge compaction unavailable ({error}); using deterministic Gobstopper"
                )));
                context::project(
                    session,
                    &history,
                    &input.message.text,
                    &input.config.extensions.gobstopper,
                    None,
                )
                .await?
            }
            Err(error) => return Err(error),
        };
        if projection.elided > 0 {
            observer(Progress::Notice(format!(
                "Gobstopper elided {} stale tool outputs in the prompt; history is retained.",
                projection.elided
            )));
        }
        let text = if input.pane_generation {
            input.message.text.clone()
        } else {
            context::prompt(&projection.messages, &input.message.text)?
        };
        let mut images = Vec::new();
        for image in &input.message.attachments {
            let bytes = attachments::read(store.root(), image)?;
            images.push(ImageInput {
                media_type: image.media_type.clone(),
                base64: base64::engine::general_purpose::STANDARD.encode(bytes),
            });
        }
        // Once start is attempted, failure or cancellation cannot establish
        // whether its transport partially delivered the request.
        prompt_submission = None;
        protocol
            .start(&mut process, Prompt { text, images })
            .await?;
        prompt_submission = Some(true);
        let started = now_ms();
        if input.config.extensions.usage {
            pending_velocity.push(VelocitySample {
                at_ms: started,
                output_tokens: baseline,
            });
        }
        let mut admitted = false;
        let mut seen_calls = BTreeSet::new();
        let mut byte_count = 0usize;
        let mut output_tokens = 0u64;
        // Velocity is a display meter; the authoritative usage lands via
        // record_usage at settle. Per-delta fsync'd transactions would
        // serialize every parallel terminal on one writer, so stream samples
        // are decimated and the true total is written once at the result.
        let mut last_velocity_ms = started;
        let mut frames = 0usize;
        loop {
            if *cancel.borrow() {
                return Ok((Terminal::Cancelled, vec![]));
            }
            if protocol.host_pending_attention()
                || capabilities
                    .as_ref()
                    .is_some_and(|manager| manager.policy_denied())
            {
                pending_attention = true;
                quota_failure = Some(Failure::Policy);
                observer(Progress::Notice(
                    "A browser or computer action needs explicit approval; automatic work has stopped.".into(),
                ));
                return Ok((Terminal::Failed, vec![]));
            }
            if frames >= crate::protocol::MAX_TURN_FRAMES {
                // Volume is not a protocol violation: the tools that already
                // ran are settled and the partial answer is kept. The turn
                // ends like any other provider turn limit.
                let detail = Diagnostic::notice(
                    "provider frame count reached the per-turn limit; the turn was ended as a turn limit",
                );
                observer(Progress::Notice(detail.as_str().to_owned()));
                diagnostic = Some(detail);
                return Ok((Terminal::TurnLimit, vec![]));
            }
            frames += 1;
            let batch = tokio::select! {
                _ = cancel.changed() => return Ok((Terminal::Cancelled, vec![])),
                batch = protocol.next(&mut process) => batch?,
            };
            byte_count = byte_count
                .checked_add(batch.bytes)
                .ok_or(Error::Protocol("total provider output limit"))?;
            if byte_count > 64 * 1024 * 1024 {
                return Err(Error::Protocol("total provider output limit"));
            }
            for event in batch.events {
                if *cancel.borrow() {
                    return Ok((Terminal::Cancelled, vec![]));
                }
                if protocol.host_pending_attention() {
                    pending_attention = true;
                    quota_failure = Some(Failure::Policy);
                    observer(Progress::Notice(
                        "Automatic approval review stopped a computer action; explicit approval is required.".into(),
                    ));
                    return Ok((Terminal::Failed, vec![]));
                }
                match event {
                    TurnEvent::Diagnostic(detail) => {
                        observer(Progress::Notice(detail.as_str().to_owned()));
                        diagnostic = Some(detail);
                    }
                    TurnEvent::Ready => {
                        if admitted {
                            return Err(Error::Protocol("duplicate initialization"));
                        }
                        admitted = true;
                    }
                    TurnEvent::OutputTokens(total) if admitted => {
                        if total < output_tokens || total > xcb_core::usage::COUNTER_LIMIT {
                            return Err(Error::Protocol("stream token counter"));
                        }
                        output_tokens = total;
                        let now = now_ms();
                        if input.config.extensions.usage
                            && now.saturating_sub(last_velocity_ms) >= 250
                        {
                            last_velocity_ms = now;
                            pending_velocity.push(VelocitySample {
                                at_ms: now,
                                output_tokens: baseline.saturating_add(output_tokens),
                            });
                        }
                        if now.saturating_sub(last_observation_flush_ms) >= OBSERVATION_FLUSH_MS {
                            last_observation_flush_ms = now;
                            store.record_observations(
                                &run,
                                &session.id,
                                &pending_velocity,
                                &pending_quota,
                            )?;
                            pending_velocity.clear();
                            pending_quota.clear();
                        }
                    }
                    TurnEvent::Delta {
                        thinking: is_thinking,
                        text,
                    } if admitted => {
                        if is_thinking {
                            if thinking.len() + text.len() > MAX_TEXT_BYTES {
                                return Err(Error::Protocol("thinking limit"));
                            }
                            thinking.push_str(&text);
                        } else {
                            answer.delta(&text)?;
                        }
                        observer(Progress::Text {
                            thinking: is_thinking,
                            text,
                        });
                    }
                    TurnEvent::Assistant(text) if admitted => answer.completed(text)?,
                    TurnEvent::Attention => {
                        pending_attention = true;
                        quota_failure = Some(Failure::Policy);
                        return Ok((Terminal::Failed, vec![]));
                    }
                    // Quota is an observation, not provider work: an account
                    // rejected before admission still settles as a quota or
                    // authentication failure rather than an unknown one.
                    TurnEvent::Quota {
                        window,
                        used_percent,
                        resets_at_ms,
                        failure,
                    } => {
                        // A subsequent quota meter update is not evidence
                        // that an explicit provider failure was rescinded.
                        quota_failure = failure.or(quota_failure);
                        if let (Some(window), Some(used_percent), Some(resets_at_ms)) =
                            (window, used_percent, resets_at_ms)
                        {
                            pending_quota.push(PendingQuota {
                                window: Id::new(window)?,
                                used_percent,
                                observed_at_ms: now_ms(),
                                resets_at_ms,
                            });
                        }
                    }
                    TurnEvent::Tool {
                        id: call_id,
                        name,
                        arguments,
                    } if admitted && tools => {
                        if !seen_calls.insert(call_id.clone()) {
                            return Err(Error::Protocol("duplicate tool call identifier"));
                        }
                        // The Codex adapter already admits at most MAX_CALLS
                        // tool calls per turn; this cap only backstops other
                        // providers against an unbounded unique-call storm.
                        if seen_calls.len() > 1024 {
                            return Err(Error::Protocol("tool call limit exceeded"));
                        }
                        tool_calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        store.begin_tool(
                            &run,
                            &call_id,
                            &name,
                            &digest(serde_json::to_vec(&arguments).unwrap_or_default()),
                        )?;
                        observer(Progress::Tool(name.clone()));
                        if *cancel.borrow() {
                            // The tool was announced, but cancellation arrived
                            // before its effect boundary. Settle its intent and
                            // do not execute buffered work after cancellation.
                            settle_tool_effects(
                                &store,
                                &run,
                                &call_id,
                                EffectState::None,
                                &mut effects,
                            )?;
                            return Ok((Terminal::Cancelled, vec![]));
                        }
                        // `settle` stays None for workspace_exec: the command
                        // tool settles its own receipt inside finish_command.
                        // Every other tool settles its receipt together with
                        // the transcript append in one durable transaction.
                        let (output, settle) = if name == "workspace_exec" {
                            match commands
                                .start(
                                    store.clone(),
                                    run.clone(),
                                    workspace.clone(),
                                    call_id.clone(),
                                    &arguments,
                                )
                                .await
                            {
                                Ok(()) => {
                                    let command = commands.wait().await;
                                    commands_joined &= command.joined;
                                    effects = combine_effects(effects, command.effects);
                                    if !command.joined {
                                        return Err(Error::Unavailable(
                                            "command stop is unproven; account custody retained",
                                        ));
                                    }
                                    (command.output, None)
                                }
                                Err(error) => {
                                    settle_tool_effects(
                                        &store,
                                        &run,
                                        &call_id,
                                        EffectState::None,
                                        &mut effects,
                                    )?;
                                    (Err(error), None)
                                }
                            }
                        } else if name == "xcb_tools_image" {
                            (
                                crate::tool_output::reopen(&store, &session.id, &arguments),
                                Some(EffectState::None),
                            )
                        } else if name == "xcb_tools_list" {
                            let result =
                                match serde_json::from_value::<CapabilityList>(arguments.clone()) {
                                    Ok(request) => {
                                        capabilities
                                            .as_mut()
                                            .expect("tools enabled")
                                            .list(request.server.as_deref())
                                            .await
                                    }
                                    Err(error) => Err(error.into()),
                                };
                            (result, Some(EffectState::None))
                        } else if name == "xcb_tools_call" {
                            match serde_json::from_value::<CapabilityCall>(arguments.clone()) {
                                Ok(request) => {
                                    let result = capabilities
                                        .as_mut()
                                        .expect("tools enabled")
                                        .call(&request.server, &request.tool, request.arguments)
                                        .await;
                                    (result.result, Some(result.effects))
                                }
                                Err(error) => (Err(error.into()), Some(EffectState::None)),
                            }
                        } else if name == "xcb_require_capability" {
                            let result = match requested_computer_capability(&arguments) {
                                Ok((capability, requirements)) => {
                                    store
                                        .require_session_capabilities(&session.id, requirements)?;
                                    capability_handoff =
                                        !requirements.allows(session.model.provider);
                                    input.config.capabilities.require_native_task(requirements).map(|()| json!({"required":capability, "handoff":capability_handoff}))
                                }
                                Err(error) => Err(error),
                            };
                            (result, Some(EffectState::None))
                        } else if name.starts_with("xcb_") {
                            let (output, call_effects) = managed_tool_call(
                                &store,
                                &mut managed_bridge,
                                &session.id,
                                &format!("{}:{call_id}", run.id),
                                &name,
                                &arguments,
                            )
                            .await;
                            (output, Some(call_effects))
                        } else {
                            // Workspace tools do bounded filesystem and SQLite
                            // work on a blocking thread; a dropped provider
                            // wait still settles this receipt via
                            // cancel_and_join.
                            let (output, call_effects) = commands
                                .workspace_call(
                                    store.clone(),
                                    run.clone(),
                                    call_id.clone(),
                                    workspace.clone(),
                                    name.clone(),
                                    arguments.clone(),
                                )
                                .await;
                            (output, Some(call_effects))
                        };
                        // Result shaping never aborts the turn: legal tool
                        // output that cannot ride the transcript bound is a
                        // tool rejection with guidance, not a protocol failure.
                        let prepared = crate::tool_output::prepare(
                            store.root(),
                            output,
                            name == "xcb_tools_call" || name == "xcb_tools_image",
                        );
                        let text = prepared.text;
                        // The durable transcript carries display-safe text;
                        // the provider still receives the exact result.
                        let message = Message {
                            id: new_id("tool"),
                            role: Role::Tool,
                            text: xcb_core::display_text(
                                &format!("{name}: {text}"),
                                MAX_TEXT_BYTES,
                            ),
                            at_ms: now_ms(),
                            attachments: prepared.attachments,
                            provenance: Some(MessageProvenance {
                                account: session.account.clone(),
                                model: session.model.clone(),
                                run: Some(run.id.clone()),
                            }),
                        };
                        if let Some(call_effects) = settle {
                            let previous = effects;
                            // A receipt write can fail after the tool ran.
                            // Until that receipt is durable, retain custody
                            // even when the filesystem result was known.
                            effects = EffectState::Uncertain;
                            if call_effects == EffectState::Uncertain {
                                store.append_tool_message(&session.id, &message)?;
                            } else {
                                store.settle_tool_and_append(
                                    &run,
                                    &call_id,
                                    &session.id,
                                    &message,
                                )?;
                                effects = combine_effects(previous, call_effects);
                            }
                        } else {
                            store.append_tool_message(&session.id, &message)?;
                        }
                        protocol
                            .reply(&mut process, &call_id, prepared.reply)
                            .await?;
                        if capabilities
                            .as_ref()
                            .is_some_and(|manager| manager.policy_denied())
                        {
                            pending_attention = true;
                            quota_failure = Some(Failure::Policy);
                            observer(Progress::Notice(
                                "The browser denied this action; automatic work has stopped for explicit approval.".into(),
                            ));
                            return Ok((Terminal::Failed, vec![]));
                        }
                        if capability_handoff {
                            answer.completed(
                                "This task requires Codex computer tools; switching to Codex."
                                    .into(),
                            )?;
                            return Ok((Terminal::TurnLimit, vec![]));
                        }
                    }
                    TurnEvent::Result {
                        terminal,
                        text,
                        models,
                    } if admitted => {
                        if !text.is_empty() {
                            answer.completed(text)?;
                        }
                        if input.config.extensions.usage {
                            pending_velocity.push(VelocitySample {
                                at_ms: now_ms(),
                                output_tokens: baseline.saturating_add(output_tokens),
                            });
                        }
                        return Ok((terminal, models));
                    }
                    _ => {
                        return Err(Error::Protocol(
                            "provider work before effective-boundary admission",
                        ));
                    }
                }
            }
        }
    };
    let deadline = Duration::from_millis(input.config.turn_timeout_ms);
    let (cancelled, mut result) = tokio::select! {
        biased;
        _ = async { if !*cancel_execution.borrow() { let _ = cancel_execution.changed().await; } } =>
            (true, Ok(Ok((Terminal::Cancelled, vec![])))),
        result = tokio::time::timeout(deadline, execution) => (false, result),
    };
    if cancelled {
        // Cooperative interruption first: a provider that understands it can
        // settle the turn and exit inside the grace period below; everything
        // else still gets the same bounded stdin-close grace and kill.
        if let Some(frame) = protocol.interruption() {
            let _ = process.send(&frame).await;
        }
    }
    // Everything the turn observed lands in one commit here — completion,
    // cancellation, limit and error paths alike. A write failure fails the
    // turn exactly as the per-event record calls it replaces.
    if let Err(error) =
        store.record_observations(&run, &session.id, &pending_velocity, &pending_quota)
        && result.is_ok()
    {
        result = Ok(Err(error));
    }
    // Cancellation drops only the execution future. Never cancel independent
    // process/listener joins or credential persistence and custody settlement.
    if let Some(command) = commands.cancel_and_join().await {
        commands_joined &= command.joined;
        effects = combine_effects(effects, command.effects);
    }
    let process_joined = if cancelled {
        process.join_graceful(CANCEL_GRACE).await
    } else {
        process.join().await
    };
    if let Some(bytes) = process.stderr_overflow() {
        // Volume is reported, never retained: provider stderr is not a
        // bounded host diagnostic and may carry credentials or paths.
        observer(Progress::Notice(format!(
            "provider wrote {bytes} bytes to stderr (notice threshold {} bytes); the stream was drained to EOF but not retained",
            crate::process::STDERR_NOTICE_BYTES
        )));
    }
    let capabilities_joined = if let Some(manager) = capabilities.as_mut() {
        let joined = manager.shutdown().await;
        effects = combine_effects(effects, manager.effects());
        pending_attention |= manager.policy_denied();
        joined
    } else {
        true
    };
    let protocol_joined = protocol.shutdown().await;
    effects = combine_effects(effects, protocol.host_effects());
    pending_attention |= protocol.host_pending_attention();
    let bridge_joined = close_bridge(bridge).await;
    let joined = process_joined
        && protocol_joined
        && bridge_joined
        && commands_joined
        && capabilities_joined;
    let (terminal, models, failure) = match result {
        Ok(Ok((terminal, models))) => (
            terminal,
            models,
            if terminal == Terminal::Failed {
                quota_failure.or(Some(Failure::Unknown))
            } else {
                None
            },
        ),
        Ok(Err(error)) => {
            let detail = Diagnostic::from_error(&error);
            observer(Progress::Notice(detail.as_str().to_owned()));
            diagnostic = Some(detail);
            // The codec's fixed category names the account failure; an
            // earlier explicit provider rejection is the next best evidence.
            let failure = match error.failure() {
                Failure::Unknown => quota_failure.unwrap_or(Failure::Unknown),
                failure => failure,
            };
            (Terminal::Failed, vec![], Some(failure))
        }
        Err(_) => {
            let detail = Diagnostic::from_error(&Error::Unavailable("provider deadline reached"));
            observer(Progress::Notice(detail.as_str().to_owned()));
            diagnostic = Some(detail);
            (Terminal::Failed, vec![], Some(Failure::Transport))
        }
    };
    let final_text = answer.into_text();
    let mut facts = TurnFacts {
        terminal,
        joined,
        effects,
        pending_attention,
        failure,
    };
    let state = classify(&final_text, &facts);
    let text_attention = !facts.pending_attention && state.attention();
    facts.pending_attention |= state.attention();
    let outcome = Outcome {
        tool_calls: Some(tool_calls.load(std::sync::atomic::Ordering::Relaxed)),
        text_attention,
        text: final_text.clone(),
        facts,
        state,
        diagnostic: if terminal == Terminal::Completed {
            None
        } else {
            diagnostic
        },
    };
    if joined {
        if !thinking.is_empty() {
            let current = store
                .session(&session.id)?
                .ok_or(Error::Unavailable("session not found"))?;
            store.append_message(
                &session.id,
                current.revision,
                &Message {
                    id: new_id("thinking"),
                    role: Role::Thinking,
                    text: thinking,
                    at_ms: now_ms(),
                    attachments: vec![],
                    provenance: Some(MessageProvenance {
                        account: session.account.clone(),
                        model: session.model.clone(),
                        run: Some(run.id.clone()),
                    }),
                },
            )?;
        }
        if !final_text.is_empty() {
            let current = store
                .session(&session.id)?
                .ok_or(Error::Unavailable("session not found"))?;
            store.append_message(
                &session.id,
                current.revision,
                &Message {
                    id: new_id("assistant"),
                    role: Role::Assistant,
                    text: final_text.clone(),
                    at_ms: now_ms(),
                    attachments: vec![],
                    provenance: Some(MessageProvenance {
                        account: session.account.clone(),
                        model: session.model.clone(),
                        run: Some(run.id.clone()),
                    }),
                },
            )?;
        }
        if input.config.extensions.usage {
            for (index, (id, counters)) in models.into_iter().enumerate() {
                let model = ModelChoice {
                    id: Id::new(id.clone())?,
                    label: id,
                    ..session.model.clone()
                };
                store.record_usage(&UsageObservation {
                    id: Id::new(format!("{}_{index}", run.id))?,
                    session: session.id.clone(),
                    account: session.account.clone(),
                    model,
                    counters,
                    at_ms: now_ms(),
                })?;
            }
        }
        if let Some(credentials) = &launch.codex_credentials {
            auth::persist_codex_auth(&store, &run, credentials, joined)?;
        }
        if effects != EffectState::Uncertain {
            // The provider refused this account's quota but reported no reset
            // (Codex's `usageLimitExceeded` and Devin's resource-exhaustion
            // error carry none; Claude's rejection may omit one), so the
            // meters recorded above leave no block: without a cooldown the
            // account would look ready and be routed to again. Recorded under
            // this run's hold, and only from a settled provider outcome; a
            // model-scoped limit stays with the run's outcome because
            // admission is per account.
            if outcome.facts.terminal == Terminal::Failed
                && outcome.facts.failure == Some(Failure::AccountQuota)
            {
                let now = now_ms();
                let recorded =
                    store
                        .quota_blocked_until(&session.account, now)
                        .and_then(|blocked| match blocked {
                            Some(_) => Ok(()),
                            None => store.record_quota_limit(
                                &run,
                                now,
                                now.saturating_add(input.config.quota_limit_cooldown_ms),
                            ),
                        });
                // The cooldown is routing bookkeeping, not custody: a failed
                // write is reported and the settled outcome still lands.
                if let Err(error) = recorded {
                    observer(Progress::Notice(format!(
                        "usage limit cooldown was not recorded: {}",
                        Diagnostic::from_error(&error).as_str()
                    )));
                }
            }
            store.settle_outcome_submitted(
                &run,
                &input.message.id,
                &outcome,
                prompt_submission,
                now_ms(),
            )?;
            launch.artifacts.release_after_join(joined, effects);
        }
    }
    Ok(outcome)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn desktop_capability_declaration_is_exact_and_cannot_set_host_route_facts() {
        let (_, desktop) = requested_computer_capability(&json!({"capability":"desktop"})).unwrap();
        assert!(desktop.desktop && !desktop.signed_in_browser && !desktop.codex_native);
        for invalid in [
            json!({"capability":"codex_native"}),
            json!({"capability":"desktop","approval":true}),
            json!({"capability":["desktop"]}),
            json!({}),
        ] {
            assert!(requested_computer_capability(&invalid).is_err());
        }
    }

    /// A supported Claude build becomes selectable only after the same
    /// host-specific sandbox checks required by the Linux launcher pass.
    /// The receipt here is synthetic test state; no provider is launched.
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_claude_admission_requires_matching_sandbox_evidence() {
        use crate::qualification::{
            LINUX_QUALIFICATION_NAME, LinuxQualification, MAX_RECEIPT_AGE_MS,
        };

        let root = tempfile::tempdir().unwrap();
        let root = xcb_core::canonical(root.path()).unwrap();
        let pin = Pin {
            provider: Provider::Claude,
            executable: root.join("synthetic-provider"),
            sha256: "1".repeat(64),
            version: claude::MIN_VERSION.into(),
            host_sha256: "2".repeat(64),
            observed_at_ms: now_ms(),
        };
        assert!(!provider_admitted(&root, &pin), "missing evidence");
        let Some(candidate) = sandbox::bwrap_candidate() else {
            // A host without bubblewrap stays unsupported. CI installs
            // it so the positive and changed-evidence cases run there.
            assert!(
                std::env::var_os("CI").is_none(),
                "Linux CI must install bubblewrap for the admission regression"
            );
            return;
        };
        let wrapper = sandbox::BwrapPin::admit(&candidate).unwrap();
        let mut receipt: LinuxQualification =
            serde_json::from_str(include_str!("../tests/fixtures/linux-qualification.json"))
                .unwrap();
        receipt.wrapper.path = candidate;
        receipt.wrapper.sha256 = wrapper.sha256;
        receipt.observed_at_ms = now_ms();
        receipt.namespaces = crate::qualification::Namespaces::live();
        let path = root.join(LINUX_QUALIFICATION_NAME);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let write = |receipt: &LinuxQualification| {
            std::fs::write(&path, serde_json::to_vec(receipt).unwrap()).unwrap();
        };
        write(&receipt);
        let qualified = sandbox::linux_sandbox(&root).qualified;
        assert_eq!(provider_admitted(&root, &pin), qualified);
        if std::env::var_os("CI").is_some() {
            assert!(
                qualified,
                "Linux CI must exercise supported Claude selection"
            );
        }
        let mut old_provider = pin.clone();
        old_provider.version = "1.0.0".into();
        assert!(!provider_admitted(&root, &old_provider));
        for provider in [Provider::Codex, Provider::Devin] {
            let mut unsupported = pin.clone();
            unsupported.provider = provider;
            assert!(!provider_admitted(&root, &unsupported));
        }
        receipt.observed_at_ms = now_ms().saturating_sub(MAX_RECEIPT_AGE_MS + 1000);
        write(&receipt);
        assert!(!provider_admitted(&root, &pin), "stale evidence");
        receipt.observed_at_ms = now_ms();
        receipt.wrapper.sha256 = "3".repeat(64);
        write(&receipt);
        assert!(!provider_admitted(&root, &pin), "a replaced wrapper");
    }

    #[test]
    fn diagnostic_is_legacy_compatible_and_bounded() {
        let original = Outcome {
            tool_calls: Some(0),
            text_attention: false,
            text: String::new(),
            facts: TurnFacts {
                terminal: Terminal::Failed,
                joined: true,
                effects: EffectState::None,
                pending_attention: false,
                failure: Some(Failure::Unknown),
            },
            state: State::Failed,
            diagnostic: None,
        };
        let mut legacy = serde_json::to_value(original).unwrap();
        assert!(legacy.get("diagnostic").is_none());
        assert!(
            serde_json::from_value::<Outcome>(legacy.clone())
                .unwrap()
                .diagnostic
                .is_none()
        );
        for text in [String::new(), "d".repeat(513), "unsafe\ntext".into()] {
            legacy["diagnostic"] = json!(text);
            assert!(serde_json::from_value::<Outcome>(legacy.clone()).is_err());
        }
        legacy["diagnostic"] = json!("d".repeat(512));
        assert_eq!(
            serde_json::from_value::<Outcome>(legacy)
                .unwrap()
                .diagnostic
                .unwrap()
                .as_str()
                .len(),
            512
        );
    }

    #[test]
    fn diagnostic_never_copies_external_error_payloads() {
        let secret = "/private/account/auth.json token=secret-provider-payload\u{1b}[31m";
        for error in [
            Error::Io(std::io::Error::other(secret)),
            Error::LaunchNotStarted(std::io::Error::other(secret)),
            Error::Database(rusqlite::Error::InvalidParameterName(secret.into())),
        ] {
            assert!(error.to_string().contains(secret));
            let diagnostic = Diagnostic::from_error(&error);
            assert!(!diagnostic.as_str().contains("/private"));
            assert!(!diagnostic.as_str().contains("token="));
            assert!(!diagnostic.as_str().contains("secret-provider-payload"));
            assert!(!diagnostic.as_str().chars().any(char::is_control));
        }
        assert_eq!(
            Diagnostic::from_error(&Error::Protocol("fixture failure")).as_str(),
            "provider protocol error: fixture failure"
        );
        assert_eq!(
            Diagnostic::from_error(&Error::CodexRpc {
                method: "turn/start",
                code: -32000,
                category: "permission denied"
            })
            .as_str(),
            "Codex turn/start failed (RPC -32000): permission denied"
        );
    }

    fn file(path: &Path, mode: u32) {
        let mut created = std::fs::File::create(path).unwrap();
        created.write_all(b"artifact").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    #[test]
    fn claude_preparation_drop_does_not_discard_unsettled_refresh_receipts() {
        for receipt in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let store =
                Store::open(&xcb_core::canonical(directory.path()).unwrap().join("state")).unwrap();
            let account = store
                .add_account(Provider::Claude, "Test", 1, None)
                .unwrap();
            let run = store.prepare_probe(&account.id, None, 2).unwrap();
            if receipt {
                store
                    .begin_tool(
                        &run,
                        "xcb_auth_runner_fixture",
                        "host_auth_refresh",
                        "fixture",
                    )
                    .unwrap();
            }
            drop(PreparationCustody {
                store: &store,
                run: Some(&run),
                active: true,
            });
            assert_eq!(
                store.run(&run.id).unwrap().unwrap().phase == "settled",
                !receipt
            );
        }
    }

    #[test]
    fn claude_auth_unproven_preparation_preserves_error_and_lease() {
        let directory = tempfile::tempdir().unwrap();
        let store =
            Store::open(&xcb_core::canonical(directory.path()).unwrap().join("state")).unwrap();
        let account = store
            .add_account(Provider::Claude, "Test", 1, None)
            .unwrap();
        let run = store.prepare_probe(&account.id, None, 2).unwrap();
        let error = settle_failed_preparation(
            &store,
            Some(&run),
            Error::AuthUnproven("safe static diagnosis"),
        );
        assert!(error.is_cleanup_unproven());
        assert!(matches!(
            error,
            Error::AuthUnproven("safe static diagnosis")
        ));
        assert_eq!(
            Diagnostic::from_error(&error).as_str(),
            "safe static diagnosis"
        );
        assert_eq!(store.unsettled_runs().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn unstarted_process_releases_only_after_complete_bridge_stop() {
        for interfere_with_socket in [false, true] {
            // Keep Unix socket paths below the platform's small length bound.
            let root = tempfile::tempdir_in("/tmp").unwrap();
            let base = xcb_core::canonical(root.path()).unwrap();
            let store = Store::open(&base.join("state")).unwrap();
            let account = store
                .add_account(Provider::Claude, "Test", 1, None)
                .unwrap();
            let run = store.prepare_probe(&account.id, None, 2).unwrap();
            let mut artifacts = LaunchArtifacts::create(&base).unwrap();
            let directory = artifacts.directory.clone();
            let socket = directory.join("egress.sock");
            artifacts.retain_before_launch();
            let bridge =
                egress::EgressBridge::start(egress::EgressBridgeOptions::new(socket.clone()))
                    .await
                    .unwrap();
            if interfere_with_socket {
                std::fs::remove_file(&socket).unwrap();
                std::fs::create_dir(&socket).unwrap();
            }
            let result = spawn_process(
                &store,
                Some(&run),
                Command::new(base.join("missing-executable")),
                &mut artifacts,
                Some(bridge),
                false,
            )
            .await;
            assert!(matches!(result, Err(Error::LaunchNotStarted(_))));
            drop(artifacts);
            assert_eq!(directory.exists(), interfere_with_socket);
            assert_eq!(
                store.prepare_probe(&account.id, None, 3).is_err(),
                interfere_with_socket
            );
            assert_eq!(
                store.run(&run.id).unwrap().unwrap().phase == "settled",
                !interfere_with_socket
            );
        }
    }

    #[tokio::test]
    async fn preparation_failure_requires_bridge_stop_before_releasing_account() {
        for interfere_with_socket in [false, true] {
            let root = tempfile::tempdir_in("/tmp").unwrap();
            let base = xcb_core::canonical(root.path()).unwrap();
            let store = Store::open(&base.join("state")).unwrap();
            let account = store
                .add_account(Provider::Claude, "Test", 1, None)
                .unwrap();
            let run = store.prepare_probe(&account.id, None, 2).unwrap();
            let mut artifacts = LaunchArtifacts::create(&base).unwrap();
            let directory = artifacts.directory.clone();
            let socket = directory.join("egress.sock");
            artifacts.retain_before_launch();
            let bridge =
                egress::EgressBridge::start(egress::EgressBridgeOptions::new(socket.clone()))
                    .await
                    .unwrap();
            if interfere_with_socket {
                std::fs::remove_file(&socket).unwrap();
                std::fs::create_dir(&socket).unwrap();
            }
            let error = discard_failed_preparation(
                &mut artifacts,
                bridge,
                Error::Unavailable("synthetic planning failure"),
            )
            .await;
            let error = settle_failed_preparation(&store, Some(&run), error);
            assert_eq!(
                matches!(error, Error::CleanupUnproven),
                interfere_with_socket
            );
            drop(artifacts);
            assert_eq!(directory.exists(), interfere_with_socket);
            assert_eq!(
                store.unsettled_runs().unwrap().len(),
                usize::from(interfere_with_socket)
            );
            assert_eq!(
                store.prepare_probe(&account.id, None, 3).is_err(),
                interfere_with_socket
            );
        }
    }

    #[tokio::test]
    async fn rejected_launch_closes_its_bridge_before_removing_artifacts() {
        let root = tempfile::tempdir_in("/tmp").unwrap();
        let base = xcb_core::canonical(root.path()).unwrap();
        let mut artifacts = LaunchArtifacts::create(&base).unwrap();
        let directory = artifacts.directory.clone();
        artifacts.retain_before_launch();
        let bridge = egress::EgressBridge::start(egress::EgressBridgeOptions::new(
            directory.join("egress.sock"),
        ))
        .await
        .unwrap();
        let mut launch = Launch {
            command: Command::new(base.join("unused")),
            cwd: base,
            bridge: Some(bridge),
            artifacts,
            prepared_run: None,
            codex_credentials: None,
        };
        launch.discard_unstarted().await;
        drop(launch);
        assert!(!directory.exists());
    }

    #[test]
    fn launch_artifacts_require_join_and_settled_effects_after_spawn() {
        let root = tempfile::tempdir().unwrap();
        let base = xcb_core::canonical(root.path()).unwrap();
        for (spawned, joined, effects, retained) in [
            (false, false, EffectState::None, false),
            (true, false, EffectState::None, true),
            (true, true, EffectState::Uncertain, true),
            (true, true, EffectState::Settled, false),
            (true, true, EffectState::None, false),
        ] {
            let mut artifacts = LaunchArtifacts::create(&base).unwrap();
            let path = artifacts.directory.clone();
            file(&path.join("provider"), 0o500);
            if spawned {
                artifacts.retain_before_launch();
                artifacts.release_after_join(joined, effects);
            }
            drop(artifacts);
            assert_eq!(path.exists(), retained);
        }
    }

    #[test]
    fn artifact_cleanup_preserves_a_replaced_directory() {
        let root = tempfile::tempdir().unwrap();
        let base = xcb_core::canonical(root.path()).unwrap();
        let artifacts = LaunchArtifacts::create(&base).unwrap();
        let path = artifacts.directory.clone();
        std::fs::rename(&path, base.join("original")).unwrap();
        private::directory(&path).unwrap();
        file(&path.join("other-run"), 0o600);
        drop(artifacts);
        assert!(path.join("other-run").exists());
    }

    #[test]
    fn artifact_cleanup_preserves_a_replaced_owner_lock() {
        let root = tempfile::tempdir().unwrap();
        let base = xcb_core::canonical(root.path()).unwrap();
        let artifacts = LaunchArtifacts::create(&base).unwrap();
        let path = artifacts.directory.clone();
        let lock_path = path.join(LAUNCH_OWNER_LOCK);
        std::fs::rename(&lock_path, path.join("previous-owner.lock")).unwrap();
        let _replacement = open_owner_lock(&lock_path).unwrap();
        drop(artifacts);
        assert!(
            path.exists(),
            "failed identity proof must prevent Drop cleanup"
        );
        assert!(!reclamation_receipt(&path).exists());
    }

    #[test]
    fn interrupted_answers_preserve_streamed_text_without_duplicates() {
        let mut answer = Answer::default();
        answer.delta("first ").unwrap();
        answer.delta("part").unwrap();
        assert_eq!(answer.into_text(), "first part");

        let mut answer = Answer::default();
        answer.delta("draft").unwrap();
        answer.completed("authoritative result".into()).unwrap();
        assert_eq!(answer.into_text(), "authoritative result");

        let mut answer = Answer::default();
        answer.completed("prior tool explanation".into()).unwrap();
        answer.delta("new partial answer").unwrap();
        assert_eq!(answer.into_text(), "new partial answer");
    }

    #[test]
    fn interrupted_answers_keep_the_last_bounded_prefix() {
        let mut answer = Answer::default();
        answer.delta(&"x".repeat(MAX_TEXT_BYTES)).unwrap();
        assert!(answer.delta("overflow").is_err());
        assert_eq!(answer.into_text().len(), MAX_TEXT_BYTES);
    }

    #[test]
    fn rejected_workspace_write_settles_receipt_and_releases_account() {
        let root = tempfile::tempdir().unwrap();
        let base = xcb_core::canonical(root.path()).unwrap();
        let workspace_root = base.join("work");
        std::fs::create_dir(&workspace_root).unwrap();
        std::fs::write(workspace_root.join("file"), "current").unwrap();
        let workspace =
            Workspace::open_with_coordination(&workspace_root, &base.join("coordination")).unwrap();
        let store = Store::open(&base.join("state")).unwrap();
        let account = store
            .add_account(Provider::Claude, "Test", 1, None)
            .unwrap();
        let run = store.prepare_probe(&account.id, None, 2).unwrap();
        let arguments = json!({"path":"file","text":"clobber","expectedRevision":digest("stale")});
        store
            .begin_tool(
                &run,
                "rejected-write",
                "workspace_write",
                &digest(arguments.to_string()),
            )
            .unwrap();
        let (result, call_effects) = workspace.call_observed("workspace_write", &arguments);
        assert!(result.is_err());
        let mut effects = EffectState::None;
        settle_tool_effects(&store, &run, "rejected-write", call_effects, &mut effects).unwrap();
        assert_eq!(effects, EffectState::None);
        store.settle(&run, State::Idle, 3).unwrap();
        assert!(store.prepare_probe(&account.id, None, 4).is_ok());
        assert_eq!(
            std::fs::read_to_string(workspace_root.join("file")).unwrap(),
            "current"
        );
    }

    #[test]
    fn a_missing_tool_receipt_and_prior_uncertainty_never_release_effect_custody() {
        let root = tempfile::tempdir().unwrap();
        let base = xcb_core::canonical(root.path()).unwrap();
        let store = Store::open(&base.join("state")).unwrap();
        let account = store
            .add_account(Provider::Claude, "Test", 1, None)
            .unwrap();
        let run = store.prepare_probe(&account.id, None, 2).unwrap();
        let mut effects = EffectState::None;
        assert!(
            settle_tool_effects(
                &store,
                &run,
                "missing-receipt",
                EffectState::Settled,
                &mut effects
            )
            .is_err()
        );
        assert_eq!(effects, EffectState::Uncertain);
        store
            .begin_tool(&run, "later-read", "workspace_read", &digest("{}"))
            .unwrap();
        settle_tool_effects(&store, &run, "later-read", EffectState::None, &mut effects).unwrap();
        assert_eq!(effects, EffectState::Uncertain);
        assert!(store.prepare_probe(&account.id, None, 3).is_err());
    }

    /// Regression test for the Linux launch-plan seam: `prepare` builds its
    /// spec through `linux_spec`, and the sandbox planner must accept the
    /// plan it produces. The old spec double-mounted the executable and the
    /// forwarder runtime, which the planner rejected with "bind target
    /// duplicated" — every launch failed before bwrap even ran.
    #[test]
    fn linux_launch_plan_is_accepted_by_the_planner() {
        let root = tempfile::tempdir().unwrap();
        let base = xcb_core::canonical(root.path()).unwrap();
        let directory = base.join("run");
        let scratch = directory.join("scratch");
        let cwd = scratch.join("work");
        let socket_dir = directory.join("egress");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(&socket_dir).unwrap();
        let executable = base.join("provider");
        file(&executable, 0o500);
        let runtime = base.join("xcb");
        file(&runtime, 0o500);
        let socket = socket_dir.join("egress.sock");
        file(&socket, 0o600);
        let env_file = scratch.join("forwarder.env");
        file(&env_file, 0o600);
        let lib = base.join("libprovider.so");
        file(&lib, 0o400);
        let wrapper = base.join("bwrap");
        file(&wrapper, 0o755);
        let pin = sandbox::BwrapPin::admit(&wrapper).unwrap();

        let spec = linux_spec(
            executable.clone(),
            runtime.clone(),
            scratch.clone(),
            directory.join("sandbox.json"),
            socket.clone(),
            env_file.clone(),
            vec![lib.clone()],
        );
        let launch = sandbox::bwrap_launch(
            &pin,
            &spec,
            &["--print".into()],
            &BTreeMap::from([("PATH".into(), "/usr/bin:/bin".into())]),
            &cwd,
        )
        .expect("the runner's launch plan must be accepted by the planner");

        // Every artifact is mounted exactly once — no duplicated targets.
        let policy: serde_json::Value = serde_json::from_str(&launch.policy).unwrap();
        let binds = policy["binds"].as_array().unwrap();
        for path in [&executable, &runtime, &lib, &scratch, &socket] {
            let path = path.to_str().unwrap();
            assert_eq!(
                binds.iter().filter(|bind| bind["target"] == path).count(),
                1,
                "{path} should appear as exactly one bind pair"
            );
        }
        // The env file rides the scratch bind and reaches the forwarder argv.
        let tail = &launch.args[launch.args.iter().position(|a| a == "--").unwrap() + 1..];
        assert_eq!(
            tail,
            [
                runtime.to_str().unwrap(),
                "egress-forward",
                socket.to_str().unwrap(),
                "48123",
                "-",
                env_file.to_str().unwrap(),
                "--",
                executable.to_str().unwrap(),
                "--print",
            ]
        );
        assert_eq!(policy["egress"]["protocol"], "connect-tcp443");
    }

    /// Alias model choices (catalog `value` ≠ `resolvedModel`, e.g. `default`)
    /// launch with the alias but the provider reports its own resolution at
    /// init — which can vary by effort. The boundary assertion must accept
    /// any well-formed reported model for aliases while concrete choices
    /// still require an exact match.
    #[test]
    fn init_boundary_accepts_resolved_alias_model() {
        let cwd = Path::new("/workspace");
        let init = |model: &str| {
            json!({
                "claude_code_version": "2.1.274",
                "cwd": "/workspace",
                "model": model,
                "apiKeySource": "none",
                "permissionMode": "auto",
                "tools": [],
                "skills": [],
                "plugins": [],
                "mcp_servers": []
            })
        };
        let choice = |id: &str, resolved: Option<&str>| ModelChoice {
            provider: Provider::Claude,
            id: Id::new(id).unwrap(),
            label: id.into(),
            mode: Mode::Fixed,
            resolved: resolved.map(|value| Id::new(value).unwrap()),
            effort: None,
            observed_at_ms: 0,
        };
        let alias = choice("default", Some("claude-opus-5[1m]"));
        validate_init(&init("claude-opus-5[1m]"), cwd, &alias, false).unwrap();
        validate_init(&init("default"), cwd, &alias, false).unwrap();
        validate_init(&init("claude-sonnet-5"), cwd, &alias, false).unwrap();
        assert!(validate_init(&init("not a model!"), cwd, &alias, false).is_err());
        assert!(validate_init(&init(""), cwd, &alias, false).is_err());

        let concrete = choice("claude-fable-5-1", None);
        validate_init(&init("claude-fable-5-1"), cwd, &concrete, false).unwrap();
        assert!(validate_init(&init("claude-sonnet-5"), cwd, &concrete, false).is_err());
    }
    /// Each boundary field fails on its own so an operator can tell which
    /// runtime property changed. Claude Code 2.1.281 started listing its
    /// builtin `agents-md` plugin at init; the assertion must still reject a
    /// non-empty plugin set, and the launch settings must disable that plugin.
    #[test]
    fn init_boundary_names_the_mismatched_field() {
        let cwd = Path::new("/workspace");
        let choice = ModelChoice {
            provider: Provider::Claude,
            id: Id::new("claude-fable-5-1").unwrap(),
            label: "Fable".into(),
            mode: Mode::Fixed,
            resolved: None,
            effort: None,
            observed_at_ms: 0,
        };
        let good = json!({
            "claude_code_version": "2.1.281",
            "cwd": "/workspace",
            "model": "claude-fable-5-1",
            "apiKeySource": "none",
            "permissionMode": "auto",
            "tools": [],
            "skills": [],
            "plugins": [],
            "mcp_servers": []
        });
        validate_init(&good, cwd, &choice, false).unwrap();
        let message = |mut init: Value, key: &str, value: Value| {
            init[key] = value;
            match validate_init(&init, cwd, &choice, false) {
                Err(Error::Protocol(message)) => message,
                other => panic!("expected a protocol error, got {other:?}"),
            }
        };
        assert_eq!(
            message(
                good.clone(),
                "plugins",
                json!([{"name": "agents-md", "path": "builtin", "source": "agents-md@builtin"}])
            ),
            "effective runtime boundary mismatch: plugins"
        );
        assert_eq!(
            message(good.clone(), "skills", json!(["doctor"])),
            "effective runtime boundary mismatch: skills"
        );
        assert_eq!(
            message(good.clone(), "tools", json!(["Bash"])),
            "effective runtime boundary mismatch: tools"
        );
        for mode in ["acceptEdits", "dontAsk", "bypassPermissions", "default"] {
            assert_eq!(
                message(good.clone(), "permissionMode", json!(mode)),
                "effective runtime boundary mismatch: permissionMode"
            );
        }
        assert_eq!(
            message(good.clone(), "apiKeySource", json!("ANTHROPIC_API_KEY")),
            "effective runtime boundary mismatch: apiKeySource"
        );
        assert_eq!(
            message(good.clone(), "cwd", json!("/elsewhere")),
            "effective runtime boundary mismatch: cwd"
        );
        assert_eq!(
            message(good.clone(), "model", json!("claude-sonnet-5")),
            "effective runtime boundary mismatch: model"
        );
        assert_eq!(
            message(good.clone(), "claude_code_version", json!("1.0.0")),
            "effective runtime boundary mismatch: claude_code_version"
        );
        assert_eq!(
            message(
                good.clone(),
                "mcp_servers",
                json!([{"name": "other", "status": "connected"}])
            ),
            "effective runtime boundary mismatch: mcp_servers"
        );
        assert!(matches!(
            validate_init(&json!({"tools": []}), cwd, &choice, false),
            Err(Error::Protocol("MCP inventory"))
        ));
    }

    #[test]
    fn launch_uses_auto_mode_with_broker_only_tools_and_disabled_extensions() {
        let choice = ModelChoice {
            provider: Provider::Claude,
            id: Id::new("opus[1m]").unwrap(),
            label: "Opus".into(),
            mode: Mode::Fixed,
            resolved: Some(Id::new("claude-opus-5-5[1m]").unwrap()),
            effort: Some(Id::new("max").unwrap()),
            observed_at_ms: 0,
        };
        for tools in [false, true] {
            let args = provider_args(&choice, tools);
            let settings = args
                .iter()
                .position(|arg| arg == "--settings")
                .map(|index| &args[index + 1])
                .expect("settings argument");
            let settings: Value = serde_json::from_str(settings).unwrap();
            assert_eq!(
                settings["enabledPlugins"]["agents-md@builtin"],
                json!(false)
            );
            assert_eq!(settings["disableAllHooks"], json!(true));
            assert_eq!(settings["disableBundledSkills"], json!(true));
            assert_eq!(settings["disableSkillShellExecution"], json!(true));
            assert_eq!(settings["disableClaudeAiConnectors"], json!(true));
            assert_eq!(settings["enableWorkflows"], json!(false));
            assert_eq!(settings["workflowKeywordTriggerEnabled"], json!(false));
            let argument = |name: &str| {
                let index = args.iter().position(|arg| arg == name).unwrap();
                &args[index + 1]
            };
            assert_eq!(argument("--permission-mode"), "auto");
            assert_eq!(argument("--permission-prompt-tool"), "stdio");
            assert_eq!(argument("--tools"), "");
            assert_eq!(argument("--setting-sources"), "");
            assert!(args.iter().any(|arg| arg == "--strict-mcp-config"));
            if tools {
                assert!(
                    argument("--allowedTools")
                        .split(',')
                        .all(|name| name.starts_with("mcp__xcb__"))
                );
            }
            assert_eq!(
                args.iter().filter(|arg| *arg == "--allowedTools").count(),
                usize::from(tools)
            );
        }
    }
    struct FixtureProtocol {
        model: ModelChoice,
        before_ready: bool,
        mailbox_failure: bool,
        passive_after_quota: bool,
        initialize_failure: Option<Error>,
        receive_failure: Option<Error>,
        step: u8,
        block_initialize: Option<tokio::sync::oneshot::Sender<()>>,
    }
    impl Protocol for FixtureProtocol {
        async fn initialize(&mut self, _: &mut StreamProcess, _: &str) -> Result<Vec<ModelChoice>> {
            if let Some(error) = self.initialize_failure.take() {
                return Err(error);
            }
            if let Some(started) = self.block_initialize.take() {
                let _ = started.send(());
                std::future::pending::<()>().await;
            }
            Ok(vec![self.model.clone()])
        }
        async fn start(&mut self, process: &mut StreamProcess, _: Prompt) -> Result<()> {
            process.send(&json!({"step":0})).await
        }
        async fn receive(&mut self, _: &mut StreamProcess, _: &[u8]) -> Result<Vec<TurnEvent>> {
            self.step += 1;
            if let Some(error) = self.receive_failure.take() {
                return Err(error);
            }
            if self.step == 1 {
                let mut events = vec![];
                if !self.before_ready {
                    events.push(TurnEvent::Ready);
                }
                if self.passive_after_quota {
                    events.extend([
                        TurnEvent::Quota {
                            window: None,
                            used_percent: None,
                            resets_at_ms: None,
                            failure: Some(Failure::AccountQuota),
                        },
                        TurnEvent::Quota {
                            window: None,
                            used_percent: None,
                            resets_at_ms: None,
                            failure: None,
                        },
                        TurnEvent::Result {
                            terminal: Terminal::Failed,
                            text: "The account quota was exhausted".into(),
                            models: vec![],
                        },
                    ]);
                    return Ok(events);
                }
                events.push(if self.mailbox_failure {
                    TurnEvent::Tool {
                        id: "fixture-call".into(),
                        name: "xcb_swarm_status".into(),
                        arguments: json!({}),
                    }
                } else {
                    TurnEvent::Tool {
                        id: "fixture-call".into(), name: "workspace_write".into(),
                        arguments: json!({"path":"created.txt","text":"confirmed write","expectedRevision":null}),
                    }
                });
                Ok(events)
            } else {
                Ok(vec![
                    TurnEvent::Delta {
                        thinking: false,
                        text: "Done".into(),
                    },
                    TurnEvent::Result {
                        terminal: Terminal::Completed,
                        text: "Done".into(),
                        models: vec![],
                    },
                ])
            }
        }
        async fn reply(
            &mut self,
            process: &mut StreamProcess,
            id: &str,
            result: Value,
        ) -> Result<()> {
            assert_eq!(id, "fixture-call");
            assert_eq!(result["isError"], self.mailbox_failure);
            process.send(&json!({"step":1})).await
        }
    }

    #[tokio::test]
    async fn shared_lifecycle_settles_tools_and_account_for_each_provider_protocol() {
        for provider in [Provider::Claude, Provider::Codex, Provider::Devin] {
            for (before_ready, mailbox_failure, cancel_tool, passive_after_quota) in [
                (false, false, false, false),
                (true, false, false, false),
                (false, true, false, false),
                (false, false, true, false),
                (false, false, false, true),
            ] {
                let root = tempfile::tempdir().unwrap();
                let base = xcb_core::canonical(root.path()).unwrap();
                let workspace = base.join("work");
                std::fs::create_dir(&workspace).unwrap();
                let store = Arc::new(Store::open(&base.join("state")).unwrap());
                if mailbox_failure {
                    // A host-side mailbox initialization error must become
                    // a tool rejection, not leave a pending effect receipt.
                    file(&store.root().join("managed"), 0o600);
                }
                let account = store
                    .add_account(provider, "Fixture", now_ms(), None)
                    .unwrap();
                let model = ModelChoice {
                    provider,
                    id: Id::new("fixture-model").unwrap(),
                    label: "Fixture".into(),
                    mode: Mode::Fixed,
                    resolved: None,
                    effort: None,
                    observed_at_ms: now_ms(),
                };
                let session = store
                    .create_session(&account.id, model.clone(), &workspace, now_ms())
                    .unwrap();
                let message = Message {
                    id: new_id("message"),
                    role: Role::User,
                    text: "Create a file".into(),
                    at_ms: now_ms(),
                    attachments: vec![],
                    provenance: None,
                };
                let session = store
                    .append_message(&session.id, session.revision, &message)
                    .unwrap();
                let artifacts = LaunchArtifacts::create(store.root()).unwrap();
                let launch_path = artifacts.directory.clone();
                let launch = Launch {
                    command: Command::new("/bin/cat"),
                    cwd: base.clone(),
                    bridge: None,
                    artifacts,
                    prepared_run: None,
                    codex_credentials: None,
                };
                let (cancel, cancellation) = watch::channel(false);
                let outcome = run_prepared(
                    store.clone(),
                    RunInput {
                        session,
                        message,
                        config: Config::default(),
                        pane_generation: false,
                    },
                    cancellation,
                    Arc::new(move |event| {
                        if cancel_tool && matches!(event, Progress::Tool(_)) {
                            cancel.send(true).unwrap();
                        }
                    }),
                    launch,
                    FixtureProtocol {
                        model,
                        before_ready,
                        mailbox_failure,
                        passive_after_quota,
                        initialize_failure: None,
                        receive_failure: None,
                        step: 0,
                        block_initialize: None,
                    },
                    Workspace::open_with_coordination(&workspace, &base.join("coordination"))
                        .unwrap(),
                )
                .await
                .unwrap();
                assert!(outcome.facts.joined);
                assert!(store.unsettled_runs().unwrap().is_empty());
                let db = rusqlite::Connection::open(store.root().join("xcb.sqlite")).unwrap();
                let pending: i64 = db
                    .query_row(
                        "SELECT count(*) FROM tool_effects WHERE settled=0",
                        [],
                        |row| row.get(0),
                    )
                    .unwrap();
                assert_eq!(pending, 0, "every started tool intent must be settled");
                assert!(!launch_path.exists());
                assert_eq!(
                    workspace.join("created.txt").exists(),
                    !before_ready && !mailbox_failure && !cancel_tool && !passive_after_quota
                );
                if before_ready {
                    assert_eq!(outcome.facts.terminal, Terminal::Failed);
                    assert_eq!(outcome.facts.effects, EffectState::None);
                } else if cancel_tool {
                    assert_eq!(outcome.facts.terminal, Terminal::Cancelled);
                    assert_eq!(outcome.facts.effects, EffectState::None);
                } else if passive_after_quota {
                    assert_eq!(outcome.facts.terminal, Terminal::Failed);
                    assert_eq!(outcome.facts.failure, Some(Failure::AccountQuota));
                    assert_eq!(outcome.state, State::Limited);
                    assert_eq!(outcome.facts.effects, EffectState::None);
                } else {
                    assert_eq!(outcome.text, "Done");
                    assert_eq!(outcome.facts.terminal, Terminal::Completed);
                    assert_eq!(
                        outcome.facts.effects,
                        if mailbox_failure {
                            EffectState::None
                        } else {
                            EffectState::Settled
                        }
                    );
                }
                let run = store.prepare_probe(&account.id, None, now_ms()).unwrap();
                store.settle(&run, State::Idle, now_ms()).unwrap();
            }
        }
    }

    /// A provider that refuses the turn for the account's quota without
    /// reporting a reset leaves no meter to block on. The settled outcome
    /// records a cooldown under the run's hold, so the account reads as
    /// limited to the summary and cannot be routed to again until it ends.
    #[tokio::test]
    async fn settled_usage_limit_without_reset_records_a_cooldown_that_blocks_the_account() {
        for provider in [Provider::Claude, Provider::Codex, Provider::Devin] {
            let root = tempfile::tempdir().unwrap();
            let base = xcb_core::canonical(root.path()).unwrap();
            let workspace = base.join("work");
            std::fs::create_dir(&workspace).unwrap();
            let store = Arc::new(Store::open(&base.join("state")).unwrap());
            let account = store
                .add_account(provider, "Fixture", now_ms(), None)
                .unwrap();
            // Claude binds quota to a credential generation, which a launch
            // establishes before the turn; a fixture turn has no launch.
            let probe = store.prepare_probe(&account.id, None, now_ms()).unwrap();
            crate::application_qualification::ensure_generation(&store, &probe).unwrap();
            store.settle(&probe, State::Idle, now_ms()).unwrap();
            let model = ModelChoice {
                provider,
                id: Id::new("fixture-model").unwrap(),
                label: "Fixture".into(),
                mode: Mode::Fixed,
                resolved: None,
                effort: None,
                observed_at_ms: now_ms(),
            };
            let session = store
                .create_session(&account.id, model.clone(), &workspace, now_ms())
                .unwrap();
            let message = Message {
                id: new_id("message"),
                role: Role::User,
                text: "Create a file".into(),
                at_ms: now_ms(),
                attachments: vec![],
                provenance: None,
            };
            let session = store
                .append_message(&session.id, session.revision, &message)
                .unwrap();
            let launch = Launch {
                command: Command::new("/bin/cat"),
                cwd: base.clone(),
                bridge: None,
                artifacts: LaunchArtifacts::create(store.root()).unwrap(),
                prepared_run: None,
                codex_credentials: None,
            };
            let config = Config {
                quota_limit_cooldown_ms: 600_000,
                ..Config::default()
            };
            let (_cancel, cancellation) = watch::channel(false);
            let notices = Arc::new(std::sync::Mutex::new(Vec::new()));
            let seen = notices.clone();
            let started = now_ms();
            let outcome = run_prepared(
                store.clone(),
                RunInput {
                    session: session.clone(),
                    message,
                    config,
                    pane_generation: false,
                },
                cancellation,
                Arc::new(move |event| {
                    if let Progress::Notice(text) = event {
                        seen.lock().unwrap().push(text);
                    }
                }),
                launch,
                FixtureProtocol {
                    model,
                    before_ready: false,
                    mailbox_failure: false,
                    passive_after_quota: true,
                    initialize_failure: None,
                    receive_failure: None,
                    step: 0,
                    block_initialize: None,
                },
                Workspace::open_with_coordination(&workspace, &base.join("coordination")).unwrap(),
            )
            .await
            .unwrap();
            assert_eq!(outcome.facts.failure, Some(Failure::AccountQuota));
            assert_eq!(outcome.state, State::Limited);
            assert!(outcome.facts.joined);
            assert!(store.unsettled_runs().unwrap().is_empty());
            assert!(
                !notices
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|text| text.contains("cooldown")),
                "{provider:?}: the cooldown must be recorded, not reported as failed"
            );
            let now = now_ms();
            let until = store
                .quota_blocked_until(&account.id, now)
                .unwrap()
                .unwrap_or_else(|| panic!("{provider:?}: settled usage limit must block"));
            assert!(
                until >= started + 600_000 && until <= now + 600_000,
                "{provider:?}: cooldown {until} is not the configured span from settlement"
            );
            // Not a meter: the account shows no percentage or reset, only the
            // retry estimate, and the next turn cannot start on it.
            let pool = store.account(&account.id).unwrap().quota_pool;
            assert!(store.quotas(&pool).unwrap().is_empty());
            assert_eq!(store.quota_limits(&pool).unwrap().len(), 1);
            let view = crate::summary::snapshot(&store, None, &Config::default(), now).unwrap();
            assert_eq!(view.accounts[0].quota_blocked_until_ms, Some(until));
            assert_eq!(view.accounts[0].remaining_percent, None);
            let current = store.session(&session.id).unwrap().unwrap();
            assert_eq!(current.state, State::Limited);
            assert!(
                store
                    .prepare_run(&session.id, current.revision, now)
                    .is_err()
            );
            assert_eq!(store.quota_blocked_until(&account.id, until).unwrap(), None);
            let run = store.prepare_probe(&account.id, None, until).unwrap();
            store.settle(&run, State::Idle, until).unwrap();
        }
    }
    #[tokio::test]
    async fn prompt_submission_receipt_distinguishes_unstarted_unknown_and_submitted() {
        struct SubmissionProtocol {
            model: ModelChoice,
            stage: u8,
        }
        impl Protocol for SubmissionProtocol {
            async fn initialize(
                &mut self,
                _: &mut StreamProcess,
                _: &str,
            ) -> Result<Vec<ModelChoice>> {
                if self.stage == 0 {
                    return Err(Error::Protocol("fixture initialization failure"));
                }
                Ok(vec![self.model.clone()])
            }
            async fn start(&mut self, process: &mut StreamProcess, _: Prompt) -> Result<()> {
                if self.stage == 1 {
                    return Err(Error::Protocol("fixture submission failure"));
                }
                process.send(&json!({"fixture": "prompt"})).await?;
                if self.stage == 2 {
                    return Err(Error::Protocol("fixture partial submission failure"));
                }
                Ok(())
            }
            async fn receive(&mut self, _: &mut StreamProcess, _: &[u8]) -> Result<Vec<TurnEvent>> {
                Ok(vec![
                    TurnEvent::Ready,
                    TurnEvent::Result {
                        terminal: Terminal::Completed,
                        text: "Fixture completed".into(),
                        models: vec![],
                    },
                ])
            }
            async fn reply(&mut self, _: &mut StreamProcess, _: &str, _: Value) -> Result<()> {
                unreachable!("the submission fixture has no tools")
            }
        }

        for (stage, expected) in [(0, Some(false)), (1, None), (2, None), (3, Some(true))] {
            let root = tempfile::tempdir().unwrap();
            let base = xcb_core::canonical(root.path()).unwrap();
            let workspace = private::directory(&base.join("work")).unwrap();
            let store = Arc::new(Store::open(&base.join("state")).unwrap());
            let account = store
                .add_account(Provider::Claude, "Fixture", now_ms(), None)
                .unwrap();
            let model = ModelChoice {
                provider: Provider::Claude,
                id: Id::new("fixture-model").unwrap(),
                label: "Fixture".into(),
                mode: Mode::Fixed,
                resolved: None,
                effort: None,
                observed_at_ms: now_ms(),
            };
            let session = store
                .create_session(&account.id, model.clone(), &workspace, now_ms())
                .unwrap();
            let message = Message {
                id: new_id("input"),
                role: Role::User,
                text: "Exact managed guidance".into(),
                at_ms: now_ms(),
                attachments: vec![],
                provenance: None,
            };
            let session = store
                .append_message(&session.id, session.revision, &message)
                .unwrap();
            let session_id = session.id.clone();
            let launch = Launch {
                command: Command::new("/bin/cat"),
                cwd: base.clone(),
                bridge: None,
                artifacts: LaunchArtifacts::create(store.root()).unwrap(),
                prepared_run: None,
                codex_credentials: None,
            };
            let (_cancel, cancellation) = watch::channel(false);
            let outcome = run_prepared(
                store.clone(),
                RunInput {
                    session,
                    message,
                    config: Config::default(),
                    pane_generation: false,
                },
                cancellation,
                Arc::new(|_| ()),
                launch,
                SubmissionProtocol { model, stage },
                Workspace::open_with_coordination(&workspace, &base.join("coordination")).unwrap(),
            )
            .await
            .unwrap();
            assert!(outcome.facts.joined);
            assert!(store.unsettled_runs().unwrap().is_empty());
            assert_eq!(outcome.facts.terminal == Terminal::Completed, stage == 3);
            // Both successful and failed workers have local prompt rows and
            // settled outcomes. Only explicit submission evidence separates
            // never attempted, uncertain transport, and successfully sent.
            assert!(store.settled_outcome(&session_id, 0).unwrap().is_some());
            let reader = Store::open_read_only(store.root()).unwrap();
            assert_eq!(
                reader.settled_input_submission(&session_id, 0).unwrap(),
                expected
            );
            assert_eq!(
                reader.settled_input_submission(&session_id, 1).unwrap(),
                None
            );
            let current = store.session(&session_id).unwrap().unwrap();
            store
                .append_message(
                    &session_id,
                    current.revision,
                    &Message {
                        id: new_id("later_input"),
                        role: Role::User,
                        text: "A later turn must not adopt the receipt".into(),
                        at_ms: now_ms(),
                        attachments: vec![],
                        provenance: None,
                    },
                )
                .unwrap();
            assert_eq!(
                reader.settled_input_submission(&session_id, 0).unwrap(),
                None
            );
        }
    }

    /// Legal file content and oversized tool results must produce `isError`
    /// tool replies while the provider turn still completes; the persisted
    /// transcript is sanitized and bounded.
    struct ToolReadProtocol {
        model: ModelChoice,
        ready: bool,
        calls: std::collections::VecDeque<(String, serde_json::Value, bool)>,
        current: Option<(String, bool)>,
    }
    impl Protocol for ToolReadProtocol {
        async fn initialize(&mut self, _: &mut StreamProcess, _: &str) -> Result<Vec<ModelChoice>> {
            Ok(vec![self.model.clone()])
        }
        async fn start(&mut self, process: &mut StreamProcess, _: Prompt) -> Result<()> {
            process.send(&json!({"step":0})).await
        }
        async fn receive(&mut self, _: &mut StreamProcess, _: &[u8]) -> Result<Vec<TurnEvent>> {
            let mut events = vec![];
            if self.ready {
                self.ready = false;
                events.push(TurnEvent::Ready);
            }
            if let Some((id, arguments, expect_error)) = self.calls.pop_front() {
                self.current = Some((id.clone(), expect_error));
                events.push(TurnEvent::Tool {
                    id,
                    name: "workspace_read".into(),
                    arguments,
                });
            } else {
                events.push(TurnEvent::Result {
                    terminal: Terminal::Completed,
                    text: "Done".into(),
                    models: vec![],
                });
            }
            Ok(events)
        }
        async fn reply(
            &mut self,
            process: &mut StreamProcess,
            id: &str,
            result: Value,
        ) -> Result<()> {
            let Some((expected_id, expect_error)) = self.current.take() else {
                panic!("unexpected tool reply {id}");
            };
            assert_eq!(id, expected_id);
            assert_eq!(result["isError"], expect_error, "reply for {id}");
            let text = result["content"][0]["text"].as_str().unwrap_or_default();
            match id {
                // The provider sees the raw result for a successful call —
                // including legal control characters — while the transcript
                // carries the sanitized copy.
                "ctrl" => assert!(text.contains('\u{0085}') && text.contains('\u{202e}')),
                "lines" => assert_eq!(
                    text,
                    "tool result exceeds 256 KiB; request a smaller result"
                ),
                "big" => assert!(text.contains("exceeds the 128 KiB read limit")),
                _ => (),
            }
            process.send(&json!({"step":1})).await
        }
    }

    #[tokio::test]
    async fn legal_or_oversized_tool_output_is_a_tool_error_not_a_turn_failure() {
        let root = tempfile::tempdir().unwrap();
        let base = xcb_core::canonical(root.path()).unwrap();
        let workspace = base.join("work");
        std::fs::create_dir(&workspace).unwrap();
        // A legal 256 KiB file is rejected by the read limit, not the turn.
        std::fs::write(workspace.join("big.txt"), "\n".repeat(256 * 1024)).unwrap();
        // A legal 128 KiB file of newlines serializes past the transcript
        // bound: the oversize result becomes an isError reply with guidance.
        std::fs::write(workspace.join("lines.txt"), "\n".repeat(128 * 1024)).unwrap();
        // Legal control characters ride the provider reply; only the
        // transcript copy is sanitized.
        std::fs::write(
            workspace.join("ctrl.txt"),
            "before \u{0085} nel and \u{202e} bidi after",
        )
        .unwrap();
        let store = Arc::new(Store::open(&base.join("state")).unwrap());
        let account = store
            .add_account(Provider::Claude, "Fixture", now_ms(), None)
            .unwrap();
        let model = ModelChoice {
            provider: Provider::Claude,
            id: Id::new("fixture-model").unwrap(),
            label: "Fixture".into(),
            mode: Mode::Fixed,
            resolved: None,
            effort: None,
            observed_at_ms: now_ms(),
        };
        let session = store
            .create_session(&account.id, model.clone(), &workspace, now_ms())
            .unwrap();
        let message = Message {
            id: new_id("message"),
            role: Role::User,
            text: "Read the files".into(),
            at_ms: now_ms(),
            attachments: vec![],
            provenance: None,
        };
        let session = store
            .append_message(&session.id, session.revision, &message)
            .unwrap();
        let artifacts = LaunchArtifacts::create(store.root()).unwrap();
        let launch = Launch {
            command: Command::new("/bin/cat"),
            cwd: base.clone(),
            bridge: None,
            artifacts,
            prepared_run: None,
            codex_credentials: None,
        };
        let (_cancel, cancellation) = watch::channel(false);
        let outcome = run_prepared(
            store.clone(),
            RunInput {
                session: session.clone(),
                message,
                config: Config::default(),
                pane_generation: false,
            },
            cancellation,
            Arc::new(|_| {}),
            launch,
            ToolReadProtocol {
                model,
                ready: true,
                calls: [
                    ("big", json!({"path":"big.txt"}), true),
                    ("lines", json!({"path":"lines.txt"}), true),
                    ("ctrl", json!({"path":"ctrl.txt"}), false),
                ]
                .into_iter()
                .map(|(id, arguments, expect_error)| (id.to_owned(), arguments, expect_error))
                .collect(),
                current: None,
            },
            Workspace::open_with_coordination(&workspace, &base.join("coordination")).unwrap(),
        )
        .await
        .unwrap();
        // Every call completed the turn; nothing propagated as a turn failure.
        assert_eq!(outcome.facts.terminal, Terminal::Completed);
        assert_eq!(outcome.text, "Done");
        assert!(outcome.facts.joined);
        let db = rusqlite::Connection::open(store.root().join("xcb.sqlite")).unwrap();
        let pending: i64 = db
            .query_row(
                "SELECT count(*) FROM tool_effects WHERE settled=0",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(pending, 0);
        // The durable transcript is sanitized and bounded; the raw control
        // characters never persisted.
        let transcript = store.messages(&session.id, 512).unwrap();
        let tool_text: String = transcript
            .iter()
            .filter(|message| message.role == Role::Tool)
            .map(|message| message.text.clone())
            .collect();
        assert_eq!(tool_text.matches("workspace_read:").count(), 3);
        assert!(!tool_text.contains('\u{0085}'));
        assert!(!tool_text.contains('\u{202e}'));
        assert!(tool_text.len() <= 4 * MAX_TEXT_BYTES);
    }

    #[tokio::test]
    async fn cancellation_during_initialization_joins_and_releases_without_submitting_a_turn() {
        let root = tempfile::tempdir().unwrap();
        let base = xcb_core::canonical(root.path()).unwrap();
        let workspace = base.join("work");
        std::fs::create_dir(&workspace).unwrap();
        let store = Arc::new(Store::open(&base.join("state")).unwrap());
        let account = store
            .add_account(Provider::Claude, "Fixture", now_ms(), None)
            .unwrap();
        let model = ModelChoice {
            provider: Provider::Claude,
            id: Id::new("fixture-model").unwrap(),
            label: "Fixture".into(),
            mode: Mode::Fixed,
            resolved: None,
            effort: None,
            observed_at_ms: now_ms(),
        };
        let session = store
            .create_session(&account.id, model.clone(), &workspace, now_ms())
            .unwrap();
        let message = Message {
            id: new_id("message"),
            role: Role::User,
            text: "Create a file".into(),
            at_ms: now_ms(),
            attachments: vec![],
            provenance: None,
        };
        let session = store
            .append_message(&session.id, session.revision, &message)
            .unwrap();
        let artifacts = LaunchArtifacts::create(store.root()).unwrap();
        let launch_path = artifacts.directory.clone();
        let launch = Launch {
            command: Command::new("/bin/cat"),
            cwd: base.clone(),
            bridge: None,
            artifacts,
            prepared_run: None,
            codex_credentials: None,
        };
        let (cancel, cancellation) = watch::channel(false);
        let (started, initialized) = tokio::sync::oneshot::channel();
        let trigger = tokio::spawn(async move {
            initialized.await.unwrap();
            cancel.send(true).unwrap();
        });
        let outcome = tokio::time::timeout(
            Duration::from_secs(3),
            run_prepared(
                store.clone(),
                RunInput {
                    session,
                    message,
                    config: Config::default(),
                    pane_generation: false,
                },
                cancellation,
                Arc::new(|_| ()),
                launch,
                FixtureProtocol {
                    model,
                    before_ready: false,
                    mailbox_failure: false,
                    passive_after_quota: false,
                    initialize_failure: None,
                    receive_failure: None,
                    step: 0,
                    block_initialize: Some(started),
                },
                Workspace::open_with_coordination(&workspace, &base.join("coordination")).unwrap(),
            ),
        )
        .await
        .expect("cancellation must not wait for the initialization deadline")
        .unwrap();
        trigger.await.unwrap();
        assert_eq!(outcome.facts.terminal, Terminal::Cancelled);
        assert!(outcome.facts.joined);
        assert_eq!(outcome.facts.effects, EffectState::None);
        assert!(store.unsettled_runs().unwrap().is_empty());
        assert!(!workspace.join("created.txt").exists());
        assert!(!launch_path.exists());
    }
    #[tokio::test]
    async fn failed_launch_releases_only_after_protocol_listener_join() {
        struct ListenerProtocol {
            store: Arc<Store>,
            joined: bool,
        }
        impl Protocol for ListenerProtocol {
            async fn initialize(
                &mut self,
                _: &mut StreamProcess,
                _: &str,
            ) -> Result<Vec<ModelChoice>> {
                unreachable!()
            }
            async fn start(&mut self, _: &mut StreamProcess, _: Prompt) -> Result<()> {
                unreachable!()
            }
            async fn receive(&mut self, _: &mut StreamProcess, _: &[u8]) -> Result<Vec<TurnEvent>> {
                unreachable!()
            }
            async fn reply(&mut self, _: &mut StreamProcess, _: &str, _: Value) -> Result<()> {
                unreachable!()
            }
            async fn shutdown(&mut self) -> bool {
                assert_eq!(
                    self.store.unsettled_runs().unwrap().len(),
                    1,
                    "custody must remain while the listener joins"
                );
                self.joined
            }
        }
        for joined in [true, false] {
            let root = tempfile::tempdir().unwrap();
            let base = xcb_core::canonical(root.path()).unwrap();
            let workspace = base.join("work");
            std::fs::create_dir(&workspace).unwrap();
            let store = Arc::new(Store::open(&base.join("state")).unwrap());
            let account = store
                .add_account(Provider::Devin, "Fixture", now_ms(), None)
                .unwrap();
            let model = ModelChoice {
                provider: Provider::Devin,
                id: Id::new("fixture-model").unwrap(),
                label: "Fixture".into(),
                mode: Mode::Fixed,
                resolved: None,
                effort: None,
                observed_at_ms: now_ms(),
            };
            let session = store
                .create_session(&account.id, model, &workspace, now_ms())
                .unwrap();
            let artifacts = LaunchArtifacts::create(store.root()).unwrap();
            let launch_path = artifacts.directory.clone();
            let launch = Launch {
                command: Command::new(base.join("missing-provider")),
                cwd: base.clone(),
                bridge: None,
                artifacts,
                prepared_run: None,
                codex_credentials: None,
            };
            let (_cancel, cancellation) = watch::channel(false);
            let input = RunInput {
                session,
                message: Message {
                    id: new_id("message"),
                    role: Role::User,
                    text: "unused".into(),
                    at_ms: now_ms(),
                    attachments: vec![],
                    provenance: None,
                },
                config: Config::default(),
                pane_generation: false,
            };
            let result = run_prepared(
                store.clone(),
                input,
                cancellation,
                Arc::new(|_| ()),
                launch,
                ListenerProtocol {
                    store: store.clone(),
                    joined,
                },
                Workspace::open_with_coordination(&workspace, &base.join("coordination")).unwrap(),
            )
            .await;
            assert!(matches!(result, Err(Error::LaunchNotStarted(_))));
            assert_eq!(store.unsettled_runs().unwrap().is_empty(), joined);
            assert_eq!(launch_path.exists(), !joined);
        }
    }

    /// The codec's fixed error category, not Unknown, settles account state
    /// when initialize or the turn RPCs fail before an answer exists.
    #[tokio::test]
    async fn codec_failure_categories_settle_account_state_not_unknown() {
        async fn run(
            provider: Provider,
            initialize_failure: Option<Error>,
            receive_failure: Option<Error>,
        ) -> (Outcome, Arc<Store>, Id) {
            let root = tempfile::tempdir().unwrap();
            let base = xcb_core::canonical(root.path()).unwrap();
            let workspace = base.join("work");
            std::fs::create_dir(&workspace).unwrap();
            let store = Arc::new(Store::open(&base.join("state")).unwrap());
            let account = store
                .add_account(provider, "Fixture", now_ms(), None)
                .unwrap();
            let model = ModelChoice {
                provider,
                id: Id::new("fixture-model").unwrap(),
                label: "Fixture".into(),
                mode: Mode::Fixed,
                resolved: None,
                effort: None,
                observed_at_ms: now_ms(),
            };
            let session = store
                .create_session(&account.id, model.clone(), &workspace, now_ms())
                .unwrap();
            let message = Message {
                id: new_id("message"),
                role: Role::User,
                text: "Create a file".into(),
                at_ms: now_ms(),
                attachments: vec![],
                provenance: None,
            };
            let session = store
                .append_message(&session.id, session.revision, &message)
                .unwrap();
            let launch = Launch {
                command: Command::new("/bin/cat"),
                cwd: base.clone(),
                bridge: None,
                artifacts: LaunchArtifacts::create(store.root()).unwrap(),
                prepared_run: None,
                codex_credentials: None,
            };
            let (_cancel, cancellation) = watch::channel(false);
            let outcome = run_prepared(
                store.clone(),
                RunInput {
                    session,
                    message,
                    config: Config::default(),
                    pane_generation: false,
                },
                cancellation,
                Arc::new(|_| ()),
                launch,
                FixtureProtocol {
                    model,
                    before_ready: false,
                    mailbox_failure: false,
                    passive_after_quota: false,
                    initialize_failure,
                    receive_failure,
                    step: 0,
                    block_initialize: None,
                },
                Workspace::open_with_coordination(&workspace, &base.join("coordination")).unwrap(),
            )
            .await
            .unwrap();
            (outcome, store, account.id)
        }

        let (outcome, store, account) = run(
            Provider::Codex,
            Some(Error::CodexRpc {
                method: "turn/start",
                code: -32000,
                category: crate::category::AUTHENTICATION,
            }),
            None,
        )
        .await;
        assert_eq!(outcome.facts.terminal, Terminal::Failed);
        assert_eq!(outcome.facts.failure, Some(Failure::Authentication));
        assert_eq!(outcome.state, State::NeedsAction);
        assert!(outcome.facts.joined);
        assert!(store.authentication_required(&account).unwrap());
        assert!(store.unsettled_runs().unwrap().is_empty());
        assert_eq!(store.quota_blocked_until(&account, now_ms()).unwrap(), None);

        let (outcome, store, account) = run(
            Provider::Codex,
            None,
            Some(Error::CodexRpc {
                method: "turn/start",
                code: -32000,
                category: crate::category::CODEX_USAGE_LIMIT,
            }),
        )
        .await;
        assert_eq!(outcome.facts.failure, Some(Failure::AccountQuota));
        assert_eq!(outcome.state, State::Limited);
        assert!(!store.authentication_required(&account).unwrap());
        // Codex's usage-limit error carries no reset: the settled failure
        // records the configured cooldown instead of leaving the account ready.
        assert!(
            store
                .quota_blocked_until(&account, now_ms())
                .unwrap()
                .is_some()
        );

        let (outcome, store, account) = run(
            Provider::Devin,
            None,
            Some(Error::DevinRpc {
                method: "session/prompt",
                code: -32002,
                category: crate::category::NETWORK,
            }),
        )
        .await;
        assert_eq!(outcome.facts.failure, Some(Failure::Transport));
        assert_eq!(outcome.state, State::Failed);
        assert!(!store.authentication_required(&account).unwrap());
        assert_eq!(store.quota_blocked_until(&account, now_ms()).unwrap(), None);

        let (outcome, store, account) = run(
            Provider::Devin,
            None,
            Some(Error::DevinRpc {
                method: "session/prompt",
                code: -32002,
                category: "provider rejected the operation",
            }),
        )
        .await;
        assert_eq!(outcome.facts.failure, Some(Failure::Unknown));
        assert_eq!(outcome.state, State::Failed);
        assert!(!store.authentication_required(&account).unwrap());
        assert_eq!(store.quota_blocked_until(&account, now_ms()).unwrap(), None);

        let (outcome, store, account) = run(
            Provider::Devin,
            None,
            Some(Error::DevinRpc {
                method: "session/prompt",
                code: -32011,
                category: crate::category::DEVIN_RESOURCE_LIMIT,
            }),
        )
        .await;
        assert_eq!(outcome.facts.failure, Some(Failure::AccountQuota));
        assert_eq!(outcome.state, State::Limited);
        assert!(
            store
                .quota_blocked_until(&account, now_ms())
                .unwrap()
                .is_some()
        );
    }

    /// Streaming providers emit one frame per delta, so frame volume is
    /// bounded by the byte budget and deadline, not a fixture-sized count: a
    /// flooded turn ends gracefully as a turn limit, never a protocol error.
    #[tokio::test]
    async fn frame_volume_ends_turn_as_turn_limit_not_protocol_failure() {
        struct FloodProtocol {
            model: ModelChoice,
        }
        impl Protocol for FloodProtocol {
            async fn initialize(
                &mut self,
                _: &mut StreamProcess,
                _: &str,
            ) -> Result<Vec<ModelChoice>> {
                Ok(vec![self.model.clone()])
            }
            async fn start(&mut self, _: &mut StreamProcess, _: Prompt) -> Result<()> {
                Ok(())
            }
            async fn next(&mut self, _: &mut StreamProcess) -> Result<crate::protocol::Batch> {
                Ok(crate::protocol::Batch {
                    bytes: 0,
                    events: vec![],
                })
            }
            async fn receive(&mut self, _: &mut StreamProcess, _: &[u8]) -> Result<Vec<TurnEvent>> {
                unreachable!()
            }
            async fn reply(&mut self, _: &mut StreamProcess, _: &str, _: Value) -> Result<()> {
                unreachable!()
            }
        }

        let root = tempfile::tempdir().unwrap();
        let base = xcb_core::canonical(root.path()).unwrap();
        let workspace = base.join("work");
        std::fs::create_dir(&workspace).unwrap();
        let store = Arc::new(Store::open(&base.join("state")).unwrap());
        let account = store
            .add_account(Provider::Claude, "Fixture", now_ms(), None)
            .unwrap();
        let model = ModelChoice {
            provider: Provider::Claude,
            id: Id::new("fixture-model").unwrap(),
            label: "Fixture".into(),
            mode: Mode::Fixed,
            resolved: None,
            effort: None,
            observed_at_ms: now_ms(),
        };
        let session = store
            .create_session(&account.id, model.clone(), &workspace, now_ms())
            .unwrap();
        let message = Message {
            id: new_id("message"),
            role: Role::User,
            text: "Write a long answer".into(),
            at_ms: now_ms(),
            attachments: vec![],
            provenance: None,
        };
        let session = store
            .append_message(&session.id, session.revision, &message)
            .unwrap();
        let launch = Launch {
            command: Command::new("/bin/cat"),
            cwd: base.clone(),
            bridge: None,
            artifacts: LaunchArtifacts::create(store.root()).unwrap(),
            prepared_run: None,
            codex_credentials: None,
        };
        let (_cancel, cancellation) = watch::channel(false);
        let notices = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = notices.clone();
        let outcome = tokio::time::timeout(
            Duration::from_secs(120),
            run_prepared(
                store.clone(),
                RunInput {
                    session,
                    message,
                    config: Config::default(),
                    pane_generation: false,
                },
                cancellation,
                Arc::new(move |event| {
                    if let Progress::Notice(text) = event {
                        seen.lock().unwrap().push(text);
                    }
                }),
                launch,
                FloodProtocol { model },
                Workspace::open_with_coordination(&workspace, &base.join("coordination")).unwrap(),
            ),
        )
        .await
        .expect("the frame limit lands far before the turn deadline")
        .unwrap();
        assert_eq!(outcome.facts.terminal, Terminal::TurnLimit);
        assert_eq!(outcome.facts.failure, None);
        assert!(outcome.facts.joined);
        assert_eq!(outcome.state, State::Idle);
        assert!(store.unsettled_runs().unwrap().is_empty());
        assert!(
            notices
                .lock()
                .unwrap()
                .iter()
                .any(|text| text.contains("frame count")),
            "the turn limit is reported as a bounded diagnostic notice"
        );
    }

    /// Parent liveness and provider settlement are independent facts.
    #[test]
    fn a_live_owner_keeps_its_launch_directory_through_a_sweep() {
        let root = tempfile::tempdir_in("/tmp").unwrap();
        let base = xcb_core::canonical(root.path()).unwrap();
        private::directory(&base.join("runs")).unwrap();

        let live = LaunchArtifacts::create(&base).unwrap();
        let live_path = live.directory.clone();
        std::fs::write(live_path.join("provider"), b"a large snapshot").unwrap();

        let sweep = reclaim_launch_artifacts(&base, false).unwrap();
        assert_eq!(sweep.live, 0);
        assert_eq!(sweep.unprovable, vec![live_path.clone()]);
        assert_eq!(sweep.reclaimed, 0);
        assert!(live_path.exists(), "a live turn's directory was deleted");

        // The owner goes away without removing anything, which is the shape a
        // kill, a crash or a parent teardown leaves behind. `retain` is how
        // this struct expresses "do not delete on drop", so dropping a
        // retained one reproduces an abandoned directory exactly: files still
        // on disk, lock no longer held.
        let mut live = live;
        live.retain_before_launch();
        drop(live);
        assert!(live_path.exists());

        let sweep = reclaim_launch_artifacts(&base, false).unwrap();
        assert_eq!(sweep.reclaimed, 0);
        assert_eq!(sweep.unprovable, vec![live_path.clone()]);
        assert_eq!(sweep.live, 0);
        assert!(live_path.exists());
        let sweep = reclaim_launch_artifacts(&base, true).unwrap();
        assert_eq!(sweep.reclaimed, 0, "--yes cannot prove provider exit");
        assert!(live_path.exists());
    }

    /// A directory from a build that predated the owner lock. Nothing about it
    /// can distinguish "abandoned" from "in use", so routine maintenance must
    /// report it and leave it, even if the operator requests reclamation.
    #[test]
    fn a_directory_without_an_owner_lock_is_reported_not_removed() {
        let root = tempfile::tempdir_in("/tmp").unwrap();
        let base = xcb_core::canonical(root.path()).unwrap();
        private::directory(&base.join("runs")).unwrap();
        let legacy = private::directory(&base.join("runs").join("launch_legacyfixture")).unwrap();
        std::fs::write(legacy.join("provider"), b"0123456789").unwrap();

        let sweep = reclaim_launch_artifacts(&base, false).unwrap();
        assert_eq!(sweep.reclaimed, 0);
        assert_eq!(sweep.live, 0);
        assert_eq!(sweep.unprovable, vec![legacy.clone()]);
        assert_eq!(sweep.unprovable_bytes, 10);
        assert!(legacy.exists());

        let sweep = reclaim_launch_artifacts(&base, true).unwrap();
        assert_eq!(sweep.reclaimed, 0);
        assert_eq!(sweep.unprovable, vec![legacy.clone()]);
        assert!(legacy.exists());
        assert!(
            !legacy.join(LAUNCH_OWNER_LOCK).exists(),
            "inspection creates no lock"
        );
    }

    #[test]
    fn settled_launch_receipts_allow_reclamation_but_dry_run_is_read_only() {
        let root = tempfile::tempdir_in("/tmp").unwrap();
        let base = xcb_core::canonical(root.path()).unwrap();
        let mut artifacts = LaunchArtifacts::create(&base).unwrap();
        let path = artifacts.path().to_path_buf();
        artifacts.retain_before_launch();
        assert!(artifacts.record_reclaimable().is_err());
        artifacts.release_after_join(true, EffectState::Uncertain);
        assert!(artifacts.record_reclaimable().is_err());
        artifacts.release_after_join(false, EffectState::Settled);
        assert!(artifacts.record_reclaimable().is_err());
        artifacts.release_after_join(true, EffectState::Settled);
        artifacts.record_reclaimable().unwrap();
        let held = reclaim_launch_artifacts(&base, true).unwrap();
        assert_eq!(held.live, 1);
        assert_eq!(held.reclaimed, 0);
        // Reproduce a crash after safe Drop sealed the receipt but before
        // directory removal. Suppress Drop's deletion for this fixture only.
        artifacts.retained = true;
        drop(artifacts);
        let receipt = reclamation_receipt(&path);
        let bytes = std::fs::read(&receipt).unwrap();
        let sweep = reclaim_launch_artifacts(&base, false).unwrap();
        assert_eq!(sweep.reclaimable, 1);
        assert_eq!(sweep.reclaimed, 0);
        assert!(path.exists());
        assert_eq!(std::fs::read(&receipt).unwrap(), bytes);
        let sweep = reclaim_launch_artifacts(&base, true).unwrap();
        assert_eq!(sweep.reclaimed, 1);
        assert!(!path.exists());
        assert!(!receipt.exists());
    }

    #[test]
    fn a_replaced_launch_identity_cannot_reuse_a_settlement_receipt() {
        let root = tempfile::tempdir_in("/tmp").unwrap();
        let base = xcb_core::canonical(root.path()).unwrap();
        let mut artifacts = LaunchArtifacts::create(&base).unwrap();
        let path = artifacts.path().to_path_buf();
        artifacts.record_reclaimable().unwrap();
        artifacts.retained = true;
        drop(artifacts);
        let preserved = path.with_extension("preserved");
        std::fs::rename(&path, &preserved).unwrap();
        private::directory(&path).unwrap();
        let owner = open_owner_lock(&path.join(LAUNCH_OWNER_LOCK)).unwrap();
        drop(owner);
        let sweep = reclaim_launch_artifacts(&base, true).unwrap();
        assert_eq!(sweep.reclaimed, 0);
        assert!(path.exists());
        assert!(preserved.exists());
    }

    /// The sweep reads a shared directory, so it must not be steerable by a
    /// name someone else can create there.
    #[test]
    fn the_sweep_ignores_names_it_does_not_own() {
        let root = tempfile::tempdir_in("/tmp").unwrap();
        let base = xcb_core::canonical(root.path()).unwrap();
        let runs = private::directory(&base.join("runs")).unwrap();
        // Not a launch directory at all.
        let unrelated = private::directory(&runs.join("keep_me")).unwrap();
        // A launch-shaped name pointing somewhere else entirely.
        let elsewhere = private::directory(&base.join("elsewhere")).unwrap();
        std::os::unix::fs::symlink(&elsewhere, runs.join("launch_symlink")).unwrap();
        // A launch-shaped name that is a file, not a directory.
        std::fs::write(runs.join("launch_regularfile"), b"x").unwrap();

        let sweep = reclaim_launch_artifacts(&base, true).unwrap();
        assert_eq!(sweep.reclaimed, 0);
        assert!(sweep.unprovable.is_empty());
        assert!(unrelated.exists());
        assert!(elsewhere.exists());
        assert!(runs.join("launch_symlink").symlink_metadata().is_ok());
    }

    /// Reclaiming is not permission to delete: a directory the owner is still
    /// entitled to keep must survive, and the normal Drop path must still be
    /// the thing that removes a finished one.
    #[test]
    fn a_retained_directory_survives_its_owner() {
        let root = tempfile::tempdir_in("/tmp").unwrap();
        let base = xcb_core::canonical(root.path()).unwrap();
        private::directory(&base.join("runs")).unwrap();
        let mut artifacts = LaunchArtifacts::create(&base).unwrap();
        let path = artifacts.directory.clone();
        artifacts.retain_before_launch();
        drop(artifacts);
        assert!(path.exists(), "retained artifacts were dropped");

        let mut released = LaunchArtifacts::create(&base).unwrap();
        let released_path = released.directory.clone();
        released.retain_before_launch();
        released.release_after_join(true, EffectState::None);
        drop(released);
        assert!(!released_path.exists());
    }
}
