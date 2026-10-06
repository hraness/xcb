#[path = "project_preflight.rs"]
mod project_preflight;

use clap::Subcommand;
use std::{
    io::Read,
    path::{Path, PathBuf},
};
use xcb_core::{Id, Provider, session::State};
use xcb_runtime::{
    Error, Result,
    managed::{self, GLOBAL_THREAD_ID, HerdStatus, ManagedStore, ManagedTask, ScheduleView},
    new_id, now_ms,
    workspace_infer::BindingOrigin,
};

#[derive(Subcommand)]
pub enum BacklogCommand {
    /// Run a prepared source-inspection recipe using this project's task grant.
    Context {
        /// Conversation id, project directory, or project name.
        target: String,
        /// Saved recipe.json from context prepare.
        recipe: PathBuf,
        /// Stable operation identity for an idempotent submission retry.
        #[arg(long)]
        id: Option<Id>,
        /// Exact directory when the target is the thread.
        #[arg(long)]
        workspace: Option<PathBuf>,
    },
    /// Run a pinned ALGAL program now in a project.
    Program {
        /// Conversation id from `xcb conversations`, or a project directory or
        /// name (the thread, in that directory).
        target: String,
        /// ALGAL manifest (at most 64 KiB) to validate and pin for this run.
        manifest: PathBuf,
        /// JSON object of typed inputs; defaults to an empty object.
        #[arg(long)]
        inputs: Option<PathBuf>,
        /// Allow up to this many agent calls (1 to 8) under the current
        /// project grant.
        #[arg(long, value_parser = clap::value_parser!(u8).range(1..=8))]
        managed_calls: Option<u8>,
        /// Human-readable title retained in the project's work history.
        #[arg(long, default_value = "ALGAL project program")]
        title: String,
        /// Stable operation identity for an idempotent submission retry.
        #[arg(long)]
        id: Option<Id>,
        /// Exact project directory; required when the target is the thread.
        #[arg(long)]
        workspace: Option<PathBuf>,
    },
    /// Inspect a managed program's checkpoint and linked worker status.
    ProgramStatus {
        /// Managed controller or linked child task id from `xcb backlog`.
        id: Id,
    },
    /// Record that an unstarted deferred item is already done.
    Complete {
        /// Deferred item from `xcb backlog`.
        id: Id,
        /// Work already done and the evidence supporting completion.
        summary: String,
        /// Current task revision; stale completion is rejected.
        #[arg(long)]
        revision: u64,
    },
    /// Reconcile uncertainty only when retained run evidence proves an outcome.
    Reconcile {
        /// Uncertain task from `xcb attention`.
        id: Id,
        /// Current task revision; stale reconciliation is rejected.
        #[arg(long)]
        revision: u64,
    },
    /// Close uncertain work you have checked yourself; it fails without a retry.
    Dismiss {
        /// Uncertain task from `xcb attention`.
        id: Id,
        /// Current task revision; a stale dismissal is rejected.
        #[arg(long)]
        revision: u64,
    },
    /// Hold work for later; --ready releases it immediately.
    Add {
        /// Conversation id from `xcb conversations`, or a project directory or
        /// name (the thread, in that directory).
        target: String,
        /// Work to retain in this project's backlog.
        prompt: String,
        /// Dispatch immediately instead of holding the task for later.
        #[arg(long)]
        ready: bool,
        /// Queue priority from 0 to 9; larger values run first.
        #[arg(long, default_value_t = 5, value_parser = clap::value_parser!(u8).range(0..=9))]
        priority: u8,
        /// Stable submission identity for retrying the same prompt and options.
        #[arg(long)]
        id: Option<Id>,
        /// Exact project directory; required when the target is the thread.
        #[arg(long)]
        workspace: Option<PathBuf>,
        /// Pin the task to one observed model (provider/model[/effort]);
        /// omitted or "auto" routes automatically.
        #[arg(long)]
        model: Option<String>,
    },
    /// Edit undispatched work; a revision protects against concurrent edits.
    Edit {
        /// Held task id from `xcb backlog`.
        id: Id,
        /// Replacement prompt for this undispatched task.
        prompt: String,
        /// Current revision from `xcb backlog --json`; stale edits are rejected.
        #[arg(long)]
        revision: u64,
        /// Keep the task's current priority when omitted.
        #[arg(long, value_parser = clap::value_parser!(u8).range(0..=9))]
        priority: Option<u8>,
    },
    /// Release held work for automatic routing and execution.
    Release {
        /// Held task id from `xcb backlog`.
        id: Id,
        /// Current revision from `xcb backlog --json`; stale releases are rejected.
        #[arg(long)]
        revision: u64,
    },
    /// Answer a task's question. This does not grant host or provider permissions.
    Reply {
        /// Task id from `xcb attention` or `xcb backlog`.
        id: Id,
        /// Answer or additional context; does not grant permissions.
        text: String,
        /// Revision of the question being answered; stale questions are rejected.
        #[arg(long)]
        revision: Option<u64>,
        /// Stable reply identity for retrying the same answer.
        #[arg(long, requires = "revision")]
        reply_id: Option<Id>,
    },
    /// Cancel never-started ordinary queued work and print its prompt for editing.
    Recall {
        /// Queued task id from `xcb backlog`.
        id: Id,
        /// Current task revision; stale recall is rejected.
        #[arg(long)]
        revision: u64,
        /// Stable operation identity for retrying this recall.
        #[arg(long)]
        operation: Id,
    },
    /// Read recent work summaries for a project, from every conversation over it.
    Memory {
        /// Project directory or name; a conversation id names its directory.
        scope: String,
    },
}

#[derive(Subcommand)]
pub enum DaemonCommand {
    /// Install a named durable ALGAL process in a project.
    Run {
        /// Conversation id from `xcb conversations`, or a project directory or
        /// name (the thread, in that directory).
        target: String,
        /// Daemon name; lowercase kebab-case, at most 48 characters.
        name: String,
        /// ALGAL process manifest (at most 64 KiB) to validate and pin.
        manifest: PathBuf,
        /// JSON object of typed interface inputs; defaults to an empty object.
        /// Mailbox capability ports are bound to this daemon's own mailboxes.
        #[arg(long)]
        inputs: Option<PathBuf>,
        /// Allow up to this many agent calls (0 to 8) under the current
        /// project grant.
        #[arg(long, default_value_t = 0, value_parser = clap::value_parser!(u8).range(0..=8))]
        calls: u8,
        /// Maximum process generations before the daemon record stops.
        #[arg(long, default_value_t = 16, value_parser = clap::value_parser!(u64).range(1..=64))]
        generations: u64,
        /// Exact project directory; required when the target is the thread.
        #[arg(long)]
        workspace: Option<PathBuf>,
    },
    /// Show a daemon's process state, wake evidence and pending child.
    Inspect {
        /// Daemon name from `xcb daemons`.
        name: String,
    },
    /// Post a message (at most 8 KiB) to a daemon's inbox.
    Send {
        /// Daemon name from `xcb daemons`.
        name: String,
        /// UTF-8 message, at most 8 KiB; the daemon reads it on a wake tick.
        text: String,
    },
    /// Stop a daemon: no new calls or tasks; a child task already running
    /// finishes first.
    Stop {
        /// Daemon name from `xcb daemons`.
        name: String,
    },
    /// Show the journal evidence when a daemon holds an uncertain intent.
    Journal {
        /// Daemon name from `xcb daemons`.
        name: String,
    },
    /// Recover an uncertain intent only by its exact recorded digest.
    Recover {
        /// Daemon name from `xcb daemons`.
        name: String,
        /// Exact uncertain intent digest from `xcb daemons journal`.
        #[arg(long)]
        intent: String,
    },
}

#[derive(Subcommand)]
pub enum ProjectCommand {
    /// Check a program and its exact workspace without starting any work.
    Preflight {
        /// Exact workspace directory, resolved relative to --cwd.
        workspace: PathBuf,
        /// Program manifest, resolved relative to --cwd.
        manifest: PathBuf,
        /// JSON input file, resolved relative to --cwd; defaults to an empty object.
        #[arg(long)]
        inputs: Option<PathBuf>,
        /// Maximum managed calls per run, not a concurrency setting.
        #[arg(long, value_parser = clap::value_parser!(u8).range(1..=8))]
        managed_calls: Option<u8>,
        /// Required file inside the workspace; repeat for multiple files.
        #[arg(long = "require-file")]
        required_files: Vec<PathBuf>,
        /// Require HEAD to match this full Git commit hash.
        #[arg(long)]
        expect_revision: Option<String>,
        /// Remaining child-task budget to use for cycle estimates.
        #[arg(long, value_parser = clap::value_parser!(u32).range(0..=100))]
        task_budget: Option<u32>,
    },
    /// Let a project start follow-up work on its own for a goal, within a
    /// task count and a time limit.
    Configure {
        /// Project directory or name; a conversation id names its directory.
        scope: String,
        /// The project goal that follow-up work serves.
        goal: String,
        /// Most tasks the project may start on its own under this grant
        /// (1 to 10,000).
        #[arg(long, value_parser = clap::value_parser!(u32).range(1..=10_000))]
        tasks: u32,
        /// Grant lifetime, 1 hour to 30 days. A new grant replaces the old grant.
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..=720))]
        hours: u64,
        /// Required when replacing an existing policy; inspect `projects --json`.
        #[arg(long)]
        revision: Option<u64>,
        /// Optional hard provider constraint inherited by automatic work.
        #[arg(long)]
        provider: Option<Provider>,
        /// Most automatic tasks running at once across the project's linked
        /// worktrees, 0 to 64; 0 leaves it to the machine's own limits.
        #[arg(long, value_parser = clap::value_parser!(u32).range(0..=64), default_value_t = 0)]
        parallel: u32,
        /// Most automatic tasks the project may start per hour, 0 to 512;
        /// 0 means no hourly limit.
        #[arg(long, value_parser = clap::value_parser!(u32).range(0..=512), default_value_t = 0)]
        per_hour: u32,
    },
    /// Raise or lower how much automatic work a project runs at once and
    /// starts per hour; the goal, budget and expiry are unchanged.
    Scale {
        /// Project directory or name from `xcb projects`.
        scope: String,
        /// Current policy revision; stale updates are rejected.
        #[arg(long)]
        revision: u64,
        /// Most automatic tasks running at once across the project's linked
        /// worktrees, 0 to 64; 0 leaves it to the machine's own limits.
        #[arg(long, value_parser = clap::value_parser!(u32).range(0..=64))]
        parallel: Option<u32>,
        /// Most automatic tasks the project may start per hour, 0 to 512;
        /// 0 means no hourly limit.
        #[arg(long, value_parser = clap::value_parser!(u32).range(0..=512))]
        per_hour: Option<u32>,
    },
    /// Show a project's grant, its running and queued work, and the
    /// schedules that feed it.
    Status {
        /// Project directory or name from `xcb projects`.
        scope: String,
    },
    /// Pause automatic follow-up work; work already running finishes.
    Pause {
        /// Project directory or name from `xcb projects`.
        scope: String,
        /// Current policy revision; stale updates are rejected.
        #[arg(long)]
        revision: u64,
    },
    /// Resume the same grant without replenishing its task budget or expiry.
    Resume {
        /// Project directory or name from `xcb projects`.
        scope: String,
        /// Current policy revision; resuming does not renew the grant.
        #[arg(long)]
        revision: u64,
    },
}

/// The directory `scope` names, resolved once by the shared scope order.
/// Relative values start at `--cwd`, as in every other command.
fn scope(store: &ManagedStore, cwd: &Path, value: &str) -> Result<String> {
    store.resolve_scope(value, &xcb_core::canonical(cwd)?)
}

/// `--workspace <dir>`: validated to its canonical path, never snapped.
fn exact_workspace(store: &ManagedStore, cwd: &Path, dir: &Path) -> Result<PathBuf> {
    Ok(PathBuf::from(store.validate_workspace(
        &xcb_core::canonical(cwd)?.join(dir),
    )?))
}

/// Where `backlog add|program`, `schedules add|program` and `daemons run`
/// create work. A conversation id keeps its conversation; the thread needs
/// `--workspace`. Anything else is a project scope, which means the thread
/// in that directory.
async fn entry_target(
    store: &ManagedStore,
    cwd: &Path,
    target: &str,
    workspace: Option<&Path>,
) -> Result<(Id, Option<PathBuf>)> {
    let exact = workspace
        .map(|dir| exact_workspace(store, cwd, dir))
        .transpose()?;
    let conversation = match Id::new(target) {
        Ok(id) if target.starts_with("c_") => match store.resolve_conversation(&id) {
            Ok(id) => Some(id),
            Err(Error::Unavailable(_)) => None,
            Err(error) => return Err(error),
        },
        _ => None,
    };
    let (conversation, workspace) = match conversation {
        Some(id) if id.as_str() != GLOBAL_THREAD_ID => return Ok((id, exact)),
        Some(id) => (
            id,
            exact.ok_or_else(|| {
                Error::guided(
                    "The thread spans projects; name the directory this work runs in.",
                    "repeat with --workspace <dir>",
                )
            })?,
        ),
        None => {
            let scope = PathBuf::from(scope(store, cwd, target)?);
            if exact.as_ref().is_some_and(|dir| *dir != scope) {
                return Err(Error::Conflict(
                    "--workspace names a different directory than the project",
                ));
            }
            (Id::new(GLOBAL_THREAD_ID)?, scope)
        }
    };
    store.global_thread().await?;
    Ok((conversation, Some(workspace)))
}

/// The herd posture report behind `xcb projects status`.
fn project_status_report(store: &ManagedStore, status: &HerdStatus, json: bool) -> Result<i32> {
    if json {
        return crate::print_json(status).map(|()| 0);
    }
    let policy = &status.policy;
    let label = store.project_status(policy)?;
    println!(
        "{} · {} · rev {}",
        xcb_core::display_text(&policy.workspace, 4096),
        label,
        policy.revision
    );
    println!("Goal: {}", xcb_core::display_text(&policy.goal, 4096));
    println!(
        "Budget: {}/{} tasks used · expires {} · provider {}",
        policy.admitted_tasks,
        policy.max_tasks,
        policy.expires_at_ms,
        policy
            .required_provider
            .map(|provider| provider.as_str())
            .unwrap_or("any")
    );
    let cap = |limit: u32| {
        if limit == 0 {
            "no limit".to_owned()
        } else {
            limit.to_string()
        }
    };
    println!(
        "Throughput: {} running ({} allowed) · {} started this hour ({} allowed)",
        status.lanes.len(),
        cap(policy.max_active),
        status.admissions_last_hour,
        cap(policy.max_per_hour)
    );
    if let Some(repo) = &policy.repo {
        println!("Repository family: {}", xcb_core::display_text(repo, 4096));
    }
    println!(
        "Open work: {} tasks · {} uncertain · {} schedules",
        status.open,
        status.uncertain,
        status.schedules.len()
    );
    for task in &status.lanes {
        println!(
            "  running {} · {}",
            task.id,
            xcb_core::display_text(&task.detail, 160)
        );
    }
    for schedule in &status.schedules {
        println!(
            "  schedule {} · {} · every {}s · due {}",
            schedule.id,
            if schedule.enabled {
                "enabled"
            } else {
                "paused"
            },
            schedule.interval_ms / 1000,
            schedule.next_due_ms
        );
    }
    Ok(0)
}

pub async fn projects(
    root: &Path,
    cwd: &Path,
    command: Option<ProjectCommand>,
    json: bool,
) -> Result<i32> {
    if let Some(ProjectCommand::Preflight {
        workspace,
        manifest,
        inputs,
        managed_calls,
        required_files,
        expect_revision,
        task_budget,
    }) = &command
    {
        let program = load_program(
            &cwd.join(manifest),
            inputs.as_ref().map(|p| cwd.join(p)).as_deref(),
            *managed_calls,
        )?;
        let report = project_preflight::inspect(
            &cwd.join(workspace),
            &program,
            required_files,
            expect_revision.as_deref(),
            *task_budget,
        )
        .await?;
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(if report["ready"] == true { 0 } else { 1 });
    }
    let store = ManagedStore::open(root)?;
    if let Some(ProjectCommand::Status { scope: value }) = command {
        let workspace = scope(&store, cwd, &value)?;
        let status = store
            .herd_status_in(&workspace)?
            .ok_or(Error::Unavailable("no project grant for this directory"))?;
        return project_status_report(&store, &status, json);
    }
    let rows = match command {
        None => store.project_policies()?,
        Some(ProjectCommand::Configure {
            scope: value,
            goal,
            tasks,
            hours,
            revision,
            provider,
            parallel,
            per_hour,
        }) => {
            let expiry = now_ms()
                .checked_add(hours * 3_600_000)
                .ok_or(Error::Unavailable("project expiry overflow"))?;
            let workspace = scope(&store, cwd, &value)?;
            let row = store.configure_project_policy_in(
                Path::new(&workspace),
                revision,
                goal,
                tasks,
                expiry,
                provider,
                parallel,
                per_hour,
            )?;
            wake(root)?;
            vec![row]
        }
        Some(ProjectCommand::Scale {
            scope: value,
            revision,
            parallel,
            per_hour,
        }) => {
            let workspace = scope(&store, cwd, &value)?;
            let current = store
                .project_policy_in(&workspace)?
                .ok_or(Error::Unavailable("no project grant for this directory"))?;
            let row = store.update_project_throughput_in(
                &workspace,
                revision,
                parallel.unwrap_or(current.max_active),
                per_hour.unwrap_or(current.max_per_hour),
            )?;
            wake(root)?;
            vec![row]
        }
        Some(ProjectCommand::Status { .. } | ProjectCommand::Preflight { .. }) => {
            unreachable!("handled above")
        }
        Some(ProjectCommand::Pause {
            scope: value,
            revision,
        }) => {
            let workspace = scope(&store, cwd, &value)?;
            vec![store.set_project_policy_enabled_in(&workspace, revision, false)?]
        }
        Some(ProjectCommand::Resume {
            scope: value,
            revision,
        }) => {
            let workspace = scope(&store, cwd, &value)?;
            let row = store.set_project_policy_enabled_in(&workspace, revision, true)?;
            wake(root)?;
            vec![row]
        }
    };
    // Grants are keyed by directory; `conversation` names the latest
    // project view over it, or null.
    let mut described = Vec::new();
    for row in &rows {
        let view = store
            .latest_conversation_for_workspace(Path::new(&row.workspace))
            .ok()
            .flatten()
            .map(|conversation| conversation.id);
        described.push((
            view,
            store.workspace_name(&row.workspace)?,
            store.project_status(row)?,
        ));
    }
    if json {
        let rows = rows
            .iter()
            .zip(&described)
            .map(|(row, (view, name, status))| {
                let mut value = serde_json::to_value(row)?;
                value["conversation"] = serde_json::to_value(view)?;
                value["name"] = serde_json::to_value(name)?;
                value["status"] = serde_json::to_value(status)?;
                Ok(value)
            })
            .collect::<Result<Vec<_>>>()?;
        crate::print_json(rows)?;
    } else if rows.is_empty() {
        println!("No project grants. Use xcb projects configure <dir> <goal> --tasks N --hours N.");
    } else {
        for (row, (_, name, status)) in rows.into_iter().zip(described) {
            println!(
                "{} · {} · {} · {}/{} tasks used · expires {} · rev {}\n  {}",
                xcb_core::display_text(&name, 160),
                xcb_core::display_text(&row.workspace, 4096),
                status,
                row.admitted_tasks,
                row.max_tasks,
                row.expires_at_ms,
                row.revision,
                xcb_core::display_text(&row.goal, 4096)
            );
        }
    }
    Ok(0)
}

#[derive(Subcommand)]
pub enum ScheduleCommand {
    /// Schedule a pinned ALGAL planner or a managed-agent program with a
    /// call limit.
    Program {
        /// Conversation id from `xcb conversations`, or a project directory or
        /// name (the thread, in that directory).
        target: String,
        /// ALGAL manifest with a text summary output and optional prompt output.
        manifest: PathBuf,
        /// JSON object satisfying the manifest's input interface.
        #[arg(long)]
        inputs: Option<PathBuf>,
        /// Allow up to this many agent calls (1 to 8) per wake-up under a
        /// project grant.
        #[arg(long, value_parser = clap::value_parser!(u8).range(1..=8))]
        managed_calls: Option<u8>,
        /// Human-readable schedule and backlog label.
        #[arg(long, default_value = "Scheduled ALGAL planner")]
        title: String,
        /// Seconds between wake-ups, from 60 seconds to 365 days.
        #[arg(long, value_parser = clap::value_parser!(u64).range(60..=31_536_000))]
        every: u64,
        /// Exact project directory; required when the target is the thread.
        #[arg(long)]
        workspace: Option<PathBuf>,
    },
    /// Enable a recurring prompt; first wake-up occurs after the interval.
    Add {
        /// Conversation id from `xcb conversations`, or a project directory or
        /// name (the thread, in that directory).
        target: String,
        /// Prompt to enqueue on each eligible wake-up.
        prompt: String,
        /// Seconds between wake-ups, from 60 seconds to 365 days.
        #[arg(long, value_parser = clap::value_parser!(u64).range(60..=31_536_000))]
        every: u64,
        /// Exact project directory; required when the target is the thread.
        #[arg(long)]
        workspace: Option<PathBuf>,
    },
    /// Pause future wake-ups; already queued work is unchanged.
    Pause {
        /// Schedule id from `xcb schedules`.
        id: Id,
        /// Current schedule revision; stale updates are rejected.
        #[arg(long)]
        revision: u64,
    },
    /// Enable future wake-ups for a paused schedule.
    Resume {
        /// Schedule id from `xcb schedules`.
        id: Id,
        /// Current schedule revision; stale updates are rejected.
        #[arg(long)]
        revision: u64,
    },
    /// Show one schedule's detail, why it is or isn't running, and how its
    /// last wake-up ended.
    Show {
        /// Schedule id from `xcb schedules`.
        id: Id,
    },
    /// Change a schedule's prompt, interval or next wake-up; conversation
    /// and directory stay fixed.
    Edit {
        /// Schedule id from `xcb schedules`.
        id: Id,
        /// Current schedule revision; stale updates are rejected.
        #[arg(long)]
        revision: u64,
        /// New prompt for each wake-up.
        #[arg(long)]
        prompt: Option<String>,
        /// New seconds between wake-ups, from 60 seconds to 365 days.
        #[arg(long, value_parser = clap::value_parser!(u64).range(60..=31_536_000))]
        every: Option<u64>,
        /// Move the next wake-up to this many seconds from now.
        #[arg(long, value_parser = clap::value_parser!(u64).range(0..=31_536_000))]
        next_in: Option<u64>,
    },
    /// Delete a schedule. Tasks it already created keep their history and
    /// finish on their own terms.
    Delete {
        /// Schedule id from `xcb schedules`.
        id: Id,
        /// Current schedule revision; stale deletes are rejected.
        #[arg(long)]
        revision: u64,
    },
}

/// List filters for `xcb schedules`; exact and deterministic.
#[derive(Default)]
pub struct ScheduleFilters {
    /// Only schedules whose directory is this canonical path.
    pub workspace: Option<String>,
    /// Some(true) shows enabled only, Some(false) paused only.
    pub enabled: Option<bool>,
    /// Only schedules whose wake-up is now or overdue.
    pub due: bool,
}

#[derive(Subcommand)]
pub enum MemoryCommand {
    /// Bind an explicit local Wordcell vault and trusted executable to a project.
    Configure {
        /// Project directory or name; a conversation id names its directory.
        scope: String,
        /// Existing local Wordcell vault directory.
        #[arg(long)]
        vault: PathBuf,
        /// Absolute installed Wordcell CLI path; its bytes and interpreter are pinned.
        #[arg(long)]
        wordcell: PathBuf,
        /// Required when replacing an existing binding.
        #[arg(long)]
        revision: Option<u64>,
    },
    /// Inspect the project binding and open upgrade conflicts without reading the vault.
    Status {
        /// Project directory or name; a conversation id names its directory.
        scope: String,
    },
    /// Search only the project's bound local vault; results are historical context.
    Search {
        /// Project directory or name with an explicit memory binding.
        scope: String,
        /// Exact local search text, at most 1024 bytes.
        query: String,
        /// Maximum number of returned hits, from 1 to 16.
        #[arg(long, default_value_t = 8, value_parser = clap::value_parser!(u8).range(1..=16))]
        limit: u8,
    },
    /// Explicitly promote a supplied note with task provenance; never a transcript.
    Promote {
        /// Source task; its project directory owns the Wordcell binding.
        task: Id,
        /// UTF-8 note, at most 8 KiB; saving the same note again changes
        /// nothing.
        #[arg(long)]
        body_file: PathBuf,
    },
}

fn read_bounded(path: &Path, max: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take((max + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > max {
        return Err(Error::Unavailable(
            "input file exceeds the command's byte limit",
        ));
    }
    Ok(bytes)
}

pub async fn memory(root: &Path, cwd: &Path, command: MemoryCommand, json: bool) -> Result<i32> {
    let store = ManagedStore::open(root)?;
    let value = match command {
        MemoryCommand::Configure {
            scope: value,
            vault,
            wordcell,
            revision,
        } => {
            let workspace = scope(&store, cwd, &value)?;
            let config = xcb_runtime::wordcell::WordcellConfig::admit(
                &wordcell,
                &xcb_core::canonical(&vault)?,
            )?;
            serde_json::to_value(store.bind_memory_in(Path::new(&workspace), revision, config)?)?
        }
        MemoryCommand::Status { scope: value } => {
            let workspace = scope(&store, cwd, &value)?;
            let conflicts: Vec<_> = store
                .migration_conflicts(true)?
                .into_iter()
                .filter(|conflict| {
                    conflict.kind == "memory" && conflict.workspace.as_deref() == Some(&workspace)
                })
                .collect();
            serde_json::json!({
                "workspace": workspace,
                "binding": store.memory_binding_in(&workspace)?,
                "conflicts": conflicts,
            })
        }
        MemoryCommand::Search {
            scope: value,
            query,
            limit,
        } => {
            let workspace = scope(&store, cwd, &value)?;
            store
                .search_memory_in(&workspace, &query, usize::from(limit))
                .await?
        }
        MemoryCommand::Promote { task, body_file } => {
            let task = store.resolve_task(&task)?;
            let note = String::from_utf8(read_bounded(&body_file, 8192)?)
                .map_err(|_| Error::Unavailable("memory note must be UTF-8"))?;
            let receipt = store.promote_memory(&task, &note).await?;
            let unsettled = receipt.status != xcb_runtime::wordcell::PromotionStatus::Completed;
            crate::print_json(&receipt)?;
            return Ok(if unsettled { 2 } else { 0 });
        }
    };
    if json {
        crate::print_json(value)?;
    } else {
        println!(
            "{}",
            xcb_core::display_text(&serde_json::to_string_pretty(&value)?, 65536)
        );
    }
    Ok(0)
}

fn wake(root: &Path) -> Result<()> {
    managed::ensure_daemon(root, &std::env::current_exe()?)
}

fn print_daemon(status: &managed::DaemonStatus, json: bool) -> Result<()> {
    if json {
        return crate::print_json(status);
    }
    println!(
        "{} · {} · generation {}/{} · {} calls",
        status.process, status.status, status.generation, status.max_generations, status.calls,
    );
    if let Some(digest) = &status.pending_call {
        println!(
            "  pending: {digest} · child {} · {}",
            status
                .pending_child
                .as_ref()
                .map(|id| id.as_str())
                .unwrap_or("unpublished"),
            status.pending_child_status.as_deref().unwrap_or("unknown"),
        );
    }
    if !status.wake.is_empty() {
        println!("  wake: {}", status.wake.len());
    }
    Ok(())
}

pub async fn daemons(
    root: &Path,
    cwd: &Path,
    command: Option<DaemonCommand>,
    json: bool,
) -> Result<i32> {
    let store = std::sync::Arc::new(ManagedStore::open(root)?);
    match command {
        Some(DaemonCommand::Run {
            target,
            name,
            manifest,
            inputs,
            calls,
            generations,
            workspace,
        }) => {
            use xcb_runtime::managed::{AdmittedDaemon, MAX_DAEMON_GENERATIONS};
            use xcb_runtime::managed_program::{MAX_INPUT_BYTES, MAX_MANIFEST_BYTES};
            let manifest = serde_json::from_slice(&read_bounded(&manifest, MAX_MANIFEST_BYTES)?)?;
            let inputs = match inputs {
                Some(path) => serde_json::from_slice(&read_bounded(&path, MAX_INPUT_BYTES)?)?,
                None => serde_json::json!({}),
            };
            let daemon = AdmittedDaemon::admit(
                manifest,
                inputs,
                calls,
                usize::try_from(generations)
                    .ok()
                    .filter(|value| *value <= MAX_DAEMON_GENERATIONS)
                    .ok_or(Error::Unavailable("invalid daemon generation bound"))?,
            )?;
            let (conversation, workspace) =
                entry_target(&store, cwd, &target, workspace.as_deref()).await?;
            let status =
                store.enqueue_daemon_at(&conversation, workspace.as_deref(), &name, &daemon)?;
            wake(root)?;
            print_daemon(&status, json)?;
        }
        Some(DaemonCommand::Inspect { name }) => {
            let status = store
                .daemon_status_for(&name)?
                .ok_or(Error::Unavailable("daemon not found"))?;
            print_daemon(&status, json)?;
        }
        Some(DaemonCommand::Send { name, text }) => {
            store.daemon_send(&name, &text)?;
            wake_saved_inbox(root, &new_id("wake"));
            if json {
                crate::print_json(serde_json::json!({"daemon":name,"sent":true}))?;
            } else {
                println!("Sent to {name}; the daemon reads it on its next wake tick.");
            }
        }
        Some(DaemonCommand::Stop { name }) => {
            store.daemon_stop(&name)?;
            if json {
                crate::print_json(serde_json::json!({"daemon":name,"stopped":true}))?;
            } else {
                println!("Daemon {name} stopped; a child task already running finishes first.");
            }
        }
        Some(DaemonCommand::Journal { name }) => {
            let journal = store
                .daemon_journal(&name)?
                .ok_or(Error::Unavailable("no daemon journal recorded"))?;
            crate::print_json(journal)?;
        }
        Some(DaemonCommand::Recover { name, intent }) => {
            let status = store.daemon_recover(&name, &intent).await?;
            wake(root)?;
            print_daemon(&status, json)?;
        }
        None => {
            let rows = store.daemons()?;
            if json {
                crate::print_json(rows)?;
            } else if rows.is_empty() {
                println!("No daemons. Use xcb daemons run <dir> <name> <manifest>.");
            } else {
                for row in &rows {
                    print_daemon(row, false)?;
                }
            }
        }
    }
    Ok(0)
}

fn print_task(task: &ManagedTask, json: bool) -> Result<()> {
    if json {
        return crate::print_json(task);
    }
    println!(
        "{} · {} · P{} · rev {} · {} · {}",
        task.id,
        task.habitat_status(),
        task.priority,
        task.revision,
        task.conversation,
        xcb_core::display_text(&task.title, 160)
    );
    if let Some(summary) = &task.last_output {
        println!("  {}", xcb_core::display_text(summary, 4096));
    } else if !task.detail.is_empty() {
        println!("  {}", xcb_core::display_text(&task.detail, 4096));
    }
    Ok(())
}

pub async fn backlog(
    root: &Path,
    cwd: &Path,
    command: Option<BacklogCommand>,
    conversation: Option<&Id>,
    workspace: Option<&Path>,
    json: bool,
) -> Result<i32> {
    let store = ManagedStore::open(root)?;
    let mut runnable = false;
    let task = match command {
        Some(BacklogCommand::Context {
            target,
            recipe,
            id,
            workspace,
        }) => {
            let recipe = crate::context::load(&recipe)?;
            let (conversation, workspace) =
                entry_target(&store, cwd, &target, workspace.as_deref()).await?;
            let task = store
                .enqueue_program_at(
                    &conversation,
                    workspace.as_deref(),
                    BindingOrigin::Cli,
                    id.unwrap_or_else(|| new_id("input")),
                    recipe.plan.question,
                    recipe.program,
                )
                .await?;
            runnable = true;
            task
        }
        Some(BacklogCommand::Program {
            target,
            manifest,
            inputs,
            managed_calls,
            title,
            id,
            workspace,
        }) => {
            let program = load_program(&manifest, inputs.as_deref(), managed_calls)?;
            let (conversation, workspace) =
                entry_target(&store, cwd, &target, workspace.as_deref()).await?;
            let task = store
                .enqueue_program_at(
                    &conversation,
                    workspace.as_deref(),
                    BindingOrigin::Cli,
                    id.unwrap_or_else(|| new_id("input")),
                    title,
                    program,
                )
                .await?;
            runnable = true;
            task
        }
        Some(BacklogCommand::ProgramStatus { id }) => {
            let id = store.resolve_task(&id)?;
            let status = store
                .program_status(&id)?
                .ok_or(Error::Unavailable("managed program not found"))?;
            if json {
                crate::print_json(status)?;
            } else {
                println!(
                    "{} · {} · {}/{} calls",
                    status.parent,
                    xcb_core::display_text(&status.phase, 160),
                    status.calls,
                    status.max_calls,
                );
                if let Some(child) = status.child {
                    println!(
                        "  child: {child} · {}",
                        xcb_core::display_text(
                            status.child_status.as_deref().unwrap_or("unknown"),
                            512
                        ),
                    );
                    println!("  xcb attention · resolve questions and approvals on the child");
                }
                if let Some(receipt) = status.receipt {
                    println!("  record: {}", xcb_core::display_text(&receipt, 160));
                }
            }
            return Ok(0);
        }
        Some(BacklogCommand::Complete {
            id,
            summary,
            revision,
        }) => {
            store
                .complete_backlog(&store.resolve_task(&id)?, revision, summary)
                .await?
        }
        Some(BacklogCommand::Reconcile { id, revision }) => {
            let id = store.resolve_task(&id)?;
            let runs = xcb_runtime::store::Store::open(root)?;
            let task = store.reconcile_uncertain(&runs, &id, revision).await?;
            runnable = true;
            task
        }
        Some(BacklogCommand::Dismiss { id, revision }) => {
            let id = store.resolve_task(&id)?;
            let runs = xcb_runtime::store::Store::open(root)?;
            store.dismiss_uncertain(&runs, &id, revision).await?
        }
        None => {
            let tasks = match workspace {
                Some(dir) => store.backlog_in(
                    exact_workspace(&store, cwd, dir)?
                        .to_str()
                        .ok_or(Error::PrivateState)?,
                    256,
                )?,
                None => {
                    let conversation = conversation
                        .map(|id| store.resolve_conversation(id))
                        .transpose()?;
                    store.backlog(conversation.as_ref(), 256)?
                }
            };
            if json {
                crate::print_json(tasks)?;
            } else if tasks.is_empty() {
                println!("No backlog or work history.");
            } else {
                for task in tasks {
                    print_task(&task, false)?;
                }
            }
            return Ok(0);
        }
        Some(BacklogCommand::Memory { scope: value }) => {
            let memory = store.working_memory_in(&scope(&store, cwd, &value)?, 32)?;
            if json {
                crate::print_json(memory)?;
            } else if memory.is_empty() {
                println!("No recorded work summaries.");
            } else {
                for item in memory {
                    println!(
                        "{} · {} · {}\n{}",
                        item.task,
                        item.state.label(),
                        xcb_core::display_text(&item.title, 160),
                        xcb_core::display_text(&item.summary, 4096)
                    );
                }
            }
            return Ok(0);
        }
        Some(BacklogCommand::Add {
            target,
            prompt,
            ready,
            priority,
            id,
            workspace,
            model,
        }) => {
            runnable = ready;
            let (conversation, workspace) =
                entry_target(&store, cwd, &target, workspace.as_deref()).await?;
            store
                .enqueue_backlog_at(
                    &conversation,
                    workspace.as_deref(),
                    BindingOrigin::Cli,
                    id.unwrap_or_else(|| new_id("input")),
                    prompt,
                    !ready,
                    priority,
                    model.filter(|model| model != "auto"),
                )
                .await?
        }
        Some(BacklogCommand::Edit {
            id,
            prompt,
            revision,
            priority,
        }) => {
            let id = store.resolve_task(&id)?;
            let priority = match priority {
                Some(priority) => priority,
                None => {
                    store
                        .task(&id)?
                        .ok_or(Error::Unavailable("managed task not found"))?
                        .priority
                }
            };
            store.edit_backlog(&id, revision, prompt, priority).await?
        }
        Some(BacklogCommand::Release { id, revision }) => {
            runnable = true;
            store
                .release_backlog(&store.resolve_task(&id)?, revision)
                .await?
        }
        Some(BacklogCommand::Recall {
            id,
            revision,
            operation,
        }) => {
            let task = store
                .recall_queued(&store.resolve_task(&id)?, revision, &operation)
                .await?;
            if json {
                crate::print_json(&task)?;
            } else {
                println!(
                    "{}",
                    xcb_core::display_text(
                        task.backlog_prompt.as_deref().unwrap_or(&task.goal),
                        xcb_core::MAX_TEXT_BYTES
                    )
                );
            }
            return Ok(0);
        }
        Some(BacklogCommand::Reply {
            id,
            text,
            revision,
            reply_id,
        }) => {
            runnable = true;
            let id = store.resolve_task(&id)?;
            let revision = match revision {
                Some(revision) => revision,
                None => {
                    store
                        .task(&id)?
                        .ok_or(Error::Unavailable("managed task not found"))?
                        .revision
                }
            };
            store
                .reply_to_task_checked(
                    &id,
                    revision,
                    reply_id.unwrap_or_else(|| new_id("reply")),
                    text,
                )
                .await?
        }
    };
    print_task(&task, json)?;
    if runnable {
        wake(root)?;
    }
    Ok(0)
}

pub fn steer(root: &Path, task: &Id, id: Option<Id>, text: String, json: bool) -> Result<i32> {
    let store = ManagedStore::open(root)?;
    let task = &store.resolve_task(task)?;
    let event = store.steer_task(task, id.unwrap_or_else(|| new_id("inbox")), text)?;
    print_inbox_event(&event, json)?;
    wake_saved_inbox(root, &event.id);
    Ok(0)
}

pub fn watch(root: &Path, target: &Id, source: &Id, id: Option<Id>, json: bool) -> Result<i32> {
    let store = ManagedStore::open(root)?;
    let target = &store.resolve_task(target)?;
    let source = &store.resolve_task(source)?;
    let id = id.unwrap_or_else(|| new_id("watch"));
    let subscription = store.watch_task(target, source, id.clone())?;
    if json {
        crate::print_json(subscription)?;
    } else {
        println!(
            "Watch {id} saved: task {source} → inbox for {target}.\nThe report arrives at the target task's next turn; closed tasks stay closed."
        );
    }
    wake_saved_inbox(root, &id);
    Ok(0)
}

fn wake_saved_inbox(root: &Path, id: &Id) {
    if let Err(error) = wake(root) {
        eprintln!(
            "warning: saved as {id}, but xcb's background supervisor couldn't start: {error}. The message is kept and arrives at the task's next turn once the supervisor runs."
        );
    }
}

pub fn inbox(
    root: &Path,
    task: Option<&Id>,
    conversation: Option<&Id>,
    before: Option<u64>,
    limit: usize,
    json: bool,
) -> Result<i32> {
    let store = ManagedStore::open(root)?;
    let task = task.map(|id| store.resolve_task(id)).transpose()?;
    let conversation = conversation
        .map(|id| store.resolve_conversation(id))
        .transpose()?;
    let events = store.inbox(task.as_ref(), conversation.as_ref(), before, limit)?;
    if json {
        crate::print_json(events)?;
    } else if events.is_empty() {
        println!("No inbox events on this page.");
    } else {
        for event in &events {
            print_inbox_event(event, false)?;
        }
        if events.len() == limit {
            println!(
                "Older events: repeat with --before {}",
                events.last().expect("nonempty inbox page").sequence
            );
        }
    }
    Ok(0)
}

fn print_inbox_event(event: &managed::InboxEvent, json: bool) -> Result<()> {
    if json {
        crate::print_json(event)?;
    } else {
        println!(
            "{} · task {} · {} · {} · sequence {}\n  {}",
            event.id,
            event.task,
            xcb_core::display_text(&event.kind, 80),
            xcb_core::display_text(&event.status, 512),
            event.sequence,
            xcb_core::display_text(&event.text, 16_384),
        );
        if let Some(reason) = &event.reason {
            println!("  {}", xcb_core::display_text(reason, 512));
        }
        if let Some(receipt) = &event.receipt {
            println!("  record: {}", xcb_core::display_text(receipt, 256));
        }
    }
    Ok(())
}

fn load_program(
    manifest: &Path,
    inputs: Option<&Path>,
    managed_calls: Option<u8>,
) -> Result<xcb_runtime::managed_program::AdmittedProgram> {
    use xcb_runtime::managed_program::{AdmittedProgram, MAX_INPUT_BYTES, MAX_MANIFEST_BYTES};
    let manifest = serde_json::from_slice(&read_bounded(manifest, MAX_MANIFEST_BYTES)?)?;
    let inputs = match inputs {
        Some(path) => serde_json::from_slice(&read_bounded(path, MAX_INPUT_BYTES)?)?,
        None => serde_json::json!({}),
    };
    match managed_calls {
        Some(calls) => AdmittedProgram::admit_managed(manifest, inputs, calls),
        None => AdmittedProgram::admit(manifest, inputs),
    }
}

pub async fn schedules(
    root: &Path,
    cwd: &Path,
    command: Option<ScheduleCommand>,
    conversation: Option<&Id>,
    filters: ScheduleFilters,
    json: bool,
) -> Result<i32> {
    let store = ManagedStore::open(root)?;
    if let Some(ScheduleCommand::Delete { id, revision }) = &command {
        let id = store.resolve_schedule(id)?;
        store.delete_schedule(&id, *revision)?;
        wake(root)?;
        if json {
            crate::print_json(serde_json::json!({"deleted": id}))?;
        } else {
            println!("Deleted schedule {id}; tasks it created are unaffected.");
        }
        return Ok(0);
    }
    let rows = match command {
        Some(ScheduleCommand::Program {
            target,
            manifest,
            inputs,
            managed_calls,
            title,
            every,
            workspace,
        }) => {
            let program = load_program(&manifest, inputs.as_deref(), managed_calls)?;
            let interval = every * 1000;
            let first = now_ms()
                .checked_add(interval)
                .ok_or(Error::Unavailable("schedule time overflow"))?;
            let (conversation, workspace) =
                entry_target(&store, cwd, &target, workspace.as_deref()).await?;
            let schedule = store
                .create_program_schedule_at(
                    &conversation,
                    workspace.as_deref(),
                    title,
                    program,
                    interval,
                    first,
                )
                .await?;
            wake(root)?;
            vec![schedule]
        }
        None => {
            let conversation = conversation
                .map(|id| store.resolve_conversation(id))
                .transpose()?;
            let now = now_ms();
            store
                .schedule_views(conversation.as_ref())?
                .into_iter()
                .filter(|view| {
                    filters
                        .workspace
                        .as_deref()
                        .is_none_or(|workspace| view.workspace.as_deref() == Some(workspace))
                        && filters
                            .enabled
                            .is_none_or(|enabled| view.schedule.enabled == enabled)
                        && (!filters.due
                            || (view.schedule.enabled && view.schedule.next_due_ms <= now))
                })
                .map(|view| view.schedule)
                .collect()
        }
        Some(ScheduleCommand::Add {
            target,
            prompt,
            every,
            workspace,
        }) => {
            let interval = every
                .checked_mul(1000)
                .ok_or(Error::Unavailable("schedule interval overflow"))?;
            let first = now_ms()
                .checked_add(interval)
                .ok_or(Error::Unavailable("schedule time overflow"))?;
            let (conversation, workspace) =
                entry_target(&store, cwd, &target, workspace.as_deref()).await?;
            let schedule = store
                .create_schedule_at(&conversation, workspace.as_deref(), prompt, interval, first)
                .await?;
            wake(root)?;
            vec![schedule]
        }
        Some(ScheduleCommand::Pause { id, revision }) => {
            vec![store.set_schedule_enabled(&store.resolve_schedule(&id)?, revision, false)?]
        }
        Some(ScheduleCommand::Resume { id, revision }) => {
            let schedule =
                store.set_schedule_enabled(&store.resolve_schedule(&id)?, revision, true)?;
            wake(root)?;
            vec![schedule]
        }
        Some(ScheduleCommand::Show { id }) => {
            vec![
                store
                    .schedule(&store.resolve_schedule(&id)?)?
                    .ok_or(Error::Unavailable("schedule not found"))?,
            ]
        }
        Some(ScheduleCommand::Edit {
            id,
            revision,
            prompt,
            every,
            next_in,
        }) => {
            let next_due_ms = next_in
                .map(|seconds| {
                    seconds
                        .checked_mul(1000)
                        .and_then(|delay| now_ms().checked_add(delay))
                        .ok_or(Error::Unavailable("schedule time overflow"))
                })
                .transpose()?;
            let schedule = store.update_schedule(
                &store.resolve_schedule(&id)?,
                revision,
                prompt,
                every
                    .map(|seconds| {
                        seconds
                            .checked_mul(1000)
                            .ok_or(Error::Unavailable("schedule interval overflow"))
                    })
                    .transpose()?,
                next_due_ms,
            )?;
            wake(root)?;
            vec![schedule]
        }
        Some(ScheduleCommand::Delete { .. }) => unreachable!("handled above"),
    };
    let views = rows
        .iter()
        .map(|row| store.schedule_view(row))
        .collect::<Result<Vec<ScheduleView>>>()?;
    if json {
        crate::print_json(&views)?;
    } else if views.is_empty() {
        println!("No schedules. Use xcb schedules add <dir> <prompt> --every <seconds>.");
    } else {
        for view in views {
            let row = &view.schedule;
            println!(
                "{} · {} · {} · every {}s · due {} · rev {}",
                row.id,
                row.conversation,
                if row.enabled { "enabled" } else { "paused" },
                row.interval_ms / 1000,
                row.next_due_ms,
                row.revision,
            );
            if let Some(workspace) = &view.workspace {
                println!("  {}", xcb_core::display_text(workspace, 4096));
            }
            if let Some(blocker) = &view.blocker {
                println!("  blocked: {}", xcb_core::display_text(blocker, 256));
            }
            if let (Some(task), Some(state)) = (&row.last_task, view.last_task_state) {
                println!(
                    "  last: {} {}{}",
                    task,
                    state.as_str(),
                    view.last_task_detail
                        .as_deref()
                        .map(|detail| format!(" · {}", xcb_core::display_text(detail, 160)))
                        .unwrap_or_default()
                );
            }
            println!("  {}", xcb_core::display_text(&row.prompt, 4096));
        }
    }
    Ok(0)
}

pub fn attention(root: &Path, json: bool) -> Result<i32> {
    let store = ManagedStore::open(root)?;
    let tasks = store.attention(256)?;
    if json {
        crate::print_json(tasks)?;
    } else if tasks.is_empty() {
        println!("No questions, approvals or actions need attention.");
    } else {
        for task in tasks {
            println!(
                "{} · {} · {} · {}",
                task.id,
                task.conversation,
                task.habitat_ui_state().label(),
                xcb_core::display_text(&task.detail, 4096)
            );
            if let Some(summary) = &task.last_output {
                println!("  {}", xcb_core::display_text(summary, 4096));
            }
            if task.attention == Some(State::NeedsApproval) {
                println!("  Inspect the gated action; a backlog reply does not grant permission.");
            }
        }
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn parses(args: &[&str]) -> bool {
        crate::Cli::try_parse_from(std::iter::once("xcb").chain(args.iter().copied())).is_ok()
    }

    #[test]
    fn scope_and_workspace_arguments_parse() {
        assert!(parses(&[
            "memory",
            "configure",
            "/abs/project",
            "--vault",
            "/abs/vault",
            "--wordcell",
            "/abs/bin/wordcell"
        ]));
        assert!(parses(&["memory", "status", "/abs/project"]));
        assert!(parses(&["memory", "search", "project", "parser"]));
        assert!(parses(&[
            "projects",
            "configure",
            "/abs/project",
            "Maintain parser",
            "--tasks",
            "2",
            "--hours",
            "1"
        ]));
        assert!(parses(&["projects", "pause", ".", "--revision", "1"]));
        assert!(parses(&[
            "backlog",
            "add",
            "c_global",
            "Fix the parser",
            "--workspace",
            "/abs/project"
        ]));
        assert!(parses(&["backlog", "add", "project", "Fix the parser"]));
        assert!(parses(&["backlog", "--workspace", "/abs/project"]));
        assert!(!parses(&[
            "backlog",
            "--workspace",
            "/abs/project",
            "--conversation",
            "c_view"
        ]));
        assert!(parses(&["backlog", "memory", "project"]));
        assert!(parses(&[
            "schedules",
            "add",
            "c_global",
            "Check",
            "--every",
            "3600",
            "--workspace",
            "/abs/project"
        ]));
        assert!(parses(&[
            "daemons",
            "run",
            "project",
            "worker",
            "daemon.json",
            "--workspace",
            "/abs/project"
        ]));
    }

    struct Scratch(PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[tokio::test]
    async fn scopes_resolve_directories_and_refuse_the_thread_and_ambiguous_names() {
        let path = std::env::temp_dir().join(new_id("habitat_scope").as_str());
        std::fs::create_dir_all(&path).unwrap();
        let scratch = Scratch(xcb_core::canonical(&path).unwrap());
        let base = &scratch.0;
        let (one, two) = (base.join("one").join("app"), base.join("two").join("app"));
        std::fs::create_dir_all(&one).unwrap();
        std::fs::create_dir_all(&two).unwrap();
        let store = ManagedStore::open(&base.join("state")).unwrap();
        let view = store.create_conversation(&one).await.unwrap();
        store.admit_workspace(&one, "command", None).unwrap();
        let thread = store.global_thread().await.unwrap().id;
        let one_text = one.to_str().unwrap().to_owned();
        assert_eq!(scope(&store, base, &one_text).unwrap(), one_text);
        assert_eq!(scope(&store, base, view.id.as_str()).unwrap(), one_text);
        assert_eq!(scope(&store, base, "app").unwrap(), one_text);
        assert!(matches!(
            scope(&store, base, GLOBAL_THREAD_ID),
            Err(Error::Conflict(_))
        ));
        store.admit_workspace(&two, "command", None).unwrap();
        assert!(matches!(
            scope(&store, base, "app"),
            Err(Error::Guided { .. })
        ));
        // Entry targets: a view keeps its conversation, a scope means the
        // thread in that directory, and the thread itself needs --workspace.
        assert_eq!(
            entry_target(&store, base, view.id.as_str(), None)
                .await
                .unwrap(),
            (view.id.clone(), None)
        );
        assert_eq!(
            entry_target(&store, base, &one_text, None).await.unwrap(),
            (thread.clone(), Some(one.clone()))
        );
        assert!(
            entry_target(&store, base, GLOBAL_THREAD_ID, None)
                .await
                .is_err()
        );
        assert_eq!(
            entry_target(&store, base, GLOBAL_THREAD_ID, Some(&two))
                .await
                .unwrap(),
            (thread, Some(two.clone()))
        );
        assert!(
            entry_target(&store, base, &one_text, Some(&two))
                .await
                .is_err()
        );
        // Relative scopes and --workspace start at --cwd, not the process cwd.
        assert_eq!(scope(&store, base, "./one/app").unwrap(), one_text);
        assert_eq!(
            scope(&store, &base.join("one"), ".").unwrap(),
            base.join("one").to_str().unwrap()
        );
        assert_eq!(
            entry_target(
                &store,
                &base.join("two"),
                GLOBAL_THREAD_ID,
                Some(Path::new("app"))
            )
            .await
            .unwrap()
            .1,
            Some(two.clone())
        );
    }
}
