//! `xcb workspaces` and `xcb doctor --upgrade-plan`: the project directories
//! the thread picks from, why a task runs where it does, and a preview of
//! the 0.9 upgrade on a private copy of the managed state.

use clap::Subcommand;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::time::Duration;
use xcb_core::Id;
use xcb_runtime::{
    Error, Result,
    managed::{ManagedStore, MigrationConflict, UpgradeReport, WorkspaceStatus},
    now_ms,
};

use crate::{cell, human_age, print_json};

#[derive(Subcommand)]
pub enum WorkspaceCommand {
    /// List known project directories with their status and open upgrade conflicts.
    List,
    /// Register a project directory so the thread can pick it.
    Add {
        /// Directory to register; relative paths start at --cwd.
        dir: PathBuf,
        /// Name to match in prompts and the JSON protocol; defaults to the directory name.
        #[arg(long)]
        name: Option<String>,
    },
    /// Stop offering a project directory; its tasks and history stay.
    Hide {
        /// Project name, directory, or a project view's conversation id.
        scope: String,
    },
    /// Offer a hidden project directory again.
    Show {
        /// Project name, directory, or a project view's conversation id.
        scope: String,
    },
    /// Explain why a task runs in its directory.
    Why {
        /// Task id from `xcb tasks` or `xcb backlog`.
        task: Id,
    },
    /// Probe every registered directory for work a dead or interrupted
    /// session left behind: uncommitted changes, unpushed commits, or a
    /// suspended merge or rebase, on a workspace no live task claims.
    Audit {
        /// Hours since a task last used the directory before its changes
        /// count as stranded. [default: 24]
        #[arg(long, default_value_t = 24)]
        stale_hours: u64,
        /// Print only stranded workspaces, one path per line, for scripts.
        #[arg(long)]
        stranded_only: bool,
    },
    /// List the project grants and memory bindings the upgrade moved, paused or dropped.
    Conflicts,
}

pub fn dispatch(
    root: &Path,
    cwd: &Path,
    command: Option<WorkspaceCommand>,
    json: bool,
) -> Result<i32> {
    let store = ManagedStore::open(root)?;
    match command.unwrap_or(WorkspaceCommand::List) {
        WorkspaceCommand::List => list(&store, json),
        WorkspaceCommand::Add { dir, name } => {
            let path = store.admit_workspace(
                &xcb_core::canonical(cwd)?.join(dir),
                "command",
                name.as_deref(),
            )?;
            let row = entry(&store, &path)?;
            if json {
                print_json(row)?;
            } else {
                println!("Added {} · {}", row.name, row.path);
            }
            Ok(0)
        }
        WorkspaceCommand::Hide { scope } => {
            let path = match registered(&store, &scope, cwd)? {
                Some(path) => path,
                None => store.resolve_scope(&scope, &xcb_core::canonical(cwd)?)?,
            };
            store.hide_workspace(&path)?;
            report_visibility(&store, &path, json, "Hid")
        }
        WorkspaceCommand::Show { scope } => {
            let path = hidden_named(&store, &scope)?.map_or_else(
                || store.resolve_scope(&scope, &xcb_core::canonical(cwd)?),
                Ok,
            )?;
            store.show_workspace(&path)?;
            report_visibility(&store, &path, json, "Showing")
        }
        WorkspaceCommand::Why { task } => why(&store, &task, json),
        WorkspaceCommand::Audit {
            stale_hours,
            stranded_only,
        } => audit(
            &store,
            Duration::from_secs(stale_hours.saturating_mul(3600)),
            stranded_only,
            json,
        ),
        WorkspaceCommand::Conflicts => {
            let conflicts = store.migration_conflicts(false)?;
            if json {
                print_json(&conflicts)?;
            } else if conflicts.is_empty() {
                println!("No upgrade conflicts.");
            } else {
                for conflict in &conflicts {
                    println!("{}", conflict_line(conflict));
                }
            }
            Ok(0)
        }
    }
}

/// A registry entry named by its stored path (as given, or joined to
/// `cwd` without resolving it) or by a unique name, whether or not it still
/// validates. Hiding must work on exactly the entries that no longer do: a
/// deleted worktree cannot be canonicalized, and a refused directory such as
/// `$HOME` never resolves as a scope.
fn registered(store: &ManagedStore, value: &str, cwd: &Path) -> Result<Option<String>> {
    let entries = store.all_workspaces()?;
    let joined = lexical(&cwd.join(value));
    let base = xcb_core::canonical(cwd)
        .ok()
        .map(|cwd| lexical(&cwd.join(value)));
    if let Some(entry) = entries.iter().find(|entry| {
        let path = Path::new(&entry.path);
        entry.path == value || path == joined || base.as_deref() == Some(path)
    }) {
        return Ok(Some(entry.path.clone()));
    }
    let named: Vec<&str> = entries
        .iter()
        .filter(|entry| entry.status != "hidden" && entry.name == value)
        .map(|entry| entry.path.as_str())
        .collect();
    Ok(match named.as_slice() {
        [only] => Some((*only).to_owned()),
        _ => None,
    })
}

/// `.` and `..` removed without touching the filesystem.
fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => (),
            std::path::Component::ParentDir => {
                out.pop();
            }
            component => out.push(component),
        }
    }
    out
}

/// A hidden entry is missing from name lookups, so `show <name>` matches
/// hidden entries by exact name first.
fn hidden_named(store: &ManagedStore, name: &str) -> Result<Option<String>> {
    let hits: Vec<String> = store
        .all_workspaces()?
        .into_iter()
        .filter(|entry| entry.status == "hidden" && entry.name == name)
        .map(|entry| entry.path)
        .collect();
    Ok(match hits.as_slice() {
        [only] => Some(only.clone()),
        _ => None,
    })
}

fn entry(store: &ManagedStore, path: &str) -> Result<WorkspaceStatus> {
    store
        .all_workspaces()?
        .into_iter()
        .find(|entry| entry.path == path)
        .ok_or(Error::Unavailable("workspace is not in the registry"))
}

fn report_visibility(store: &ManagedStore, path: &str, json: bool, verb: &str) -> Result<i32> {
    let row = entry(store, path)?;
    if json {
        print_json(row)?;
    } else {
        println!("{verb} {} · {}", row.name, row.path);
    }
    Ok(0)
}

fn list(store: &ManagedStore, json: bool) -> Result<i32> {
    let rows = store.all_workspaces()?;
    let open = store.migration_conflicts(true)?;
    let open_for = |path: &str| {
        open.iter()
            .filter(|conflict| conflict.workspace.as_deref() == Some(path))
            .count()
    };
    if json {
        let rows: Vec<serde_json::Value> = rows
            .iter()
            .map(|row| {
                let mut value = serde_json::to_value(row)?;
                if let Some(object) = value.as_object_mut() {
                    object.insert("openConflicts".into(), json!(open_for(&row.path)));
                }
                Ok(value)
            })
            .collect::<Result<_>>()?;
        print_json(rows)?;
        return Ok(0);
    }
    if rows.is_empty() {
        println!("No project directories yet. Next: xcb workspaces add <dir>");
        return Ok(0);
    }
    let now = now_ms();
    println!(
        "  {} {} {} {} {} {:>5}  STATUS",
        cell("NAME", 20),
        cell("PATH", 44),
        cell("REPO", 20),
        cell("ADDED BY", 9),
        cell("LAST USED", 10),
        "TASKS"
    );
    for row in &rows {
        let conflicts = open_for(&row.path);
        let flag = if conflicts == 0 {
            String::new()
        } else {
            format!(
                " · {conflicts} upgrade conflict{}",
                if conflicts == 1 { "" } else { "s" }
            )
        };
        println!(
            "  {} {} {} {} {} {:>5}  {}{flag}",
            cell(&row.name, 20),
            cell(&row.path, 44),
            cell(row.repo.as_deref().unwrap_or("-"), 20),
            cell(&row.admitted_by, 9),
            cell(&human_age(now, row.last_used_ms), 10),
            row.task_count,
            row.status,
        );
    }
    if !open.is_empty() {
        println!(
            "{} open upgrade conflict{}. Next: xcb workspaces conflicts",
            open.len(),
            if open.len() == 1 { "" } else { "s" }
        );
    }
    Ok(0)
}

fn audit(store: &ManagedStore, stale: Duration, stranded_only: bool, json: bool) -> Result<i32> {
    let rows = store.audit_workspaces(stale)?;
    let rows: Vec<_> = if stranded_only {
        rows.into_iter().filter(|row| row.stranded).collect()
    } else {
        rows
    };
    if json {
        print_json(&rows)?;
        return Ok(0);
    }
    if stranded_only {
        for row in &rows {
            println!("{}", row.path);
        }
        return Ok(0);
    }
    let now = now_ms();
    println!(
        "  {} {} {} {} {} STATE",
        cell("NAME", 20),
        cell("PATH", 44),
        cell("BRANCH", 24),
        cell("CHANGES", 14),
        cell("LAST USED", 10),
    );
    for row in &rows {
        let (branch, changes, state) = match &row.git {
            None => (
                "-".to_owned(),
                "-".to_owned(),
                "not a repository".to_owned(),
            ),
            Some(git) => {
                let changes = format!(
                    "+{} Δ{} ?{}{}",
                    git.ahead,
                    git.tracked,
                    git.untracked,
                    git.operation.map(|op| format!(" {op}")).unwrap_or_default(),
                );
                let state = if row.stranded {
                    "STRANDED".to_owned()
                } else if row.claimed {
                    "live task".to_owned()
                } else if git.partial {
                    "probe partial".to_owned()
                } else {
                    "clean".to_owned()
                };
                (
                    git.branch.clone().unwrap_or_else(|| "detached".into()),
                    changes,
                    state,
                )
            }
        };
        println!(
            "  {} {} {} {} {} {}",
            cell(&row.name, 20),
            cell(&row.path, 44),
            cell(&branch, 24),
            cell(&changes, 14),
            cell(&human_age(now, row.last_used_ms), 10),
            state,
        );
    }
    let stranded = rows.iter().filter(|row| row.stranded).count();
    if stranded > 0 {
        println!(
            "{stranded} workspace{} hold{} work no live task claims. Review or preserve each before deleting its directory.",
            if stranded == 1 { "" } else { "s" },
            if stranded == 1 { "s" } else { "" },
        );
    }
    Ok(0)
}

fn why(store: &ManagedStore, task: &Id, json: bool) -> Result<i32> {
    let id = store.resolve_task(task)?;
    let task = store
        .task(&id)?
        .ok_or(Error::Unavailable("managed task not found"))?;
    if json {
        print_json(json!({
            "task": task.id.as_str(),
            "conversation": task.conversation.as_str(),
            "workspace": task.workspace,
            "binding": task.binding,
            "movedFrom": task.moved_from.as_ref().map(Id::as_str),
        }))?;
        return Ok(0);
    }
    let workspace = xcb_core::display_text(&task.workspace, 4096);
    let Some(binding) = &task.binding else {
        println!(
            "{} runs in {workspace} · bound by its project view",
            task.id
        );
        return Ok(0);
    };
    println!("{} runs in {workspace}", task.id);
    println!(
        "  source       {} ({} confidence)",
        binding.source.as_str(),
        binding.confidence.as_str()
    );
    println!("  created by   {}", binding.origin.as_str());
    println!(
        "  reason       {}",
        xcb_core::display_text(&binding.reason, 160)
    );
    if !binding.alternatives.is_empty() {
        println!(
            "  also         {}",
            xcb_core::display_text(&binding.alternatives.join(", "), 4096 * 4)
        );
    }
    if let Some(moved) = &task.moved_from {
        println!("  moved from   {moved}");
    }
    Ok(0)
}

fn conflict_line(conflict: &MigrationConflict) -> String {
    let stranded = if conflict.stranded_tasks == 0 {
        String::new()
    } else {
        format!(" · {} stranded tasks", conflict.stranded_tasks)
    };
    format!(
        "{} {:<8} {:<13} {} · from {}{stranded} · {}",
        conflict.id,
        conflict.kind,
        conflict.disposition.replace('_', " "),
        xcb_core::display_text(conflict.workspace.as_deref().unwrap_or("-"), 4096),
        conflict.conversation,
        if conflict.resolved_at_ms.is_some() {
            "resolved"
        } else {
            "open"
        },
    )
}

/// `xcb doctor --upgrade-plan`: migrate a private copy of the managed store
/// and report what the upgrade would do. The source is only read, so this
/// works while an old supervisor holds its lock; the copy is always removed.
pub fn upgrade_plan(root: &Path, json: bool) -> Result<i32> {
    if !root.join("managed").join("managed.sqlite").is_file() {
        if json {
            print_json(json!({"managedState": false}))?;
        } else {
            println!("No managed state yet; there is nothing to upgrade.");
        }
        return Ok(0);
    }
    let report = preview(root)?;
    if json {
        print_json(&report)?;
    } else {
        for line in plan_lines(&report) {
            println!("{line}");
        }
    }
    Ok(0)
}

/// Run `migrate_copy` in a fresh private directory under the state root and
/// delete the copy whatever the outcome.
fn preview(root: &Path) -> Result<UpgradeReport> {
    let scratch = root.join(format!(
        "upgrade-plan-{}",
        xcb_runtime::new_id("copy").as_str()
    ));
    xcb_runtime::private::directory(&scratch)?;
    let report = ManagedStore::migrate_copy(root, &scratch);
    let removed = std::fs::remove_dir_all(&scratch);
    let report = report?;
    removed?;
    Ok(report)
}

fn plan_lines(report: &UpgradeReport) -> Vec<String> {
    let tally = |kind: &str, disposition: &str| {
        report
            .conflicts
            .iter()
            .filter(|conflict| conflict.kind == kind && conflict.disposition == disposition)
            .count()
    };
    let stranded: u64 = report
        .conflicts
        .iter()
        .filter(|conflict| conflict.kind == "grant")
        .map(|conflict| conflict.stranded_tasks)
        .sum();
    let dropped = report
        .conflicts
        .iter()
        .filter(|conflict| conflict.disposition == "dropped")
        .count();
    let mut lines = vec![if report.from_version >= report.to_version {
        format!(
            "Managed state is already on schema {}; the upgrade has nothing to change.",
            report.to_version
        )
    } else {
        format!(
            "Upgrade preview: schema {} → {}, run on a private copy; nothing was changed.",
            report.from_version, report.to_version
        )
    }];
    lines.push(format!(
        "  conversations  {} (kept as project views)",
        report.conversations
    ));
    lines.push(format!(
        "  tasks          {} (each keeps its directory)",
        report.tasks
    ));
    lines.push(format!("  directories    {} known", report.workspaces));
    lines.push(format!(
        "  grants         {} after upgrade · {} moved · {} paused · {} superseded ({stranded} stranded tasks)",
        report.grants,
        tally("grant", "moved"),
        tally("grant", "winner_paused"),
        tally("grant", "superseded"),
    ));
    lines.push(format!(
        "  memory         {} bound after upgrade · {} moved · {} merged · {} unbound",
        report.memory_bindings,
        tally("memory", "moved"),
        tally("memory", "merged"),
        tally("memory", "unbound"),
    ));
    if dropped > 0 {
        lines.push(format!(
            "  dropped        {dropped} unreadable or orphaned rows (kept for audit)"
        ));
    }
    if tally("grant", "winner_paused") + tally("grant", "superseded") + tally("memory", "unbound")
        > 0
    {
        lines.push(
            "Next: after upgrading, xcb workspaces conflicts, then resume paused grants with xcb projects"
                .into(),
        );
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conflict(kind: &str, disposition: &str, stranded: u64) -> MigrationConflict {
        MigrationConflict {
            id: format!("x_{kind}_{disposition}"),
            kind: kind.into(),
            workspace: Some("/w".into()),
            conversation: "c_a".into(),
            disposition: disposition.into(),
            stranded_tasks: stranded,
            payload: "{}".into(),
            created_at_ms: 1,
            resolved_at_ms: None,
        }
    }

    #[test]
    fn upgrade_plan_counts_each_disposition() {
        let report = UpgradeReport {
            from_version: 6,
            to_version: 7,
            conversations: 3,
            tasks: 9,
            workspaces: 2,
            grants: 1,
            memory_bindings: 0,
            conflicts: vec![
                conflict("grant", "winner_paused", 0),
                conflict("grant", "superseded", 2),
                conflict("memory", "unbound", 0),
                conflict("orphan", "dropped", 0),
            ],
        };
        let lines = plan_lines(&report).join("\n");
        assert!(lines.contains("schema 6 → 7"), "{lines}");
        assert!(
            lines.contains("1 paused · 1 superseded (2 stranded tasks)"),
            "{lines}"
        );
        assert!(lines.contains("0 merged · 1 unbound"), "{lines}");
        assert!(lines.contains("dropped        1"), "{lines}");
        assert!(lines.contains("Next: after upgrading"), "{lines}");
    }
}
