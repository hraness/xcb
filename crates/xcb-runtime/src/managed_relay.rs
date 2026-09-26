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

use std::cell::Cell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use xcb_core::Id;

use crate::cloud::commands::{self, CommandBody};
use crate::cloud::lane::{self, CommandOutcome, OpenedCommand, RelayLane};
use crate::managed::{ManagedStore, fault_text, record_supervisor_fault};
use crate::{Error, Result, digest};

/// Remote commands land at most this long after a controller posts them.
const POLL_INTERVAL: Duration = Duration::from_secs(2);
/// Lane boot retry after a transport or custody failure.
const BOOT_RETRY: Duration = Duration::from_secs(15);
/// Fleet projection publish cadence when nothing changed sooner.
const PROJECTION_INTERVAL: Duration = Duration::from_secs(30);
/// Nonterminal task rows a fleet projection carries at most.
const PROJECTION_TASK_ROWS: usize = 64;
/// Single-field character bound inside a projection body.
const PROJECTION_FIELD_CHARS: usize = 200;
/// The projection scope controllers read for fleet state.
const FLEET_SCOPE: &str = "fleet";

/// The relay-lane host owned by the supervisor loop. Holds the lane plus
/// retry/projection scheduling state; `tick` is the only entry point.
pub struct RelayHost {
    lane: Option<RelayLane>,
    root: PathBuf,
    next_boot: Instant,
    disabled: bool,
    projection_due: bool,
    projection_next: Instant,
    /// Last published fleet revision — the CAS pin for the next write.
    fleet_revision: u64,
    /// Digest of the last published plaintext; unchanged projections are
    /// not re-posted.
    fleet_fingerprint: Option<String>,
}

impl RelayHost {
    pub fn new(root: &Path) -> Self {
        Self {
            lane: None,
            root: root.to_path_buf(),
            next_boot: Instant::now(),
            disabled: false,
            projection_due: false,
            projection_next: Instant::now() + PROJECTION_INTERVAL,
            fleet_revision: 0,
            fleet_fingerprint: None,
        }
    }

    /// How long until the next relay poll — the `select!` arm sleeps this
    /// long. A live lane keeps the supervisor resident.
    pub fn poll_interval(&self) -> Duration {
        POLL_INTERVAL
    }

    /// True while a lane is live — a linked machine stays resident to
    /// serve remote commands rather than idle-exiting.
    pub fn live(&self) -> bool {
        self.lane.is_some()
    }

    /// One relay pass: boot when needed, pump the command queue, publish
    /// the fleet projection when due.
    pub async fn tick(&mut self, managed: &Arc<ManagedStore>) {
        if self.lane.is_none() {
            if self.disabled || Instant::now() < self.next_boot {
                return;
            }
            self.next_boot = Instant::now() + BOOT_RETRY;
            match lane::load_lane_keys(&self.root) {
                Ok(Some(keys)) => match RelayLane::boot(keys).await {
                    Ok(mut lane) => {
                        // Seed the CAS pin from the row a previous boot
                        // may have left behind.
                        if let Ok(revision) = lane.projection_revision(FLEET_SCOPE).await {
                            self.fleet_revision = revision;
                        }
                        self.lane = Some(lane);
                    }
                    Err(error) => {
                        if fatal(&error) {
                            self.disabled = true;
                        }
                        record_supervisor_fault(
                            managed.root(),
                            &format!("relay lane boot failed: {}", fault_text(&error)),
                        );
                    }
                },
                Ok(None) => {}
                Err(error) => record_supervisor_fault(
                    managed.root(),
                    &format!("relay lane custody failed: {}", fault_text(&error)),
                ),
            }
            return;
        }

        let lane = match self.lane.as_mut() {
            Some(lane) => lane,
            None => return,
        };
        let refresh = Cell::new(false);
        let mut handler =
            async |opened: &OpenedCommand| execute_command(managed, opened, &refresh).await;
        if let Err(error) = lane.pump(&mut handler).await {
            if fatal(&error) {
                self.disabled = true;
            }
            record_supervisor_fault(
                managed.root(),
                &format!("relay lane pump failed: {}", fault_text(&error)),
            );
            self.lane = None;
            return;
        }
        if refresh.get() {
            self.projection_due = true;
        }

        let now = Instant::now();
        if !self.projection_due && now < self.projection_next {
            return;
        }
        match fleet_projection(managed) {
            Ok(plaintext) => {
                let fingerprint = digest(&plaintext);
                if self.fleet_fingerprint.as_deref() == Some(fingerprint.as_str()) {
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
                        self.projection_due = false;
                        self.projection_next = Instant::now() + PROJECTION_INTERVAL;
                    }
                    Err(error) => {
                        if fatal(&error) {
                            self.disabled = true;
                        }
                        record_supervisor_fault(
                            managed.root(),
                            &format!("relay projection failed: {}", fault_text(&error)),
                        );
                        self.projection_next = Instant::now() + PROJECTION_INTERVAL;
                    }
                }
            }
            Err(error) => {
                record_supervisor_fault(
                    managed.root(),
                    &format!("fleet projection build failed: {}", fault_text(&error)),
                );
                self.projection_next = Instant::now() + PROJECTION_INTERVAL;
            }
        }
    }

    /// Drop the lane's presence row on supervisor shutdown.
    pub async fn shutdown(&mut self) {
        if let Some(mut lane) = self.lane.take() {
            let _ = lane.disconnect().await;
        }
    }
}

/// Relay errors that mean the lane can never recover under this custody —
/// a revoked device or a dead session binding.
fn fatal(error: &crate::Error) -> bool {
    matches!(
        error,
        crate::Error::Protocol(
            "relay unauthenticated" | "relay forbidden-device-class" | "relay device-revoked"
        )
    )
}

/// Map one opened command onto the managed surface and produce the sealed
/// result payload. Every failure path still settles `failed` — a rejected
/// body or a missing task is a terminal answer, not a retry.
async fn execute_command(
    managed: &Arc<ManagedStore>,
    opened: &OpenedCommand,
    refresh: &Cell<bool>,
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
            let task = Id::new(task.clone())?;
            managed
                .steer_task(&task, operation, text.clone())
                .map(|_| json!({"steered": task.as_str()}))
        }
        CommandBody::TaskCancel { task } => {
            let task = Id::new(task.clone())?;
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
            let task = Id::new(attention.clone())?;
            managed
                .reply_to_task(&task, answer.clone())
                .await
                .map(|_| json!({"answered": task.as_str()}))
        }
        CommandBody::DaemonSend { daemon, text } => managed
            .daemon_send(daemon, text)
            .map(|_| json!({"sent": daemon})),
        CommandBody::ProjectionRefresh => {
            refresh.set(true);
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

/// Task dispatch lands as an ordinary managed task in the workspace's
/// live conversation — or a fresh conversation when none exists.
async fn dispatch(
    managed: &Arc<ManagedStore>,
    workspace: &str,
    prompt: &str,
    operation: &Id,
) -> Result<Value> {
    // The store records canonical workspace paths; a spelled alias like
    // /tmp/… must canonicalise to the same bytes or the conversation
    // conflict check rejects the submit.
    let path = std::fs::canonicalize(workspace)?;
    let conversation = match managed.latest_conversation_for_workspace(&path)? {
        Some(conversation) => conversation,
        None => managed.create_conversation(&path).await?,
    };
    managed
        .submit_new(
            &conversation.id,
            operation.clone(),
            prompt.to_string(),
            vec![],
            &path,
        )
        .await?;
    Ok(json!({
        "conversation": conversation.id.as_str(),
        "dispatched": true,
    }))
}

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
            }));
        }
        if task.attention.is_some() && attention.len() < PROJECTION_TASK_ROWS {
            attention.push(json!({
                "task": task.id.as_str(),
                "detail": xcb_core::display_text(&task.detail, PROJECTION_FIELD_CHARS),
            }));
        }
    }
    serde_json::to_vec(&json!({
        "version": 1,
        "counts": counts,
        "tasks": rows,
        "attention": attention,
    }))
    .map_err(crate::Error::Json)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tempfile::TempDir;

    /// The store records canonical workspace paths; a spelled alias must
    /// resolve to the same bytes or the submit conflicts.
    #[tokio::test]
    async fn dispatch_accepts_noncanonical_workspace_spelling() {
        let root = TempDir::new().unwrap();
        // Custody rejects symlinked ancestors (/var → /private/var on
        // macOS), so the state root itself must be canonical.
        let root = root.path().canonicalize().unwrap();
        let store = Arc::new(ManagedStore::open(&root).unwrap());
        let workspace = root.join("ws");
        std::fs::create_dir(&workspace).unwrap();
        // A symlinked workspace is the /tmp → /private/tmp shape the bug
        // came from: the alias exists but spells differently.
        let alias = root.join("ws-link");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&workspace, &alias).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&workspace, &alias).unwrap();
        let result = dispatch(
            &store,
            alias.to_str().unwrap(),
            "remote prompt",
            &Id::new("m_remote_test".to_string()).unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(result["dispatched"], serde_json::json!(true));
    }
}
