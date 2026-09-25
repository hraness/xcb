use super::*;
use crate::agent_overview::{MAX_AGENTS, MAX_RESPONSE_BYTES};
use xcb_core::{
    session::Role,
    ui::{AgentRow, TranscriptContext},
};

impl Store {
    /// Read one assistant message per selected direct session across workspaces.
    /// Managed worker sessions belong to their durable conversation instead.
    /// Selection ranks like the overview: stale attention never displaces
    /// running work from the bounded result.
    pub(crate) fn agent_overview(&self, focused: Option<&Id>, now: u64) -> Result<Vec<AgentRow>> {
        // A saved working flag is not evidence that its terminal is still alive.
        // This read never changes custody or recovers an account.
        let live: std::collections::BTreeSet<_> = self
            .unsettled_runs()?
            .into_iter()
            .filter(|run| run.owner.as_ref().is_some_and(RunOwner::alive))
            .filter_map(|run| run.session)
            .collect();
        let db = self.db()?;
        let outcomes_available: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='run_outcomes')",
            [],
            |row| row.get(0),
        )?;
        let outcomes = if outcomes_available {
            "run_outcomes"
        } else {
            "(SELECT NULL AS session,NULL AS run,NULL AS input_sequence,NULL AS payload WHERE 0)"
        };
        let mut query = db.prepare(&format!(
            "WITH selected AS (
                SELECT id,last_active FROM sessions
                WHERE CASE WHEN json_valid(payload)
                    THEN json_extract(payload,'$.managed_task') IS NULL ELSE 0 END
                ORDER BY id=?1 DESC,
                    CASE
                        WHEN json_extract(payload,'$.state') IN ('needs_answer','needs_action',
                            'needs_approval','uncertain','failed','limited')
                        THEN CASE WHEN last_active>?3 THEN 0 ELSE 2 END
                        WHEN json_extract(payload,'$.state')='working' THEN 1 ELSE 3 END,
                    last_active DESC,id LIMIT ?2
            )
            SELECT s.id,s.payload,m.id,m.payload,o.payload,r.payload,f.payload,fr.payload
            FROM selected selected_session JOIN sessions s ON s.id=selected_session.id
            LEFT JOIN messages m ON m.session=s.id AND m.sequence=(
                SELECT sequence FROM messages WHERE session=s.id
                AND CASE WHEN json_valid(payload) THEN json_extract(payload,'$.role')='assistant' ELSE 0 END
                ORDER BY sequence DESC LIMIT 1
            )
            LEFT JOIN {outcomes} o ON o.session=s.id AND o.run=(
                CASE WHEN json_valid(m.payload) THEN json_extract(m.payload,'$.provenance.run') END)
            LEFT JOIN runs r ON r.id=o.run AND r.phase='settled'
            LEFT JOIN {outcomes} f ON f.session=s.id AND f.input_sequence=(
                SELECT MAX(input_sequence) FROM {outcomes} WHERE session=s.id)
            LEFT JOIN runs fr ON fr.id=f.run AND fr.phase='settled'",
        ))?;
        let recent = crate::agent_overview::recent_attention_since(now);
        let records = query.query_map(
            params![focused.map(Id::as_str), MAX_AGENTS as i64, recent],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                ))
            },
        )?;
        let mut rows = Vec::new();
        for record in records {
            let (
                id,
                payload,
                message_id,
                message_payload,
                outcome_payload,
                run_payload,
                latest_outcome,
                latest_run,
            ) = record?;
            let Ok(session) = decode::<Session>(&payload) else {
                continue;
            };
            if session.id.as_str() != id
                || session.managed_task.is_some()
                || session.validate().is_err()
            {
                continue;
            }
            let message = message_payload.as_deref().and_then(|payload| {
                let message = decode::<Message>(payload).ok()?;
                (message.validate().is_ok()
                    && message.role == Role::Assistant
                    && message_id.as_deref() == Some(message.id.as_str()))
                .then_some(message)
            });
            let category = message.as_ref().and_then(|message| {
                let record = decode::<SettledOutcome>(outcome_payload.as_deref()?).ok()?;
                let run = decode::<RunRecord>(run_payload.as_deref()?).ok()?;
                let provenance = message.provenance.as_ref()?;
                (record.version == 1
                    && run.validate().is_ok()
                    && validate_outcome(&record.outcome).is_ok()
                    && record.run == run.id
                    && record.session == session.id
                    && run.session.as_ref() == Some(&session.id)
                    && run.phase == "settled"
                    && provenance.run.as_ref() == Some(&record.run)
                    && provenance.account == run.account
                    && run.model.as_ref() == Some(&provenance.model)
                    && record.outcome.text == message.text)
                    .then(|| record.outcome.state.label().into())
            });
            let state = if session.state == State::Working && !live.contains(&session.id) {
                State::Uncertain
            } else {
                session.state
            };
            let mut response = message
                .as_ref()
                .map(|message| xcb_core::display_text(&message.text, MAX_RESPONSE_BYTES))
                .unwrap_or_default();
            let mut category = category;
            // A session that ended without a response shows why: the latest
            // settled outcome's bounded diagnostic, identity-checked like a
            // response category. Never a live or unsettled run.
            if response.is_empty()
                && matches!(state, State::Failed | State::Limited | State::Uncertain)
                && let Some((text, label)) =
                    failure_preview(&session, latest_outcome.as_deref(), latest_run.as_deref())
            {
                response = text;
                category = Some(label);
            }
            rows.push(AgentRow {
                context: TranscriptContext::Session(session.id),
                task: None,
                title: session.title,
                workspace: session.workspace,
                model: Some(session.model.key()),
                state,
                activity: state.label().into(),
                response,
                category,
                updated_at_ms: session.last_active_at_ms,
            });
        }
        crate::agent_overview::sort(&mut rows, now);
        Ok(rows)
    }
}

/// The latest settled outcome's diagnostic for a session that ended without a
/// response, with the same identity checks as a response category.
fn failure_preview(
    session: &Session,
    outcome_payload: Option<&str>,
    run_payload: Option<&str>,
) -> Option<(String, String)> {
    let record = decode::<SettledOutcome>(outcome_payload?).ok()?;
    let run = decode::<RunRecord>(run_payload?).ok()?;
    (record.version == 1
        && run.validate().is_ok()
        && validate_outcome(&record.outcome).is_ok()
        && record.run == run.id
        && record.session == session.id
        && run.session.as_ref() == Some(&session.id)
        && run.phase == "settled")
        .then_some(())?;
    let diagnostic = record.outcome.diagnostic.as_ref()?.as_str();
    Some((
        xcb_core::display_text(diagnostic, MAX_RESPONSE_BYTES),
        record.outcome.state.label().into(),
    ))
}

#[cfg(test)]
#[path = "store_overview_tests.rs"]
mod tests;
