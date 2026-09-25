//! Read-only summaries shared by the managed and direct terminal views.
use xcb_core::ui::{AgentRow, STALE_ATTENTION_MS, TranscriptContext};

pub(crate) const MAX_AGENTS: usize = 128;
pub(crate) const MAX_RESPONSE_BYTES: usize = 2048;

/// The oldest update time, in the stores' millisecond columns, that still
/// counts as recent attention when an overview query ranks rows before its
/// limit. Mirrors [`AgentRow::stale_attention`].
pub(crate) fn recent_attention_since(now: u64) -> i64 {
    i64::try_from(now.saturating_sub(STALE_ATTENTION_MS)).unwrap_or(i64::MAX)
}

/// Combine independently bounded sources without losing the open conversation.
/// A session and a managed conversation remain distinct even if their IDs match.
pub(crate) fn combine(
    mut managed: Vec<AgentRow>,
    direct: Vec<AgentRow>,
    focused: &TranscriptContext,
    now: u64,
) -> Vec<AgentRow> {
    managed.extend(direct);
    sort(&mut managed, now);
    if managed.len() > MAX_AGENTS {
        let focused = managed[MAX_AGENTS..]
            .iter()
            .find(|row| &row.context == focused)
            .cloned();
        managed.truncate(MAX_AGENTS);
        if let Some(focused) = focused {
            managed[MAX_AGENTS - 1] = focused;
            sort(&mut managed, now);
        }
    }
    managed
}

pub(crate) fn sort(rows: &mut [AgentRow], now: u64) {
    rows.sort_by(|a, b| {
        (
            a.overview_priority(now),
            std::cmp::Reverse(a.updated_at_ms),
            context_key(a),
        )
            .cmp(&(
                b.overview_priority(now),
                std::cmp::Reverse(b.updated_at_ms),
                context_key(b),
            ))
    });
}

fn context_key(row: &AgentRow) -> (&str, u8) {
    match &row.context {
        TranscriptContext::Conversation(id) => (id.as_str(), 0),
        TranscriptContext::Session(id) => (id.as_str(), 1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xcb_core::{Id, session::State};

    const NOW: u64 = 30 * STALE_ATTENTION_MS;

    fn row(context: TranscriptContext, state: State, updated: u64) -> AgentRow {
        AgentRow {
            context,
            state,
            updated_at_ms: updated,
            task: None,
            title: "Session".into(),
            workspace: "/workspace".into(),
            model: None,
            activity: state.label().into(),
            response: String::new(),
            category: None,
        }
    }

    fn session(name: String, state: State, updated: u64) -> AgentRow {
        row(
            TranscriptContext::Session(Id::new(name).unwrap()),
            state,
            updated,
        )
    }

    #[test]
    fn union_caps_globally_and_retains_focused_identity() {
        let focused = TranscriptContext::Conversation(Id::new("focused").unwrap());
        let managed = vec![row(focused.clone(), State::Idle, 0)];
        let direct = (0..MAX_AGENTS)
            .map(|index| session(format!("s{index}"), State::Working, index as u64))
            .collect();
        let rows = combine(managed, direct, &focused, NOW);
        assert_eq!(rows.len(), MAX_AGENTS);
        assert_eq!(rows.last().unwrap().context, focused);
        assert_eq!(
            rows.iter()
                .filter(|row| row.state == State::Working)
                .count(),
            MAX_AGENTS - 1
        );
    }

    #[test]
    fn union_preserves_cross_mode_identity_and_prioritizes_attention() {
        let id = Id::new("same-id").unwrap();
        let focused = TranscriptContext::Conversation(id.clone());
        let managed = vec![row(focused.clone(), State::Working, NOW - 10)];
        let direct = vec![row(
            TranscriptContext::Session(id.clone()),
            State::NeedsApproval,
            NOW - 20,
        )];
        let rows = combine(managed, direct, &focused, NOW);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].context, TranscriptContext::Session(id));
        assert_eq!(rows[1].context, focused);
    }

    #[test]
    fn overview_attention_includes_failures_and_limits_without_changing_session_policy() {
        for state in [State::Failed, State::Limited] {
            let recent = session("recent".into(), state, NOW - 1);
            assert_eq!(recent.overview_priority(NOW), 0);
            assert!(recent.needs_attention());
            assert!(!state.attention());
        }
        let working = session("working".into(), State::Working, 0);
        assert_eq!(working.overview_priority(NOW), 1);
        assert_eq!(session("idle".into(), State::Idle, NOW).overview_priority(NOW), 3);
    }

    #[test]
    fn stale_attention_follows_running_work_and_cannot_crowd_it_out_of_the_cap() {
        let focused = TranscriptContext::Conversation(Id::new("focused").unwrap());
        let managed = vec![row(focused.clone(), State::Idle, NOW - 5)];
        let old = NOW - STALE_ATTENTION_MS;
        let mut direct: Vec<_> = (0..MAX_AGENTS)
            .map(|index| session(format!("old{index}"), State::Failed, old - index as u64))
            .collect();
        direct.push(session("today".into(), State::NeedsAnswer, old + 1));
        direct.extend(
            (0..3).map(|index| session(format!("run{index}"), State::Working, index as u64)),
        );
        let rows = combine(managed, direct, &focused, NOW);
        assert_eq!(rows.len(), MAX_AGENTS);
        let names: Vec<_> = rows
            .iter()
            .map(|row| match &row.context {
                TranscriptContext::Conversation(id) | TranscriptContext::Session(id) => {
                    id.to_string()
                }
            })
            .collect();
        assert_eq!(&names[..5], ["today", "run2", "run1", "run0", "old0"]);
        assert_eq!(rows.last().unwrap().context, focused);
        assert!(rows[4].stale_attention(NOW) && !rows[0].stale_attention(NOW));
    }

    #[test]
    fn recent_attention_bound_matches_the_row_rule() {
        let since = recent_attention_since(NOW);
        let at = |updated: i64| session("s".into(), State::Failed, updated as u64);
        assert!(!at(since + 1).stale_attention(NOW));
        assert!(at(since).stale_attention(NOW));
        assert_eq!(recent_attention_since(1), 0);
    }
}
