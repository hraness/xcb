use clap::Subcommand;
use std::path::Path;
use xcb_core::{Id, session::State};
use xcb_runtime::{
    Error, Result,
    managed::{self, ManagedStore, ManagedTask},
    new_id, now_ms,
};

#[derive(Subcommand)]
pub enum BacklogCommand {
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
pub enum ScheduleCommand {
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
