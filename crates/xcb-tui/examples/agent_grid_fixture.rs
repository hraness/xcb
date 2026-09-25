//! Isolated, deterministic terminal fixture for the agent overview.
//! No provider, account, application database, or persistent user state is used.
use serde_json::json;
use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    os::unix::fs::OpenOptionsExt,
    sync::mpsc,
    thread,
    time::Duration,
};
use xcb_core::{
    Id,
    session::{Message, Role, State},
    ui::{AgentRow, ConversationRow, Intent, STALE_ATTENTION_MS, TranscriptContext, Update, View},
};

const WORKSPACE: &str = "/synthetic/agent-grid/workspace";

fn id(value: &str) -> Id {
    Id::new(value).expect("synthetic identifier")
}

/// The grid ages attention against the wall clock, so rows are stamped now.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
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
    let base = now_ms();
    view.conversations.push(ConversationRow {
        id: id("grid-main"),
        title: "Overview acceptance chat".into(),
        workspace: WORKSPACE.into(),
        messages: 2,
        updated_at_ms: base,
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
                    "Other workspace"
                } else {
                    "Workspace work"
                }
            ),
            workspace: if index >= 16 {
                "/synthetic/agent-grid/other"
            } else {
                WORKSPACE
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
            updated_at_ms: base + number as u64,
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
            "usage: agent_grid_fixture --events NEW_PRIVATE_FILE --control PRIVATE_FILE",
        ));
    }
    let path = args
        .next()
        .ok_or_else(|| io::Error::other("missing event file"))?;
    if args.next().as_deref() != Some("--control") {
        return Err(io::Error::other("missing --control PRIVATE_FILE"));
    }
    let control = args
        .next()
        .ok_or_else(|| io::Error::other("missing control file"))?;
    if args.next().is_some() {
        return Err(io::Error::other("unexpected fixture argument"));
    }
    let mut events = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    let mut view = fixture();
    writeln!(
        events,
        "{}",
        json!({"kind":"fixture", "agents":view.agents.len(), "workspaces":2, "conversation":"grid-main"})
    )?;
    events.flush()?;
    let (updates, input) = mpsc::channel();
    let (output, intents) = mpsc::sync_channel(32);
    updates
        .send(Update::View(Box::new(view.clone())))
        .map_err(io::Error::other)?;
    let worker = thread::spawn(move || -> io::Result<()> {
        let mut revision = 0;
        loop {
            // The PTY driver publishes explicit updates and waits for this
            // acknowledgment. No timing-dependent changes race user input.
            let command: serde_json::Value =
                serde_json::from_slice(&fs::read(&control)?).map_err(io::Error::other)?;
            let next = command["revision"].as_u64().unwrap_or(0);
            if next > revision {
                match command["action"].as_str() {
                    Some("recency") => {
                        view.agents.reverse();
                        let base = now_ms() + 100_000;
                        for (index, row) in view.agents.iter_mut().enumerate() {
                            row.updated_at_ms = base + index as u64;
                        }
                    }
                    Some("stale") => {
                        // A minute past two days keeps the age label stable.
                        let at = now_ms().saturating_sub(2 * STALE_ATTENTION_MS + 60_000);
                        for number in command["agents"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(serde_json::Value::as_u64)
                        {
                            let context = TranscriptContext::Conversation(id(&format!(
                                "grid-agent-{number:02}"
                            )));
                            view.agents
                                .iter_mut()
                                .find(|row| row.context == context)
                                .ok_or_else(|| io::Error::other("unknown synthetic agent"))?
                                .updated_at_ms = at;
                        }
                    }
                    Some("attention") => {
                        let number = command["agent"].as_u64().unwrap_or(18);
                        let context =
                            TranscriptContext::Conversation(id(&format!("grid-agent-{number:02}")));
                        let row = view
                            .agents
                            .iter_mut()
                            .find(|row| row.context == context)
                            .ok_or_else(|| io::Error::other("unknown synthetic agent"))?;
                        row.state = State::NeedsAnswer;
                        row.activity = "needs answer".into();
                        row.category = Some("question".into());
                        row.response = format!("ATTENTION-{number:02}: synthetic question");
                    }
                    Some("restore") => view = fixture(),
                    _ => return Err(io::Error::other("unknown synthetic update")),
                }
                updates
                    .send(Update::View(Box::new(view.clone())))
                    .map_err(io::Error::other)?;
                revision = next;
                writeln!(
                    events,
                    "{}",
                    json!({"kind":"fixture_update", "revision":revision, "action":command["action"]})
                )?;
                events.flush()?;
            }
            let intent = match intents.recv_timeout(Duration::from_millis(20)) {
                Ok(intent) => intent,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            };
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
