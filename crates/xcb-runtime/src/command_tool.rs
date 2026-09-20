//! One independently owned command per provider turn. Dropping a provider
//! future cannot drop the command's join, publication or durable settlement.
use crate::{
    Error, Result,
    broker::{Workspace, snapshot::CommandChanges},
    command::{CommandBackend, CommandInput, CommandOutcome, CommandRequest},
    digest, new_id, private,
    store::{RunRecord, Store},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, Metadata},
    io::{Read, Seek, SeekFrom},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::{sync::watch, task::JoinHandle};
use xcb_core::policy::EffectState;

pub(crate) struct CommandToolResult {
    pub output: Result<Value>,
    pub effects: EffectState,
    pub joined: bool,
}
struct Active {
    cancel: watch::Sender<bool>,
    task: JoinHandle<CommandToolResult>,
}
#[derive(Default)]
pub(crate) struct CommandTools {
    active: Option<Active>,
}

pub fn default_root() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").ok_or(Error::PrivateState)?;
    Ok(PathBuf::from(home).join(".local/share/xcb-command"))
}

impl CommandTools {
    pub fn start(
        &mut self,
        store: Arc<Store>,
        run: RunRecord,
        workspace: Arc<Workspace>,
        call: String,
        arguments: &Value,
    ) -> Result<()> {
        if self.active.is_some() {
            return Err(Error::Conflict("a command is already active"));
        }
        let request: CommandRequest = serde_json::from_value(arguments.clone())?;
        request.validate()?;
        let backend_root = default_root()?;
        let backend = CommandBackend::load(&backend_root).map_err(|_| Error::Unavailable("offline command runner is not ready; run the supported setup-command-runner.py installer"))?;
        let command_id = new_id("cmd");
        let snapshots = backend_root.join("snapshots");
        private::directory(&snapshots)?;
        let snapshot_path = snapshots.join(format!("{command_id}.json"));
        let snapshot = workspace.command_snapshot()?;
        let snapshot_sha256 = snapshot.save(&snapshot_path)?;
        let input = CommandInput {
            command_id,
            run_id: run.id.clone(),
            workspace_id: snapshot.document.workspace_id.clone(),
            snapshot_path,
            snapshot_sha256,
        };
        let owned_input = OwnedSnapshot::capture(&input.snapshot_path, &input.snapshot_sha256)?;
        let custody = match backend.prepare(&input, &request) {
            Ok(custody) => custody,
            Err(error) => {
                discard_unsubmitted(&store, &run, owned_input);
                return Err(error);
            }
        };
        // The strict run decoder in earlier releases rejects this marker. It
        // is written before the independent owner can launch guest work.
        if let Err(error) = store.record_command_custody(&run, &custody) {
            // A failed database commit can be ambiguous. Re-read durable
            // custody instead of assuming the marker was never published.
            discard_unsubmitted(&store, &run, owned_input);
            return Err(error);
        }
        let (cancel, cancellation) = watch::channel(false);
        let task = tokio::spawn(async move {
            let result = backend
                .execute(input, request, custody.clone(), cancellation.clone())
                .await;
            let outcome = match result {
                Ok(outcome) if outcome.joined && outcome.custody == custody => outcome,
                _ => {
                    return CommandToolResult {
                        output: Err(Error::Unavailable(
                            "command stop is unproven; account custody retained",
                        )),
                        effects: EffectState::None,
                        joined: false,
                    };
                }
            };
            let publication = publish(&workspace, &snapshot, &outcome, *cancellation.borrow());
            finish_command(&store, &run, &call, &outcome, publication, owned_input)
        });
        self.active = Some(Active { cancel, task });
        Ok(())
    }
    pub async fn wait(&mut self) -> CommandToolResult {
        let Some(active) = self.active.as_mut() else {
            return CommandToolResult {
                output: Err(Error::Conflict("no active command")),
                effects: EffectState::None,
                joined: true,
            };
        };
        // Await by reference. Cancellation of this wait leaves the handle in
        // self for the outer run owner to signal and independently join.
        let result = (&mut active.task).await;
        self.active.take();
        result.unwrap_or(CommandToolResult {
            output: Err(Error::Unavailable("command owner failed; custody retained")),
            effects: EffectState::Uncertain,
            joined: false,
        })
    }
    pub async fn cancel_and_join(&mut self) -> Option<CommandToolResult> {
        self.active.as_ref()?.cancel.send_replace(true);
        Some(self.wait().await)
    }
}
impl Drop for CommandTools {
    fn drop(&mut self) {
        if let Some(active) = &self.active {
            active.cancel.send_replace(true);
        }
    }
}
// This capability is created only for the unique input just published by this
// invocation. It is never reconstructed from a historical job or caller path.
struct OwnedSnapshot {
    path: PathBuf,
    parent: File,
    file: File,
    identity: Metadata,
    sha256: String,
}
const SNAPSHOT_LIMIT: u64 = 96 * 1024 * 1024;

fn same_snapshot(left: &Metadata, right: &Metadata) -> bool {
    left.dev() == right.dev()
        && left.ino() == right.ino()
        && left.uid() == right.uid()
        && left.gid() == right.gid()
        && left.mode() == right.mode()
        && left.nlink() == right.nlink()
        && left.len() == right.len()
        && left.mtime() == right.mtime()
        && left.mtime_nsec() == right.mtime_nsec()
        && left.ctime() == right.ctime()
        && left.ctime_nsec() == right.ctime_nsec()
}

impl OwnedSnapshot {
    fn capture(path: &Path, sha256: &str) -> Result<Self> {
        let directory = private::check_directory(path.parent().ok_or(Error::PrivateState)?)?;
        let parent = File::from(
            rustix::fs::open(
                &directory,
                rustix::fs::OFlags::RDONLY
                    | rustix::fs::OFlags::DIRECTORY
                    | rustix::fs::OFlags::NOFOLLOW
                    | rustix::fs::OFlags::CLOEXEC,
                rustix::fs::Mode::empty(),
            )
            .map_err(std::io::Error::from)?,
        );
        let file = private::open_file(path, SNAPSHOT_LIMIT)?;
        let identity = file.metadata()?;
        let mut owned = Self {
            path: path.to_owned(),
            parent,
            file,
            identity,
            sha256: sha256.to_owned(),
        };
        owned.verify()?;
        Ok(owned)
    }

    fn check_parent(&self) -> Result<()> {
        let path = self.path.parent().ok_or(Error::PrivateState)?;
        private::check_directory(path)?;
        let named = std::fs::symlink_metadata(path)?;
        let retained = self.parent.metadata()?;
        if named.dev() != retained.dev() || named.ino() != retained.ino() {
            return Err(Error::Conflict("command snapshot parent changed"));
        }
        Ok(())
    }

    fn verify(&mut self) -> Result<()> {
        self.check_parent()?;
        private::check_file(&self.file, SNAPSHOT_LIMIT)?;
        if !same_snapshot(&self.identity, &self.file.metadata()?) {
            return Err(Error::Conflict("command snapshot identity changed"));
        }
        self.file.seek(SeekFrom::Start(0))?;
        let mut hasher = Sha256::new();
        let mut buffer = [0u8; 65536];
        let mut total = 0u64;
        loop {
            let count = self.file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            total += count as u64;
            if total > SNAPSHOT_LIMIT {
                return Err(Error::PrivateState);
            }
            hasher.update(&buffer[..count]);
        }
        if total != self.identity.len() || hex::encode(hasher.finalize()) != self.sha256 {
            return Err(Error::Conflict("command snapshot digest changed"));
        }
        private::check_file(&self.file, SNAPSHOT_LIMIT)?;
        if !same_snapshot(&self.identity, &self.file.metadata()?) {
            return Err(Error::Conflict("command snapshot changed during cleanup"));
        }
        self.check_parent()?;
        private::same_file(&self.path, &self.file)?;
        Ok(())
    }

    fn remove(mut self) -> Result<()> {
        self.verify()?;
        let name = self.path.file_name().ok_or(Error::PrivateState)?;
        // Remove through the retained parent, never through an unchecked
        // re-resolved ancestor. A failure does not change process join proof.
        rustix::fs::unlinkat(&self.parent, name, rustix::fs::AtFlags::empty())
            .map_err(std::io::Error::from)?;
        self.parent.sync_all()?;
        self.check_parent()?;
        Ok(())
    }
}

fn discard_unsubmitted(store: &Store, run: &RunRecord, input: OwnedSnapshot) {
    // Called only before execute/spawn. Absence must be observed in the
    // durable owned run; an ambiguous write or lost authority retains input.
    if store.verify_owned_run(run).is_ok()
        && store
            .run(&run.id)
            .is_ok_and(|current| current.is_some_and(|current| current.command_custody.is_none()))
        && store.verify_owned_run(run).is_ok()
    {
        let _ = input.remove();
    }
}

fn finish_command(
    store: &Store,
    run: &RunRecord,
    call: &str,
    outcome: &CommandOutcome,
    publication: (Result<Value>, EffectState),
    input: OwnedSnapshot,
) -> CommandToolResult {
    if !outcome.joined {
        return CommandToolResult {
            output: Err(Error::Unavailable(
                "command stop is unproven; account custody retained",
            )),
            effects: EffectState::Uncertain,
            joined: false,
        };
    }
    let (mut output, effects) = publication;
    if store.clear_command_custody(run, &outcome.custody).is_err() {
        return CommandToolResult {
            output: Err(Error::Unavailable("command custody settlement failed")),
            effects: EffectState::Uncertain,
            joined: true,
        };
    }
    if effects != EffectState::Uncertain && store.settle_tool(run, call).is_err() {
        return CommandToolResult {
            output: Err(Error::Unavailable(
                "command effect receipt could not settle",
            )),
            effects: EffectState::Uncertain,
            joined: true,
        };
    }
    // Neither a no-child/busy receipt nor a failed/cancelled/uncertain staged
    // command is disposable. Only successful publication and both durable
    // settlements establish this input's end of life.
    if effects != EffectState::Uncertain
        && let Ok(value) = &mut output
        && value.get("published").and_then(Value::as_bool) == Some(true)
    {
        if input.remove().is_ok() {
            value["inputSnapshotCleanup"] = json!("removed");
        } else {
            value["inputSnapshotCleanup"] = json!("unconfirmed");
            value["notice"] = json!(
                "Command joined and publication settled; input snapshot cleanup was not confirmed."
            );
        }
    }
    CommandToolResult {
        output,
        effects,
        joined: true,
    }
}

fn clipped_at(value: &str, maximum: usize) -> (&str, bool) {
    let mut end = value.len().min(maximum);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    (&value[..end], end != value.len())
}
fn publish(
    workspace: &Workspace,
    snapshot: &crate::broker::snapshot::CommandSnapshot,
    outcome: &CommandOutcome,
    cancelled: bool,
) -> (Result<Value>, EffectState) {
    let mut effects = EffectState::None;
    let result = (|| -> Result<Value> {
        if !outcome.joined || outcome.custody.workspace_id != snapshot.document.workspace_id {
            return Err(Error::Unavailable(
                "command receipt does not prove this workspace joined",
            ));
        }
        let Some(output) = outcome.output.as_ref() else {
            let (error, truncated) = clipped_at(
                outcome.error.as_deref().unwrap_or("command did not start"),
                4096,
            );
            return Ok(
                json!({"commandId":outcome.custody.command_id,"status":"not_started",
                "joined":true,"published":false,"error":error,"truncated":truncated,
                "network":"none","platform":"linux"}),
            );
        };
        let eligible = !cancelled
            && !output.cancelled
            && !output.timed_out
            && !output.truncated
            && output.exit_code == Some(0)
            && outcome.error.is_none();
        let mut publication = None;
        if eligible {
            let (Some(path), Some(expected)) = (&output.changes_path, &output.changes_sha256)
            else {
                return Err(Error::Unavailable(
                    "command changes lack a bound receipt; staged state retained",
                ));
            };
            let bytes = private::read(path, 24 * 1024 * 1024)?;
            if digest(&bytes) != *expected {
                return Err(Error::Conflict("staged command changes changed"));
            }
            let changes: CommandChanges = serde_json::from_slice(&bytes)?;
            let (applied, observed) = workspace.publish_command_changes(snapshot, changes);
            effects = observed;
            publication = Some(applied?);
        }
        let mut excluded_bytes = 0;
        let excluded = snapshot
            .excluded
            .iter()
            .take_while(|path| {
                excluded_bytes += path.len();
                excluded_bytes <= 2048
            })
            .collect::<Vec<_>>();
        let (stdout, stdout_cut) = clipped_at(&output.stdout, 16384);
        let (stderr, stderr_cut) = clipped_at(&output.stderr, 16384);
        let (error, error_cut) = outcome
            .error
            .as_deref()
            .map(|value| {
                let (text, cut) = clipped_at(value, 4096);
                (Some(text), cut)
            })
            .unwrap_or((None, false));
        Ok(
            json!({"commandId":outcome.custody.command_id,"exitCode":output.exit_code,
            "stdout":stdout,"stderr":stderr,"timedOut":output.timed_out,
            "cancelled":cancelled || output.cancelled,"truncated":output.truncated || stdout_cut || stderr_cut || error_cut,
            "joined":true,"network":"none","platform":"linux","published":eligible,
            "scratchCleanupPending":output.cleanup_pending,
            "scratchCleanupNotice":output.cleanup_pending.then_some("Command joined; private scratch cleanup is pending. Execution and publication results remain valid."),
            "publication":publication,"stagedChangesRetained":!eligible,
            "gitInspectionAvailable":snapshot.document.git.is_some(),"gitUnavailable":snapshot.git_unavailable,
            "excludedInputPaths":excluded,"excludedPathsTruncated":excluded.len() != snapshot.excluded.len(),"error":error}),
        )
    })();
    (result, effects)
}

/// Explicit recovery reads the exact trusted guest receipt before clearing the
/// pending marker. It never publishes staged workspace edits during recovery.
pub async fn recover(store: &Store, run: &RunRecord, expected_digest: &str) -> Result<RunRecord> {
    run.verify_recovery_stop()?;
    let custody = run
        .command_custody
        .as_ref()
        .ok_or(Error::Conflict("run has no command custody"))?;
    let root = default_root()?;
    let outcome = match CommandBackend::recover_recorded_join(&root, custody)? {
        Some(outcome) => outcome,
        None => CommandBackend::load(&root)?.recover(custody).await?,
    };
    if !outcome.joined || outcome.custody != *custody {
        return Err(Error::Unavailable(
            "guest command stop is unproven; account custody retained",
        ));
    }
    store.reconcile_command_custody(&run.id, expected_digest, custody)
}

#[cfg(test)]
mod tests;
