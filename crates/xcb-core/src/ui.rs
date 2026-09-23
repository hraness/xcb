use crate::{
    Id, Provider,
    models::ModelChoice,
    panes::Pane,
    session::{Attachment, Message, Session, State, Subagent},
    usage::Estimate,
};

#[derive(Debug, Clone)]
pub struct AccountRow {
    pub id: Id,
    pub provider: Provider,
    /// Fixed display identity: the provider account email once observed,
    /// otherwise `provider/<id prefix>`. Never a user-authored label.
    pub name: String,
    pub email: Option<String>,
    pub subscription: String,
    pub remaining_percent: Option<f64>,
    pub resets_at_ms: Option<u64>,
    /// Known account-wide exhaustion, independent of telemetry freshness.
    pub quota_blocked_until_ms: Option<u64>,
    pub runway: Estimate,
    pub busy: bool,
    pub enabled: bool,
    pub authentication_required: bool,
}

impl AccountRow {
    /// A reported retry time is not a promise that the provider will accept a turn.
    pub fn quota_block_label(&self, now: u64) -> Option<String> {
        let until = self.quota_blocked_until_ms.filter(|until| *until > now)?;
        let minutes = (until - now).div_ceil(60_000);
        let wait = if minutes >= 60 * 24 {
            format!("{}d", minutes / (60 * 24))
        } else if minutes >= 60 {
            format!("{}h {}m", minutes / 60, minutes % 60)
        } else {
            format!("{minutes}m")
        };
        Some(format!("quota limited · retry in ~{wait}"))
    }
}

#[derive(Debug, Clone)]
pub struct ConversationRow {
    pub id: Id,
    pub title: String,
    pub workspace: String,
    /// Durable messages recorded in the conversation.
    pub messages: usize,
    pub updated_at_ms: u64,
}

#[derive(Debug, Clone)]
pub struct TaskRow {
    pub id: Id,
    pub title: String,
    pub state: State,
    /// Managed worker phase label (`queued — waiting for a route`,
    /// `running`, `needs input`, …). `state` folds queued and running into
    /// `State::Working`, so this keeps the supervisor's wire phase
    /// distinguishable — callers that classify match the label's leading
    /// word; `None` for rows produced without one.
    pub status: Option<String>,
    pub detail: String,
    /// Routed `model · account` once a worker is dispatched; a bare provider
    /// name while the route is only a preference.
    pub route: Option<String>,
    /// Why the supervisor picked `route`, when it recorded one.
    pub route_reason: Option<String>,
    pub workspace: String,
    pub updated_at_ms: u64,
}

/// The route a fresh session would take right now — the account with the
/// most remaining quota and the provider's default model. Previewed in the
/// chrome when no session is bound; nothing is persisted until a real
/// submission creates the session.
#[derive(Debug, Clone)]
pub struct RoutePreview {
    pub account: String,
    pub provider: Provider,
    pub model: String,
}

/// Bounded durable work history, including work deliberately held for later.
#[derive(Debug, Clone)]
pub struct BacklogRow {
    pub id: Id,
    pub conversation: Id,
    pub title: String,
    pub prompt: String,
    pub summary: String,
    pub status: String,
    pub state: State,
    pub deferred: bool,
    pub priority: u8,
    pub revision: u64,
    pub updated_at_ms: u64,
}

#[derive(Debug, Clone)]
pub struct ScheduleRow {
    pub id: Id,
    pub conversation: Id,
    pub prompt: String,
    pub interval_ms: u64,
    pub next_due_ms: u64,
    pub enabled: bool,
    pub revision: u64,
}

#[derive(Debug, Clone)]
pub struct ProjectRow {
    pub conversation: Id,
    pub goal: String,
    pub enabled: bool,
    pub remaining_tasks: u32,
    pub expires_at_ms: u64,
    pub required_provider: Option<Provider>,
    pub revision: u64,
    pub status: String,
}

pub enum HabitatCommand {
    ConfigureProject {
        expected_revision: Option<u64>,
        goal: String,
        max_tasks: u32,
        expires_at_ms: u64,
        required_provider: Option<Provider>,
    },
    ProjectEnabled {
        conversation: Id,
        expected_revision: u64,
        enabled: bool,
    },
    CompleteBacklog {
        id: Id,
        expected_revision: u64,
        summary: String,
    },
    ReconcileTask {
        id: Id,
        expected_revision: u64,
    },
    MemorySearch {
        query: String,
    },
    Enqueue {
        prompt: String,
        deferred: bool,
        priority: u8,
    },
    Edit {
        id: Id,
        expected_revision: u64,
        prompt: String,
        priority: u8,
    },
    Release {
        id: Id,
        expected_revision: u64,
    },
    Reply {
        id: Id,
        text: String,
    },
    Schedule {
        prompt: String,
        interval_ms: u64,
    },
    ScheduleEnabled {
        id: Id,
        expected_revision: u64,
        enabled: bool,
    },
}

#[derive(Debug, Clone)]
pub struct View {
    pub conversation: Option<Id>,
    pub conversations: Vec<ConversationRow>,
    pub session: Option<Session>,
    pub sessions: Vec<Session>,
    pub accounts: Vec<AccountRow>,
    pub models: Vec<ModelChoice>,
    pub messages: Vec<Message>,
    pub tasks: Vec<TaskRow>,
    pub backlog: Vec<BacklogRow>,
    pub schedules: Vec<ScheduleRow>,
    pub projects: Vec<ProjectRow>,
    pub subagents: Vec<Subagent>,
    pub activity: Vec<String>,
    pub extensions: Vec<(String, String)>,
    pub pane: Pane,
    pub panes: Vec<Pane>,
    pub pane_revision: Option<String>,
    pub pane_error: Option<String>,
    pub state: State,
    /// True when the focused session's live run is owned by another terminal
    /// instance; the session is actively working elsewhere, not unsettled.
    pub remote_active: bool,
    /// Current control conversation has cancellable managed work, independently
    /// of the global task swarm shown in `state` and `tasks`.
    pub managed_cancel_available: bool,
    /// Set only while no session is bound.
    pub pending_route: Option<RoutePreview>,
    pub tokens_per_second: Option<f64>,
    pub share_percent: Option<f64>,
    pub total_runway_seconds: Option<f64>,
    pub runway_coverage: (usize, usize),
    pub reduced_motion: bool,
}
impl Default for View {
    fn default() -> Self {
        Self {
            conversation: None,
            conversations: vec![],
            session: None,
            sessions: vec![],
            accounts: vec![],
            models: vec![],
            messages: vec![],
            tasks: vec![],
            backlog: vec![],
            schedules: vec![],
            projects: vec![],
            subagents: vec![],
            activity: vec![],
            extensions: vec![],
            pane: Pane::focus(),
            panes: Pane::presets(),
            pane_revision: None,
            pane_error: None,
            state: State::Idle,
            remote_active: false,
            managed_cancel_available: false,
            pending_route: None,
            tokens_per_second: None,
            share_percent: None,
            total_runway_seconds: None,
            runway_coverage: (0, 0),
            reduced_motion: false,
        }
    }
}

pub enum Intent {
    Habitat(HabitatCommand),
    Submit {
        id: Id,
        text: String,
        attachments: Vec<Attachment>,
    },
    Cancel,
    Quit,
    NewSession,
    Conversation(Id),
    Resume(Id),
    Account(Id),
    Model(String),
    SetDefault,
    Pane(Id),
    SavePane {
        pane: Pane,
        expected: Option<String>,
    },
    GeneratePane(String),
    AttachPath(String),
    AttachRgba {
        width: usize,
        height: usize,
        bytes: Vec<u8>,
    },
    Extension {
        name: String,
        enabled: bool,
    },
    Refresh,
}

pub enum Update {
    View(Box<View>),
    Delta {
        session: Id,
        thinking: bool,
        text: String,
    },
    ClearStream(Id),
    /// A submission the kernel rejected; restores the complete draft — text
    /// and attachments — to the composer so nothing is silently lost.
    Draft {
        text: String,
        attachments: Vec<Attachment>,
    },
    Attachment(Attachment),
    PaneCandidate(Pane),
    Notice(String),
    Stopped,
}
