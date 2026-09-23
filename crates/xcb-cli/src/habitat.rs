use clap::Subcommand;
use std::{
    io::Read,
    path::{Path, PathBuf},
};
use xcb_core::{Id, Provider, session::State};
use xcb_runtime::{
    Error, Result,
    managed::{self, ManagedStore, ManagedTask},
    new_id, now_ms,
};

#[derive(Subcommand)]
pub enum BacklogCommand {
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
    /// Hold work for later; --ready releases it immediately.
    Add {
        /// Persistent conversation id from `xcb conversations`.
        conversation: Id,
        /// Work to retain in this conversation's backlog.
        prompt: String,
        /// Dispatch immediately instead of holding the task for later.
        #[arg(long)]
        ready: bool,
        /// Queue priority from 0 to 9; larger values run first.
        #[arg(long, default_value_t = 5, value_parser = clap::value_parser!(u8).range(0..=9))]
        priority: u8,
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
    },
    /// Read recent work summaries for a persistent conversation.
    Memory {
        /// Persistent conversation id from `xcb conversations`.
        conversation: Id,
    },
}

#[derive(Subcommand)]
pub enum ProjectCommand {
    /// Grant bounded automatic follow-up work for a project goal.
    Configure {
        /// Persistent project conversation from `xcb conversations`.
        conversation: Id,
        /// Authoritative project goal for automatically admitted work.
        goal: String,
        /// Maximum automatically admitted tasks in this grant.
        #[arg(long, value_parser = clap::value_parser!(u32).range(1..=100))]
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
    },
    /// Pause automatic dispatch; running work is allowed to settle.
    Pause {
        /// Project conversation from `xcb projects`.
        conversation: Id,
        /// Current policy revision; stale updates are rejected.
        #[arg(long)]
        revision: u64,
    },
    /// Resume the same grant without replenishing its task budget or expiry.
    Resume {
        /// Project conversation from `xcb projects`.
        conversation: Id,
        /// Current policy revision; resuming does not renew the grant.
        #[arg(long)]
        revision: u64,
    },
}

pub fn projects(root: &Path, command: Option<ProjectCommand>, json: bool) -> Result<i32> {
    let store = ManagedStore::open(root)?;
    let rows = match command {
        None => store.project_policies()?,
        Some(ProjectCommand::Configure {
            conversation,
            goal,
            tasks,
            hours,
            revision,
            provider,
        }) => {
            let expiry = now_ms()
                .checked_add(hours * 3_600_000)
                .ok_or(Error::Unavailable("project expiry overflow"))?;
            let row = store.configure_project_policy(
                &conversation,
                revision,
                goal,
                tasks,
                expiry,
                provider,
            )?;
            wake(root)?;
            vec![row]
        }
        Some(ProjectCommand::Pause {
            conversation,
            revision,
        }) => {
            vec![store.set_project_policy_enabled(&conversation, revision, false)?]
        }
        Some(ProjectCommand::Resume {
            conversation,
            revision,
        }) => {
            let row = store.set_project_policy_enabled(&conversation, revision, true)?;
            wake(root)?;
            vec![row]
        }
    };
    if json {
        crate::print_json(rows)?;
    } else if rows.is_empty() {
        println!(
            "No project grants. Use xcb projects configure <conversation> <goal> --tasks N --hours N."
        );
    } else {
        for row in rows {
            println!(
                "{} · {} · {}/{} tasks used · expires {} · rev {}\n  {}",
                row.conversation,
                row.status(),
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
    /// Schedule a pinned, deterministic ALGAL planning program.
    Program {
        /// Persistent project conversation from `xcb conversations`.
        conversation: Id,
        /// ALGAL manifest with a text summary output and optional prompt output.
        manifest: PathBuf,
        /// JSON object satisfying the manifest's input interface.
        #[arg(long)]
        inputs: Option<PathBuf>,
        /// Human-readable schedule and backlog label.
        #[arg(long, default_value = "Scheduled ALGAL planner")]
        title: String,
        /// Seconds between wake-ups, from 60 seconds to 365 days.
        #[arg(long, value_parser = clap::value_parser!(u64).range(60..=31_536_000))]
        every: u64,
    },
    /// Enable a recurring prompt; first wake-up occurs after the interval.
    Add {
        /// Persistent conversation to wake, from `xcb conversations`.
        conversation: Id,
        /// Prompt to enqueue on each eligible wake-up.
        prompt: String,
        /// Seconds between wake-ups, from 60 seconds to 365 days.
        #[arg(long, value_parser = clap::value_parser!(u64).range(60..=31_536_000))]
        every: u64,
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
}

#[derive(Subcommand)]
pub enum MemoryCommand {
    /// Bind an explicit local Wordcell vault and trusted executable to a project.
    Configure {
        /// Persistent project conversation from `xcb conversations`.
        conversation: Id,
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
    /// Inspect the project binding without reading the vault.
    Status {
        /// Persistent project conversation from `xcb conversations`.
        conversation: Id,
    },
    /// Search only the project's bound local vault; results are historical context.
    Search {
        /// Project conversation with an explicit memory binding.
        conversation: Id,
        /// Exact local search text, at most 1024 bytes.
        query: String,
        /// Maximum number of returned hits, from 1 to 16.
        #[arg(long, default_value_t = 8, value_parser = clap::value_parser!(u8).range(1..=16))]
        limit: u8,
    },
    /// Explicitly promote a supplied note with task provenance; never a transcript.
    Promote {
        /// Source task whose conversation owns the Wordcell binding.
        task: Id,
        /// UTF-8 note, at most 8 KiB; identical promotion is idempotent.
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

pub async fn memory(root: &Path, command: MemoryCommand, json: bool) -> Result<i32> {
    let store = ManagedStore::open(root)?;
    let value = match command {
        MemoryCommand::Configure {
            conversation,
            vault,
            wordcell,
            revision,
        } => {
            let config =
                xcb_runtime::wordcell::WordcellConfig::admit(&wordcell, &vault.canonicalize()?)?;
            serde_json::to_value(store.bind_memory(&conversation, revision, config)?)?
        }
        MemoryCommand::Status { conversation } => {
            serde_json::to_value(store.memory_binding(&conversation)?)?
        }
        MemoryCommand::Search {
            conversation,
            query,
            limit,
        } => {
            store
                .search_memory(&conversation, &query, usize::from(limit))
                .await?
        }
        MemoryCommand::Promote { task, body_file } => {
            let item = store
                .task(&task)?
                .ok_or(Error::Unavailable("managed task not found"))?;
            let note = String::from_utf8(read_bounded(&body_file, 8192)?)
                .map_err(|_| Error::Unavailable("memory note must be UTF-8"))?;
            let receipt = store
                .promote_memory(&item.conversation, &task, &note)
                .await?;
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

fn print_task(task: &ManagedTask, json: bool) -> Result<()> {
    if json {
        return crate::print_json(task);
    }
    println!(
        "{} · {} · P{} · rev {} · {} · {}",
        task.id,
        if task.deferred {
            "backlog"
        } else {
            task.state.label()
        },
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
    command: Option<BacklogCommand>,
    conversation: Option<&Id>,
    json: bool,
) -> Result<i32> {
    let store = ManagedStore::open(root)?;
    let mut runnable = false;
    let task = match command {
        Some(BacklogCommand::Complete {
            id,
            summary,
            revision,
        }) => store.complete_backlog(&id, revision, summary).await?,
        Some(BacklogCommand::Reconcile { id, revision }) => {
            let runs = xcb_runtime::store::Store::open(root)?;
            let task = store.reconcile_uncertain(&runs, &id, revision).await?;
            runnable = true;
            task
        }
        None => {
            let tasks = store.backlog(conversation, 256)?;
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
        Some(BacklogCommand::Memory { conversation }) => {
            let memory = store.working_memory(&conversation, 32)?;
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
            conversation,
            prompt,
            ready,
            priority,
        }) => {
            runnable = ready;
            store
                .enqueue_backlog(&conversation, new_id("input"), prompt, !ready, priority)
                .await?
        }
        Some(BacklogCommand::Edit {
            id,
            prompt,
            revision,
            priority,
        }) => {
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
            store.release_backlog(&id, revision).await?
        }
        Some(BacklogCommand::Reply { id, text }) => {
            runnable = true;
            store.reply_to_task(&id, text).await?
        }
    };
    print_task(&task, json)?;
    if runnable {
        wake(root)?;
    }
    Ok(0)
}

pub async fn schedules(
    root: &Path,
    command: Option<ScheduleCommand>,
    conversation: Option<&Id>,
    json: bool,
) -> Result<i32> {
    let store = ManagedStore::open(root)?;
    let rows = match command {
        Some(ScheduleCommand::Program {
            conversation,
            manifest,
            inputs,
            title,
            every,
        }) => {
            let manifest = serde_json::from_slice(&read_bounded(
                &manifest,
                xcb_runtime::managed_program::MAX_MANIFEST_BYTES,
            )?)?;
            let inputs = match inputs {
                Some(path) => serde_json::from_slice(&read_bounded(
                    &path,
                    xcb_runtime::managed_program::MAX_INPUT_BYTES,
                )?)?,
                None => serde_json::json!({}),
            };
            let program = xcb_runtime::managed_program::AdmittedProgram::admit(manifest, inputs)?;
            let interval = every * 1000;
            let first = now_ms()
                .checked_add(interval)
                .ok_or(Error::Unavailable("schedule time overflow"))?;
            let schedule = store
                .create_program_schedule(&conversation, title, program, interval, first)
                .await?;
            wake(root)?;
            vec![schedule]
        }
        None => store.schedules(conversation)?,
        Some(ScheduleCommand::Add {
            conversation,
            prompt,
            every,
        }) => {
            let interval = every
                .checked_mul(1000)
                .ok_or(Error::Unavailable("schedule interval overflow"))?;
            let first = now_ms()
                .checked_add(interval)
                .ok_or(Error::Unavailable("schedule time overflow"))?;
            let schedule = store
                .create_schedule(&conversation, prompt, interval, first)
                .await?;
            wake(root)?;
            vec![schedule]
        }
        Some(ScheduleCommand::Pause { id, revision }) => {
            vec![store.set_schedule_enabled(&id, revision, false)?]
        }
        Some(ScheduleCommand::Resume { id, revision }) => {
            let schedule = store.set_schedule_enabled(&id, revision, true)?;
            wake(root)?;
            vec![schedule]
        }
    };
    if json {
        crate::print_json(rows)?;
    } else if rows.is_empty() {
        println!("No schedules. Use xcb schedules add <conversation> <prompt> --every <seconds>.");
    } else {
        for row in rows {
            println!(
                "{} · {} · {} · every {}s · due {} · rev {}\n  {}",
                row.id,
                row.conversation,
                if row.enabled { "enabled" } else { "paused" },
                row.interval_ms / 1000,
                row.next_due_ms,
                row.revision,
                xcb_core::display_text(&row.prompt, 4096)
            );
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
