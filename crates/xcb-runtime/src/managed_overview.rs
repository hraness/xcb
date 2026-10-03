use super::*;
use crate::agent_overview::{MAX_AGENTS, MAX_RESPONSE_BYTES};
use crate::workspace_infer::KnownWorkspace;
use xcb_core::ui::{AgentRow, TranscriptContext, WorkspaceRow};

const PROGRESS_FRESH_MS: u64 = 30_000;

pub(super) fn progress_label(event: Progress) -> String {
    match event {
        Progress::Text { thinking: true, .. } => "thinking".into(),
        Progress::Text {
            thinking: false, ..
        } => "writing response".into(),
        Progress::Tool(name) => format!("running tool {name}"),
        Progress::Notice(text) => text,
        Progress::Subagent(subagent) => format!("running subagent {}", subagent.label),
    }
}

fn activity(task: &ManagedTask, beat: Option<&ProgressBeat>, now: u64) -> String {
    if task.state == TaskState::Running
        && let Some(beat) = beat
        && beat.at_ms > task.updated_at_ms
        && beat.at_ms <= now
        && now - beat.at_ms <= PROGRESS_FRESH_MS
    {
        return xcb_core::display_text(&beat.text, MAX_PROGRESS_TEXT);
    }
    task.habitat_status().into()
}

impl ManagedStore {
    /// One selected task per project view, chosen by attention, active work,
    /// then recency, plus one per project directory for the thread's tasks.
    /// The query ranks identifiers before loading at most 128 task payloads;
    /// no conversation transcript is read.
    pub(super) fn agent_overview(
        &self,
        focused: &Id,
        progress: &BTreeMap<Id, ProgressBeat>,
        now: u64,
    ) -> Result<Vec<AgentRow>> {
        let db = self.db()?;
        // A UNION: views keep one card per conversation; the thread, which
        // spans projects, gets one card per task workspace instead.
        let mut query = db.prepare(
            "WITH task_priority AS (
                SELECT id,conversation,workspace,updated_at,
                    CASE
                    WHEN state IN ('completed','failed','cancelled') THEN 3
                    WHEN state IN ('needs_input','uncertain') THEN 0
                    WHEN state='running' THEN 1
                    WHEN json_valid(payload)=0 THEN 3
                    WHEN COALESCE(json_extract(payload,'$.deferred'),0)=1 THEN 3
                    WHEN state='queued' AND (
                        json_extract(payload,'$.detail') LIKE ?3 || '%'
                        OR json_extract(payload,'$.detail')=?4
                        OR json_extract(payload,'$.detail') LIKE 'project authority%'
                        OR json_extract(payload,'$.detail') LIKE 'program waiting for linked child evidence:%'
                    ) THEN 0
                    WHEN state='queued' THEN 2 ELSE 3 END AS priority
                FROM tasks
            ), ranked AS (
                SELECT id,conversation,updated_at,priority,
                    row_number() OVER (PARTITION BY conversation
                        ORDER BY priority,updated_at DESC,id) AS position
                FROM task_priority WHERE conversation<>?6
            ), thread_ranked AS (
                SELECT id,workspace,updated_at,priority,
                    row_number() OVER (PARTITION BY workspace
                        ORDER BY priority,updated_at DESC,id) AS position
                FROM task_priority WHERE conversation=?6 AND workspace IS NOT NULL
            ), cards AS (
                SELECT c.id AS conversation,c.payload AS payload,c.updated_at AS updated,
                    r.id AS task,r.priority AS priority,r.updated_at AS task_updated,
                    NULL AS workspace,NULL AS name
                FROM conversations c
                LEFT JOIN ranked r ON r.conversation=c.id AND r.position=1
                WHERE c.id<>?6
                UNION ALL
                SELECT c.id,c.payload,r.updated_at,r.id,r.priority,r.updated_at,r.workspace,w.name
                FROM thread_ranked r
                JOIN conversations c ON c.id=?6
                LEFT JOIN workspaces w ON w.path=r.workspace
                WHERE r.position=1
            )
            SELECT k.conversation,k.payload,k.updated,t.id,t.payload,k.workspace,k.name
            FROM cards k
            LEFT JOIN tasks t ON t.id=k.task
            ORDER BY k.conversation=?1 DESC,
                CASE
                    WHEN (t.state='failed' AND CASE WHEN json_valid(t.payload)
                            THEN COALESCE(json_extract(t.payload,'$.deferred'),0)=0 ELSE 1 END)
                        OR k.priority=0
                    THEN CASE WHEN max(k.updated,COALESCE(k.task_updated,0))>?5 THEN 0 ELSE 3 END
                    WHEN k.priority IN (1,2) THEN k.priority
                    ELSE 4 END,
                max(k.updated,COALESCE(k.task_updated,0)) DESC,k.conversation,k.workspace LIMIT ?2",
        )?;
        let records = query.query_map(
            params![
                focused.as_str(),
                MAX_AGENTS as i64,
                NO_ACCOUNT_DETAIL,
                routing::NO_QUOTA_AVAILABLE_ROUTE,
                crate::agent_overview::recent_attention_since(now),
                GLOBAL_THREAD_ID,
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                ))
            },
        )?;
        let mut rows = Vec::new();
        let mut unreadable = Vec::new();
        for record in records {
            let (id, payload, updated, task_id, task_payload, task_workspace, name) = record?;
            let Ok(mut conversation) = decode::<ManagedConversation>(&payload) else {
                continue;
            };
            let Ok(updated) = u64::try_from(updated) else {
                continue;
            };
            // A thread card's time is its project's latest task, which can
            // predate the thread row itself.
            conversation.updated_at_ms = if conversation.is_thread() {
                updated.max(conversation.created_at_ms)
            } else {
                updated
            };
            if conversation.id.as_str() != id || conversation.validate().is_err() {
                continue;
            }
            let thread = conversation.is_thread();
            // A thread card is one project directory: it is named after the
            // directory and carries the task's workspace, never the thread's.
            let (title, workspace) = match (thread, task_workspace) {
                (true, Some(workspace)) => (
                    name.unwrap_or_else(|| workspace_label(&workspace)),
                    workspace,
                ),
                (true, None) => continue,
                (false, _) => (
                    conversation.title,
                    conversation.workspace.unwrap_or_default(),
                ),
            };
            let mut row = AgentRow {
                context: TranscriptContext::Conversation(conversation.id.clone()),
                task: None,
                title,
                workspace,
                model: None,
                state: State::Idle,
                activity: "idle".into(),
                response: String::new(),
                category: None,
                updated_at_ms: updated,
            };
            if let (Some(task_id), Some(payload)) = (task_id, task_payload) {
                let task = decode::<ManagedTask>(&payload).and_then(|task| {
                    task.validate()?;
                    // The generated column mirrors the payload, so a thread
                    // task's workspace is checked against its own card.
                    if task.id.as_str() != task_id
                        || task.conversation != conversation.id
                        || task.workspace != row.workspace
                    {
                        return Err(Error::Conflict("overview task identity mismatch"));
                    }
                    Ok(task)
                });
                match task {
                    Ok(task) => {
                        row.state = task.habitat_ui_state();
                        row.activity = activity(&task, progress.get(&task.id), now);
                        row.updated_at_ms = updated.max(task.updated_at_ms);
                        row.response = task
                            .last_output
                            .as_deref()
                            .map(|text| xcb_core::display_text(text, MAX_RESPONSE_BYTES))
                            .unwrap_or_default();
                        row.category = if row.response.is_empty() {
                            None
                        } else {
                            task.settle.or_else(|| {
                                (row.state != State::Working).then(|| row.state.label().into())
                            })
                        };
                        // A failed, limited, or unproven task with no output
                        // shows its recorded detail so the card says why.
                        if row.response.is_empty()
                            && matches!(
                                row.state,
                                State::Failed | State::Limited | State::Uncertain
                            )
                            && !task.detail.trim().is_empty()
                        {
                            row.response = xcb_core::display_text(&task.detail, MAX_RESPONSE_BYTES);
                            row.category = Some(row.state.label().into());
                        }
                        // A provider preference on queued work is not an
                        // observed model. Bind metadata to this selected task.
                        row.model = task.session.and(task.route);
                        row.task = Some(task.id);
                    }
                    Err(_) => {
                        row.state = State::Uncertain;
                        row.activity = "task record unavailable".into();
                        unreadable.push(task_id);
                    }
                }
            }
            rows.push(row);
        }
        drop(query);
        drop(db);
        if !unreadable.is_empty()
            && let Ok(mut known) = self.unreadable.lock()
        {
            for id in unreadable {
                if known.len() < 256 {
                    known.insert(id);
                }
            }
        }
        crate::agent_overview::sort(&mut rows, now);
        Ok(rows)
    }

    /// Known project directories for the thread's header and picker, each
    /// with its count of nonterminal tasks.
    pub(super) fn workspace_rows(&self, limit: usize) -> Result<Vec<WorkspaceRow>> {
        let known = self.known_workspaces(limit)?;
        let active = self.active_by_workspace()?;
        Ok(known
            .into_iter()
            .map(|entry| WorkspaceRow {
                active: active.get(&entry.path).copied().unwrap_or(0),
                container: entry.container,
                new: false,
                path: entry.path,
                name: entry.name,
                repo: entry.repo,
                last_used_ms: entry.last_used_ms,
            })
            .collect())
    }

    fn active_by_workspace(&self) -> Result<BTreeMap<String, usize>> {
        let db = self.db()?;
        let mut query = db.prepare(
            "SELECT workspace,count(*) FROM tasks
             WHERE workspace IS NOT NULL AND state IN ('queued','running','needs_input')
             GROUP BY workspace",
        )?;
        let rows = query.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        let mut counts = BTreeMap::new();
        for row in rows {
            let (workspace, count) = row?;
            counts.insert(workspace, usize::try_from(count).unwrap_or(0));
        }
        Ok(counts)
    }

    /// Resolve a TUI project argument (a registry path, a name or repository
    /// tail, or a path to snap) to a directory usable as the thread's focus
    /// or a move target. A container is refused unless a human admitted it.
    pub(super) fn ui_workspace(&self, value: &str) -> std::result::Result<String, String> {
        let value = value.trim();
        if value.is_empty() {
            return Err("name a project directory".into());
        }
        let known = self
            .known_workspaces(MAX_UI_WORKSPACES)
            .map_err(|error| error.to_string())?;
        let path = if let Some(entry) = known.iter().find(|entry| entry.path == value) {
            entry.path.clone()
        } else if value.contains('/') || value == "." || value == ".." || value.starts_with('~') {
            self.snap_root(&expand_home(value))
                .map_err(|error| format!("`{value}`: {error}"))?
        } else {
            let hits: Vec<&KnownWorkspace> = known
                .iter()
                .filter(|entry| {
                    entry.name == value
                        || entry
                            .repo
                            .as_deref()
                            .and_then(|repo| repo.rsplit('/').next())
                            == Some(value)
                })
                .collect();
            match hits.as_slice() {
                [entry] => entry.path.clone(),
                [] => {
                    return Err(format!(
                        "no known project is named `{value}`; /workspace add <dir> admits one"
                    ));
                }
                _ => {
                    return Err(format!(
                        "`{value}` names several projects: {}",
                        hits.iter()
                            .map(|entry| entry.path.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
            }
        };
        match known.iter().find(|entry| entry.path == path) {
            Some(entry) if entry.container && !entry.explicit_add => Err(format!(
                "`{path}` holds other projects; /workspace add `{path}` to use it as one"
            )),
            Some(_) => Ok(path),
            None => Err(format!(
                "`{path}` is not a known project; /workspace add `{path}` to use it"
            )),
        }
    }
}

const MAX_UI_WORKSPACES: usize = 256;

/// A typed directory argument with a leading `~` expanded and a relative
/// path joined to this process's working directory.
pub(super) fn expand_home(value: &str) -> PathBuf {
    let path = match (value.strip_prefix('~'), xcb_core::home_dir()) {
        (Some(rest), Some(home)) if rest.is_empty() || rest.starts_with('/') => {
            home.join(rest.trim_start_matches('/'))
        }
        _ => PathBuf::from(value),
    };
    match std::env::current_dir() {
        Ok(cwd) if path.is_relative() => cwd.join(path),
        _ => path,
    }
}

/// A directory's display name when the registry has none: its basename.
pub(super) fn workspace_label(path: &str) -> String {
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(path)
        .to_owned()
}

#[cfg(test)]
#[path = "managed_overview_tests.rs"]
mod tests;
