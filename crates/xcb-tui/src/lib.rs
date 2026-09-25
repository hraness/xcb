mod agent_grid;
pub mod composer;
pub mod external_editor;
pub mod input_recovery;
mod interaction;
#[cfg(test)]
mod overview_tests;
mod recovery_ui;
pub mod render;

use composer::{Composer, ComposerAction};
use crossterm::{
    event::{
        self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags,
        MouseEventKind, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};
use ratatui_textarea::TextArea;
use std::{
    cell::Cell,
    collections::VecDeque,
    io::{self, IsTerminal},
    sync::mpsc::{Receiver, SyncSender, TryRecvError},
    time::{Duration, Instant},
};
use xcb_core::{
    Id,
    panes::Pane,
    session::{Attachment, State},
    ui::{HabitatCommand, Intent, TranscriptContext, Update, View},
    usage::Estimate,
};

/// One slash command as shown in the typeahead menu and `/help`. `args` is the
/// usage hint; `needs_args` marks commands that cannot run bare — completing
/// one lands the cursor after a space instead of executing immediately.
pub struct SlashCommand {
    pub name: &'static str,
    /// Single-letter shortcut, e.g. "/m" for "/model"; "" when none.
    pub alias: &'static str,
    pub args: &'static str,
    pub summary: &'static str,
    pub needs_args: bool,
}
pub const SLASH_COMMANDS: &[SlashCommand] = &[
    SlashCommand {
        name: "/detach",
        alias: "",
        args: "[index|all]",
        summary: "remove a pending attachment",
        needs_args: false,
    },
    SlashCommand {
        name: "/resume",
        alias: "",
        args: "",
        summary: "resume a saved conversation or session",
        needs_args: false,
    },
    SlashCommand {
        name: "/rename",
        alias: "",
        args: "[name]",
        summary: "rename this conversation or session",
        needs_args: false,
    },
    SlashCommand {
        name: "/status",
        alias: "",
        args: "",
        summary: "inspect current routing and agent state",
        needs_args: false,
    },
    SlashCommand {
        name: "/history",
        alias: "",
        args: "",
        summary: "browse saved transcript and older pages",
        needs_args: false,
    },
    SlashCommand {
        name: "/copy",
        alias: "",
        args: "",
        summary: "copy the last assistant answer",
        needs_args: false,
    },
    SlashCommand {
        name: "/clear",
        alias: "",
        args: "",
        summary: "clear display without deleting history",
        needs_args: false,
    },
    SlashCommand {
        name: "/editor",
        alias: "",
        args: "",
        summary: "edit this draft in VISUAL or EDITOR",
        needs_args: false,
    },
    SlashCommand {
        name: "/drafts",
        alias: "",
        args: "",
        summary: "recover private drafts from earlier terminals",
        needs_args: false,
    },
    SlashCommand {
        name: "/tools",
        alias: "",
        args: "",
        summary: "expand tool output in the transcript",
        needs_args: false,
    },
    SlashCommand {
        name: "/thinking",
        alias: "",
        args: "",
        summary: "show or hide reasoning output",
        needs_args: false,
    },
    SlashCommand {
        name: "/agents",
        alias: "",
        args: "",
        summary: "inspect live managed agents and tasks",
        needs_args: false,
    },
    SlashCommand {
        name: "/overview",
        alias: "",
        args: "[all|active|attention|filter <text>|clear|hide|show]",
        summary: "filter sessions; F6 browses the grid",
        needs_args: false,
    },
    SlashCommand {
        name: "/task",
        alias: "",
        args: "[prompt]",
        summary: "write a new task instead of guiding a selected task",
        needs_args: false,
    },
    SlashCommand {
        name: "/queue",
        alias: "",
        args: "[prompt]",
        summary: "queue ordinary project work",
        needs_args: false,
    },
    SlashCommand {
        name: "/cancel",
        alias: "",
        args: "[task-id]",
        summary: "cancel an explicitly observed task",
        needs_args: false,
    },
    SlashCommand {
        name: "/steer",
        alias: "",
        args: "<task-id> <guidance>",
        summary: "queue guidance for the next authorized task turn",
        needs_args: true,
    },
    SlashCommand {
        name: "/watch",
        alias: "",
        args: "<target-task> <source-task>",
        summary: "request another task's terminal report in the target inbox",
        needs_args: true,
    },
    SlashCommand {
        name: "/inbox",
        alias: "",
        args: "[all|task-id]",
        summary: "inspect durable guidance and delivery receipts",
        needs_args: false,
    },
    SlashCommand {
        name: "/project",
        alias: "",
        args: "[all|grant <tasks> <hours> <goal>|pause|resume]",
        summary: "bounded automatic project work and remaining budget",
        needs_args: false,
    },
    SlashCommand {
        name: "/program",
        alias: "",
        args: "[task-id]",
        summary: "inspect resumable programs and their worker tasks",
        needs_args: false,
    },
    SlashCommand {
        name: "/memory",
        alias: "",
        args: "search <query>",
        summary: "search the project's bound Wordcell vault",
        needs_args: true,
    },
    SlashCommand {
        name: "/attention",
        alias: "",
        args: "",
        summary: "questions, approvals and actions across agents",
        needs_args: false,
    },
    SlashCommand {
        name: "/backlog",
        alias: "/b",
        args: "[all|add …|edit <id> …|run <id>]",
        summary: "persistent work queue and completed work",
        needs_args: false,
    },
    SlashCommand {
        name: "/reply",
        alias: "",
        args: "<task-id> <answer>",
        summary: "answer a task; permissions still require approval",
        needs_args: true,
    },
    SlashCommand {
        name: "/schedule",
        alias: "",
        args: "[all|every <seconds> …|pause <id>|resume <id>]",
        summary: "local recurring prompts for this agent",
        needs_args: false,
    },
    SlashCommand {
        name: "/accounts",
        alias: "/a",
        args: "",
        summary: "pick the billing account",
        needs_args: false,
    },
    SlashCommand {
        name: "/attach",
        alias: "",
        args: "<path>",
        summary: "attach a file or image",
        needs_args: true,
    },
    SlashCommand {
        name: "/default",
        alias: "/d",
        args: "",
        summary: "make this account/model the default",
        needs_args: false,
    },
    SlashCommand {
        name: "/exit",
        alias: "/e",
        args: "",
        summary: "quit xcb",
        needs_args: false,
    },
    SlashCommand {
        name: "/help",
        alias: "/h",
        args: "",
        summary: "keyboard shortcuts and commands",
        needs_args: false,
    },
    SlashCommand {
        name: "/model",
        alias: "/m",
        args: "[query]",
        summary: "pick a model",
        needs_args: false,
    },
    SlashCommand {
        name: "/mouse",
        alias: "",
        args: "",
        summary: "toggle wheel scrolling vs. terminal text selection",
        needs_args: false,
    },
    SlashCommand {
        name: "/new",
        alias: "/n",
        args: "",
        summary: "start fresh work",
        needs_args: false,
    },
    SlashCommand {
        name: "/pane",
        alias: "/p",
        args: "[id|edit|generate …]",
        summary: "switch or manage panes",
        needs_args: false,
    },
    SlashCommand {
        name: "/plugin",
        alias: "",
        args: "<name> on|off",
        summary: "toggle an extension",
        needs_args: true,
    },
    SlashCommand {
        name: "/quit",
        alias: "/q",
        args: "",
        summary: "quit xcb",
        needs_args: false,
    },
    SlashCommand {
        name: "/reload",
        alias: "/r",
        args: "",
        summary: "refresh provider metadata",
        needs_args: false,
    },
    SlashCommand {
        name: "/sessions",
        alias: "/s",
        args: "",
        summary: "switch conversations or sessions",
        needs_args: false,
    },
    SlashCommand {
        name: "/tasks",
        alias: "/t",
        args: "",
        summary: "inspect managed work",
        needs_args: false,
    },
];

#[derive(Clone, PartialEq, Eq)]
pub enum PickAction {
    Pane(Id),
    Model(String),
    Account(Id),
    Conversation(Id),
    NewConversation,
    Session(Id),
    Task(Id),
    Backlog(Id),
    Inbox(Id),
    Schedule(Id),
    Project(Id),
    Program(Id),
    Text(String),
    EditPane,
    CancelTask { id: Id, revision: u64 },
    Recovery(usize),
}
#[derive(Clone)]
pub struct PickItem {
    pub label: String,
    pub action: PickAction,
}

pub enum EditorKind {
    Prompt,
    Pane { expected: Option<String> },
}
pub enum Modal {
    Picker {
        title: String,
        query: String,
        items: Vec<PickItem>,
        selected: usize,
    },
    Editor {
        title: String,
        textarea: Box<TextArea<'static>>,
        kind: EditorKind,
        error: Option<String>,
    },
    /// Scrollable read-only detail view (managed task inspect): full route,
    /// workspace and detail that a one-line notice could not show.
    Inspect {
        title: String,
        lines: Vec<String>,
        scroll: u16,
    },
    Help {
        scroll: u16,
    },
    HistorySearch {
        query: String,
        original: String,
        matches: Vec<String>,
        selected: usize,
    },
    Transcript {
        title: String,
        lines: Vec<String>,
        query: String,
        scroll: u32,
        matches: Vec<usize>,
        selected: usize,
        search: bool,
        has_more: bool,
    },
}

/// Draft text and pending attachments scoped to one session. Kept per session
/// so switching sessions never carries the previous draft into the new one.
#[derive(Default)]
struct SessionDraft {
    text: String,
    attachments: Vec<Attachment>,
    target: Option<interaction::ComposerTarget>,
    origin: Option<input_recovery::RecoveryInput>,
}

/// Bound on remembered per-context drafts; the least recently used is evicted.
const MAX_DRAFT_SESSIONS: usize = 64;

/// How long a notice stays on screen without any key press before it is
/// dropped on the next refresh.
const NOTICE_TTL: Duration = Duration::from_secs(8);

/// Byte bound of the Prompt/Pane editor dialog.
const EDITOR_MAX: usize = 64 * 1024;
const EDITOR_PASTE_TOO_LARGE: &str =
    "Paste exceeds the 64 KiB editor limit; attach a file or trim it";

fn view_context(view: &View) -> Option<Id> {
    view.conversation
        .clone()
        .or_else(|| view.session.as_ref().map(|session| session.id.clone()))
}

fn display_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// Compact age for rows: `12s ago`, `3m 7s ago`, `1h 4m ago`, `2d 3h ago`.
fn age_label(ms: u64) -> String {
    let seconds = ms / 1000;
    if seconds >= 86400 {
        format!("{}d {}h ago", seconds / 86400, seconds % 86400 / 3600)
    } else if seconds >= 3600 {
        format!("{}h {}m ago", seconds / 3600, seconds % 3600 / 60)
    } else if seconds >= 60 {
        format!("{}m {}s ago", seconds / 60, seconds % 60)
    } else {
        format!("{seconds}s ago")
    }
}

fn due_label(timestamp: u64) -> String {
    let now = display_now_ms();
    if timestamp <= now {
        "now or overdue".into()
    } else {
        format!("in {}", age_label(timestamp - now).trim_end_matches(" ago"))
    }
}

/// The managed worker phase a task row reports. `TaskRow::state` folds queued
/// and running into `State::Working`, so the supervisor's own phase label
/// keeps them distinguishable; rows produced without one fall back to the
/// mapped session state.
fn task_status(task: &xcb_core::ui::TaskRow) -> &str {
    task.status.as_deref().unwrap_or_else(|| task.state.label())
}

/// `TaskRow::status` is a display label (`queued — waiting for a route`), so
/// phase classification matches its leading word rather than the whole
/// string. Rows published without a status fall back to the mapped session
/// state, which cannot tell queued from running — those count as live work.
fn task_queued(task: &xcb_core::ui::TaskRow) -> bool {
    task.status
        .as_deref()
        .is_some_and(|status| status.starts_with("queued"))
}

/// Scrollable detail view for a managed task — the routed model/account,
/// workspace and latest detail that a one-line notice could not show.
fn inspect_task(task: &xcb_core::ui::TaskRow) -> Modal {
    let status = task_status(task);
    let mut headline = status.to_owned();
    // The mapped view state only adds signal when it asks for the user.
    if task.state.attention() && task.state.label() != status {
        headline.push_str(" · ");
        headline.push_str(task.state.label());
    }
    headline.push_str(" · updated ");
    headline.push_str(&age_label(
        display_now_ms().saturating_sub(task.updated_at_ms),
    ));
    let mut lines = vec![
        task.title.clone(),
        headline,
        String::new(),
        format!("task       {}", task.id),
        format!("workspace  {}", task.workspace),
        format!(
            "route      {}",
            task.route.as_deref().unwrap_or("not routed yet")
        ),
    ];
    if let Some(reason) = &task.route_reason {
        lines.push(format!("routing    {reason}"));
    }
    if let Some(settle) = &task.settle {
        lines.push(format!("last turn  {}", settle.replace('_', " ")));
    }
    lines.push(String::new());
    lines.push(task.detail.clone());
    Modal::Inspect {
        title: format!("{} · {}", task.id, status),
        lines,
        scroll: 0,
    }
}

fn inspect_inbox(event: &xcb_core::ui::InboxRow) -> Modal {
    Modal::Inspect {
        title: format!("Inbox {}", event.id),
        lines: vec![
            format!("event         {}", event.id),
            format!("task          {}", event.task),
            format!("conversation  {}", event.conversation),
            format!("sequence      {}", event.sequence),
            format!("kind          {}", event.kind),
            format!("delivery      {}", event.status),
            format!("created       {} ms since epoch", event.created_at_ms),
            format!("updated       {} ms since epoch", event.updated_at_ms),
            String::new(),
            "Content".into(),
            event.text.clone(),
            String::new(),
            "Delivery receipt".into(),
            event.receipt.clone().unwrap_or_else(|| "No settled delivery receipt yet.".into()),
            String::new(),
            "Delivery records inclusion in a worker turn, not proof the model followed the guidance. Guidance cannot grant approval or change project authority.".into(),
        ],
        scroll: 0,
    }
}

fn program_items(view: &View) -> Vec<PickItem> {
    view.programs
        .iter()
        .map(|program| PickItem {
            label: format!(
                "{} · {} · {}/{} calls · {}",
                program.parent,
                xcb_core::display_text(&program.phase, 80),
                program.calls,
                program.max_calls,
                xcb_core::display_text(
                    program.child_status.as_deref().unwrap_or("no active child"),
                    80
                ),
            ),
            action: PickAction::Program(program.parent.clone()),
        })
        .collect()
}

fn inspect_program(program: &xcb_core::ui::ProgramRow) -> Modal {
    let mut lines = vec![
        format!("program  {}", program.parent),
        format!("phase    {}", xcb_core::display_text(&program.phase, 160)),
        format!("calls    {}/{}", program.calls, program.max_calls),
    ];
    if let Some(child) = &program.child {
        lines.extend([
            format!("child    {child}"),
            format!(
                "status   {}",
                xcb_core::display_text(program.child_status.as_deref().unwrap_or("unknown"), 512)
            ),
            String::new(),
            format!("/backlog all · inspect worker {child}"),
            "/attention · resolve the worker's question or approval".into(),
            "A waiting program does not answer approvals or hold the workspace.".into(),
        ]);
    }
    lines.extend([
        String::new(),
        format!(
            "receipt  {}",
            program.receipt.as_deref().unwrap_or("no checkpoint yet")
        ),
        format!("xcb backlog program-status {} --json", program.parent),
    ]);
    Modal::Inspect {
        title: format!("Program {}", program.parent),
        lines,
        scroll: 0,
    }
}

/// Fingerprint of the rendered parts of a `View`. Used to skip repaints when a
/// refresh publishes a snapshot identical to what is already on screen. Only
/// fields the renderer reads participate; messages are append-only in the
/// store, so the transcript is identified by its tail.
fn fingerprint(view: &View) -> u64 {
    fingerprint_at(view, display_now_ms())
}

fn fingerprint_at(view: &View, now: u64) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    (view.state as u8).hash(&mut hasher);
    view.remote_active.hash(&mut hasher);
    view.managed_cancel_available.hash(&mut hasher);
    view.reduced_motion.hash(&mut hasher);
    view.runway_coverage.hash(&mut hasher);
    view.tokens_per_second.map(f64::to_bits).hash(&mut hasher);
    view.share_percent.map(f64::to_bits).hash(&mut hasher);
    view.total_runway_seconds
        .map(f64::to_bits)
        .hash(&mut hasher);
    view.conversation.as_ref().map(Id::as_str).hash(&mut hasher);
    for conversation in &view.conversations {
        conversation.id.as_str().hash(&mut hasher);
        conversation.title.hash(&mut hasher);
        conversation.workspace.hash(&mut hasher);
        conversation.updated_at_ms.hash(&mut hasher);
    }
    for agent in &view.agents {
        match &agent.context {
            TranscriptContext::Conversation(id) => {
                0_u8.hash(&mut hasher);
                id.hash(&mut hasher);
            }
            TranscriptContext::Session(id) => {
                1_u8.hash(&mut hasher);
                id.hash(&mut hasher);
            }
        }
        agent.task.hash(&mut hasher);
        agent.title.hash(&mut hasher);
        agent.workspace.hash(&mut hasher);
        agent.model.hash(&mut hasher);
        (agent.state as u8).hash(&mut hasher);
        agent.activity.hash(&mut hasher);
        agent.response.hash(&mut hasher);
        agent.category.hash(&mut hasher);
        agent.updated_at_ms.hash(&mut hasher);
        // Ageing reorders the grid and the card shows its age in hours.
        agent_grid::stale_attention_age(agent, now)
            .map(|age| age / 3_600_000)
            .hash(&mut hasher);
    }
    if let Some(session) = &view.session {
        session.id.as_str().hash(&mut hasher);
        session.account.as_str().hash(&mut hasher);
        session.model.key().hash(&mut hasher);
        session.model.label.hash(&mut hasher);
        session.workspace.hash(&mut hasher);
        session.title.hash(&mut hasher);
        session.pane.as_str().hash(&mut hasher);
        (session.state as u8).hash(&mut hasher);
        session.revision.hash(&mut hasher);
    }
    for session in &view.sessions {
        session.id.as_str().hash(&mut hasher);
        session.title.hash(&mut hasher);
        (session.state as u8).hash(&mut hasher);
        session.revision.hash(&mut hasher);
    }
    for account in &view.accounts {
        account.id.as_str().hash(&mut hasher);
        account.busy.hash(&mut hasher);
        account.enabled.hash(&mut hasher);
        account.authentication_required.hash(&mut hasher);
        account
            .remaining_percent
            .map(f64::to_bits)
            .hash(&mut hasher);
        account.resets_at_ms.hash(&mut hasher);
        account.quota_blocked_until_ms.hash(&mut hasher);
        // Retry estimates repaint only when their displayed minute changes.
        account.quota_block_label(now).hash(&mut hasher);
        match &account.runway {
            Estimate::Known { seconds } => seconds.to_bits().hash(&mut hasher),
            Estimate::Unknown { reason } => reason.hash(&mut hasher),
        }
    }
    for model in &view.models {
        model.key().hash(&mut hasher);
        model.label.hash(&mut hasher);
    }
    view.messages.len().hash(&mut hasher);
    for message in &view.messages {
        message.id.as_str().hash(&mut hasher);
        (message.role as u8).hash(&mut hasher);
        message.text.hash(&mut hasher);
        for attachment in &message.attachments {
            attachment.digest.hash(&mut hasher);
        }
    }
    if let Some(page) = &view.transcript {
        page.first_sequence.hash(&mut hasher);
        page.has_older.hash(&mut hasher);
    }
    for task in &view.tasks {
        task.id.as_str().hash(&mut hasher);
        task.revision.hash(&mut hasher);
        task.title.hash(&mut hasher);
        (task.state as u8).hash(&mut hasher);
        task.status.hash(&mut hasher);
        task.detail.hash(&mut hasher);
        task.route.hash(&mut hasher);
        task.route_reason.hash(&mut hasher);
        task.settle.hash(&mut hasher);
        task.updated_at_ms.hash(&mut hasher);
    }
    for task in &view.backlog {
        task.id.as_str().hash(&mut hasher);
        task.revision.hash(&mut hasher);
        (task.state as u8).hash(&mut hasher);
    }
    for schedule in &view.schedules {
        schedule.id.as_str().hash(&mut hasher);
        schedule.revision.hash(&mut hasher);
        schedule.next_due_ms.hash(&mut hasher);
    }
    for project in &view.projects {
        project.conversation.as_str().hash(&mut hasher);
        project.revision.hash(&mut hasher);
        project.status.hash(&mut hasher);
    }
    for program in &view.programs {
        program.parent.hash(&mut hasher);
        program.phase.hash(&mut hasher);
        program.calls.hash(&mut hasher);
        program.max_calls.hash(&mut hasher);
        program.child.hash(&mut hasher);
        program.child_status.hash(&mut hasher);
        program.receipt.hash(&mut hasher);
    }
    for event in &view.inbox {
        event.id.as_str().hash(&mut hasher);
        event.task.as_str().hash(&mut hasher);
        event.conversation.as_str().hash(&mut hasher);
        event.sequence.hash(&mut hasher);
        event.kind.hash(&mut hasher);
        event.text.hash(&mut hasher);
        event.status.hash(&mut hasher);
        event.created_at_ms.hash(&mut hasher);
        event.updated_at_ms.hash(&mut hasher);
        event.receipt.hash(&mut hasher);
    }
    for agent in &view.subagents {
        agent.id.as_str().hash(&mut hasher);
        (agent.state as u8).hash(&mut hasher);
        agent.label.hash(&mut hasher);
        agent.model.hash(&mut hasher);
    }
    view.activity.len().hash(&mut hasher);
    if let Some(last) = view.activity.last() {
        last.hash(&mut hasher);
    }
    for (name, state) in &view.extensions {
        name.hash(&mut hasher);
        state.hash(&mut hasher);
    }
    view.pane.id.as_str().hash(&mut hasher);
    view.pane_revision.hash(&mut hasher);
    view.pane_error.hash(&mut hasher);
    for pane in &view.panes {
        pane.id.as_str().hash(&mut hasher);
        pane.title.hash(&mut hasher);
    }
    hasher.finish()
}

/// A submitted prompt not yet confirmed by a View carrying its message.
struct PendingEcho {
    id: Id,
    /// Conversation/session context the submit targeted; `None` when unbound —
    /// the kernel binds whatever context it creates, so the echo follows.
    session: Option<Id>,
    text: String,
    attachments: usize,
    recovery_attachments: Vec<Attachment>,
}

enum InboxScope {
    Current,
    All,
    Task(Id),
}

impl InboxScope {
    fn title(&self) -> &'static str {
        match self {
            Self::Current => "This agent · inbox · recent delivery history",
            Self::All => "All agents · inbox · recent delivery history",
            Self::Task(_) => "Task inbox · recent delivery history",
        }
    }

    fn items(&self, view: &View) -> Vec<PickItem> {
        view.inbox
            .iter()
            .filter(|event| match self {
                Self::Current => view.conversation.as_ref() == Some(&event.conversation),
                Self::All => true,
                Self::Task(task) => &event.task == task,
            })
            .map(|event| PickItem {
                label: format!(
                    "{} · {} · {} · {} · {}",
                    event.kind,
                    event.status,
                    event.task,
                    xcb_core::display_text(&event.text, 60).replace(['\n', '\t'], " "),
                    event.id
                ),
                action: PickAction::Inbox(event.id.clone()),
            })
            .collect()
    }
}

#[derive(Default)]
pub struct App {
    pub view: View,
    pub(crate) agent_grid: agent_grid::AgentGrid,
    pub composer: Composer,
    pub stream: String,
    pub thinking: String,
    pub notice: String,
    pub attachments: Vec<Attachment>,
    pub modal: Option<Modal>,
    pub show_thinking: bool,
    pub show_history: bool,
    /// Also expands persisted tool-call output inline in the transcript.
    pub show_activity: bool,
    /// When the current run started, set on the first View reporting it;
    /// drives the elapsed-time status badge while a turn is live.
    pub working_since: Option<Instant>,
    /// Absolute index of the viewport's top line while `paused`; ignored when
    /// the viewport follows the tail.
    pub scroll: Cell<u32>,
    /// While true the transcript viewport is pinned to `scroll`; while false it
    /// follows the tail as new output arrives.
    pub paused: Cell<bool>,
    /// Top line index rendered last frame (max across scrollable panes).
    scroll_top: Cell<u32>,
    /// Tail offset rendered last frame (max across scrollable panes).
    scroll_tail: Cell<u32>,
    drafts: VecDeque<(Id, SessionDraft)>,
    pending_image: bool,
    /// Session an in-flight attachment belongs to; the arriving image is routed
    /// there even if the user switched sessions meanwhile.
    pending_image_session: Option<Id>,
    /// Submitted prompts echoed into the transcript optimistically until a
    /// View confirms the message landed — submit feedback is instant while
    /// the kernel persists and the provider starts.
    pending_echoes: VecDeque<PendingEcho>,
    /// Preserve a command's identity while its full channel retains the draft.
    inbox_draft_event: Option<(String, Id)>,
    /// Refresh an open receipt inspector when durable delivery changes.
    inbox_inspect: Option<Id>,
    program_inspect: Option<Id>,
    inbox_scope: Option<InboxScope>,
    /// Highlighted row of the slash-command typeahead menu.
    slash_selected: Cell<usize>,
    /// Esc closes the menu without canceling the turn; typing reopens it.
    slash_dismissed: Cell<bool>,
    /// Composer text the menu state belongs to; any edit resets selection.
    slash_text: std::cell::RefCell<String>,
    /// Per-frame render state: wrapped transcript rows per message, the
    /// streaming tail's last wrap, and textarea viewport mirrors. Interior
    /// mutability lets the render tree read `&App` while refreshing it.
    pub(crate) render_cache: std::cell::RefCell<render::RenderCache>,
    dirty: bool,
    view_fingerprint: u64,
    /// Mouse capture is off by default so terminal-native drag selection and
    /// copy keep working; `/mouse` turns wheel scrolling on.
    pub mouse_capture: bool,
    /// Set by `/mouse`; the terminal loop applies the change and clears it.
    mouse_toggled: bool,
    /// Whether the terminal accepted the kitty keyboard-enhancement flags;
    /// only then does Shift-Enter arrive distinguishable from Enter.
    pub keyboard_enhanced: bool,
    /// The help dialog was opened by `?` on an empty composer; a second `?`
    /// closes it and types the literal character instead.
    help_via_question: bool,
    /// Notice text last observed and when it appeared; drives expiry.
    notice_seen: String,
    notice_since: Option<Instant>,
    composer_target: Option<interaction::ComposerTarget>,
    pending_habitat: VecDeque<interaction::PendingHabitat>,
    live_picker: Option<interaction::LivePicker>,
    live_inspect: Option<PickAction>,
    external_editor_requested: bool,
    initial_view_pending: bool,
    pub transcript_clear_before: Option<Id>,
    history_messages: Vec<xcb_core::session::Message>,
    history_context: Option<TranscriptContext>,
    history_first: Option<u64>,
    history_more: bool,
    history_request: Option<(Id, TranscriptContext)>,
    pub(crate) viewport_height: Cell<u16>,
    recovery: Option<input_recovery::RecoveryJournal>,
    recovery_directory: Option<std::path::PathBuf>,
    recovery_entries: Vec<input_recovery::RecoveryEntry>,
    recovery_choices: Vec<recovery_ui::RecoveryChoice>,
    recovery_extras: VecDeque<recovery_ui::RetainedInput>,
    recovery_composer_origin: Option<input_recovery::RecoveryInput>,
    recovery_fingerprint: u64,
    recovery_saved: Option<Instant>,
}
impl App {
    /// The pending `/mouse` change, if any: `Some(true)` enables capture.
    pub fn take_mouse_toggle(&mut self) -> Option<bool> {
        std::mem::take(&mut self.mouse_toggled).then_some(self.mouse_capture)
    }
    /// Notices are transient: any key press dismisses one, and the periodic
    /// view refresh drops one that has been on screen for `NOTICE_TTL`.
    fn track_notice(&mut self) {
        if self.notice != self.notice_seen {
            self.notice_seen.clone_from(&self.notice);
            self.notice_since = (!self.notice.is_empty()).then(Instant::now);
        }
    }
    fn expire_notice(&mut self) {
        self.track_notice();
        if self
            .notice_since
            .is_some_and(|since| since.elapsed() >= NOTICE_TTL)
        {
            self.notice.clear();
            self.notice_seen.clear();
            self.notice_since = None;
            self.dirty = true;
        }
    }
    fn open_help(&mut self, via_question: bool) {
        self.modal = Some(Modal::Help { scroll: 0 });
        self.help_via_question = via_question;
    }
    fn managed_mode(&self) -> bool {
        self.view
            .extensions
            .iter()
            .any(|(name, _)| name == "algal supervisor")
    }
    fn has_live_work(&self) -> bool {
        matches!(self.view.state, State::Working)
            || self.view.remote_active
            || self
                .view
                .tasks
                .iter()
                .any(|task| task.state == State::Working)
    }
    fn can_cancel_work(&self) -> bool {
        if self.managed_mode() {
            self.view.managed_cancel_available
        } else {
            self.has_live_work()
        }
    }
    /// Absolute top line index rendered last frame; used to anchor PageUp.
    pub fn scroll_top(&self) -> u32 {
        self.scroll_top.get()
    }
    /// Tail offset rendered last frame; used to resume following on PageDown.
    pub fn scroll_tail(&self) -> u32 {
        self.scroll_tail.get()
    }
    /// Scroll the transcript viewport: negative deltas pin an absolute line
    /// index upward so streamed output cannot move what the user is reading;
    /// positive deltas step down and resume following at the tail.
    fn scroll_transcript(&self, delta: i32) {
        if delta < 0 {
            let top = if self.paused.get() {
                self.scroll.get()
            } else {
                self.scroll_top.get()
            };
            self.scroll.set(top.saturating_sub(delta.unsigned_abs()));
            self.paused.set(true);
        } else if self.paused.get() {
            let next = self.scroll.get().saturating_add(delta as u32);
            if next >= self.scroll_tail.get() {
                self.paused.set(false);
                self.scroll.set(0);
            } else {
                self.scroll.set(next);
            }
        }
    }
    /// Optimistic echoes of submitted prompts still awaiting a View carrying
    /// the persisted message — `(text, attachment count)` pairs bound to the
    /// current conversation/session or still awaiting the context the kernel creates.
    pub fn pending_echoes(&self) -> impl Iterator<Item = (&str, usize)> {
        let current = view_context(&self.view);
        self.pending_echoes
            .iter()
            .filter(move |echo| echo.session.is_none() || echo.session == current)
            .map(|echo| (echo.text.as_str(), echo.attachments))
    }
    /// True when state changed since the last draw and a repaint is needed.
    pub fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }
    /// Commands matching the composer's current `/` prefix, in menu order. The
    /// menu only covers the command token — typing a space closes it.
    pub fn slash_matches(&self) -> Vec<&'static SlashCommand> {
        let text = self.composer.text();
        if !text.starts_with('/') || text.contains(char::is_whitespace) || text.len() > 64 {
            return Vec::new();
        }
        const MANAGED: &[&str] = &[
            "/detach",
            "/resume",
            "/rename",
            "/status",
            "/history",
            "/copy",
            "/clear",
            "/editor",
            "/drafts",
            "/tools",
            "/thinking",
            "/agents",
            "/overview",
            "/task",
            "/queue",
            "/cancel",
            "/steer",
            "/watch",
            "/inbox",
            "/project",
            "/program",
            "/memory",
            "/attention",
            "/backlog",
            "/reply",
            "/schedule",
            "/attach",
            "/exit",
            "/help",
            "/mouse",
            "/new",
            "/quit",
            "/sessions",
            "/tasks",
        ];
        let available =
            |command: &SlashCommand| !self.managed_mode() || MANAGED.contains(&command.name);
        // A complete alias is a command, not an ambiguous prefix. Adding
        // /schedule or /attention must never steal /s or /a from users.
        if let Some(command) = SLASH_COMMANDS.iter().find(|command| command.alias == text) {
            return if available(command) {
                vec![command]
            } else {
                vec![]
            };
        }
        let mut matches: Vec<_> = SLASH_COMMANDS
            .iter()
            .filter(|command| available(command))
            .filter(|command| command.name.starts_with(&text))
            .collect();
        matches.sort_by_key(|command| command.name);
        matches
    }
    /// The open typeahead menu as `(matches, selected)`, if any. Lazily resyncs
    /// menu state against the live composer text so any edit — typed, pasted,
    /// or a restored draft — resets selection and un-dismisses the menu.
    pub fn slash_menu(&self) -> Option<(Vec<&'static SlashCommand>, usize)> {
        let text = self.composer.text();
        if *self.slash_text.borrow() != text {
            *self.slash_text.borrow_mut() = text;
            self.slash_selected.set(0);
            self.slash_dismissed.set(false);
        }
        let matches = self.slash_matches();
        if self.slash_dismissed.get()
            || !self.composer.text().starts_with('/')
            || self.composer.text().contains(char::is_whitespace)
        {
            return None;
        }
        Some((
            matches.clone(),
            self.slash_selected
                .get()
                .min(matches.len().saturating_sub(1)),
        ))
    }
    fn save_draft(&mut self, session: Id, draft: SessionDraft) {
        if let Some(position) = self.drafts.iter().position(|(id, _)| id == &session) {
            self.drafts.remove(position);
        }
        self.drafts.push_back((session, draft));
        while self.drafts.len() > MAX_DRAFT_SESSIONS {
            if let Some((context, draft)) = self.drafts.pop_front() {
                self.retain_rejected_with_origin(
                    context,
                    draft.text,
                    draft.attachments,
                    draft.target,
                    draft.origin,
                );
            }
        }
    }
    fn take_draft(&mut self, session: &Id) -> Option<SessionDraft> {
        self.drafts
            .iter()
            .position(|(id, _)| id == session)
            .and_then(|position| self.drafts.remove(position))
            .map(|(_, draft)| draft)
    }
    fn stash_attachment(&mut self, session: &Id, attachment: Attachment) {
        let mut draft = self.take_draft(session).unwrap_or_default();
        if draft.attachments.len() < 8
            && !draft
                .attachments
                .iter()
                .any(|held| held.digest == attachment.digest)
        {
            draft.attachments.push(attachment);
        }
        self.save_draft(session.clone(), draft);
    }
    fn restore_draft(&mut self, text: String, attachments: Vec<Attachment>) {
        // A composer the user already started typing into is never clobbered;
        // the rejected text stays recoverable from prompt history.
        if self.composer.text().is_empty() {
            self.composer.set_text(&text);
        }
        for attachment in attachments {
            if self.attachments.len() >= 8 {
                break;
            }
            if !self
                .attachments
                .iter()
                .any(|held| held.digest == attachment.digest)
            {
                self.attachments.push(attachment);
            }
        }
    }
    pub fn apply(&mut self, update: Update) -> bool {
        match update {
            Update::Submitted { id, .. } => {
                self.pending_echoes.retain(|echo| echo.id != id);
                self.dirty = true;
            }
            Update::SubmitRejected {
                id,
                context,
                text,
                attachments,
                reason,
            } => {
                if let Some(index) = self.pending_echoes.iter().position(|echo| echo.id == id) {
                    let pending = self.pending_echoes.remove(index).expect("matched submit");
                    let context = context
                        .map(|context| match context {
                            TranscriptContext::Conversation(id)
                            | TranscriptContext::Session(id) => id,
                        })
                        .or(pending.session);
                    self.composer.remember(&text);
                    if context == view_context(&self.view)
                        && self.composer.text().is_empty()
                        && self.attachments.is_empty()
                        && self.composer_target.is_none()
                        && !self.pending_image
                    {
                        self.composer.set_text(&text);
                        self.attachments = attachments;
                    } else if let Some(context) = context {
                        self.retain_rejected(context, text, attachments, None);
                    } else {
                        self.restore_draft(text, attachments);
                    }
                }
                self.notice = xcb_core::display_text(&reason, 1024);
                self.dirty = true;
            }
            Update::TranscriptPageRejected {
                context,
                request,
                reason,
            } => {
                if self.history_request.as_ref() == Some(&(request, context)) {
                    self.history_request = None;
                    self.notice = xcb_core::display_text(&reason, 1024);
                    self.dirty = true;
                }
            }
            Update::QueuedRecallRejected {
                context: _,
                id,
                operation,
                reason,
            } => {
                self.pending_habitat.retain(|entry| {
                    entry.task.as_ref() != Some(&id) || entry.operation != operation
                });
                self.notice = xcb_core::display_text(&reason, 1024);
                self.dirty = true;
            }
            Update::TranscriptPage { request, page } => self.accept_history_page(request, page),
            Update::HabitatDraft {
                context,
                task,
                operation,
                text,
            } => self.habitat_outcome(false, context, task, operation, text),
            Update::HabitatAccepted {
                context,
                task,
                operation,
                text,
            } => self.habitat_outcome(true, context, task, operation, text),
            Update::QueuedDraft {
                context,
                id,
                operation,
                text,
            } => self.accept_queued_draft(context, id, operation, text),
            Update::View(mut view) => {
                self.initial_view_pending = false;
                self.expire_notice();
                if view.pane_error.is_some() {
                    view.pane = self.view.pane.clone();
                    view.pane_revision = self.view.pane_revision.clone();
                }
                let previous = view_context(&self.view);
                let next = view_context(&view);
                if previous != next {
                    let target = self.composer_target.take();
                    let origin = self.recovery_composer_origin.take();
                    self.transcript_clear_before = None;
                    self.history_request = None;
                    if matches!(self.modal, Some(Modal::Transcript { .. })) {
                        self.modal = None;
                    }
                    // Before the first context arrives, input already belongs to
                    // the context being opened. A delayed initial view must not
                    // erase part of a command or an attachment typed meanwhile.
                    let preserve_unbound_input = previous.is_none()
                        && (!self.composer.text().is_empty() || !self.attachments.is_empty());
                    // The draft belongs to the conversation or session it was typed in:
                    // stash it and restore the target context's own draft.
                    if let Some(previous) = previous {
                        let text = self.composer.text();
                        let attachments = std::mem::take(&mut self.attachments);
                        if !text.is_empty() || !attachments.is_empty() {
                            self.save_draft(
                                previous,
                                SessionDraft {
                                    text,
                                    attachments,
                                    target,
                                    origin,
                                },
                            );
                        }
                    }
                    if !preserve_unbound_input {
                        let draft = next.and_then(|id| self.take_draft(&id)).unwrap_or_default();
                        self.composer.set_text(&draft.text);
                        self.attachments = draft.attachments;
                        self.composer_target = draft.target;
                        self.recovery_composer_origin = draft.origin;
                    }
                    self.stream.clear();
                    self.thinking.clear();
                    self.scroll.set(0);
                    self.paused.set(false);
                    self.scroll_top.set(0);
                    self.scroll_tail.set(0);
                    self.dirty = true;
                }
                let fingerprint = fingerprint(&view);
                if fingerprint != self.view_fingerprint {
                    self.view_fingerprint = fingerprint;
                    self.dirty = true;
                }
                self.view = *view;
                self.refresh_live_surfaces();
                if let Some(Modal::Picker {
                    title,
                    items,
                    query,
                    selected,
                }) = &mut self.modal
                    && title == "Recent managed programs"
                {
                    let query_lower = query.to_lowercase();
                    let previous = items
                        .iter()
                        .filter(|item| item.label.to_lowercase().contains(&query_lower))
                        .nth(*selected)
                        .and_then(|item| match &item.action {
                            PickAction::Program(id) => Some(id.clone()),
                            _ => None,
                        });
                    *items = program_items(&self.view);
                    let filtered: Vec<_> = items
                        .iter()
                        .filter(|item| item.label.to_lowercase().contains(&query_lower))
                        .collect();
                    *selected = previous.and_then(|id| filtered.iter().position(|item| matches!(&item.action, PickAction::Program(parent) if parent == &id)))
                        .unwrap_or_else(|| (*selected).min(filtered.len().saturating_sub(1)));
                }
                if let Some(id) = &self.program_inspect {
                    let expected = format!("Program {id}");
                    if let Some(Modal::Inspect { title, scroll, .. }) = &self.modal
                        && *title == expected
                    {
                        let old_scroll = *scroll;
                        if let Some(program) =
                            self.view.programs.iter().find(|row| &row.parent == id)
                        {
                            let mut modal = inspect_program(program);
                            if let Modal::Inspect { scroll, .. } = &mut modal {
                                *scroll = old_scroll;
                            }
                            self.modal = Some(modal);
                        } else {
                            self.modal = Some(Modal::Inspect {
                                title: expected,
                                lines: vec![format!(
                                    "Outside the current bounded view. Use xcb backlog program-status {id} --json for current state."
                                )],
                                scroll: 0,
                            });
                        }
                    } else {
                        self.program_inspect = None;
                    }
                }
                if let Some(scope) = &self.inbox_scope {
                    if let Some(Modal::Picker { title, items, .. }) = &mut self.modal
                        && title.as_str() == scope.title()
                    {
                        *items = scope.items(&self.view);
                    } else {
                        self.inbox_scope = None;
                    }
                }
                if let Some(id) = &self.inbox_inspect {
                    let title = format!("Inbox {id}");
                    if let Some(Modal::Inspect {
                        title: current,
                        scroll,
                        ..
                    }) = &self.modal
                        && *current == title
                    {
                        let scroll = *scroll;
                        if let Some(event) = self.view.inbox.iter().find(|event| &event.id == id) {
                            let mut modal = inspect_inbox(event);
                            if let Modal::Inspect { scroll: next, .. } = &mut modal {
                                *next = scroll;
                            }
                            self.modal = Some(modal);
                        }
                    } else {
                        self.inbox_inspect = None;
                    }
                }
                // The badge timer follows the run lifecycle, not session
                // identity — a remote-owned run still counts as working.
                if self.has_live_work() {
                    if self.working_since.is_none() {
                        let elapsed = self
                            .view
                            .tasks
                            .iter()
                            .filter(|task| task.state == State::Working)
                            .map(|task| display_now_ms().saturating_sub(task.updated_at_ms))
                            .max()
                            .unwrap_or(0);
                        self.working_since =
                            Instant::now().checked_sub(Duration::from_millis(elapsed));
                    }
                } else {
                    self.working_since = None;
                }
                if !self.pending_echoes.is_empty() {
                    self.pending_echoes.retain(|echo| {
                        !self.view.messages.iter().any(|message| {
                            message.role == xcb_core::session::Role::User && message.id == echo.id
                        })
                    });
                    self.dirty = true;
                }
            }
            Update::Delta {
                session,
                thinking,
                text,
            } if self
                .view
                .session
                .as_ref()
                .is_some_and(|current| current.id == session) =>
            {
                let target = if thinking {
                    &mut self.thinking
                } else {
                    &mut self.stream
                };
                let remaining = xcb_core::MAX_TEXT_BYTES.saturating_sub(target.len());
                target.push_str(&xcb_core::display_text(&text, remaining));
                self.dirty = true;
            }
            Update::ClearStream(session)
                if self
                    .view
                    .session
                    .as_ref()
                    .is_some_and(|current| current.id == session) =>
            {
                self.stream.clear();
                self.thinking.clear();
                self.dirty = true;
            }
            Update::Draft { text, attachments } => {
                // A rejected submission retracts its optimistic echo.
                if let Some(position) = self
                    .pending_echoes
                    .iter()
                    .rposition(|echo| echo.text == text)
                {
                    self.pending_echoes.remove(position);
                }
                self.restore_draft(text, attachments);
                self.dirty = true;
            }
            Update::Attachment(attachment) => {
                self.pending_image = false;
                // An image that lands after a context switch belongs to the
                // conversation/session that requested it, not the one now on screen.
                match self
                    .pending_image_session
                    .take()
                    .filter(|context| view_context(&self.view).as_ref() != Some(context))
                {
                    Some(session) => self.stash_attachment(&session, attachment),
                    None => {
                        if self.attachments.len() < 8 {
                            self.attachments.push(attachment);
                        }
                    }
                }
                self.dirty = true;
            }
            Update::PaneCandidate(pane) => {
                self.edit_pane(&pane, None);
                self.dirty = true;
            }
            Update::Notice(text) => {
                self.pending_image = false;
                self.pending_image_session = None;
                self.notice = xcb_core::display_text(&text, 1024);
                self.dirty = true;
            }
            Update::Stopped => return false,
            _ => (),
        }
        true
    }
    fn picker(&mut self, title: &str, items: Vec<PickItem>) {
        self.inbox_scope = None;
        self.live_picker = None;
        self.live_inspect = None;
        self.modal = Some(Modal::Picker {
            title: title.into(),
            query: String::new(),
            items,
            selected: 0,
        });
    }
    fn edit_pane(&mut self, pane: &Pane, expected: Option<String>) {
        let text = serde_json::to_string_pretty(pane).expect("valid pane");
        self.modal = Some(Modal::Editor {
            title: "Pane declaration · Ctrl-S validates and applies".into(),
            textarea: Box::new(TextArea::from(text.lines())),
            kind: EditorKind::Pane { expected },
            error: None,
        });
    }
    fn try_send(&mut self, output: &SyncSender<Intent>, intent: Intent) -> bool {
        let intent = match intent {
            Intent::Habitat(
                command @ (HabitatCommand::ConfigureProject { .. }
                | HabitatCommand::Schedule { .. }
                | HabitatCommand::MemorySearch { .. }),
            ) => {
                let Some(conversation) = self.view.conversation.clone() else {
                    self.notice =
                        "Open a conversation before changing its project settings.".into();
                    return false;
                };
                Intent::HabitatAt {
                    conversation,
                    command,
                }
            }
            intent => intent,
        };
        if matches!(
            intent,
            Intent::NewSession | Intent::Conversation(_) | Intent::Resume(_)
        ) && !self.recovery_capacity_available()
        {
            self.notice =
                "Recover or discard retained input before opening another conversation.".into();
            return false;
        }
        if output.try_send(intent).is_err() {
            self.notice = "The command queue is full or closed. Nothing was submitted.".into();
            false
        } else {
            true
        }
    }
    fn send(&mut self, output: &SyncSender<Intent>, intent: Intent) {
        self.try_send(output, intent);
    }
    fn request_cancel(&mut self, output: &SyncSender<Intent>) {
        if self.managed_mode() {
            self.managed_cancel(output);
            return;
        }
        if self.try_send(output, Intent::Cancel) {
            self.notice = if self.managed_mode() {
                "Cancellation requested for this conversation; check the task status for settlement."
            } else if self.view.remote_active {
                "This turn is running in another terminal; cancel it there."
            } else {
                "Stopping the current turn and queued follow-ups."
            }
            .into();
        }
    }
    fn request_attachment(&mut self, output: &SyncSender<Intent>, intent: Intent) {
        if self.pending_image {
            self.notice =
                "Wait for the current image to finish loading before adding another.".into();
        } else if self.try_send(output, intent) {
            self.pending_image = true;
            self.pending_image_session = view_context(&self.view);
        }
    }
    fn send_habitat(
        &mut self,
        output: &SyncSender<Intent>,
        intent: Intent,
        command: &str,
        arguments: &str,
    ) {
        let pending = self.habitat_pending(&intent);
        if pending.is_some()
            && (self.pending_habitat.len() >= 16 || !self.recovery_capacity_available())
        {
            self.composer.set_text(&format!("{command} {arguments}"));
            self.notice = "Waiting for earlier input acknowledgements; command retained.".into();
            return;
        }
        let tracked = pending.is_some();
        if let Some(pending) = pending {
            self.pending_habitat.push_back(pending);
        }
        if tracked && !self.flush_recovery(true) {
            self.pending_habitat.pop_back();
            self.composer.set_text(&format!("{command} {arguments}"));
            return;
        }
        if self.try_send(output, intent) {
            self.inbox_draft_event = None;
        } else {
            if tracked {
                self.pending_habitat.pop_back();
            }
            self.composer.set_text(&format!("{command} {arguments}"));
            self.flush_recovery(true);
        }
    }

    fn inbox_event_id(&mut self, command: &str, arguments: &str) -> Id {
        let draft = format!("{command} {arguments}");
        if let Some((previous, event)) = &self.inbox_draft_event
            && previous == &draft
        {
            return event.clone();
        }
        let event = Id::new(format!("inbox_{}", uuid::Uuid::new_v4().simple()))
            .expect("generated inbox event id");
        self.inbox_draft_event = Some((draft, event.clone()));
        event
    }

    fn send_inbox(
        &mut self,
        output: &SyncSender<Intent>,
        intent: Intent,
        command: &str,
        arguments: &str,
    ) {
        self.send_habitat(output, intent, command, arguments);
    }

    fn habitat_command(&mut self, command: &str, arguments: &str, output: &SyncSender<Intent>) {
        let (action, tail) = arguments.split_once(' ').unwrap_or((arguments, ""));
        let tail = tail.trim();
        match command {
            "/steer" if !tail.is_empty() => {
                if let Ok(task) = Id::new(action) {
                    let event = self.inbox_event_id(command, arguments);
                    self.send_inbox(output, Intent::Habitat(HabitatCommand::Steer {
                        task, event, text: tail.into(),
                    }), command, arguments);
                } else {
                    self.notice = "Use /steer <task-id> <guidance>. Task IDs are shown in /tasks and /backlog.".into();
                }
            }
            "/steer" => {
                if let Ok(id) = Id::new(action) { self.select_task_target(&id, false); }
                else { self.notice = "Use /steer <task-id> [guidance], or select a task in /agents and press s.".into(); }
            }
            "/watch" => {
                if let (Ok(task), Ok(source)) = (Id::new(action), Id::new(tail)) {
                    let event = self.inbox_event_id(command, arguments);
                    self.send_inbox(output, Intent::Habitat(HabitatCommand::WatchTask {
                        task, source, event,
                    }), command, arguments);
                } else {
                    self.notice = "Use /watch <target-task-id> <source-task-id>. Request a terminal report from the same project/workspace.".into();
                }
            }
            "/inbox" => {
                let scope = if arguments.is_empty() {
                    InboxScope::Current
                } else if arguments == "all" {
                    InboxScope::All
                } else if let Ok(id) = Id::new(arguments) {
                    InboxScope::Task(id)
                } else {
                    self.notice = "Use /inbox for this conversation, /inbox all, or /inbox <task-id>.".into();
                    return;
                };
                let items = scope.items(&self.view);
                if items.is_empty() {
                    self.notice = "No matching events in the current bounded inbox view. Use xcb inbox --task <task-id> to inspect task history.".into();
                } else {
                    self.notice = "Showing a recent global inbox snapshot. Use xcb inbox --task <task-id> for paged history beyond this bounded view.".into();
                }
                self.picker(scope.title(), items);
                self.inbox_scope = Some(scope);
            }
            "/project" if arguments.is_empty() || arguments == "all" => {
                let all = arguments == "all";
                self.picker("Project grants · goal / budget / status", self.view.projects.iter()
                    .filter(|project| all || self.view.conversation.as_ref() == Some(&project.conversation))
                    .map(|project| PickItem {
                        label: format!("{} · {} · {} tasks left · {}", xcb_core::display_text(&project.goal, 52), project.status, project.remaining_tasks, project.conversation),
                        action: PickAction::Project(project.conversation.clone()),
                    }).collect());
            }
            "/project" if matches!(action, "pause" | "resume") => {
                if let Some(project) = self.view.projects.iter().find(|project| if tail.is_empty() {
                    self.view.conversation.as_ref() == Some(&project.conversation)
                } else { project.conversation.as_str() == tail }) {
                    self.send_habitat(output, Intent::Habitat(HabitatCommand::ProjectEnabled { conversation: project.conversation.clone(), expected_revision: project.revision, enabled: action == "resume" }), command, arguments);
                } else { self.notice = "No matching project grant. /project all lists grants.".into(); }
            }
            "/project" if action == "grant" => {
                let mut parts = tail.splitn(3, ' ');
                let tasks = parts.next().and_then(|v| v.parse::<u32>().ok());
                let hours = parts.next().and_then(|v| v.parse::<u64>().ok());
                let goal = parts.next().unwrap_or("").trim();
                if let (Some(max_tasks), Some(hours)) = (tasks, hours)
                    && (1..=100).contains(&max_tasks) && (1..=720).contains(&hours) && !goal.is_empty() {
                    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
                    let current = self.view.projects.iter().find(|p| self.view.conversation.as_ref() == Some(&p.conversation));
                    self.send_habitat(output, Intent::Habitat(HabitatCommand::ConfigureProject {
                        expected_revision: current.map(|p| p.revision), goal: goal.into(), max_tasks,
                        expires_at_ms: now.saturating_add(hours * 3_600_000), required_provider: current.and_then(|p| p.required_provider),
                    }), command, arguments);
                } else { self.notice = "Use /project grant <1–100 tasks> <1–720 hours> <goal>. This authorizes automatic follow-up work.".into(); }
            }
            "/project" => self.notice = "Use /project [all], /project grant <tasks> <hours> <goal>, or /project pause|resume [conversation].".into(),
            "/memory" if action == "search" && !tail.is_empty() => {
                self.send_habitat(output, Intent::Habitat(HabitatCommand::MemorySearch { query: tail.into() }), command, arguments);
            }
            "/memory" => self.notice = "Use /memory search <query>. Bind a vault first with xcb memory configure.".into(),
            "/backlog" if action == "complete" => {
                let (id, summary) = tail.split_once(' ').unwrap_or((tail, ""));
                if !summary.trim().is_empty() && let Some(task) = self.view.backlog.iter().find(|task| task.id.as_str() == id) {
                    self.send_habitat(output, Intent::Habitat(HabitatCommand::CompleteBacklog { id: task.id.clone(), expected_revision: task.revision, summary: summary.trim().into() }), command, arguments);
                } else { self.notice = "Use /backlog complete <deferred-task-id> <work summary>.".into(); }
            }
            "/backlog" if action == "reconcile" => {
                if let Some(task) = self.view.backlog.iter().find(|task| task.id.as_str() == tail) {
                    self.send_habitat(output, Intent::Habitat(HabitatCommand::ReconcileTask { id: task.id.clone(), expected_revision: task.revision }), command, arguments);
                } else { self.notice = "Use /backlog reconcile <task-id>; retained run evidence must prove the outcome.".into(); }
            }
            "/attention" | "/backlog" if command == "/attention" || arguments.is_empty() || arguments == "all" => {
                let attention = command == "/attention";
                let all = attention || arguments == "all";
                let items = self.view.backlog.iter()
                    .filter(|task| all || self.view.conversation.as_ref() == Some(&task.conversation))
                    .filter(|task| !attention || task.state.attention())
                    .map(|task| PickItem {
                        label: format!("{} · {} · P{} · {} · {}", xcb_core::display_text(&task.title, 52),
                            if attention { task.state.label() } else { &task.status }, task.priority,
                            xcb_core::display_text(task.conversation.as_str(), 12), xcb_core::display_text(task.id.as_str(), 12)),
                        action: PickAction::Backlog(task.id.clone()),
                    }).collect();
                self.picker(if attention { "Attention · questions / approvals / actions" } else if all { "All agents · backlog and history" } else { "This agent · backlog and history" }, items);
            }
            "/backlog" if action == "add" && !tail.is_empty() => {
                let Some(conversation) = self.view.conversation.clone() else {
                    self.composer.set_text(&format!("{command} {arguments}"));
                    self.notice = "Open a conversation before adding work.".into(); return;
                };
                let id = self.inbox_event_id(command, arguments);
                self.send_habitat(output, Intent::Habitat(HabitatCommand::EnqueueIn { conversation, id, prompt: tail.into(), deferred: true, priority: 5 }), command, arguments);
            }
            "/backlog" if action == "run" => {
                if let Some(task) = self.view.backlog.iter().find(|task| task.id.as_str() == tail) {
                    self.send_habitat(output, Intent::Habitat(HabitatCommand::Release { id: task.id.clone(), expected_revision: task.revision }), command, arguments);
                } else { self.notice = "Task not in the current backlog view. /backlog all lists task ids; /reload refreshes it.".into(); }
            }
            "/backlog" if action == "edit" => {
                let (id, prompt) = tail.split_once(' ').unwrap_or((tail, ""));
                if let Some(task) = self.view.backlog.iter().find(|task| task.id.as_str() == id) {
                    if prompt.trim().is_empty() {
                        self.composer.set_text(&format!("/backlog edit {} {}", task.id, task.prompt));
                    } else {
                        self.send_habitat(output, Intent::Habitat(HabitatCommand::Edit { id: task.id.clone(), expected_revision: task.revision, prompt: prompt.trim().into(), priority: task.priority }), command, arguments);
                    }
                } else { self.notice = "Task not in the current backlog view. /backlog all lists task ids.".into(); }
            }
            "/reply" if !tail.is_empty() => {
                if let Some(task) = self.view.backlog.iter().find(|task| task.id.as_str() == action).cloned() {
                    let reply = self.inbox_event_id(command, &format!("{}:{arguments}", task.revision));
                    self.send_habitat(output, Intent::Habitat(HabitatCommand::Reply { id: task.id, expected_revision: task.revision, reply, text: tail.into() }), command, arguments);
                } else { self.notice = "Task not in the current backlog view. /attention lists tasks needing you.".into(); }
            }
            "/program" if arguments.is_empty() => {
                self.picker("Recent managed programs", program_items(&self.view));
            }
            "/program" => {
                if let Some(program) = self.view.programs.iter().find(|row| row.parent.as_str() == arguments) {
                    self.modal = Some(inspect_program(program));
                    self.program_inspect = Some(program.parent.clone());
                } else {
                    self.notice = "Program is outside the current view. Use xcb backlog program-status <id> --json. Create a pinned program with xcb backlog program or xcb schedules program.".into();
                }
            }
            "/schedule" if arguments.is_empty() || arguments == "all" => {
                let all = arguments == "all";
                self.picker(if all { "All agents · schedules" } else { "This agent · schedules" },
                    self.view.schedules.iter()
                        .filter(|schedule| all || self.view.conversation.as_ref() == Some(&schedule.conversation))
                        .map(|schedule| PickItem {
                            label: format!("{} · {} · every {}s · {} · {}", xcb_core::display_text(&schedule.prompt, 52),
                                if schedule.enabled { "enabled" } else { "paused" }, schedule.interval_ms / 1000,
                                xcb_core::display_text(schedule.conversation.as_str(), 12), xcb_core::display_text(schedule.id.as_str(), 16)),
                            action: PickAction::Schedule(schedule.id.clone()),
                        }).collect());
            }
            "/schedule" if action == "every" => {
                let (seconds, prompt) = tail.split_once(' ').unwrap_or((tail, ""));
                if let Ok(seconds) = seconds.parse::<u64>()
                    && (60..=31_536_000).contains(&seconds) && !prompt.trim().is_empty() {
                    self.send_habitat(output, Intent::Habitat(HabitatCommand::Schedule { prompt: prompt.trim().into(), interval_ms: seconds * 1000 }), command, arguments);
                } else { self.notice = "Use /schedule every <seconds> <prompt>; interval must be 60 seconds to 365 days.".into(); }
            }
            "/schedule" if matches!(action, "pause" | "resume") => {
                if let Some(schedule) = self.view.schedules.iter().find(|schedule| schedule.id.as_str() == tail) {
                    self.send_habitat(output, Intent::Habitat(HabitatCommand::ScheduleEnabled { id: schedule.id.clone(), expected_revision: schedule.revision, enabled: action == "resume" }), command, arguments);
                } else { self.notice = "Schedule not in the current view. /schedule all lists schedule ids.".into(); }
            }
            "/schedule" => self.notice = "Use /schedule [all], /schedule every <seconds> <prompt>, or /schedule pause|resume <id>.".into(),
            "/reply" => self.notice = "Use /reply <task-id> <answer>. Approvals still use the existing permission gate.".into(),
            _ => self.notice = "Use /backlog [all], /backlog add <prompt>, /backlog edit <id> [prompt], or /backlog run <id>.".into(),
        }
    }

    fn slash(&mut self, input: &str, output: &SyncSender<Intent>) -> bool {
        let (command, arguments) = input.split_once(' ').unwrap_or((input, ""));
        // Single-letter aliases resolve to the full command before dispatch.
        let command = SLASH_COMMANDS
            .iter()
            .find(|entry| entry.alias == command)
            .map_or(command, |entry| entry.name);
        let command = match command {
            "/resume" => "/sessions",
            "/agents" => "/tasks",
            other => other,
        };
        let arguments = arguments.trim();
        if self.interaction_command(command, arguments, output) {
            return true;
        }
        if matches!(
            command,
            "/backlog"
                | "/attention"
                | "/schedule"
                | "/reply"
                | "/project"
                | "/program"
                | "/memory"
                | "/steer"
                | "/watch"
                | "/inbox"
        ) {
            if self.managed_mode() {
                self.habitat_command(command, arguments, output);
                self.record_live_picker(command, arguments);
            } else {
                self.notice =
                    "Open xcb chat to manage a persistent conversation's backlog and schedules."
                        .into();
            }
            return true;
        }
        match command {
            "/help" => self.open_help(false),
            "/overview" => self.overview_command(arguments),
            "/mouse" => {
                self.mouse_capture = !self.mouse_capture;
                self.mouse_toggled = true;
                self.notice = if self.mouse_capture {
                    "Mouse capture on: scroll the panel under the pointer; click an agent to add a reference. Hold Shift (Option on macOS) to select text."
                } else {
                    "Mouse capture off: terminal text selection works; PageUp/PageDown scroll the transcript."
                }
                .into();
            }
            "/quit" | "/exit" => {
                self.send(output, Intent::Quit);
                return false;
            }
            "/new" => {
                self.composer_target = None;
                self.send(output, Intent::NewSession);
            }
            "/default" => self.send(output, Intent::SetDefault),
            "/model" | "/models" if arguments.is_empty() => self.picker(
                "Models · fixed, Adaptive, and Fusion",
                self.view
                    .models
                    .iter()
                    // A bound session only offers its own provider's catalog;
                    // without one every observed provider is listed.
                    .filter(|choice| {
                        self.view
                            .session
                            .as_ref()
                            .is_none_or(|session| choice.provider == session.model.provider)
                    })
                    .map(|choice| PickItem {
                        label: format!(
                            "{} · {}{} · {:?}",
                            choice.provider,
                            choice.label,
                            choice
                                .resolved
                                .as_ref()
                                .map(|resolved| format!(" → {resolved}"))
                                .unwrap_or_default(),
                            choice.mode
                        ),
                        action: PickAction::Model(choice.key()),
                    })
                    .collect(),
            ),
            "/model" => self.send(output, Intent::Model(arguments.into())),
            "/accounts" => self.picker(
                "Accounts · select an account",
                self.view
                    .accounts
                    .iter()
                    .map(|account| PickItem {
                        label: format!(
                            "{} · {} · {} · {}{}{}{}",
                            account.name,
                            account.provider,
                            account.subscription,
                            account
                                .quota_block_label(display_now_ms())
                                .unwrap_or_else(|| account
                                    .remaining_percent
                                    .map(|percent| format!("{percent:.0}% left"))
                                    .unwrap_or_else(|| "usage unmeasured".into())),
                            if account.busy { " · busy" } else { "" },
                            if account.enabled { "" } else { " · disabled" },
                            if account.authentication_required {
                                " · reconnect required"
                            } else {
                                ""
                            }
                        ),
                        action: PickAction::Account(account.id.clone()),
                    })
                    .collect(),
            ),
            "/tasks" => self.picker(
                "Managed tasks",
                self.view
                    .tasks
                    .iter()
                    .map(|task| PickItem {
                        label: format!(
                            "{} · {} · {} · {}{}",
                            task.id,
                            task.title,
                            task_status(task),
                            task.detail,
                            task.route
                                .as_deref()
                                .map(|route| format!(" · {route}"))
                                .unwrap_or_default()
                        ),
                        action: PickAction::Task(task.id.clone()),
                    })
                    .collect(),
            ),
            "/sessions" if self.managed_mode() => {
                let mut items = vec![PickItem {
                    label: "＋ new conversation".into(),
                    action: PickAction::NewConversation,
                }];
                items.extend(self.view.conversations.iter().map(|conversation| PickItem {
                    label: format!(
                        "{} · {} msgs · {} · {}",
                        conversation.title,
                        conversation.messages,
                        age_label(display_now_ms().saturating_sub(conversation.updated_at_ms)),
                        conversation.workspace
                    ),
                    action: PickAction::Conversation(conversation.id.clone()),
                }));
                self.picker("Control conversations", items)
            }
            "/sessions" => self.picker(
                "Direct provider sessions",
                self.view
                    .sessions
                    .iter()
                    .map(|session| PickItem {
                        label: format!(
                            "{} · {} · {}",
                            session.title,
                            session.model.label,
                            session.state.label()
                        ),
                        action: PickAction::Session(session.id.clone()),
                    })
                    .collect(),
            ),
            "/pane" if arguments.is_empty() => {
                let mut items: Vec<_> = self
                    .view
                    .panes
                    .iter()
                    .map(|pane| PickItem {
                        label: format!("{} · {}", pane.id, pane.title),
                        action: PickAction::Pane(pane.id.clone()),
                    })
                    .collect();
                items.push(PickItem {
                    label: "Edit this pane".into(),
                    action: PickAction::EditPane,
                });
                items.push(PickItem {
                    label: "Generate a pane…".into(),
                    action: PickAction::Text("/pane generate ".into()),
                });
                self.picker("Panes", items);
            }
            "/pane" if arguments == "edit" => {
                self.edit_pane(&self.view.pane.clone(), self.view.pane_revision.clone())
            }
            "/pane" if arguments.starts_with("generate ") => {
                self.send(output, Intent::GeneratePane(arguments[9..].into()))
            }
            "/pane" => match Id::new(arguments) {
                Ok(id) => self.send(output, Intent::Pane(id)),
                Err(_) => {
                    self.notice = "Use /pane, /pane edit, or /pane generate <description>".into()
                }
            },
            "/attach" if !arguments.is_empty() => {
                self.request_attachment(
                    output,
                    Intent::AttachPath(arguments.trim_matches('"').trim_matches('\'').into()),
                );
            }
            "/plugin" => {
                let pieces: Vec<_> = arguments.split_whitespace().collect();
                if pieces.len() == 2 && ["on", "off"].contains(&pieces[1]) {
                    self.send(
                        output,
                        Intent::Extension {
                            name: pieces[0].into(),
                            enabled: pieces[1] == "on",
                        },
                    );
                } else {
                    self.notice = "/plugin auto-continue|gobstopper|usage|hooks on|off".into();
                }
            }
            "/reload" => self.send(output, Intent::Refresh),
            _ => {
                self.notice =
                    "Unknown command. /help lists commands; no command text was sent to the model."
                        .into()
            }
        }
        self.record_live_picker(command, arguments);
        true
    }
    pub fn handle(&mut self, event: Event, output: &SyncSender<Intent>) -> bool {
        // A resize can arrive while a dialog owns input; invalidate hidden
        // grid coordinates before that dialog consumes the event.
        if matches!(event, Event::Resize(_, _)) {
            agent_grid::clear_geometry(self);
        }
        // Only inputs that can change the view schedule a repaint: painting a
        // frame per pointer-motion or focus event is pure churn.
        let repaints = match &event {
            Event::Key(key) => key.kind != KeyEventKind::Release,
            Event::Mouse(mouse) => {
                !matches!(mouse.kind, MouseEventKind::Moved | MouseEventKind::Drag(_))
            }
            Event::FocusGained | Event::FocusLost => false,
            _ => true,
        };
        if repaints {
            self.dirty = true;
        }
        // A key press or paste acknowledges whatever notice was showing; the
        // handler below sets a fresh one when it has something to say.
        if matches!(&event, Event::Key(key) if key.kind != KeyEventKind::Release)
            || matches!(&event, Event::Paste(_))
        {
            self.notice.clear();
            self.track_notice();
        }
        if self.modal.is_some() {
            return self.modal_event(event, output);
        }
        if self.overview_event(&event) {
            return true;
        }
        if let Event::Key(key) = &event {
            if key.kind == KeyEventKind::Release {
                return true;
            }
            // An open slash-command menu owns navigation and completion; global
            // toggles and Ctrl-C cancel stay reachable.
            if let Some((matches, selected)) = self.slash_menu() {
                if matches.is_empty() {
                    if key.code == KeyCode::Esc {
                        self.slash_dismissed.set(true);
                        return true;
                    }
                    if matches!(key.code, KeyCode::Up | KeyCode::Down | KeyCode::Tab) {
                        return true;
                    }
                } else {
                    match key.code {
                        KeyCode::Up
                            if !key
                                .modifiers
                                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                        {
                            self.slash_selected.set(if selected == 0 {
                                matches.len() - 1
                            } else {
                                selected - 1
                            });
                            return true;
                        }
                        KeyCode::Down
                            if !key
                                .modifiers
                                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                        {
                            self.slash_selected.set((selected + 1) % matches.len());
                            return true;
                        }
                        KeyCode::Char('p') | KeyCode::Char('n')
                            if key.modifiers.contains(KeyModifiers::CONTROL) =>
                        {
                            self.slash_selected.set(if key.code == KeyCode::Char('p') {
                                if selected == 0 {
                                    matches.len() - 1
                                } else {
                                    selected - 1
                                }
                            } else {
                                (selected + 1) % matches.len()
                            });
                            return true;
                        }
                        KeyCode::Tab => {
                            let mut text = matches[selected].name.to_owned();
                            text.push(' ');
                            self.composer.set_text(&text);
                            return true;
                        }
                        KeyCode::Enter => {
                            let command = matches[selected];
                            if command.needs_args {
                                self.composer.set_text(&format!("{} ", command.name));
                                return true;
                            }
                            // Route through the composer's own submit path so the
                            // command lands in prompt history like a typed line.
                            self.composer.set_text(command.name);
                            if let ComposerAction::Submit(text) = self.composer.handle(Event::Key(
                                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                            )) {
                                return self.slash(&text, output);
                            }
                            return true;
                        }
                        KeyCode::Esc => {
                            self.slash_dismissed.set(true);
                            return true;
                        }
                        _ => (),
                    }
                }
            }
            if let Some(keep_running) = self.interaction_key(*key, output) {
                return keep_running;
            }
            if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                // Clear a draft before interrupting work; quit only
                // after the composer is empty and no work can stop.
                if !self.composer.text().is_empty() {
                    self.composer.clear_to_history();
                    self.notice =
                        "Draft cleared (Ctrl-R restores). Press Ctrl-C again to quit.".into();
                } else if self.can_cancel_work() {
                    self.request_cancel(output);
                } else {
                    self.send(output, Intent::Quit);
                    return false;
                }
                return true;
            }
            match key.code {
                KeyCode::Char('?')
                    if self.composer.text().is_empty()
                        && !key
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    self.open_help(true);
                    return true;
                }
                KeyCode::F(1) => {
                    self.open_help(false);
                    return true;
                }
                KeyCode::PageUp => {
                    self.scroll_transcript(-i32::from(
                        self.viewport_height.get().max(10).saturating_sub(3),
                    ));
                    return true;
                }
                KeyCode::PageDown => {
                    self.scroll_transcript(i32::from(
                        self.viewport_height.get().max(10).saturating_sub(3),
                    ));
                    return true;
                }
                // Plain End edits a draft (cursor to end of line); with a
                // modifier, or when there is nothing to edit, it jumps the
                // transcript back to the newest output.
                KeyCode::End
                    if key
                        .modifiers
                        .intersects(KeyModifiers::SHIFT | KeyModifiers::CONTROL)
                        || self.composer.text().is_empty() =>
                {
                    self.paused.set(false);
                    self.scroll.set(0);
                    return true;
                }
                KeyCode::Tab if self.composer.text().starts_with('/') => {
                    // The menu is open iff matches exist and it is not
                    // dismissed; a dismissed menu leaves Tab a no-op.
                    return true;
                }
                _ => (),
            }
        }
        if let Event::Mouse(mouse) = &event {
            // The grid consumes wheel events inside its rectangle above.
            // Remaining wheel events scroll the transcript, never the composer.
            // Mouse events only arrive while `/mouse` capture is on; off, the
            // terminal keeps selection and turns the wheel into arrow keys.
            match mouse.kind {
                MouseEventKind::ScrollUp => self.scroll_transcript(-3),
                MouseEventKind::ScrollDown => self.scroll_transcript(3),
                _ => (),
            }
            return true;
        }
        if self.pending_image && matches!(&event, Event::Key(key) if key.code == KeyCode::Enter) {
            self.notice = "Waiting for the image to finish loading; your draft is retained.".into();
            return true;
        }
        match self.composer.handle(event) {
            ComposerAction::Submit(text) => {
                if text.starts_with('/') {
                    return self.slash(&text, output);
                }
                if self.composer_target.is_some() && self.managed_mode() {
                    self.targeted_submit(text, false, output);
                    return true;
                }
                if !text.trim().is_empty() || !self.attachments.is_empty() {
                    if self.initial_view_pending {
                        self.composer.set_text(&text);
                        self.notice = "Opening your conversation; draft retained.".into();
                        return true;
                    }
                    if self.pending_echoes.len() >= 8 || !self.recovery_capacity_available() {
                        self.composer.set_text(&text);
                        self.notice = "Waiting for earlier submissions; draft retained.".into();
                        return true;
                    }
                    let attachments = std::mem::take(&mut self.attachments);
                    let id = Id::new(format!("m_{}", uuid::Uuid::new_v4().simple()))
                        .expect("generated message id");
                    let echo = PendingEcho {
                        id: id.clone(),
                        session: view_context(&self.view),
                        attachments: attachments.len(),
                        recovery_attachments: attachments.clone(),
                        text: text.clone(),
                    };
                    self.pending_echoes.push_back(echo);
                    if !self.flush_recovery(true) {
                        self.pending_echoes.pop_back();
                        self.composer.set_text(&text);
                        self.attachments = attachments;
                        return true;
                    }
                    let context = self
                        .view
                        .conversation
                        .clone()
                        .map(TranscriptContext::Conversation)
                        .or_else(|| {
                            self.view
                                .session
                                .as_ref()
                                .map(|session| TranscriptContext::Session(session.id.clone()))
                        });
                    let intent = match context {
                        Some(context) => Intent::SubmitTo {
                            context,
                            id,
                            text,
                            attachments,
                        },
                        None => Intent::Submit {
                            id,
                            text,
                            attachments,
                        },
                    };
                    match output.try_send(intent) {
                        Ok(()) => {
                            self.notice.clear();
                        }
                        Err(
                            std::sync::mpsc::TrySendError::Full(Intent::SubmitTo {
                                text,
                                attachments,
                                ..
                            })
                            | std::sync::mpsc::TrySendError::Disconnected(Intent::SubmitTo {
                                text,
                                attachments,
                                ..
                            })
                            | std::sync::mpsc::TrySendError::Full(Intent::Submit {
                                text,
                                attachments,
                                ..
                            })
                            | std::sync::mpsc::TrySendError::Disconnected(Intent::Submit {
                                text,
                                attachments,
                                ..
                            }),
                        ) => {
                            self.pending_echoes.pop_back();
                            self.composer.set_text(&text);
                            self.attachments = attachments;
                            self.flush_recovery(true);
                            self.notice = "Command queue unavailable; draft retained.".into();
                        }
                        Err(_) => (),
                    }
                }
            }
            ComposerAction::Cancel => {
                // Esc interrupts a live turn; idle it is a quiet no-op.
                if self.paused.get() {
                    self.paused.set(false);
                    self.scroll.set(0);
                } else if self.can_cancel_work() {
                    self.request_cancel(output);
                }
            }
            ComposerAction::Quit => {
                self.send(output, Intent::Quit);
                return false;
            }
            ComposerAction::Clipboard => self.clipboard(output),
            ComposerAction::History => self.open_history_search(),
            ComposerAction::Editor => self.external_editor_requested = true,
            ComposerAction::Rejected(reason) => self.notice = reason.into(),
            ComposerAction::None => (),
        }
        true
    }
    fn clipboard(&mut self, output: &SyncSender<Intent>) {
        let Ok(mut clipboard) = arboard::Clipboard::new() else {
            self.notice =
                "Clipboard unavailable. Paste text normally or use /attach <path>.".into();
            return;
        };
        if let Ok(image) = clipboard.get_image() {
            if self.attachments.len() >= 8
                || image
                    .width
                    .checked_mul(image.height)
                    .is_none_or(|pixels| pixels > 16_000_000)
            {
                self.notice = "Image limit reached (8 images, 16 megapixels each).".into();
                return;
            }
            self.request_attachment(
                output,
                Intent::AttachRgba {
                    width: image.width,
                    height: image.height,
                    bytes: image.bytes.into_owned(),
                },
            );
        } else if let Ok(text) = clipboard.get_text() {
            if let ComposerAction::Rejected(reason) = self.composer.handle(Event::Paste(text)) {
                self.notice = reason.into();
            }
        } else {
            self.notice = "No supported text or image on the clipboard.".into();
        }
    }
    fn modal_event(&mut self, event: Event, output: &SyncSender<Intent>) -> bool {
        if self.recovery_event(&event) {
            return true;
        }
        if self.history_search_event(&event) || self.transcript_event(&event, output) {
            return true;
        }
        if self.inspector_action(&event, output) {
            return true;
        }
        // Ctrl-C inside a dialog keeps the composer's ordering: cancel a live
        // run first; idle, it closes the dialog and warns, so a second press
        // is what quits. Esc still only closes the dialog.
        if let Event::Key(key) = &event
            && key.kind != KeyEventKind::Release
            && key.code == KeyCode::Char('c')
            && key.modifiers.contains(KeyModifiers::CONTROL)
        {
            // A local dialog consumes Ctrl-C before any worker action.
            // Prompt-editor text is newer than the composer draft it was
            // opened from; it returns to the composer instead of vanishing.
            if let Some(Modal::Editor {
                textarea,
                kind: EditorKind::Prompt,
                ..
            }) = self.modal.take()
            {
                let text = textarea.lines().join("\n");
                if !text.is_empty() {
                    self.composer.set_text(&text);
                }
            }
            self.notice = if self.composer.text().is_empty() {
                "Dialog closed. Press Ctrl-C again to quit."
            } else {
                "Dialog closed; draft kept. Ctrl-C again clears it (Ctrl-R restores)."
            }
            .into();
            return true;
        }
        if let (Some(Modal::Help { .. }), Event::Key(key)) = (&self.modal, &event)
            && key.kind != KeyEventKind::Release
            && key.code == KeyCode::Char('?')
        {
            self.modal = None;
            // `?` opened help from an empty composer, so a second `?` means
            // the character itself was wanted.
            if std::mem::take(&mut self.help_via_question) && self.composer.text().is_empty() {
                self.composer.handle(Event::Paste("?".into()));
            }
            return true;
        }
        if matches!(
            &event,
            Event::Key(key) if key.kind != KeyEventKind::Release && key.code == KeyCode::Esc
        ) {
            self.modal = None;
            self.help_via_question = false;
            return true;
        }
        let mut chosen = None;
        let mut save = None;
        if let Some(modal) = &mut self.modal {
            match modal {
                Modal::Picker {
                    query,
                    items,
                    selected,
                    ..
                } => {
                    if let Event::Paste(text) = &event {
                        let text = xcb_core::display_text(text, 1024).replace(['\r', '\n'], " ");
                        if query.len() + text.len() <= 1024 {
                            query.push_str(&text);
                            *selected = 0;
                        }
                        return true;
                    }
                    let filtered = items
                        .iter()
                        .filter(|item| interaction::item_matches(item, query))
                        .count();
                    if let Event::Mouse(mouse) = event {
                        match mouse.kind {
                            MouseEventKind::ScrollUp => *selected = selected.saturating_sub(3),
                            MouseEventKind::ScrollDown => {
                                *selected = (*selected + 3).min(filtered.saturating_sub(1))
                            }
                            _ => (),
                        }
                        return true;
                    }
                    if let Event::Key(key) = event {
                        if key.kind == KeyEventKind::Release {
                            return true;
                        }
                        match key.code {
                            KeyCode::Up | KeyCode::Char('p')
                                if !key.modifiers.contains(KeyModifiers::ALT)
                                    && (key.code == KeyCode::Up
                                        || key.modifiers.contains(KeyModifiers::CONTROL)) =>
                            {
                                *selected = if *selected == 0 {
                                    filtered.saturating_sub(1)
                                } else {
                                    *selected - 1
                                };
                            }
                            KeyCode::Down | KeyCode::Char('n')
                                if !key.modifiers.contains(KeyModifiers::ALT)
                                    && (key.code == KeyCode::Down
                                        || key.modifiers.contains(KeyModifiers::CONTROL)) =>
                            {
                                if filtered > 0 {
                                    *selected = (*selected + 1) % filtered;
                                }
                            }
                            KeyCode::PageUp => {
                                *selected = selected.saturating_sub(10);
                            }
                            KeyCode::PageDown => {
                                *selected = (*selected + 10).min(filtered.saturating_sub(1));
                            }
                            KeyCode::Home => *selected = 0,
                            KeyCode::End => *selected = filtered.saturating_sub(1),
                            KeyCode::Backspace => {
                                query.pop();
                                *selected = 0;
                            }
                            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                query.clear();
                                *selected = 0;
                            }
                            KeyCode::Char(ch)
                                if !key
                                    .modifiers
                                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                                    && query.len() + ch.len_utf8() <= 1024 =>
                            {
                                query.push(ch);
                                *selected = 0;
                            }
                            KeyCode::Enter => {
                                chosen = items
                                    .iter()
                                    .filter(|item| interaction::item_matches(item, query))
                                    .nth(*selected)
                                    .map(|item| item.action.clone())
                            }
                            _ => (),
                        }
                    }
                }
                Modal::Help { scroll } => match &event {
                    Event::Key(key) if key.kind != KeyEventKind::Release => match key.code {
                        KeyCode::Up => *scroll = scroll.saturating_sub(1),
                        KeyCode::Down => *scroll = scroll.saturating_add(1),
                        KeyCode::PageUp => *scroll = scroll.saturating_sub(10),
                        KeyCode::PageDown | KeyCode::Char(' ') => {
                            *scroll = scroll.saturating_add(10)
                        }
                        KeyCode::Home => *scroll = 0,
                        KeyCode::End => *scroll = u16::MAX,
                        _ => (),
                    },
                    Event::Mouse(mouse) => match mouse.kind {
                        MouseEventKind::ScrollUp => *scroll = scroll.saturating_sub(3),
                        MouseEventKind::ScrollDown => *scroll = scroll.saturating_add(3),
                        _ => (),
                    },
                    _ => (),
                },
                Modal::HistorySearch { .. } | Modal::Transcript { .. } => {}
                Modal::Inspect { scroll, .. } => {
                    // Read-only dialog: navigation moves the viewport; the
                    // offset is clamped to the wrapped body height at render.
                    if let Event::Mouse(mouse) = event {
                        match mouse.kind {
                            MouseEventKind::ScrollUp => *scroll = scroll.saturating_sub(3),
                            MouseEventKind::ScrollDown => *scroll = scroll.saturating_add(3),
                            _ => (),
                        }
                        return true;
                    }
                    if let Event::Key(key) = event {
                        if key.kind == KeyEventKind::Release {
                            return true;
                        }
                        match key.code {
                            KeyCode::Up => *scroll = scroll.saturating_sub(1),
                            KeyCode::Down => *scroll = scroll.saturating_add(1),
                            KeyCode::Char('p') | KeyCode::Char('n')
                                if key.modifiers.contains(KeyModifiers::CONTROL) =>
                            {
                                *scroll = if key.code == KeyCode::Char('p') {
                                    scroll.saturating_sub(1)
                                } else {
                                    scroll.saturating_add(1)
                                };
                            }
                            KeyCode::PageUp => *scroll = scroll.saturating_sub(10),
                            KeyCode::PageDown => *scroll = scroll.saturating_add(10),
                            KeyCode::Home => *scroll = 0,
                            KeyCode::End => *scroll = u16::MAX,
                            _ => (),
                        }
                    }
                }
                Modal::Editor {
                    textarea,
                    kind,
                    error,
                    ..
                } => {
                    if matches!(&event, Event::Key(key) if key.code == KeyCode::Char('s') && key.modifiers.contains(KeyModifiers::CONTROL))
                    {
                        let text = textarea.lines().join("\n");
                        match kind {
                            EditorKind::Prompt => chosen = Some(PickAction::Text(text)),
                            EditorKind::Pane { expected } => match Pane::parse(text.as_bytes()) {
                                Ok(pane) => save = Some((pane, expected.clone())),
                                Err(problem) => *error = Some(problem.to_string()),
                            },
                        }
                    } else {
                        match event {
                            Event::Paste(text) => {
                                let text = text.replace("\r\n", "\n");
                                if textarea.lines().iter().map(String::len).sum::<usize>()
                                    + text.len()
                                    <= EDITOR_MAX
                                {
                                    textarea.insert_str(xcb_core::display_text(&text, EDITOR_MAX));
                                } else {
                                    self.notice = EDITOR_PASTE_TOO_LARGE.into();
                                }
                            }
                            Event::Key(key)
                                if key.kind != KeyEventKind::Release
                                    && (textarea
                                        .lines()
                                        .iter()
                                        .map(String::len)
                                        .sum::<usize>()
                                        < EDITOR_MAX
                                        || !matches!(key.code, KeyCode::Char(_))) =>
                            {
                                textarea.input(key);
                            }
                            _ => (),
                        }
                    }
                }
            }
        }
        if let Some((pane, expected)) = save {
            self.modal = None;
            self.send(output, Intent::SavePane { pane, expected });
        }
        if let Some(PickAction::Account(id)) = &chosen {
            // Account state can change in another terminal while this picker
            // is open. Check the latest view before dispatching the selection.
            match self.view.accounts.iter().find(|account| &account.id == id) {
                Some(account) if account.authentication_required => {
                    self.notice = "This account needs reconnection before it can run tasks.".into();
                    return true;
                }
                Some(account) if !account.enabled => {
                    self.notice = "This account is disabled. Enable it before selecting it.".into();
                    return true;
                }
                None => {
                    self.notice =
                        "This account is no longer available. Reopen the account picker.".into();
                    return true;
                }
                _ => (),
            }
        }
        if let Some(action) = chosen {
            self.modal = None;
            self.live_picker = None;
            self.live_inspect = Some(action.clone());
            match action {
                PickAction::CancelTask { id, revision } => {
                    self.cancel_selected(id, revision, output)
                }
                PickAction::Recovery(index) => self.recover_entry(index),
                PickAction::Pane(id) => self.send(output, Intent::Pane(id)),
                PickAction::Model(id) => self.send(output, Intent::Model(id)),
                PickAction::Account(id) => self.send(output, Intent::Account(id)),
                PickAction::Conversation(id) => self.send(output, Intent::Conversation(id)),
                PickAction::NewConversation => self.send(output, Intent::NewSession),
                PickAction::Session(id) => self.send(output, Intent::Resume(id)),
                PickAction::Task(id) => {
                    if let Some(task) = self.view.tasks.iter().find(|task| task.id == id) {
                        let mut modal = inspect_task(task);
                        if let Modal::Inspect { lines, .. } = &mut modal {
                            let events = self
                                .view
                                .inbox
                                .iter()
                                .filter(|event| event.task == id)
                                .count();
                            lines.extend([
                                String::new(),
                                format!("Inbox: {events} events in the current view · /inbox {id}"),
                                format!("/steer {id} <guidance>"),
                            ]);
                        }
                        self.modal = Some(modal);
                    }
                }
                PickAction::Inbox(id) => {
                    if let Some(event) = self.view.inbox.iter().find(|event| event.id == id) {
                        self.modal = Some(inspect_inbox(event));
                        self.inbox_inspect = Some(id);
                    } else {
                        self.notice = "This event is outside the current bounded inbox view. Use xcb inbox --task <task-id> to inspect its history.".into();
                    }
                }
                PickAction::Backlog(id) => {
                    if let Some(task) = self.view.backlog.iter().find(|task| task.id == id) {
                        let mut lines = vec![
                            task.title.clone(),
                            format!(
                                "{} · {} · P{} · revision {}",
                                task.conversation, task.status, task.priority, task.revision
                            ),
                            format!("attention: {}", task.state.label()),
                            String::new(),
                            "Prompt".into(),
                            task.prompt.clone(),
                            String::new(),
                            "Latest summary".into(),
                            task.summary.clone(),
                            String::new(),
                        ];
                        lines.push(format!("/inbox {} · delivery history", task.id));
                        lines.push(format!("/steer {} <guidance>", task.id));
                        if task.deferred {
                            lines.push(format!("/backlog run {}", task.id));
                            lines.push(format!("/backlog edit {} <new prompt>", task.id));
                        }
                        if task.state == State::NeedsAnswer {
                            lines.push(format!("/reply {} <answer>", task.id));
                        } else if matches!(
                            task.state,
                            State::NeedsApproval | State::NeedsAction | State::Uncertain
                        ) {
                            lines.push("Review the gated action or recovery detail above. A reply cannot grant host or provider permission.".into());
                        }
                        self.modal = Some(Modal::Inspect {
                            title: format!("{} · {}", task.id, task.status),
                            lines,
                            scroll: 0,
                        });
                    }
                }
                PickAction::Program(id) => {
                    if let Some(program) = self.view.programs.iter().find(|row| row.parent == id) {
                        self.modal = Some(inspect_program(program));
                        self.program_inspect = Some(id);
                    }
                }
                PickAction::Project(id) => {
                    if let Some(project) = self.view.projects.iter().find(|p| p.conversation == id)
                    {
                        self.modal = Some(Modal::Inspect {
                            title: format!("Project {} · {}", project.conversation, project.status),
                            lines: vec![project.goal.clone(), String::new(),
                                format!("{} tasks remaining · revision {}", project.remaining_tasks, project.revision),
                                format!("Grant expires {}", due_label(project.expires_at_ms)),
                                format!("Required provider: {}", project.required_provider.map_or("automatic".into(), |p| p.to_string())),
                                String::new(), format!("/project {} {}", if project.enabled { "pause" } else { "resume" }, project.conversation),
                                "Pause stops automatic dispatch; running work settles. Resume does not renew the grant.".into()],
                            scroll: 0,
                        });
                    }
                }
                PickAction::Schedule(id) => {
                    if let Some(schedule) = self
                        .view
                        .schedules
                        .iter()
                        .find(|schedule| schedule.id == id)
                    {
                        self.modal = Some(Modal::Inspect {
                            title: format!("Schedule {}", schedule.id),
                            lines: vec![
                                format!("conversation {}", schedule.conversation),
                                format!(
                                    "{} · every {} seconds · revision {}",
                                    if schedule.enabled {
                                        "enabled"
                                    } else {
                                        "paused"
                                    },
                                    schedule.interval_ms / 1000,
                                    schedule.revision
                                ),
                                format!("Next wake-up {}", due_label(schedule.next_due_ms)),
                                String::new(),
                                schedule.prompt.clone(),
                                String::new(),
                                format!(
                                    "/schedule {} {}",
                                    if schedule.enabled { "pause" } else { "resume" },
                                    schedule.id
                                ),
                            ],
                            scroll: 0,
                        });
                    }
                }
                PickAction::Text(text) => self.composer.set_text(&text),
                PickAction::EditPane => {
                    self.edit_pane(&self.view.pane.clone(), self.view.pane_revision.clone())
                }
            }
            self.refresh_live_surfaces();
        }
        true
    }
}

struct Restore {
    keyboard_enhanced: bool,
}
impl Drop for Restore {
    fn drop(&mut self) {
        if self.keyboard_enhanced {
            let _ = execute!(io::stdout(), PopKeyboardEnhancementFlags);
        }
        // Disabling capture that was never enabled is harmless.
        let _ = execute!(
            io::stdout(),
            DisableBracketedPaste,
            DisableMouseCapture,
            LeaveAlternateScreen
        );
        let _ = disable_raw_mode();
    }
}

pub fn run(input: Receiver<Update>, output: SyncSender<Intent>) -> io::Result<()> {
    run_with_options(input, output, RunOptions::default())
}

#[derive(Default)]
pub struct RunOptions {
    /// Trusted application-state directory for private, per-terminal input journals.
    pub recovery_directory: Option<std::path::PathBuf>,
}

pub fn run_with_options(
    input: Receiver<Update>,
    output: SyncSender<Intent>,
    options: RunOptions,
) -> io::Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(io::Error::other(
            "xcb chat needs a terminal; use xcb run for headless work",
        ));
    }
    enable_raw_mode()?;
    // Only terminals that answer the kitty protocol query get the enhancement
    // flags pushed; elsewhere pushing them is a no-op at best and Shift-Enter
    // is indistinguishable from Enter, which the help text reflects.
    let keyboard_enhanced = crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false);
    let _restore = Restore { keyboard_enhanced };
    // Mouse capture stays off so the terminal's own drag-select and copy keep
    // working; `/mouse` enables wheel scrolling on request.
    execute!(io::stdout(), EnterAlternateScreen, EnableBracketedPaste)?;
    if keyboard_enhanced {
        execute!(
            io::stdout(),
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )?;
    }
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let mut app = App {
        keyboard_enhanced,
        initial_view_pending: true,
        ..App::default()
    };
    if let Some(directory) = options.recovery_directory {
        app.configure_recovery(directory);
    }
    let mut ticks = 0u64;
    let mut needs_draw = true;
    let mut blink = 0u64;
    loop {
        for _ in 0..128 {
            match input.try_recv() {
                Ok(update) => {
                    if !app.apply(update) {
                        app.flush_recovery(true);
                        return Ok(());
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    app.flush_recovery(true);
                    return Ok(());
                }
            }
        }
        app.flush_recovery(false);
        needs_draw |= app.take_dirty();
        // The attention blink and the working spinner/elapsed badge are the
        // only states that change with time alone.
        let phase = (ticks / 16) % 2;
        if !needs_draw && app.view.state.attention() && !app.view.reduced_motion && phase != blink {
            needs_draw = true;
        }
        let live = matches!(app.view.state, State::Working)
            || app.view.remote_active
            || app.overview_animating();
        if !needs_draw && live && ticks.is_multiple_of(4) {
            needs_draw = true;
        }
        if needs_draw {
            terminal.draw(|frame| render::draw(frame, &mut app, ticks))?;
            needs_draw = false;
            blink = phase;
        }
        if event::poll(Duration::from_millis(50))? {
            // Coalesce bursts: drain every queued event before the next draw
            // so a paste storm or mouse flood paints once, not once per event.
            let mut quit = !app.handle(event::read()?, &output);
            while !quit && event::poll(Duration::ZERO)? {
                quit = !app.handle(event::read()?, &output);
            }
            if quit {
                break;
            }
        }
        if std::mem::take(&mut app.external_editor_requested) {
            app.flush_recovery(true);
            let original = app.composer.text();
            // Leave the terminal usable for the editor, then restore every mode
            // before surfacing either edited text or a failure.
            if keyboard_enhanced {
                execute!(io::stdout(), PopKeyboardEnhancementFlags)?;
            }
            execute!(
                io::stdout(),
                DisableBracketedPaste,
                DisableMouseCapture,
                LeaveAlternateScreen
            )?;
            disable_raw_mode()?;
            let edited = external_editor::edit(&original);
            enable_raw_mode()?;
            execute!(io::stdout(), EnterAlternateScreen, EnableBracketedPaste)?;
            if keyboard_enhanced {
                execute!(
                    io::stdout(),
                    PushKeyboardEnhancementFlags(
                        KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                    )
                )?;
            }
            if app.mouse_capture {
                execute!(io::stdout(), EnableMouseCapture)?;
            }
            match edited {
                Ok(text) => {
                    app.composer.set_text(&text);
                    app.notice = "Editor draft loaded. Enter sends when ready.".into();
                }
                Err(error) => app.notice = error.to_string(),
            }
            terminal.clear()?;
            needs_draw = true;
            app.flush_recovery(true);
        }
        match app.take_mouse_toggle() {
            Some(true) => execute!(io::stdout(), EnableMouseCapture)?,
            Some(false) => execute!(io::stdout(), DisableMouseCapture)?,
            None => (),
        }
        ticks = ticks.wrapping_add(1);
        // Publishing is kernel-driven: serve() pushes a full view on its own
        // cadence and picks up config.json writes itself, so no TUI-side
        // refresh timer duplicates publishes. Ctrl-L and /reload still send
        // an explicit Intent::Refresh via /reload.
    }
    app.flush_recovery(true);
    Ok(())
}

#[cfg(test)]
mod habitat_surface_tests {
    use super::*;
    use std::sync::mpsc::sync_channel;
    use xcb_core::ui::BacklogRow;

    #[test]
    fn pending_input_is_journaled_before_dispatch_and_failed_journaling_blocks_send() {
        use std::os::unix::fs::PermissionsExt;
        let root =
            std::env::temp_dir().join(format!("xcb-ui-before-send-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let (tx, rx) = sync_channel(4);
        let mut app = app();
        app.configure_recovery(root.join("input"));
        app.composer.set_text("Keep this input until acknowledged");
        app.handle(
            Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            &tx,
        );
        let Intent::SubmitTo { id, .. } = rx.try_recv().unwrap() else {
            panic!("bound new work")
        };
        let snapshot: serde_json::Value =
            serde_json::from_slice(&std::fs::read(app.recovery.as_ref().unwrap().path()).unwrap())
                .unwrap();
        assert_eq!(snapshot["text"], "");
        assert_eq!(snapshot["other_inputs"][0]["operation"], id.as_str());
        assert_eq!(snapshot["other_inputs"][0]["uncertain_pending"], true);
        // Lose access to the task-owned journal directory. The next request must
        // remain local because its pending identity cannot be saved first.
        std::fs::rename(root.join("input"), root.join("moved")).unwrap();
        app.composer.set_text("Must not dispatch");
        app.handle(
            Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            &tx,
        );
        assert!(rx.try_recv().is_err());
        assert_eq!(app.composer.text(), "Must not dispatch");
        drop(app);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn first_view_must_arrive_before_live_terminal_submission() {
        let (tx, rx) = sync_channel(4);
        let mut app = App {
            initial_view_pending: true,
            ..App::default()
        };
        app.open_recovery();
        assert!(app.modal.is_none());
        app.composer.set_text("early input");
        app.handle(
            Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            &tx,
        );
        assert!(rx.try_recv().is_err());
        assert_eq!(app.composer.text(), "early input");
        app.apply(Update::View(Box::default()));
        app.handle(
            Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            &tx,
        );
        assert!(matches!(rx.try_recv(), Ok(Intent::Submit { .. })));
    }

    fn app() -> App {
        let conversation = Id::new("project_a").unwrap();
        let mut app = App::default();
        app.view.conversation = Some(conversation.clone());
        app.view
            .extensions
            .push(("algal supervisor".into(), "enabled".into()));
        app.view.backlog = vec![
            BacklogRow {
                id: Id::new("task_a").unwrap(),
                conversation,
                title: "Queued review".into(),
                prompt: "Review the changes".into(),
                summary: "Prior evidence".into(),
                status: "backlog".into(),
                state: State::Idle,
                deferred: true,
                priority: 7,
                revision: 23,
                updated_at_ms: 0,
            },
            BacklogRow {
                id: Id::new("task_b").unwrap(),
                conversation: Id::new("project_b").unwrap(),
                title: "Publish".into(),
                prompt: "Publish changes".into(),
                summary: "Review deployment approval".into(),
                status: "needs input".into(),
                state: State::NeedsApproval,
                deferred: false,
                priority: 5,
                revision: 11,
                updated_at_ms: 0,
            },
        ];
        app
    }

    fn inbox_event(id: &str, task: &str, conversation: &str) -> xcb_core::ui::InboxRow {
        xcb_core::ui::InboxRow {
            id: Id::new(id).unwrap(),
            task: Id::new(task).unwrap(),
            conversation: Id::new(conversation).unwrap(),
            sequence: 7,
            kind: "steering".into(),
            text: "Preserve the existing settings\nThen check the parser".into(),
            status: "queued".into(),
            created_at_ms: 1,
            updated_at_ms: 2,
            receipt: None,
        }
    }

    #[test]
    fn program_inspector_tracks_child_attention_without_answering_it() {
        let (tx, rx) = sync_channel(8);
        let mut app = app();
        app.view.programs.push(xcb_core::ui::ProgramRow {
            parent: Id::new("program_a").unwrap(),
            phase: "waiting".into(),
            calls: 1,
            max_calls: 2,
            child: Some(Id::new("child_a").unwrap()),
            child_status: Some("needs approval".into()),
            receipt: Some("sha256:checkpoint".into()),
        });
        app.slash("/program", &tx);
        assert!(
            matches!(&app.modal, Some(Modal::Picker { items, .. }) if items.len() == 1 && items[0].label.contains("needs approval"))
        );
        app.slash("/program program_a", &tx);
        assert!(
            matches!(&app.modal, Some(Modal::Inspect { lines, .. }) if lines.iter().any(|line| line.contains("child_a")) && lines.iter().any(|line| line.contains("/attention")))
        );
        assert!(rx.try_recv().is_err());
        let before = fingerprint_at(&app.view, 0);
        let mut next = app.view.clone();
        next.programs[0].child_status = Some("completed".into());
        next.programs[0].phase = "resuming".into();
        assert_ne!(before, fingerprint_at(&next, 0));
        app.apply(Update::View(Box::new(next)));
        assert!(
            matches!(&app.modal, Some(Modal::Inspect { lines, .. }) if lines.iter().any(|line| line.contains("resuming")) && !lines.iter().any(|line| line.contains("needs approval")))
        );
        let mut next = app.view.clone();
        next.programs.clear();
        app.apply(Update::View(Box::new(next)));
        assert!(
            matches!(&app.modal, Some(Modal::Inspect { lines, .. }) if lines[0].contains("Outside the current bounded view"))
        );
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn steering_and_watch_are_explicit_task_events_not_replies_or_new_work() {
        let (tx, rx) = sync_channel(8);
        let mut app = app();
        app.slash("/steer task_not_in_bounded_view retain settings", &tx);
        let first = match rx.try_recv().unwrap() {
            Intent::Habitat(HabitatCommand::Steer { task, event, text }) => {
                assert_eq!(task.as_str(), "task_not_in_bounded_view");
                assert_eq!(text, "retain settings");
                event
            }
            _ => panic!("explicit steering intent"),
        };
        app.slash("/watch task_a task_b", &tx);
        match rx.try_recv().unwrap() {
            Intent::Habitat(HabitatCommand::WatchTask {
                task,
                source,
                event,
            }) => {
                assert_eq!(task.as_str(), "task_a");
                assert_eq!(source.as_str(), "task_b");
                assert_ne!(event, first);
            }
            _ => panic!("explicit watch intent"),
        }
        for invalid in [
            "/steer",
            "/steer task_a",
            "/steer invalid/id text",
            "/watch task_a",
            "/watch task_a task_b extra",
        ] {
            app.slash(invalid, &tx);
            assert!(rx.try_recv().is_err(), "{invalid}");
        }
    }

    #[test]
    fn full_command_queue_retains_inbox_draft_and_identity_until_sent() {
        let (tx, rx) = sync_channel(1);
        tx.try_send(Intent::Refresh).unwrap();
        let mut app = app();
        let command = "/steer task_a preserve this guidance";
        app.slash(command, &tx);
        assert_eq!(app.composer.text(), command);
        let event = app.inbox_draft_event.as_ref().unwrap().1.clone();
        app.slash(command, &tx);
        assert_eq!(app.inbox_draft_event.as_ref().unwrap().1, event);
        rx.try_recv().unwrap();
        app.slash(command, &tx);
        assert!(
            matches!(rx.try_recv(), Ok(Intent::Habitat(HabitatCommand::Steer { event: sent, .. })) if sent == event)
        );
        assert!(app.inbox_draft_event.is_none());
    }

    #[test]
    fn inbox_scopes_and_inspection_preserve_delivery_evidence() {
        let (tx, rx) = sync_channel(8);
        let mut app = app();
        app.view.inbox = vec![
            inbox_event("event_a", "task_a", "project_a"),
            inbox_event("event_b", "task_b", "project_b"),
        ];
        app.slash("/inbox", &tx);
        assert!(matches!(&app.modal, Some(Modal::Picker { items, .. }) if items.len() == 1));
        app.slash("/inbox all", &tx);
        assert!(matches!(&app.modal, Some(Modal::Picker { items, .. }) if items.len() == 2));
        app.slash("/inbox task_b", &tx);
        assert!(
            matches!(&app.modal, Some(Modal::Picker { items, .. }) if items.len() == 1 && items[0].label.contains("event_b"))
        );
        let mut view = app.view.clone();
        view.inbox[1].status = "prepared".into();
        app.apply(Update::View(Box::new(view)));
        assert!(
            matches!(&app.modal, Some(Modal::Picker { items, .. }) if items.len() == 1 && items[0].label.contains("prepared"))
        );
        app.handle(
            Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            &tx,
        );
        let Some(Modal::Inspect { lines, .. }) = &app.modal else {
            panic!("inbox inspector")
        };
        let content = lines.join("\n");
        for expected in [
            "event_b",
            "task_b",
            "project_b",
            "prepared",
            "No settled delivery receipt yet.",
            "not proof the model followed",
        ] {
            assert!(content.contains(expected), "{content}");
        }
        assert!(
            rx.try_recv().is_err(),
            "inspection cannot acknowledge or authorize work"
        );
        let mut view = app.view.clone();
        view.inbox[1].status = "settled delivery".into();
        view.inbox[1].receipt = Some("turn_123: exact batch receipt".into());
        app.take_dirty();
        app.apply(Update::View(Box::new(view)));
        assert!(app.take_dirty(), "delivery changes repaint");
        let Some(Modal::Inspect { lines, .. }) = &app.modal else {
            panic!("inbox inspector")
        };
        assert!(
            lines
                .iter()
                .any(|line| line.contains("turn_123: exact batch receipt"))
        );
        assert!(lines.iter().any(|line| line.contains("settled delivery")));
    }

    #[test]
    fn receipt_only_change_invalidates_inbox_fingerprint() {
        let mut app = app();
        app.view
            .inbox
            .push(inbox_event("event_a", "task_a", "project_a"));
        let before = fingerprint_at(&app.view, 0);
        app.view.inbox[0].receipt = Some("receipt_1".into());
        assert_ne!(before, fingerprint_at(&app.view, 0));
    }

    #[test]
    fn backlog_scope_and_attention_preserve_other_agents_approvals() {
        let (tx, rx) = sync_channel(8);
        let mut app = app();
        app.slash("/backlog", &tx);
        assert!(matches!(&app.modal, Some(Modal::Picker { items, .. }) if items.len() == 1));
        app.slash("/backlog all", &tx);
        assert!(matches!(&app.modal, Some(Modal::Picker { items, .. }) if items.len() == 2));
        app.slash("/attention", &tx);
        assert!(
            matches!(&app.modal, Some(Modal::Picker { items, .. }) if items.len() == 1 && items[0].label.contains("needs approval"))
        );
        assert!(
            rx.try_recv().is_err(),
            "inspecting attention cannot grant permissions"
        );
    }

    #[test]
    fn backlog_mutations_carry_snapshot_revision_and_keep_priority() {
        let (tx, rx) = sync_channel(8);
        let mut app = app();
        app.slash("/backlog edit task_a new plan", &tx);
        assert!(
            matches!(rx.try_recv(), Ok(Intent::Habitat(HabitatCommand::Edit {
            expected_revision: 23, priority: 7, prompt, ..
        })) if prompt == "new plan")
        );
        app.slash("/backlog run task_a", &tx);
        assert!(matches!(
            rx.try_recv(),
            Ok(Intent::Habitat(HabitatCommand::Release {
                expected_revision: 23,
                ..
            }))
        ));
        app.slash("/backlog add investigate later", &tx);
        assert!(matches!(
            rx.try_recv(),
            Ok(Intent::Habitat(HabitatCommand::EnqueueIn {
                deferred: true,
                ..
            }))
        ));
    }

    #[test]
    fn schedule_rejects_invalid_intervals_without_submitting_prompt() {
        let (tx, rx) = sync_channel(8);
        let mut app = app();
        for input in [
            "/schedule every 0 task",
            "/schedule every 59 task",
            "/schedule every 18446744073709551615 task",
            "/schedule every 60",
        ] {
            app.slash(input, &tx);
            assert!(rx.try_recv().is_err());
        }
        app.slash("/schedule every 3600 follow project", &tx);
        assert!(
            matches!(rx.try_recv(), Ok(Intent::HabitatAt { command: HabitatCommand::Schedule { interval_ms: 3_600_000, prompt }, .. }) if prompt == "follow project")
        );
    }

    #[test]
    fn new_backlog_revision_invalidates_view_fingerprint() {
        let mut app = app();
        let before = fingerprint_at(&app.view, 0);
        app.view.backlog[0].revision += 1;
        assert_ne!(before, fingerprint_at(&app.view, 0));
    }

    #[test]
    fn full_command_queue_restores_backlog_draft() {
        let (tx, _rx) = sync_channel(1);
        tx.try_send(Intent::Refresh).unwrap();
        let mut app = app();
        app.slash("/backlog add retain this draft", &tx);
        assert_eq!(app.composer.text(), "/backlog add retain this draft");
    }

    #[test]
    fn project_grant_is_bounded_and_preserves_provider_and_revision() {
        let (tx, rx) = sync_channel(8);
        let mut app = app();
        app.view.projects.push(xcb_core::ui::ProjectRow {
            conversation: app.view.conversation.clone().unwrap(),
            goal: "Existing goal".into(),
            enabled: true,
            remaining_tasks: 3,
            expires_at_ms: u64::MAX,
            required_provider: Some(xcb_core::Provider::Codex),
            revision: 17,
            status: "following project".into(),
        });
        for input in [
            "/project grant 0 24 goal",
            "/project grant 101 24 goal",
            "/project grant 5 721 goal",
            "/project grant 5 24",
        ] {
            app.slash(input, &tx);
            assert!(rx.try_recv().is_err());
        }
        app.slash("/project grant 5 24 Maintain the parser", &tx);
        assert!(
            matches!(rx.try_recv(), Ok(Intent::HabitatAt { command: HabitatCommand::ConfigureProject {
            expected_revision: Some(17), max_tasks: 5, required_provider: Some(xcb_core::Provider::Codex), goal, ..
        }, .. }) if goal == "Maintain the parser")
        );
        app.slash("/project pause", &tx);
        assert!(matches!(
            rx.try_recv(),
            Ok(Intent::Habitat(HabitatCommand::ProjectEnabled {
                expected_revision: 17,
                enabled: false,
                ..
            }))
        ));
    }

    #[test]
    fn completion_and_memory_controls_do_not_submit_new_provider_prompts() {
        let (tx, rx) = sync_channel(8);
        let mut app = app();
        app.slash("/backlog complete task_a Tests already cover this", &tx);
        assert!(
            matches!(rx.try_recv(), Ok(Intent::Habitat(HabitatCommand::CompleteBacklog { expected_revision: 23, summary, .. })) if summary == "Tests already cover this")
        );
        app.slash("/memory search parser decisions", &tx);
        assert!(
            matches!(rx.try_recv(), Ok(Intent::HabitatAt { conversation, command: HabitatCommand::MemorySearch { query } }) if query == "parser decisions" && conversation.as_str() == "project_a")
        );
    }
}

#[cfg(test)]
mod quota_display_tests {
    use super::*;

    #[test]
    fn idle_quota_countdown_repaints_at_the_displayed_minute_boundary() {
        let mut view = View {
            accounts: vec![xcb_core::ui::AccountRow {
                id: xcb_core::Id::new("limited").unwrap(),
                provider: xcb_core::Provider::Claude,
                name: "claude/limited".into(),
                email: None,
                subscription: "Max".into(),
                remaining_percent: None,
                resets_at_ms: None,
                quota_blocked_until_ms: Some(600_000),
                authentication_required: false,
                runway: Estimate::unknown("stale"),
                busy: false,
                enabled: true,
            }],
            ..View::default()
        };
        assert_eq!(fingerprint_at(&view, 0), fingerprint_at(&view, 59_999));
        assert_ne!(fingerprint_at(&view, 59_999), fingerprint_at(&view, 60_000));
        assert_ne!(
            fingerprint_at(&view, 599_999),
            fingerprint_at(&view, 600_000)
        );
        view.accounts[0].quota_blocked_until_ms = None;
        assert_eq!(fingerprint_at(&view, 0), fingerprint_at(&view, u64::MAX));
        let healthy = fingerprint_at(&view, 0);
        view.accounts[0].authentication_required = true;
        assert_ne!(healthy, fingerprint_at(&view, 0));
    }
}
