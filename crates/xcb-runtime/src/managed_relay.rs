//! The supervisor's relay lane: a linked, admitted machine boots
//! `RelayLane`, polls device-addressed commands on a fixed cadence, and
//! executes them through ordinary `ManagedStore` operations — never a
//! parallel authority. It also publishes the bounded fleet projection
//! controllers read through `xcb fleet` and `xcb attention --remote`.
//!
//! The lane is optional infrastructure: boot failure (offline, not yet
//! admitted, revoked) never blocks local work, and a dead lane reboots on
//! the retry cadence unless custody itself is gone. A revoked device is
//! auth-fatal — the host disables rather than retrying forever.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::sync::watch;
use xcb_core::Id;

use crate::cloud::commands::{self, CommandBody};
use crate::cloud::lane::{self, CommandOutcome, OpenedCommand, RelayLane};
use crate::cloud::{reauth, relay_gate};
use crate::managed::{
    Intake, IntakeCues, ManagedStore, Origin, clear_relay_connection_fault, clear_relay_fault,
    clear_relay_projection_fault, fault_text, record_relay_fault,
};
use crate::{Error, Result, digest};

/// Remote commands land at most this long after a controller posts them.
const POLL_INTERVAL: Duration = Duration::from_secs(2);
/// Lane boot retry after a transport or custody failure. Consecutive
/// failures double the delay up to `BOOT_RETRY_MAX`; one healthy pump
/// resets it.
const BOOT_RETRY: Duration = Duration::from_secs(15);
/// Longest delay between lane reboot attempts.
const BOOT_RETRY_MAX: Duration = Duration::from_secs(300);
/// Fleet projection publish cadence when nothing changed sooner.
const PROJECTION_INTERVAL: Duration = Duration::from_secs(30);
/// Unchanged projections still republish on this bound so `updated_at`
/// proves the lane can write; shared with readers via
/// `cloud::PROJECTION_TOUCH_MS`.
const PROJECTION_TOUCH: Duration = Duration::from_millis(crate::cloud::PROJECTION_TOUCH_MS);
/// Nonterminal task rows a fleet projection carries at most.
const PROJECTION_TASK_ROWS: usize = 64;
/// Single-field character bound inside a projection body.
const PROJECTION_FIELD_CHARS: usize = 200;
/// The projection scope controllers read for fleet state.
const FLEET_SCOPE: &str = "fleet";
/// Relay failures remain visible independently of local worker failures.
/// Repeats are coalesced by the recorder; recovery only clears this record.
fn relay_fault(root: &Path, message: &str) {
    record_relay_fault(
        root,
        message,
        message.starts_with("relay projection ") || message.starts_with("fleet projection "),
    );
}

/// A healthy command pump proves the connection recovered, but says nothing
/// about a failed fleet publication. Only an actual publication clears that
/// failure; otherwise every healthy poll would hide it between publish retries.
fn relay_recovered(root: &Path, projection_published: bool) {
    clear_relay_connection_fault(root);
    if projection_published {
        clear_relay_projection_fault(root);
    }
}

/// The relay lane on its own thread and runtime beside the supervisor loop.
/// Relay calls wait on the network (each bounded at 30 s), so running them
/// inline would stall dispatch, worker settlement and the supervisor
/// heartbeat whenever the relay is slow.
pub(crate) struct RelayTask {
    resident: Arc<AtomicBool>,
    stop: watch::Sender<bool>,
    done: Option<tokio::sync::oneshot::Receiver<()>>,
}

impl RelayTask {
    pub(crate) fn spawn(root: &Path, managed: Arc<ManagedStore>) -> Self {
        // Keep the supervisor until the lane has checked local linkage,
        // including while its thread is starting.
        let resident = Arc::new(AtomicBool::new(true));
        let (stop, stopped) = watch::channel(false);
        let (finished, done) = tokio::sync::oneshot::channel();
        let root = root.to_path_buf();
        let fault_root = managed.root().to_path_buf();
        let lane_resident = resident.clone();
        let spawned = std::thread::Builder::new()
            .name("xcb-relay".into())
            .spawn(move || {
                match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => {
                        runtime.block_on(relay_loop(&root, &managed, lane_resident.clone(), stopped));
                    }
                    Err(_) => relay_fault(
                        managed.root(),
                        "relay lane could not start its runtime; remote commands are unavailable until the supervisor restarts",
                    ),
                }
                lane_resident.store(false, Ordering::Relaxed);
                let _ = finished.send(());
            });
        if spawned.is_err() {
            resident.store(false, Ordering::Relaxed);
            relay_fault(
                &fault_root,
                "relay lane could not start its thread; remote commands are unavailable until the supervisor restarts",
            );
        }
        Self {
            resident,
            stop,
            done: spawned.ok().map(|_| done),
        }
    }

    /// A linked machine stays resident through connection and retry waits,
    /// so a temporary outage cannot strand its remote command queue.
    pub(crate) fn keeps_resident(&self) -> bool {
        self.resident.load(Ordering::Relaxed)
    }

    /// Stop polling and drop the presence row. An in-flight pass gets
    /// `within` to finish; past that the supervisor exits without it, and
    /// the relay closes an interrupted command as ambiguous on the next
    /// boot, never as applied.
    pub(crate) async fn shutdown(self, within: Duration) {
        let _ = self.stop.send(true);
        if let Some(done) = self.done {
            let _ = tokio::time::timeout(within, done).await;
        }
    }
}

async fn relay_loop(
    root: &Path,
    managed: &Arc<ManagedStore>,
    resident: Arc<AtomicBool>,
    mut stopped: watch::Receiver<bool>,
) {
    let mut host = RelayHost::new(root);
    host.resident = resident;
    let mut poll = tokio::time::interval(host.poll_interval());
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            biased;
            _ = stopped.changed() => break,
            _ = poll.tick() => {
                host.tick(managed).await;
            }
        }
    }
    host.shutdown().await;
}

/// The relay-lane host owned by the supervisor loop. Holds the lane plus
/// retry/projection scheduling state; `tick` is the only entry point.
pub struct RelayHost {
    lane: Option<RelayLane>,
    /// The supervisor's residency signal, published before any network
    /// wait and retained through retry backoff. This is not connectivity.
    resident: Arc<AtomicBool>,
    root: PathBuf,
    next_boot: Instant,
    /// Delay applied to the next reboot attempt; grows on consecutive
    /// failures and resets on a healthy pump.
    boot_delay: Duration,
    disabled: bool,
    /// Only an authentication failure may recover after an explicit,
    /// committed sign-in. Device revocation and class rejection stay latched.
    reauth_recoverable: bool,
    reauth_generation: Option<String>,
    projection_due: bool,
    projection_next: Instant,
    /// Last published fleet revision — the CAS pin for the next write.
    fleet_revision: u64,
    /// Digest of the last published plaintext; unchanged projections are
    /// not re-posted until the touch bound expires.
    fleet_fingerprint: Option<String>,
    /// When the last projection write landed; an unchanged body is
    /// republished once `PROJECTION_TOUCH` elapses.
    projection_written: Instant,
}

impl RelayHost {
    pub fn new(root: &Path) -> Self {
        Self {
            lane: None,
            resident: Arc::new(AtomicBool::new(false)),
            root: root.to_path_buf(),
            next_boot: Instant::now(),
            boot_delay: BOOT_RETRY,
            disabled: false,
            reauth_recoverable: false,
            reauth_generation: None,
            projection_due: false,
            projection_next: Instant::now() + PROJECTION_INTERVAL,
            fleet_revision: 0,
            fleet_fingerprint: None,
            projection_written: Instant::now(),
        }
    }

    /// How long until the next relay poll — the `select!` arm sleeps this
    /// long.
    pub fn poll_interval(&self) -> Duration {
        POLL_INTERVAL
    }

    /// True while a lane is connected. A linked machine also remains
    /// resident during boot and recovery, when this returns false.
    #[cfg(test)]
    fn live(&self) -> bool {
        self.lane.is_some()
    }

    /// One relay pass: boot when needed, pump the command queue, publish
    /// the fleet projection when due.
    pub async fn tick(&mut self, managed: &Arc<ManagedStore>) {
        self.tick_with(managed, RelayLane::boot).await;
    }

    async fn tick_with(
        &mut self,
        managed: &Arc<ManagedStore>,
        boot: impl AsyncFnOnce(lane::LaneKeys) -> Result<RelayLane>,
    ) {
        // This guard spans every await in the pass, including boot,
        // command effects, acknowledgements, and projection retries. A
        // transition waits for completion; it never cancels this future.
        let _pump = match relay_gate::try_pump(&self.root) {
            Ok(Some(guard)) => guard,
            Ok(None) => return,
            Err(error) => {
                relay_fault(
                    managed.root(),
                    &format!("relay pause check failed: {}", fault_text(&error)),
                );
                return;
            }
        };
        match reauth::relay_state(&self.root) {
            Ok(reauth::RelayReauthState::Pending) => return,
            Ok(reauth::RelayReauthState::Ready { generation }) => {
                self.observe_reauth(generation);
            }
            Err(error) => {
                relay_fault(
                    managed.root(),
                    &format!("relay sign-in check failed: {}", fault_text(&error)),
                );
                return;
            }
        }
        if self.lane.is_none() {
            if self.disabled || Instant::now() < self.next_boot {
                return;
            }
            self.boot_with(managed, boot).await;
            return;
        }

        let lane = match self.lane.as_mut() {
            Some(lane) => lane,
            None => return,
        };
        let refresh = AtomicBool::new(false);
        let mut handler =
            async |opened: &OpenedCommand| execute_command(managed, opened, &refresh).await;
        if let Err(error) = lane.pump(&mut handler).await {
            if fatal(&error) {
                self.disabled = true;
                self.reauth_recoverable = authentication_failure(&error);
                self.resident.store(false, Ordering::Relaxed);
            }
            relay_fault(
                managed.root(),
                &format!("relay lane pump failed: {}", fault_text(&error)),
            );
            self.lane = None;
            self.next_boot = Instant::now() + self.boot_delay;
            self.boot_delay = (self.boot_delay * 2).min(BOOT_RETRY_MAX);
            return;
        }
        // A full pump without an error proves the lane healthy — reset
        // the reboot delay.
        self.boot_delay = BOOT_RETRY;
        relay_recovered(managed.root(), false);
        if refresh.load(Ordering::Relaxed) {
            self.projection_due = true;
        }

        let now = Instant::now();
        if !self.projection_due && now < self.projection_next {
            return;
        }
        match fleet_projection(managed) {
            Ok(plaintext) => {
                let fingerprint = digest(&plaintext);
                if self.fleet_fingerprint.as_deref() == Some(fingerprint.as_str())
                    && self.projection_written.elapsed() < PROJECTION_TOUCH
                {
                    self.projection_due = false;
                    self.projection_next = Instant::now() + PROJECTION_INTERVAL;
                    return;
                }
                let mut result = lane
                    .publish_projection(FLEET_SCOPE, &plaintext, self.fleet_revision)
                    .await;
                if matches!(result, Err(Error::Protocol("relay conflict"))) {
                    // The CAS pin is stale — a restart or a racing writer
                    // moved the row. Resync and retry once; a publish
                    // failure says nothing about lane health.
                    match lane.projection_revision(FLEET_SCOPE).await {
                        Ok(revision) => {
                            self.fleet_revision = revision;
                            result = lane
                                .publish_projection(FLEET_SCOPE, &plaintext, revision)
                                .await;
                        }
                        Err(resync) => result = Err(resync),
                    }
                }
                match result {
                    Ok(revision) => {
                        self.fleet_revision = revision;
                        self.fleet_fingerprint = Some(fingerprint);
                        self.projection_written = Instant::now();
                        self.projection_due = false;
                        self.projection_next = Instant::now() + PROJECTION_INTERVAL;
                        relay_recovered(managed.root(), true);
                    }
                    Err(error) => {
                        if fatal(&error) {
                            self.disabled = true;
                            self.reauth_recoverable = authentication_failure(&error);
                            self.resident.store(false, Ordering::Relaxed);
                            self.lane = None;
                        }
                        relay_fault(
                            managed.root(),
                            &format!("relay projection failed: {}", fault_text(&error)),
                        );
                        self.projection_next = Instant::now() + PROJECTION_INTERVAL;
                    }
                }
            }
            Err(error) => {
                relay_fault(
                    managed.root(),
                    &format!("fleet projection build failed: {}", fault_text(&error)),
                );
                self.projection_next = Instant::now() + PROJECTION_INTERVAL;
            }
        }
    }

    fn observe_reauth(&mut self, generation: Option<String>) {
        let Some(generation) = generation else {
            return;
        };
        if self.reauth_generation.as_ref() == Some(&generation) {
            return;
        }
        self.reauth_generation = Some(generation);
        if self.disabled && !self.reauth_recoverable {
            return;
        }
        // The old lane holds the previous session in memory. Reboot with
        // committed custody, resetting projection CAS state along with it.
        self.lane = None;
        self.disabled = false;
        self.reauth_recoverable = false;
        self.next_boot = Instant::now();
        self.boot_delay = BOOT_RETRY;
        self.projection_due = true;
        self.fleet_revision = 0;
        self.fleet_fingerprint = None;
    }

    async fn boot_with(
        &mut self,
        managed: &Arc<ManagedStore>,
        boot: impl AsyncFnOnce(lane::LaneKeys) -> Result<RelayLane>,
    ) {
        let fault = match lane::load_lane_keys(&self.root) {
            Ok(Some(keys)) => {
                // A single call can outlast the supervisor's idle window.
                // Publish linkage before awaiting it, and keep retrying
                // while this device remains authorized.
                self.resident.store(true, Ordering::Relaxed);
                match boot(keys).await {
                    Ok(mut lane) => {
                        // Seed the CAS pin from the row a previous boot
                        // may have left behind.
                        if let Ok(revision) = lane.projection_revision(FLEET_SCOPE).await {
                            self.fleet_revision = revision;
                        }
                        self.lane = Some(lane);
                        return;
                    }
                    Err(error) => {
                        if fatal(&error) {
                            self.disabled = true;
                            self.reauth_recoverable = authentication_failure(&error);
                            self.resident.store(false, Ordering::Relaxed);
                        }
                        Some(format!("relay lane boot failed: {}", fault_text(&error)))
                    }
                }
            }
            Ok(None) => {
                self.resident.store(false, Ordering::Relaxed);
                clear_relay_fault(managed.root());
                None
            }
            Err(error) => Some(format!("relay lane custody failed: {}", fault_text(&error))),
        };
        // Back off from the completed attempt: a slow network failure
        // must not spend the retry delay while the request is in flight.
        self.next_boot = Instant::now() + self.boot_delay;
        if let Some(fault) = fault {
            self.boot_delay = (self.boot_delay * 2).min(BOOT_RETRY_MAX);
            relay_fault(managed.root(), &fault);
        }
    }

    /// Drop the lane's presence row on supervisor shutdown.
    pub async fn shutdown(&mut self) {
        self.resident.store(false, Ordering::Relaxed);
        // A transition owns session mutation. Dropping an old idle lane
        // leaves presence to expire without racing its committed sign-in.
        let Ok(Some(_pump)) = relay_gate::try_pump(&self.root) else {
            self.lane = None;
            return;
        };
        if !matches!(
            reauth::relay_state(&self.root),
            Ok(reauth::RelayReauthState::Ready { .. })
        ) {
            self.lane = None;
            return;
        }
        if let Some(mut lane) = self.lane.take() {
            let _ = lane.disconnect().await;
        }
    }
}

impl Drop for RelayHost {
    fn drop(&mut self) {
        self.resident.store(false, Ordering::Relaxed);
    }
}

/// Relay errors that mean the lane can never recover under this custody —
/// a revoked device or a dead session binding. The codes are the relay's
/// closed vocabulary (`@hraness/relay` `wire/errors.ts`).
fn fatal(error: &crate::Error) -> bool {
    matches!(
        error,
        crate::Error::Protocol(
            "relay unauthenticated" | "relay forbidden-device-class" | "relay revoked-device"
        )
    )
}

fn authentication_failure(error: &crate::Error) -> bool {
    matches!(error, Error::Protocol("relay unauthenticated"))
}

/// Map one opened command onto the managed surface and produce the sealed
/// result payload. Every failure path still settles `failed` — a rejected
/// body or a missing task is a terminal answer, not a retry.
async fn execute_command(
    managed: &Arc<ManagedStore>,
    opened: &OpenedCommand,
    refresh: &AtomicBool,
) -> Result<CommandOutcome> {
    let body = match commands::decode(&opened.plaintext) {
        Ok(body) if commands::kind_of(&body) == opened.kind => body,
        _ => {
            return Ok(failed("payload-rejected", "command body rejected"));
        }
    };
    let operation = Id::new(format!("m_remote_{}", opened.public_id))?;
    let result = match &body {
        CommandBody::TaskDispatch { workspace, prompt } => {
            dispatch(managed, workspace, prompt, &operation).await
        }
        CommandBody::TaskSteer { task, text } => {
            let task = managed.resolve_task(&Id::new(task.clone())?)?;
            managed
                .steer_task(&task, operation, text.clone())
                .map(|_| json!({"steered": task.as_str()}))
        }
        CommandBody::TaskCancel { task } => {
            let task = managed.resolve_task(&Id::new(task.clone())?)?;
            match managed.task(&task)? {
                Some(row) if !row.state.terminal() => managed
                    .cancel_task(&task, row.revision)
                    .await
                    .map(|_| json!({"cancelled": task.as_str()})),
                Some(_) => Ok(json!({"alreadyTerminal": task.as_str()})),
                None => Err(Error::Unavailable("managed task not found")),
            }
        }
        CommandBody::AttentionAnswer { attention, answer } => {
            let task = managed.resolve_task(&Id::new(attention.clone())?)?;
            managed
                .reply_to_task(&task, answer.clone())
                .await
                .map(|_| json!({"answered": task.as_str()}))
        }
        CommandBody::DaemonSend { daemon, text } => managed
            .daemon_send(daemon, text)
            .map(|_| json!({"sent": daemon})),
        CommandBody::ProjectionRefresh => {
            refresh.store(true, Ordering::Relaxed);
            Ok(json!({"refresh": "queued"}))
        }
    };
    match result {
        Ok(value) => Ok(CommandOutcome::Applied {
            result_code: "done".to_string(),
            plaintext: serde_json::to_vec(&value).map_err(crate::Error::Json)?,
        }),
        Err(error) => Ok(CommandOutcome::Failed {
            result_code: "effect-failed".to_string(),
            plaintext: serde_json::to_vec(&json!({
                "error": xcb_core::display_text(&fault_text(&error), 400)
            }))
            .map_err(crate::Error::Json)?,
        }),
    }
}

fn failed(result_code: &str, detail: &str) -> CommandOutcome {
    CommandOutcome::Failed {
        result_code: result_code.to_string(),
        plaintext: detail.as_bytes().to_vec(),
    }
}

/// The wire `workspace` value that asks the device to infer the project.
const INFER: &str = "@infer";

/// Task dispatch lands in the device's thread. The wire `workspace` is an
/// absolute path (bound exactly, never snapped), a known project name (never
/// resolved against the supervisor's cwd) or `@infer`. A retried operation
/// replays its committed task.
async fn dispatch(
    managed: &Arc<ManagedStore>,
    workspace: &str,
    prompt: &str,
    operation: &Id,
) -> Result<Value> {
    let result = |task: &crate::managed::ManagedTask, source: &str| {
        json!({
            "conversation": task.conversation.as_str(),
            "dispatched": true,
            "task": task.id.as_str(),
            "workspace": task.workspace,
            "workspaceSource": source,
        })
    };
    // A retry replays its committed task before the name lookup or the
    // admission below can fail on a registry that changed since.
    if let Some((task, binding)) = managed.replay_thread_submission(operation, prompt)? {
        return Ok(result(&task, binding.source.as_str()));
    }
    let mut cues = IntakeCues {
        origin: Origin::Relay,
        explicit: None,
        target: None,
        focus: None,
        launch_hint: None,
        infer_only: false,
    };
    if workspace == INFER {
        cues.infer_only = true;
    } else if Path::new(workspace).is_absolute() {
        // A controller naming a directory is an explicit act: admit it.
        // The canonical spelling is what the task records.
        let path = managed.admit_workspace(Path::new(workspace), "dispatch", None)?;
        cues.explicit = Some(PathBuf::from(path));
    } else {
        cues.explicit = Some(PathBuf::from(named_workspace(managed, workspace)?));
    }
    match managed
        .submit_to_thread(operation.clone(), prompt.to_string(), vec![], cues)
        .await?
    {
        Intake::Accepted { task, binding, .. } => Ok(result(&task, binding.source.as_str())),
        Intake::Ask { candidates, reason } => {
            let names: Vec<&str> = candidates.iter().map(|row| row.name.as_str()).collect();
            Err(Error::Guided {
                message: if names.is_empty() {
                    format!("workspace ambiguous: {reason}")
                } else {
                    format!("workspace ambiguous: {}", names.join(", "))
                },
                next: None,
            })
        }
    }
}

/// A relative wire workspace is a registry name with exactly one hit.
fn named_workspace(managed: &ManagedStore, name: &str) -> Result<String> {
    match managed.lookup_name(name)?.as_slice() {
        [only] => Ok(only.clone()),
        [] => {
            let mut known: Vec<String> = managed
                .known_workspaces(8)?
                .into_iter()
                .filter(|entry| !entry.container)
                .map(|entry| entry.name)
                .collect();
            known.sort();
            Err(Error::Guided {
                message: if known.is_empty() {
                    format!("no project named `{name}` on this device")
                } else {
                    format!(
                        "no project named `{name}` on this device; known projects: {}",
                        known.join(", ")
                    )
                },
                next: None,
            })
        }
        several => Err(Error::Guided {
            message: format!("`{name}` names several projects: {}", several.join(", ")),
            next: None,
        }),
    }
}

/// Fleet projection schema version; `xcb` and `capabilities` are additive.
const FLEET_VERSION: u64 = 1;
/// What this build accepts in `task_dispatch.workspace` besides an absolute
/// path, so a controller can gate names and `@infer` per device.
const FLEET_CAPABILITIES: [&str; 3] = ["thread", "workspace-names", "infer"];

/// The bounded `fleet` projection: device id, nonterminal tasks and open
/// attention. Ids and states only — titles and details truncate hard.
fn fleet_projection(managed: &Arc<ManagedStore>) -> Result<Vec<u8>> {
    let tasks = managed.tasks(256)?;
    let mut rows = Vec::new();
    let mut attention = Vec::new();
    let mut counts: BTreeMap<String, u64> = BTreeMap::new();
    for task in &tasks {
        let state = task.state.as_str().to_string();
        *counts.entry(state.clone()).or_default() += 1;
        if task.state.terminal() {
            continue;
        }
        if rows.len() < PROJECTION_TASK_ROWS {
            rows.push(json!({
                "task": task.id.as_str(),
                "state": state,
                "workspace": task.workspace,
                // A task without a binding was bound by its project view.
                "workspaceSource": task
                    .binding
                    .as_ref()
                    .map_or("explicit", |binding| binding.source.as_str()),
            }));
        }
        if task.attention.is_some() && attention.len() < PROJECTION_TASK_ROWS {
            attention.push(json!({
                "task": task.id.as_str(),
                "detail": xcb_core::display_text(&task.detail, PROJECTION_FIELD_CHARS),
            }));
        }
    }
    fleet_body(counts, rows, attention)
}

/// Serialize the fleet body, dropping the newest-listed rows until it fits
/// the relay's plaintext bound. Counts always survive.
fn fleet_body(
    counts: BTreeMap<String, u64>,
    mut rows: Vec<Value>,
    mut attention: Vec<Value>,
) -> Result<Vec<u8>> {
    loop {
        let body = serde_json::to_vec(&json!({
            "version": FLEET_VERSION,
            "xcb": env!("CARGO_PKG_VERSION"),
            "capabilities": FLEET_CAPABILITIES,
            "counts": counts,
            "tasks": rows,
            "attention": attention,
        }))
        .map_err(crate::Error::Json)?;
        if body.len() <= lane::MAX_PROJECTION_PLAINTEXT || (rows.is_empty() && attention.is_empty())
        {
            return Ok(body);
        }
        if attention.len() > rows.len() {
            attention.pop();
        } else {
            rows.pop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::now_ms;
    use crate::workspace_infer::{BindingConfidence, BindingOrigin, BindingSource};
    use std::sync::Arc;
    use tempfile::TempDir;
    use xcb_core::ui::GLOBAL_THREAD_ID;

    struct Fixture {
        _temp: TempDir,
        base: PathBuf,
        store: Arc<ManagedStore>,
    }

    /// Custody rejects symlinked ancestors (/var → /private/var on macOS),
    /// so the state root itself must be canonical; workspaces live beside it.
    fn fixture() -> Fixture {
        let temp = TempDir::new().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let store = Arc::new(ManagedStore::open(&base.join("state")).unwrap());
        Fixture {
            _temp: temp,
            base,
            store,
        }
    }

    impl Fixture {
        fn dir(&self, name: &str) -> PathBuf {
            let path = self.base.join(name);
            std::fs::create_dir_all(&path).unwrap();
            path.canonicalize().unwrap()
        }

        async fn dispatch(&self, workspace: &str, prompt: &str, operation: &str) -> Result<Value> {
            dispatch(
                &self.store,
                workspace,
                prompt,
                &Id::new(format!("m_remote_{operation}")).unwrap(),
            )
            .await
        }

        fn task(&self, result: &Value) -> crate::managed::ManagedTask {
            let id = Id::new(result["task"].as_str().unwrap()).unwrap();
            self.store.task(&id).unwrap().unwrap()
        }

        fn projection(&self) -> Value {
            serde_json::from_slice(&fleet_projection(&self.store).unwrap()).unwrap()
        }
    }

    fn text(path: &Path) -> &str {
        path.to_str().unwrap()
    }

    fn error_text(result: Result<Value>) -> String {
        result.unwrap_err().to_string()
    }

    /// The store records canonical workspace paths; a spelled alias must
    /// resolve to the same bytes or the submit conflicts.
    #[tokio::test]
    async fn dispatch_accepts_noncanonical_workspace_spelling() {
        let f = fixture();
        let workspace = f.dir("work");
        // A symlinked workspace is the /tmp → /private/tmp shape the bug
        // came from: the alias exists but spells differently.
        let alias = f.base.join("ws-link");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&workspace, &alias).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&workspace, &alias).unwrap();
        let result = f
            .dispatch(text(&alias), "remote prompt", "test")
            .await
            .unwrap();
        assert_eq!(result["dispatched"], json!(true));
        assert_eq!(result["conversation"], json!(GLOBAL_THREAD_ID));
        assert_eq!(result["workspace"], json!(text(&workspace)));
        assert_eq!(f.task(&result).workspace, text(&workspace));
    }

    #[tokio::test]
    async fn two_dispatches_share_the_thread() {
        let f = fixture();
        let a = f.dir("a");
        let b = f.dir("b");
        let first = f.dispatch(text(&a), "first", "one").await.unwrap();
        let second = f.dispatch(text(&b), "second", "two").await.unwrap();
        assert_eq!(first["conversation"], json!(GLOBAL_THREAD_ID));
        assert_eq!(second["conversation"], json!(GLOBAL_THREAD_ID));
        assert_ne!(first["task"], second["task"]);
        let conversations = f.store.conversations(16).unwrap();
        assert_eq!(conversations.len(), 1, "no per-directory view is created");
        assert!(conversations[0].workspace.is_none());
    }

    #[tokio::test]
    async fn explicit_subdirectory_is_not_snapped() {
        let f = fixture();
        let repo = f.dir("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let sub = f.dir("repo/sub");
        let result = f
            .dispatch(text(&sub), "in the subdirectory", "sub")
            .await
            .unwrap();
        assert_eq!(result["workspace"], json!(text(&sub)));
        assert_eq!(f.task(&result).workspace, text(&sub));
    }

    /// A relative value is a registry name, never a path under the
    /// supervisor's inherited cwd (the test binary runs in the crate, where
    /// `src` exists).
    #[tokio::test]
    async fn relative_workspace_is_name_lookup_not_cwd() {
        let f = fixture();
        assert!(std::path::Path::new("src").is_dir());
        let refused = error_text(f.dispatch("src", "no cwd lookup", "cwd").await);
        assert!(refused.contains("no project named `src`"), "{refused}");
        let named = f.dir("elsewhere/proj");
        f.store.admit_workspace(&named, "command", None).unwrap();
        let result = f.dispatch("proj", "by name", "name").await.unwrap();
        assert_eq!(result["workspace"], json!(text(&named)));
        assert_eq!(result["workspaceSource"], json!("explicit"));
        assert!(f.store.tasks(16).unwrap().len() == 1);
    }

    /// A retried dispatch replays its committed task even when the name it
    /// used has since become ambiguous or the directory is gone (I3).
    #[tokio::test]
    async fn retried_dispatch_replays_before_resolving() {
        let f = fixture();
        let one = f.dir("one/proj");
        f.store.admit_workspace(&one, "command", None).unwrap();
        let first = f.dispatch("proj", "by name", "retry").await.unwrap();
        let two = f.dir("two/proj");
        f.store.admit_workspace(&two, "command", None).unwrap();
        let again = f.dispatch("proj", "by name", "retry").await.unwrap();
        assert_eq!(again, first);
        let gone = f.dir("gone");
        let absolute = f.dispatch(text(&gone), "absolute", "gone").await.unwrap();
        std::fs::remove_dir(&gone).unwrap();
        let again = f.dispatch(text(&gone), "absolute", "gone").await.unwrap();
        assert_eq!(again, absolute);
        assert_eq!(f.store.tasks(16).unwrap().len(), 2);
    }

    #[tokio::test]
    async fn ambiguous_name_rejected_with_candidates() {
        let f = fixture();
        let one = f.dir("one/proj");
        let two = f.dir("two/proj");
        f.store.admit_workspace(&one, "command", None).unwrap();
        f.store.admit_workspace(&two, "command", None).unwrap();
        let refused = error_text(f.dispatch("proj", "which one?", "ambiguous").await);
        assert!(refused.contains("names several projects"), "{refused}");
        assert!(refused.contains(text(&one)), "{refused}");
        assert!(refused.contains(text(&two)), "{refused}");
        assert!(f.store.tasks(16).unwrap().is_empty());
    }

    /// Needs the intake lane's name-mention rung.
    #[tokio::test]
    async fn infer_sentinel_binds_named_project_and_fails_ambiguous() {
        let f = fixture();
        let alpha = f.dir("alpha");
        let beta = f.dir("beta");
        f.store.admit_workspace(&alpha, "command", None).unwrap();
        f.store.admit_workspace(&beta, "command", None).unwrap();
        let result = f
            .dispatch(INFER, "fix the flaky parser test in alpha", "mention")
            .await
            .unwrap();
        assert_eq!(result["workspace"], json!(text(&alpha)));
        assert_eq!(result["workspaceSource"], json!("mention"));
        // Two unrelated directories share the name: the device asks.
        let other = f.dir("other/alpha");
        f.store.admit_workspace(&other, "command", None).unwrap();
        let refused = error_text(
            f.dispatch(INFER, "fix the flaky parser test in alpha", "twice")
                .await,
        );
        assert!(refused.starts_with("workspace ambiguous: "), "{refused}");
        assert!(refused.contains("alpha"), "{refused}");
    }

    /// A short remote prompt never binds to whatever ran last; it asks.
    #[tokio::test]
    async fn infer_short_prompt_without_cue_asks() {
        let f = fixture();
        let alpha = f.dir("alpha");
        let beta = f.dir("beta");
        f.store.admit_workspace(&beta, "command", None).unwrap();
        f.dispatch(text(&alpha), "earlier relay work", "earlier")
            .await
            .unwrap();
        let refused = error_text(f.dispatch(INFER, "run the tests", "short").await);
        assert!(refused.starts_with("workspace ambiguous: "), "{refused}");
        assert!(
            refused.contains("alpha") && refused.contains("beta"),
            "{refused}"
        );
        assert_eq!(f.store.tasks(16).unwrap().len(), 1, "an ask writes nothing");
    }

    #[tokio::test]
    async fn home_workspace_refused() {
        let f = fixture();
        let home = std::env::var("HOME").unwrap();
        let refused = error_text(f.dispatch(&home, "anything", "home").await);
        assert!(refused.contains("workspace is not allowed"), "{refused}");
        assert!(f.store.tasks(16).unwrap().is_empty());
        assert!(f.store.all_workspaces().unwrap().is_empty());
    }

    #[tokio::test]
    async fn hidden_home_workspace_refused() {
        let f = fixture();
        let home = PathBuf::from(std::env::var("HOME").unwrap());
        let Ok(hidden) = tempfile::Builder::new()
            .prefix(".xcb-relay-")
            .tempdir_in(&home)
        else {
            return;
        };
        let inside = hidden.path().join("repo");
        std::fs::create_dir_all(&inside).unwrap();
        for path in [hidden.path(), inside.as_path()] {
            let refused = error_text(f.dispatch(text(path), "anything", "hidden").await);
            assert!(refused.contains("hidden or library directory"), "{refused}");
        }
        assert!(f.store.tasks(16).unwrap().is_empty());
    }

    /// I9: every key a 0.8 controller read is still there with its meaning.
    #[tokio::test]
    async fn result_keys_are_additive() {
        let f = fixture();
        let work = f.dir("work");
        let result = f.dispatch(text(&work), "keys", "keys").await.unwrap();
        let keys: Vec<&str> = result
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        for key in [
            "conversation",
            "dispatched",
            "task",
            "workspace",
            "workspaceSource",
        ] {
            assert!(keys.contains(&key), "{key} missing from {result}");
        }
        assert_eq!(keys.len(), 5, "{result}");
        assert!(result["conversation"].is_string());
        assert_eq!(result["dispatched"], json!(true));
        assert!(result["task"].as_str().unwrap().starts_with("t_"));
    }

    #[tokio::test]
    async fn fleet_rows_carry_workspace_source() {
        let f = fixture();
        let work = f.dir("work");
        let view_dir = f.dir("view");
        f.dispatch(text(&work), "thread task", "fleet")
            .await
            .unwrap();
        let view = f.store.create_conversation(&view_dir).await.unwrap();
        f.store
            .submit_new(
                &view.id,
                Id::new("m_view_task").unwrap(),
                "view task".into(),
                vec![],
                &view_dir,
            )
            .await
            .unwrap();
        let projection = f.projection();
        let rows = projection["tasks"].as_array().unwrap();
        assert_eq!(rows.len(), 2, "{projection}");
        for row in rows {
            assert_eq!(row["workspaceSource"], json!("explicit"), "{row}");
            for key in ["task", "state", "workspace"] {
                assert!(row.get(key).is_some(), "{key} missing from {row}");
            }
        }
    }

    #[tokio::test]
    async fn fleet_projection_carries_version_and_capabilities() {
        let f = fixture();
        let projection = f.projection();
        assert_eq!(projection["version"], json!(1));
        assert_eq!(projection["xcb"], json!(env!("CARGO_PKG_VERSION")));
        assert_eq!(
            projection["capabilities"],
            json!(["thread", "workspace-names", "infer"])
        );
        for key in ["counts", "tasks", "attention"] {
            assert!(projection.get(key).is_some(), "{key} missing");
        }
    }

    #[test]
    fn fleet_body_stays_within_the_relay_bound() {
        let long = format!("/{}", "w".repeat(4000));
        let rows: Vec<Value> = (0..PROJECTION_TASK_ROWS)
            .map(|n| json!({"task": format!("t_{n}"), "state": "queued", "workspace": long, "workspaceSource": "explicit"}))
            .collect();
        let attention: Vec<Value> = (0..PROJECTION_TASK_ROWS)
            .map(
                |n| json!({"task": format!("t_{n}"), "detail": "d".repeat(PROJECTION_FIELD_CHARS)}),
            )
            .collect();
        let mut counts = BTreeMap::new();
        counts.insert("queued".to_string(), 64);
        let body = fleet_body(counts, rows, attention).unwrap();
        assert!(body.len() <= lane::MAX_PROJECTION_PLAINTEXT);
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["counts"]["queued"], json!(64));
        assert!(!value["tasks"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn repeated_operation_replays() {
        let f = fixture();
        let work = f.dir("work");
        let first = f
            .dispatch(text(&work), "same prompt", "again")
            .await
            .unwrap();
        let second = f
            .dispatch(text(&work), "same prompt", "again")
            .await
            .unwrap();
        assert_eq!(first, second);
        assert_eq!(f.store.tasks(16).unwrap().len(), 1);
    }

    #[tokio::test]
    async fn relay_tasks_carry_relay_origin_binding() {
        let f = fixture();
        let work = f.dir("work");
        let result = f.dispatch(text(&work), "bound", "origin").await.unwrap();
        let binding = f.task(&result).binding.unwrap();
        assert_eq!(binding.origin, BindingOrigin::Relay);
        assert_eq!(binding.source, BindingSource::Explicit);
        assert_eq!(binding.confidence, BindingConfidence::High);
        let entry = f
            .store
            .all_workspaces()
            .unwrap()
            .into_iter()
            .find(|entry| entry.path == text(&work))
            .unwrap();
        assert_eq!(entry.admitted_by, "dispatch");
    }

    /// The lane runs beside the supervisor: an unlinked machine never goes
    /// live, and shutdown returns promptly.
    #[tokio::test]
    async fn the_relay_lane_runs_beside_the_supervisor_and_stops_promptly() {
        let f = fixture();
        let lane = RelayTask::spawn(&f.base.join("state"), f.store.clone());
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!lane.keeps_resident(), "no custody, no lane");
        let started = Instant::now();
        lane.shutdown(Duration::from_secs(5)).await;
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    fn linked_fixture() -> Fixture {
        use crate::cloud::crypto::{AccountKey, DeviceIdentity};
        use crate::cloud::custody::{self, CloudSession, RelayLink};

        let f = fixture();
        let root = f.base.join("state");
        let device = DeviceIdentity::generate().unwrap();
        custody::store_device(&root, &device, "executor", "fixture", &device.device).unwrap();
        custody::store_account_key(&root, &AccountKey::generate(), 1).unwrap();
        custody::store_session(
            &root,
            &CloudSession::issue("fixture-token".into(), "fixture-refresh".into(), now_ms()),
        )
        .unwrap();
        custody::store_link(
            &root,
            &RelayLink {
                deployment_url: "https://fixture.invalid".into(),
                boot_generation: 0,
            },
        )
        .unwrap();
        f
    }

    /// The idle supervisor must survive both a connection call longer
    /// than its idle window and retry delays that grow beyond that window.
    /// The injected boot never opens a network connection.
    #[tokio::test]
    async fn a_linked_relay_keeps_the_supervisor_during_slow_boot_and_retries() {
        let f = linked_fixture();
        let mut host = RelayHost::new(&f.base.join("state"));
        let resident = host.resident.clone();
        tokio::select! {
            _ = host.boot_with(&f.store, async |_| std::future::pending().await) => {
                panic!("synthetic boot never completes");
            }
            _ = tokio::time::sleep(Duration::from_millis(20)) => {}
        }
        assert!(resident.load(Ordering::Relaxed), "connection is in flight");
        assert!(!host.live(), "residency does not claim connectivity");

        for _ in 0..8 {
            let delay = host.boot_delay;
            let before = Instant::now();
            host.boot_with(&f.store, async |_| {
                Err(Error::Unavailable("synthetic offline relay"))
            })
            .await;
            assert!(resident.load(Ordering::Relaxed), "retry must stay alive");
            assert!(!host.disabled);
            assert!(host.next_boot >= before + delay);
            assert!(host.boot_delay <= BOOT_RETRY_MAX);
            // Ordinary ticks inside backoff neither connect nor clear
            // residency, including the five-minute maximum delay.
            host.tick(&f.store).await;
            assert!(resident.load(Ordering::Relaxed));
        }
        assert_eq!(host.boot_delay, BOOT_RETRY_MAX);
        drop(host);
        assert!(
            !resident.load(Ordering::Relaxed),
            "a stopped lane releases residency"
        );
    }

    #[tokio::test]
    async fn revoked_or_unlinked_relays_allow_the_supervisor_to_idle_exit() {
        let f = linked_fixture();
        let root = f.base.join("state");
        let mut host = RelayHost::new(&root);
        host.boot_with(&f.store, async |_| {
            Err(Error::Protocol("relay revoked-device"))
        })
        .await;
        assert!(host.disabled);
        assert!(!host.resident.load(Ordering::Relaxed));
        assert!(
            crate::managed::relay_fault(f.store.root())
                .is_some_and(|fault| fault.contains("revoked-device")),
            "a revoked device remains visible until linkage is removed"
        );

        let mut host = RelayHost::new(&root);
        host.boot_with(&f.store, async |_| {
            Err(Error::Unavailable("synthetic offline relay"))
        })
        .await;
        assert!(host.resident.load(Ordering::Relaxed));
        crate::cloud::custody::clear_session(&root).unwrap();
        host.next_boot = Instant::now();
        host.tick(&f.store).await;
        assert!(
            !host.resident.load(Ordering::Relaxed),
            "linkage was removed"
        );
        assert!(crate::managed::relay_fault(f.store.root()).is_none());
    }

    #[tokio::test]
    async fn reauth_waits_for_the_full_boot_pass_while_local_work_remains_available() {
        let f = linked_fixture();
        let root = f.base.join("state");
        let managed = f.store.clone();
        let pass_root = root.clone();
        let (entered, started) = tokio::sync::oneshot::channel();
        let (release, finish) = tokio::sync::oneshot::channel();
        let pass = async move {
            let mut host = RelayHost::new(&pass_root);
            host.tick_with(&managed, async move |_| {
                entered.send(()).unwrap();
                finish.await.unwrap();
                Err(Error::Unavailable("synthetic completed boot"))
            })
            .await;
            host
        };
        let handoff = async {
            started.await.unwrap();
            let waiting_root = root.clone();
            let waiting = tokio::spawn(async move {
                relay_gate::transition(&waiting_root, Duration::from_secs(2)).await
            });
            tokio::time::sleep(Duration::from_millis(30)).await;
            assert!(!waiting.is_finished(), "boot is still in flight");
            let workspace = f.dir("local-work");
            let conversation = f.store.create_conversation(&workspace).await.unwrap();
            f.store
                .submit_new(
                    &conversation.id,
                    Id::new("m_during_reauth").unwrap(),
                    "local work during sign-in".into(),
                    vec![],
                    &workspace,
                )
                .await
                .unwrap();
            assert_eq!(f.store.tasks(16).unwrap().len(), 1);
            release.send(()).unwrap();
            waiting.await.unwrap().unwrap()
        };
        // Production polls this future on the relay's own thread/runtime;
        // it does not require its borrowed command handler to be Send.
        let (_host, guard) = tokio::join!(pass, handoff);
        guard.check().unwrap();
        assert!(relay_gate::try_pump(&root).unwrap().is_none());
    }

    #[tokio::test]
    async fn a_transition_skips_boot_without_disabling_the_host() {
        let f = linked_fixture();
        let root = f.base.join("state");
        let guard = relay_gate::transition(&root, Duration::from_secs(1))
            .await
            .unwrap();
        let mut host = RelayHost::new(&root);
        host.tick_with(&f.store, async |_| panic!("transition must exclude boot"))
            .await;
        assert!(!host.disabled);
        assert!(!host.live());
        guard.check().unwrap();
    }

    #[tokio::test]
    async fn only_a_new_committed_generation_recovers_authentication_failure() {
        let f = linked_fixture();
        let root = f.base.join("state");
        let mut host = RelayHost::new(&root);
        host.observe_reauth(Some("first-committed-generation".into()));
        host.boot_with(&f.store, async |_| {
            Err(Error::Protocol("relay unauthenticated"))
        })
        .await;
        assert!(host.disabled);
        host.observe_reauth(None);
        host.observe_reauth(Some("first-committed-generation".into()));
        assert!(
            host.disabled,
            "the same receipt cannot revive a rejected session"
        );
        crate::cloud::custody::store_session(
            &root,
            &crate::cloud::custody::CloudSession::issue(
                "arbitrary-token".into(),
                "arbitrary-refresh".into(),
                now_ms(),
            ),
        )
        .unwrap();
        host.tick_with(&f.store, async |_| {
            panic!("a token-file edit is not committed reauth")
        })
        .await;
        assert!(host.disabled);
        host.observe_reauth(Some("second-committed-generation".into()));
        assert!(!host.disabled);
        assert!(host.next_boot <= Instant::now());
        assert_eq!(host.boot_delay, BOOT_RETRY);
        assert!(host.projection_due);
    }

    #[tokio::test]
    async fn committed_reauth_does_not_clear_device_revocation_or_class_failure() {
        for reason in ["relay revoked-device", "relay forbidden-device-class"] {
            let f = linked_fixture();
            let mut host = RelayHost::new(&f.base.join("state"));
            host.boot_with(&f.store, async |_| Err(Error::Protocol(reason)))
                .await;
            host.observe_reauth(Some("new-committed-generation".into()));
            assert!(host.disabled, "{reason} is not an expired sign-in");
            assert!(!host.resident.load(Ordering::Relaxed));
        }
    }

    /// Local and relay failures are independently actionable: neither can
    /// hide the other, even when both keep recurring.
    #[test]
    fn relay_and_local_faults_remain_visible_together() {
        let f = fixture();
        let root = f.store.root();
        crate::managed::record_supervisor_fault(
            root,
            "worker outcome could not be recorded: synthetic",
        );
        relay_fault(root, "relay lane pump failed: synthetic");
        assert_eq!(
            crate::managed::supervisor_fault(root).as_deref(),
            Some("worker outcome could not be recorded: synthetic")
        );
        assert_eq!(
            crate::managed::relay_fault(root).as_deref(),
            Some("relay lane pump failed: synthetic")
        );
        let status = f.store.status_text().unwrap();
        assert!(status.contains("worker outcome could not be recorded: synthetic"));
        assert!(status.contains("Remote relay: relay lane pump failed: synthetic"));
        let first = crate::private::read(&root.join("relay.fault.json"), 4096).unwrap();
        relay_fault(root, "relay lane pump failed: synthetic");
        assert_eq!(
            crate::private::read(&root.join("relay.fault.json"), 4096).unwrap(),
            first,
            "repeated relay failures are coalesced"
        );
        relay_fault(root, "relay lane boot failed: synthetic");
        assert_eq!(
            crate::managed::relay_fault(root).as_deref(),
            Some("relay lane boot failed: synthetic")
        );
        assert_eq!(
            crate::managed::supervisor_fault(root).as_deref(),
            Some("worker outcome could not be recorded: synthetic")
        );
        assert!(fatal(&Error::Protocol("relay revoked-device")));
        assert!(!fatal(&Error::Protocol("relay invalid-argument: device")));
    }

    #[test]
    fn recovery_clears_only_the_relay_operation_that_succeeded() {
        let f = fixture();
        let root = f.store.root();
        crate::managed::record_supervisor_fault(root, "local failure: synthetic");
        relay_fault(root, "relay lane pump failed: synthetic");
        relay_recovered(root, false);
        assert!(crate::managed::relay_fault(root).is_none());
        // Recovery also resets coalescing: a new occurrence is visible.
        relay_fault(root, "relay lane pump failed: synthetic");
        assert!(crate::managed::relay_fault(root).is_some());
        for message in [
            "relay projection failed: synthetic",
            "fleet projection build failed: synthetic",
        ] {
            relay_fault(root, message);
            relay_fault(root, "relay lane pump failed: synthetic");
            let pending = crate::managed::relay_fault(root).unwrap();
            assert!(pending.contains(message));
            assert!(pending.contains("relay lane pump failed: synthetic"));
            relay_recovered(root, false);
            assert_eq!(crate::managed::relay_fault(root).as_deref(), Some(message));
            relay_recovered(root, true);
            assert!(crate::managed::relay_fault(root).is_none());
        }
        assert_eq!(
            crate::managed::supervisor_fault(root).as_deref(),
            Some("local failure: synthetic")
        );
    }
}
