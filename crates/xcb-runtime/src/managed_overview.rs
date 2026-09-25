use super::*;
use crate::agent_overview::{MAX_AGENTS, MAX_RESPONSE_BYTES};
use xcb_core::ui::{AgentRow, TranscriptContext};

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
    /// One selected task per conversation, chosen by attention, active work,
    /// then recency, across all workspaces. The query ranks identifiers before
    /// loading at most 128 task payloads; no conversation transcript is read.
    pub(super) fn agent_overview(
        &self,
        focused: &Id,
        progress: &BTreeMap<Id, ProgressBeat>,
        now: u64,
    ) -> Result<Vec<AgentRow>> {
        let db = self.db()?;
        let mut query = db.prepare(
            "WITH task_priority AS (
                SELECT id,conversation,updated_at,
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
                FROM task_priority
            )
            SELECT c.id,c.payload,c.updated_at,t.id,t.payload
            FROM conversations c
            LEFT JOIN ranked r ON r.conversation=c.id AND r.position=1
            LEFT JOIN tasks t ON t.id=r.id
            ORDER BY c.id=?1 DESC,
                CASE WHEN t.state='failed' AND CASE WHEN json_valid(t.payload)
                    THEN COALESCE(json_extract(t.payload,'$.deferred'),0)=0 ELSE 1 END
                    THEN 0 ELSE COALESCE(r.priority,3) END,
                max(c.updated_at,COALESCE(r.updated_at,0)) DESC,c.id LIMIT ?2",
        )?;
        let records = query.query_map(
            params![
                focused.as_str(),
                MAX_AGENTS as i64,
                NO_ACCOUNT_DETAIL,
                routing::NO_QUOTA_AVAILABLE_ROUTE
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            },
        )?;
        let mut rows = Vec::new();
        let mut unreadable = Vec::new();
        for record in records {
            let (id, payload, updated, task_id, task_payload) = record?;
            let Ok(mut conversation) = decode::<ManagedConversation>(&payload) else {
                continue;
            };
            let Ok(updated) = u64::try_from(updated) else {
                continue;
            };
            conversation.updated_at_ms = updated;
            if conversation.id.as_str() != id || conversation.validate().is_err() {
                continue;
            }
            let mut row = AgentRow {
                context: TranscriptContext::Conversation(conversation.id.clone()),
                task: None,
                title: conversation.title,
                workspace: conversation.workspace,
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
        crate::agent_overview::sort(&mut rows);
        Ok(rows)
    }
}

#[cfg(test)]
#[path = "managed_overview_tests.rs"]
mod tests;
