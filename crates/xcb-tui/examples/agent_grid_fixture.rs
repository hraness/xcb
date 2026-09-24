//! Isolated, deterministic terminal fixture for the agent overview.
//! No provider, account, application database, or persistent user state is used.
use serde_json::json;
use std::{
    fs::OpenOptions,
    io::{self, Write},
    os::unix::fs::OpenOptionsExt,
    sync::mpsc,
    thread,
};
use xcb_core::{
    Id,
    session::{Message, Role, State},
    ui::{AgentRow, ConversationRow, Intent, TranscriptContext, Update, View},
};

const PROJECT: &str = "/synthetic/agent-grid/project";

fn id(value: &str) -> Id {
    Id::new(value).expect("synthetic identifier")
}

fn fixture() -> View {
    let states = [
        State::Working,
        State::Working,
        State::NeedsAnswer,
        State::NeedsApproval,
        State::NeedsAction,
        State::Limited,
        State::Failed,
        State::Uncertain,
        State::Cancelled,
        State::Idle,
        State::Idle,
        State::Working,
        State::NeedsAnswer,
        State::Idle,
        State::Working,
        State::Idle,
        State::Idle,
        State::Working,
    ];
    let mut view = View {
        conversation: Some(id("grid-main")),
        reduced_motion: true,
        ..View::default()
    };
    view.conversations.push(ConversationRow {
        id: id("grid-main"),
        title: "Overview acceptance chat".into(),
        workspace: PROJECT.into(),
        messages: 2,
        updated_at_ms: 1000,
    });
    for (index, state) in states.into_iter().enumerate() {
        let number = index + 1;
        let context = TranscriptContext::Conversation(id(&format!("grid-agent-{number:02}")));
        let activity = if index == 0 {
            "thinking"
        } else if state == State::Working {
            "working"
        } else {
            state.label()
        };
        // Agent 02 is working again after a completed response: the response
        // color must retain its category independently of the live status.
        let category = if index == 1 {
            Some("done")
        } else {
            match state {
                State::Idle => Some("done"),
                State::NeedsAnswer => Some("question"),
                State::NeedsApproval => Some("needs_approval"),
                State::NeedsAction => Some("needs_action"),
                State::Limited => Some("limited"),
                State::Failed => Some("failed"),
                State::Uncertain => Some("uncertain"),
                State::Cancelled => Some("cancelled"),
                _ => None,
            }
        };
        view.agents.push(AgentRow {
            context,
            task: Some(id(&format!("grid-task-{number:02}"))),
            title: format!(
                "Agent {number:02} {}",
                if index >= 16 {
                    "Other project"
                } else {
                    "Project work"
                }
            ),
            workspace: if index >= 16 {
                "/synthetic/agent-grid/other"
            } else {
                PROJECT
            }
            .into(),
            model: Some(
                if index % 2 == 0 {
                    "codex/fixture"
                } else {
                    "claude/fixture"
                }
                .into(),
            ),
            state,
            activity: activity.into(),
            response: format!("RESPONSE-{number:02}: synthetic latest reply"),
            category: category.map(str::to_owned),
            updated_at_ms: 1000 + number as u64,
        });
    }
    view.messages.push(Message {
        id: id("grid-user"),
        role: Role::User,
        text: "Keep all guidance in this main chat.".into(),
        at_ms: 1000,
        attachments: vec![],
        provenance: None,
    });
    view.messages.push(Message {
        id: id("grid-assistant"),
        role: Role::Assistant,
        text: (1..=80)
            .map(|number| {
                format!(
                    "TRANSCRIPT-{number:02}: main conversation remains independently scrollable.\n"
                )
            })
            .collect(),
        at_ms: 1001,
        attachments: vec![],
        provenance: None,
    });
    view
}

fn main() -> io::Result<()> {
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() != Some("--events") {
        return Err(io::Error::other(
            "usage: agent_grid_fixture --events NEW_PRIVATE_FILE",
        ));
    }
    let path = args
        .next()
        .ok_or_else(|| io::Error::other("missing event file"))?;
    if args.next().is_some() {
        return Err(io::Error::other("unexpected fixture argument"));
    }
    let mut events = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    let view = fixture();
    writeln!(
        events,
        "{}",
        json!({"kind":"fixture", "agents":view.agents.len(), "project_agents":16, "conversation":"grid-main"})
    )?;
    events.flush()?;
    let (updates, input) = mpsc::channel();
    let (output, intents) = mpsc::sync_channel(32);
    updates
        .send(Update::View(Box::new(view)))
        .map_err(io::Error::other)?;
    let worker = thread::spawn(move || -> io::Result<()> {
        for intent in intents {
            let mut quit = false;
            let event = match intent {
                Intent::SubmitTo {
                    context,
                    id,
                    text,
                    attachments,
                } => {
                    let record = json!({"kind":"submit_to", "context":context, "text":text, "attachments":attachments.len()});
                    let _ = updates.send(Update::Submitted { id, context });
                    record
                }
                Intent::Submit {
                    text, attachments, ..
                } => json!({"kind":"submit", "text":text, "attachments":attachments.len()}),
                Intent::Conversation(id) => json!({"kind":"conversation", "id":id.as_str()}),
                Intent::Resume(id) => json!({"kind":"resume", "id":id.as_str()}),
                Intent::Habitat(_) | Intent::HabitatAt { .. } => json!({"kind":"habitat"}),
                Intent::Quit => {
                    quit = true;
                    json!({"kind":"quit"})
                }
                Intent::Refresh => json!({"kind":"refresh"}),
                _ => json!({"kind":"other"}),
            };
            writeln!(events, "{event}")?;
            events.flush()?;
            if quit {
                break;
            }
        }
        Ok(())
    });
    let result = xcb_tui::run(input, output);
    worker
        .join()
        .map_err(|_| io::Error::other("fixture logger panicked"))??;
    result
}
