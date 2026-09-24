//! Interactive TUI acceptance fixture. Uses in-memory synthetic data only;
//! never starts a provider, touches accounts, or opens an application database.
//! Run in a PTY: cargo run -p xcb-tui --example ux_fixture -- --recovery /tmp/xcb-fixture-input
use std::{io, path::PathBuf, sync::mpsc, thread};
use xcb_core::{
    Id, Provider,
    session::{Message, Role, State},
    ui::{
        BacklogRow, ConversationRow, HabitatCommand, InboxRow, Intent, TaskRow, TranscriptContext,
        TranscriptPage, Update, View,
    },
};
use xcb_tui::{RunOptions, run_with_options};

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}
fn id(value: &str) -> Id {
    Id::new(value).expect("fixture identifier")
}
fn message(value: &str, role: Role, text: impl Into<String>) -> Message {
    Message {
        id: id(value),
        role,
        text: text.into(),
        at_ms: now_ms(),
        attachments: Vec::new(),
        provenance: None,
    }
}
fn fixture() -> View {
    let conversation = id("fixture-conversation");
    let mut view = View {
        conversation: Some(conversation.clone()),
        conversations: vec![ConversationRow {
            id: conversation.clone(),
            title: "TUI acceptance fixture".into(),
            workspace: "/tmp/xcb-fixture".into(),
            messages: 4,
            updated_at_ms: now_ms(),
        }],
        extensions: vec![("algal supervisor".into(), "synthetic fixture".into())],
        state: State::NeedsAnswer,
        managed_cancel_available: true,
        ..View::default()
    };
    view.messages = vec![
        message(
            "fixture-user",
            Role::User,
            "Make the terminal easier to use. Preserve 東京, café, and 👩‍💻.",
        ),
        message(
            "fixture-tool",
            Role::Tool,
            "workspace_read: Read src/main.rs\nFound the prompt editor and footer.",
        ),
        message(
            "fixture-answer",
            Role::Assistant,
            "## A quieter workspace\nThe **prompt** stays at the bottom. Use `Ctrl-T` for history and `F3` to search it.\n\n- Unicode: 東京 · café · 👩‍💻\n- [Local design notes](docs/plans/codex-familiar-tui.md)\n\n```diff\n-old footer with bright blocks\n+quiet footer and a visible target\n```\n\nThis fixture never runs providers. Send **reject** to exercise draft recovery, or **change question** to replace the pending question. /attention opens the question; /agents opens task controls.",
        ),
    ];
    for (name, title, state, status, revision) in [
        (
            "fixture-running",
            "Improve composer",
            State::Working,
            "running",
            3,
        ),
        (
            "fixture-question",
            "Choose a test directory",
            State::NeedsAnswer,
            "needs input",
            7,
        ),
    ] {
        view.backlog.push(BacklogRow {
            id: id(name),
            conversation: conversation.clone(),
            title: title.into(),
            prompt: format!("Synthetic work: {title}"),
            summary: if state == State::NeedsAnswer {
                "Use tests/unit or tests/integration?"
            } else {
                "Reviewing the editor"
            }
            .into(),
            status: status.into(),
            state,
            deferred: false,
            priority: 5,
            revision,
            updated_at_ms: now_ms(),
        });
        view.tasks.push(TaskRow {
            id: id(name),
            revision,
            title: title.into(),
            state,
            status: Some(status.into()),
            detail: "Synthetic task; no process is running".into(),
            route: Some("codex/fixture · synthetic account".into()),
            route_reason: Some("Acceptance fixture".into()),
            settle: None,
            workspace: "/tmp/xcb-fixture".into(),
            updated_at_ms: now_ms(),
        });
    }
    view.accounts.push(xcb_core::ui::AccountRow {
        id: id("fixture-account"),
        provider: Provider::Codex,
        name: "synthetic account".into(),
        email: None,
        subscription: "fixture".into(),
        remaining_percent: Some(84.0),
        resets_at_ms: None,
        quota_blocked_until_ms: None,
        runway: xcb_core::usage::Estimate::unknown("synthetic fixture"),
        busy: true,
        enabled: true,
        authentication_required: false,
    });
    view.transcript = Some(TranscriptPage {
        context: TranscriptContext::Conversation(conversation),
        messages: view.messages.clone(),
        first_sequence: Some(10),
        has_older: true,
    });
    view
}

fn update_task(view: &mut View, task: &Id, state: State, detail: &str) {
    if let Some(row) = view.backlog.iter_mut().find(|row| &row.id == task) {
        row.state = state;
        row.revision += 1;
        row.status = state.label().into();
        row.summary = detail.into();
        if let Some(display) = view.tasks.iter_mut().find(|row| &row.id == task) {
            display.state = state;
            display.revision = row.revision;
            display.status = Some(row.status.clone());
            display.detail = detail.into();
        }
    }
    view.state = if view.backlog.iter().any(|row| row.state.attention()) {
        State::NeedsAnswer
    } else if view.backlog.iter().any(|row| row.state == State::Working) {
        State::Working
    } else {
        State::Idle
    };
    view.managed_cancel_available = view
        .backlog
        .iter()
        .any(|row| row.state == State::Working || row.state.attention());
}

fn main() -> io::Result<()> {
    let mut args = std::env::args().skip(1);
    let recovery_directory = match args.next().as_deref() {
        Some("--recovery") => {
            Some(PathBuf::from(args.next().ok_or_else(|| {
                io::Error::other("--recovery needs a private directory")
            })?))
        }
        None => None,
        Some(_) => {
            return Err(io::Error::other(
                "usage: ux_fixture [--recovery /tmp/private-fixture-input]",
            ));
        }
    };
    let (updates, input) = mpsc::channel();
    let (output, intents) = mpsc::sync_channel(32);
    let worker = thread::spawn(move || {
        let mut view = fixture();
        let context = id("fixture-conversation");
        let mut sequence = 20u64;
        if updates.send(Update::View(Box::new(view.clone()))).is_err() {
            return;
        }
        while let Ok(intent) = intents.recv() {
            sequence += 1;
            match intent {
                Intent::Quit => break,
                Intent::Submit {
                    id: prompt_id,
                    text,
                    attachments,
                }
                | Intent::SubmitTo {
                    id: prompt_id,
                    text,
                    attachments,
                    ..
                } => {
                    if text == "reject" {
                        let _ = updates.send(Update::SubmitRejected {
                            id: prompt_id,
                            context: Some(TranscriptContext::Conversation(context.clone())),
                            text,
                            attachments,
                            reason: "Fixture rejected the submission; the draft is retained."
                                .into(),
                        });
                        continue;
                    }
                    if text == "change question" {
                        update_task(
                            &mut view,
                            &id("fixture-question"),
                            State::NeedsAnswer,
                            "Replacement question: use smoke or integration tests?",
                        );
                    }
                    let mut prompt = message(prompt_id.as_str(), Role::User, text);
                    prompt.attachments = attachments;
                    view.messages.push(prompt);
                    view.messages.push(message(&format!("fixture-response-{sequence}"), Role::Assistant, "Received in the **synthetic fixture**.\n```rust\nlet prompt = \"kept safely\";\n```"));
                }
                Intent::TranscriptPage {
                    request, context, ..
                } => {
                    let _ = updates.send(Update::TranscriptPage { request, page: TranscriptPage { context, messages: vec![message("fixture-older", Role::Assistant, "Earlier conversation: the history needle is here.\nThis page is fetched through the normal UI channel.")], first_sequence: Some(1), has_older: false } });
                    continue;
                }
                Intent::Habitat(HabitatCommand::Steer { task, event, text }) => {
                    view.inbox.push(InboxRow {
                        id: event.clone(),
                        task: task.clone(),
                        conversation: context.clone(),
                        sequence,
                        kind: "guidance".into(),
                        text: text.clone(),
                        status: "delivered".into(),
                        created_at_ms: now_ms(),
                        updated_at_ms: now_ms(),
                        receipt: Some(
                            "Synthetic worker included the guidance in its next turn.".into(),
                        ),
                    });
                    let _ = updates.send(Update::HabitatAccepted {
                        context: context.clone(),
                        task: Some(task),
                        operation: event,
                        text,
                    });
                }
                Intent::Habitat(HabitatCommand::Reply {
                    id: task,
                    expected_revision,
                    reply,
                    text,
                }) => {
                    if view.backlog.iter().any(|row| {
                        row.id == task
                            && row.revision == expected_revision
                            && row.state == State::NeedsAnswer
                    }) {
                        update_task(
                            &mut view,
                            &task,
                            State::Working,
                            "Answer recorded by fixture",
                        );
                        let _ = updates.send(Update::HabitatAccepted {
                            context: context.clone(),
                            task: Some(task),
                            operation: reply,
                            text,
                        });
                    } else {
                        let _ = updates.send(Update::HabitatDraft {
                            context: context.clone(),
                            task: Some(task),
                            operation: reply,
                            text,
                        });
                    }
                }
                Intent::Habitat(
                    HabitatCommand::Enqueue {
                        id: task,
                        prompt,
                        deferred,
                        priority,
                    }
                    | HabitatCommand::EnqueueIn {
                        id: task,
                        prompt,
                        deferred,
                        priority,
                        ..
                    },
                ) => {
                    view.backlog.push(BacklogRow {
                        id: task.clone(),
                        conversation: context.clone(),
                        title: prompt.chars().take(60).collect(),
                        prompt: prompt.clone(),
                        summary: "Waiting in the synthetic queue".into(),
                        status: "queued — waiting for a route".into(),
                        state: State::Working,
                        deferred,
                        priority,
                        revision: 1,
                        updated_at_ms: now_ms(),
                    });
                    let _ = updates.send(Update::HabitatAccepted {
                        context: context.clone(),
                        task: None,
                        operation: task,
                        text: prompt,
                    });
                }
                Intent::Habitat(HabitatCommand::CancelTask {
                    id: task,
                    expected_revision,
                }) => {
                    if view
                        .backlog
                        .iter()
                        .any(|row| row.id == task && row.revision == expected_revision)
                    {
                        update_task(&mut view, &task, State::Cancelled, "Cancelled by fixture");
                    } else {
                        let _ = updates.send(Update::Notice(
                            "Task changed; the fixture rejected stale cancellation.".into(),
                        ));
                    }
                }
                Intent::Habitat(HabitatCommand::RecallQueued {
                    id: task,
                    expected_revision,
                    operation,
                }) => {
                    if let Some(row) = view.backlog.iter().find(|row| {
                        row.id == task
                            && row.revision == expected_revision
                            && row.status.starts_with("queued")
                    }) {
                        let text = row.prompt.clone();
                        update_task(&mut view, &task, State::Cancelled, "Recalled by fixture");
                        let _ = updates.send(Update::QueuedDraft {
                            context: context.clone(),
                            id: task,
                            operation,
                            text,
                        });
                    } else {
                        let _ = updates.send(Update::QueuedRecallRejected {
                            context: context.clone(),
                            id: task,
                            operation,
                            reason: "Fixture task is no longer queued.".into(),
                        });
                    }
                }
                Intent::Rename {
                    expected_title,
                    title,
                    ..
                } => {
                    if view.conversations[0].title == expected_title {
                        view.conversations[0].title = title;
                    }
                }
                Intent::Refresh => (),
                _ => {
                    let _ = updates.send(Update::Notice(
                        "This action is outside the synthetic fixture.".into(),
                    ));
                }
            }
            if updates.send(Update::View(Box::new(view.clone()))).is_err() {
                break;
            }
        }
    });
    let result = run_with_options(input, output, RunOptions { recovery_directory });
    let _ = worker.join();
    result
}
