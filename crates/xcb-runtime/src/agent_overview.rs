//! Read-only summaries shared by the managed and direct terminal views.
use xcb_core::{
    session::State,
    ui::{AgentRow, TranscriptContext},
};

pub(crate) const MAX_AGENTS: usize = 128;
pub(crate) const MAX_RESPONSE_BYTES: usize = 2048;

pub(crate) fn priority(state: State) -> u8 {
    if state.attention() || matches!(state, State::Failed | State::Limited) {
        0
    } else if state == State::Working {
        1
    } else {
        2
    }
}

/// Combine independently bounded sources without losing the open conversation.
/// A session and a managed conversation remain distinct even if their IDs match.
pub(crate) fn combine(
    mut managed: Vec<AgentRow>,
    direct: Vec<AgentRow>,
    focused: &TranscriptContext,
) -> Vec<AgentRow> {
    managed.extend(direct);
    sort(&mut managed);
    if managed.len() > MAX_AGENTS {
        let focused = managed[MAX_AGENTS..]
            .iter()
            .find(|row| &row.context == focused)
            .cloned();
        managed.truncate(MAX_AGENTS);
        if let Some(focused) = focused {
            managed[MAX_AGENTS - 1] = focused;
            sort(&mut managed);
        }
    }
    managed
}

pub(crate) fn sort(rows: &mut [AgentRow]) {
    rows.sort_by(|a, b| {
        (
            priority(a.state),
            std::cmp::Reverse(a.updated_at_ms),
            context_key(a),
        )
            .cmp(&(
                priority(b.state),
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
    use xcb_core::Id;

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

    #[test]
    fn union_caps_globally_and_retains_focused_identity() {
        let focused = TranscriptContext::Conversation(Id::new("focused").unwrap());
        let managed = vec![row(focused.clone(), State::Idle, 0)];
        let direct = (0..MAX_AGENTS)
            .map(|index| {
                row(
                    TranscriptContext::Session(Id::new(format!("s{index}")).unwrap()),
                    State::Working,
                    index as u64,
                )
            })
            .collect();
        let rows = combine(managed, direct, &focused);
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
        let managed = vec![row(focused.clone(), State::Working, 20)];
        let direct = vec![row(
            TranscriptContext::Session(id.clone()),
            State::NeedsApproval,
            10,
        )];
        let rows = combine(managed, direct, &focused);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].context, TranscriptContext::Session(id));
        assert_eq!(rows[1].context, focused);
    }

    #[test]
    fn overview_attention_includes_failures_and_limits_without_changing_session_policy() {
        for state in [State::Failed, State::Limited] {
            assert_eq!(priority(state), 0);
            assert!(!state.attention());
        }
        assert_eq!(priority(State::Working), 1);
        assert_eq!(priority(State::Idle), 2);
    }
}
