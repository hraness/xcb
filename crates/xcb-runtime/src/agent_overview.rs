//! Read-only summaries shared by the managed and direct terminal views.
use xcb_core::{session::State, ui::AgentRow};

pub(crate) const MAX_AGENTS: usize = 128;
pub(crate) const MAX_RESPONSE_BYTES: usize = 2048;

pub(crate) fn priority(state: State) -> u8 {
    if state.attention() {
        0
    } else if state == State::Working {
        1
    } else {
        2
    }
}

pub(crate) fn sort(rows: &mut [AgentRow]) {
    rows.sort_by(|a, b| {
        (
            priority(a.state),
            std::cmp::Reverse(a.updated_at_ms),
            context_id(a),
        )
            .cmp(&(
                priority(b.state),
                std::cmp::Reverse(b.updated_at_ms),
                context_id(b),
            ))
    });
}

fn context_id(row: &AgentRow) -> &str {
    match &row.context {
        xcb_core::ui::TranscriptContext::Conversation(id)
        | xcb_core::ui::TranscriptContext::Session(id) => id.as_str(),
    }
}
