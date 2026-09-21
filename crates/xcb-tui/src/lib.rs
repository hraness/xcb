pub mod composer;
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
use std::{
    cell::Cell,
    collections::VecDeque,
    io::{self, IsTerminal},
    sync::mpsc::{Receiver, SyncSender, TryRecvError},
    time::{Duration, Instant},
};
use tui_textarea::TextArea;
use xcb_core::{
    Id,
    panes::Pane,
    session::{Attachment, State},
    ui::{Intent, Update, View},
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
        name: "/new",
        alias: "/n",
        args: "",
        summary: "start a new session",
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
        summary: "switch sessions",
        needs_args: false,
    },
];

#[derive(Clone)]
pub enum PickAction {
    Pane(Id),
    Model(String),
    Account(Id),
    Session(Id),
    Text(String),
    EditPane,
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
    Help,
}

/// Draft text and pending attachments scoped to one session. Kept per session
/// so switching sessions never carries the previous draft into the new one.
#[derive(Default)]
struct SessionDraft {
    text: String,
    attachments: Vec<Attachment>,
}

/// Bound on remembered per-session drafts; the least recently used is evicted.
const MAX_DRAFT_SESSIONS: usize = 64;

fn display_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
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
    view.reduced_motion.hash(&mut hasher);
    view.runway_coverage.hash(&mut hasher);
    view.tokens_per_second.map(f64::to_bits).hash(&mut hasher);
    view.share_percent.map(f64::to_bits).hash(&mut hasher);
    view.total_runway_seconds
        .map(f64::to_bits)
        .hash(&mut hasher);
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
    if let Some(last) = view.messages.last() {
        last.id.as_str().hash(&mut hasher);
        (last.role as u8).hash(&mut hasher);
        last.text.len().hash(&mut hasher);
        last.attachments.len().hash(&mut hasher);
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

#[derive(Default)]
pub struct App {
    pub view: View,
    pub composer: Composer,
    pub stream: String,
    pub thinking: String,
    pub notice: String,
    pub attachments: Vec<Attachment>,
    pub modal: Option<Modal>,
    pub show_thinking: bool,
    pub show_history: bool,
    pub show_activity: bool,
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
    /// Highlighted row of the slash-command typeahead menu.
    slash_selected: Cell<usize>,
    /// Esc closes the menu without canceling the turn; typing reopens it.
    slash_dismissed: Cell<bool>,
    /// Composer text the menu state belongs to; any edit resets selection.
    slash_text: std::cell::RefCell<String>,
    dirty: bool,
    view_fingerprint: u64,
}
impl App {
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
        SLASH_COMMANDS
            .iter()
            .filter(|command| command.name.starts_with(&text))
            .collect()
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
        if matches.is_empty() || self.slash_dismissed.get() {
            return None;
        }
        Some((
            matches.clone(),
            self.slash_selected.get().min(matches.len() - 1),
        ))
    }
    fn save_draft(&mut self, session: Id, draft: SessionDraft) {
        if let Some(position) = self.drafts.iter().position(|(id, _)| id == &session) {
            self.drafts.remove(position);
        }
        self.drafts.push_back((session, draft));
        while self.drafts.len() > MAX_DRAFT_SESSIONS {
            self.drafts.pop_front();
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
            Update::View(mut view) => {
                if view.pane_error.is_some() {
                    view.pane = self.view.pane.clone();
                    view.pane_revision = self.view.pane_revision.clone();
                }
                let previous = self.view.session.as_ref().map(|session| session.id.clone());
                let next = view.session.as_ref().map(|session| session.id.clone());
                if previous != next {
                    // The draft belongs to the session it was typed in: stash it
                    // and restore the target session's own draft.
                    if let Some(previous) = previous {
                        let text = self.composer.text();
                        let attachments = std::mem::take(&mut self.attachments);
                        if !text.is_empty() || !attachments.is_empty() {
                            self.save_draft(previous, SessionDraft { text, attachments });
                        }
                    }
                    let draft = next.and_then(|id| self.take_draft(&id)).unwrap_or_default();
                    self.composer.set_text(&draft.text);
                    self.attachments = draft.attachments;
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
                self.restore_draft(text, attachments);
                self.dirty = true;
            }
            Update::Attachment(attachment) => {
                self.pending_image = false;
                // An image that lands after a session switch belongs to the
                // session that requested it, not the one now on screen.
                match self.pending_image_session.take().filter(|session| {
                    self.view
                        .session
                        .as_ref()
                        .is_some_and(|current| current.id != *session)
                }) {
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
    fn send(&mut self, output: &SyncSender<Intent>, intent: Intent) {
        if output.try_send(intent).is_err() {
            self.notice = "The command queue is full or closed. Nothing was submitted.".into();
        }
    }
    fn slash(&mut self, input: &str, output: &SyncSender<Intent>) -> bool {
        let (command, arguments) = input.split_once(' ').unwrap_or((input, ""));
        // Single-letter aliases resolve to the full command before dispatch.
        let command = SLASH_COMMANDS
            .iter()
            .find(|entry| entry.alias == command)
            .map_or(command, |entry| entry.name);
        let arguments = arguments.trim();
        match command {
            "/help" => self.modal = Some(Modal::Help),
            "/quit" | "/exit" => {
                self.send(output, Intent::Quit);
                return false;
            }
            "/new" => self.send(output, Intent::NewSession),
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
                            "{} · {} · {} · {}{}{}",
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
                            if account.enabled { "" } else { " · disabled" }
                        ),
                        action: PickAction::Account(account.id.clone()),
                    })
                    .collect(),
            ),
            "/sessions" => self.picker(
                "Sessions",
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
                self.pending_image = true;
                self.pending_image_session =
                    self.view.session.as_ref().map(|session| session.id.clone());
                self.send(
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
        true
    }
    pub fn handle(&mut self, event: Event, output: &SyncSender<Intent>) -> bool {
        if !matches!(&event, Event::Key(key) if key.kind == KeyEventKind::Release) {
            self.dirty = true;
        }
        if self.modal.is_some() {
            return self.modal_event(event, output);
        }
        if let Event::Key(key) = &event {
            if key.kind == KeyEventKind::Release {
                return true;
            }
            // An open slash-command menu owns navigation and completion; global
            // toggles and Ctrl-C cancel stay reachable.
            if let Some((matches, selected)) = self.slash_menu() {
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
                        if matches[selected].needs_args {
                            text.push(' ');
                        }
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
            if key.modifiers.contains(KeyModifiers::CONTROL) {
                match key.code {
                    KeyCode::Char('c') => {
                        // Standard interrupt ordering: a live turn is cancelled
                        // first, then a draft clears, then an idle empty
                        // composer quits.
                        if matches!(self.view.state, State::Working) || self.view.remote_active {
                            self.send(output, Intent::Cancel);
                            self.notice = if self.view.remote_active {
                                "This turn is running in another terminal; cancel it there."
                            } else {
                                "Stopping the current turn and queued follow-ups."
                            }
                            .into();
                        } else if !self.composer.text().is_empty() {
                            self.composer.set_text("");
                            self.notice = "Draft cleared. Press Ctrl-C again to quit.".into();
                        } else {
                            self.send(output, Intent::Quit);
                            return false;
                        }
                        return true;
                    }
                    KeyCode::Char('t') => {
                        self.show_thinking = !self.show_thinking;
                        return true;
                    }
                    KeyCode::Char('o') => {
                        self.show_history = !self.show_history;
                        return true;
                    }
                    KeyCode::Char('u') => {
                        self.show_activity = !self.show_activity;
                        return true;
                    }
                    KeyCode::Char('p') => {
                        self.slash("/model", output);
                        return true;
                    }
                    KeyCode::Char('l') => {
                        self.send(output, Intent::Refresh);
                        return true;
                    }
                    _ => (),
                }
            }
            match key.code {
                KeyCode::Char('?') if self.composer.text().is_empty() => {
                    self.modal = Some(Modal::Help);
                    return true;
                }
                KeyCode::PageUp => {
                    self.scroll_transcript(-10);
                    return true;
                }
                KeyCode::PageDown => {
                    self.scroll_transcript(10);
                    return true;
                }
                KeyCode::End => {
                    self.paused.set(false);
                    self.scroll.set(0);
                    return true;
                }
                KeyCode::Backspace if key.modifiers.contains(KeyModifiers::ALT) => {
                    self.attachments.pop();
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
            // The wheel always scrolls the transcript — never the composer.
            // With mouse capture enabled the terminal delivers real scroll
            // events instead of translating them into arrow keys.
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
                if !text.trim().is_empty() || !self.attachments.is_empty() {
                    let attachments = std::mem::take(&mut self.attachments);
                    match output.try_send(Intent::Submit { text, attachments }) {
                        Ok(()) => self.notice.clear(),
                        Err(
                            std::sync::mpsc::TrySendError::Full(Intent::Submit {
                                text,
                                attachments,
                            })
                            | std::sync::mpsc::TrySendError::Disconnected(Intent::Submit {
                                text,
                                attachments,
                            }),
                        ) => {
                            self.composer.set_text(&text);
                            self.attachments = attachments;
                            self.notice = "Command queue unavailable; draft retained.".into();
                        }
                        Err(_) => (),
                    }
                }
            }
            ComposerAction::Cancel => {
                // Esc interrupts a live turn; idle it is a quiet no-op.
                if matches!(self.view.state, State::Working) || self.view.remote_active {
                    self.send(output, Intent::Cancel);
                    self.notice = if self.view.remote_active {
                        "This turn is running in another terminal; cancel it there."
                    } else {
                        "Stopping the current turn and queued follow-ups."
                    }
                    .into();
                }
            }
            ComposerAction::Quit => {
                self.send(output, Intent::Quit);
                return false;
            }
            ComposerAction::Clipboard => self.clipboard(output),
            ComposerAction::History => self.picker(
                "Prompt history",
                self.composer
                    .history()
                    .map(|text| PickItem {
                        label: text.lines().next().unwrap_or("").into(),
                        action: PickAction::Text(text.clone()),
                    })
                    .collect(),
            ),
            ComposerAction::Editor => {
                self.modal = Some(Modal::Editor {
                    title: "Prompt editor".into(),
                    textarea: Box::new(self.composer.textarea.clone()),
                    kind: EditorKind::Prompt,
                    error: None,
                })
            }
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
            self.pending_image = true;
            self.pending_image_session =
                self.view.session.as_ref().map(|session| session.id.clone());
            self.send(
                output,
                Intent::AttachRgba {
                    width: image.width,
                    height: image.height,
                    bytes: image.bytes.into_owned(),
                },
            );
        } else if let Ok(text) = clipboard.get_text() {
            self.composer.handle(Event::Paste(text));
        } else {
            self.notice = "No supported text or image on the clipboard.".into();
        }
    }
    fn modal_event(&mut self, event: Event, output: &SyncSender<Intent>) -> bool {
        // Ctrl-C inside a dialog keeps the global ordering: cancel a live run
        // first, quit when idle. Esc still only closes the dialog.
        if let Event::Key(key) = &event
            && key.kind != KeyEventKind::Release
            && key.code == KeyCode::Char('c')
            && key.modifiers.contains(KeyModifiers::CONTROL)
        {
            if matches!(self.view.state, State::Working) || self.view.remote_active {
                self.send(output, Intent::Cancel);
                self.notice = if self.view.remote_active {
                    "This turn is running in another terminal; cancel it there."
                } else {
                    "Stopping the current turn and queued follow-ups."
                }
                .into();
                return true;
            }
            self.send(output, Intent::Quit);
            return false;
        }
        if matches!(
            (&self.modal, &event),
            (
                Some(Modal::Help),
                Event::Key(key)
            ) if key.kind != KeyEventKind::Release
                && matches!(key.code, KeyCode::Char('?') | KeyCode::Esc)
        ) || matches!(
            &event,
            Event::Key(key) if key.kind != KeyEventKind::Release && key.code == KeyCode::Esc
        ) {
            self.modal = None;
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
                    let filtered = items
                        .iter()
                        .filter(|item| item.label.to_lowercase().contains(&query.to_lowercase()))
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
                                    && query.len() < 128 =>
                            {
                                query.push(ch);
                                *selected = 0;
                            }
                            KeyCode::Enter => {
                                chosen = items
                                    .iter()
                                    .filter(|item| {
                                        item.label.to_lowercase().contains(&query.to_lowercase())
                                    })
                                    .nth(*selected)
                                    .map(|item| item.action.clone())
                            }
                            _ => (),
                        }
                    }
                }
                Modal::Help => {}
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
                            Event::Paste(text)
                                if textarea.lines().iter().map(String::len).sum::<usize>()
                                    + text.len()
                                    <= 64 * 1024 =>
                            {
                                textarea.insert_str(xcb_core::display_text(
                                    &text.replace("\r\n", "\n"),
                                    64 * 1024,
                                ));
                            }
                            Event::Key(key)
                                if key.kind != KeyEventKind::Release
                                    && (textarea
                                        .lines()
                                        .iter()
                                        .map(String::len)
                                        .sum::<usize>()
                                        < 64 * 1024
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
            match action {
                PickAction::Pane(id) => self.send(output, Intent::Pane(id)),
                PickAction::Model(id) => self.send(output, Intent::Model(id)),
                PickAction::Account(id) => self.send(output, Intent::Account(id)),
                PickAction::Session(id) => self.send(output, Intent::Resume(id)),
                PickAction::Text(text) => self.composer.set_text(&text),
                PickAction::EditPane => {
                    self.edit_pane(&self.view.pane.clone(), self.view.pane_revision.clone())
                }
            }
        }
        true
    }
}

struct Restore;
impl Drop for Restore {
    fn drop(&mut self) {
        let _ = execute!(
            io::stdout(),
            PopKeyboardEnhancementFlags,
            DisableBracketedPaste,
            DisableMouseCapture,
            LeaveAlternateScreen
        );
        let _ = disable_raw_mode();
    }
}

pub fn run(input: Receiver<Update>, output: SyncSender<Intent>) -> io::Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(io::Error::other(
            "xcb chat needs a terminal; use xcb run for headless work",
        ));
    }
    enable_raw_mode()?;
    let _restore = Restore;
    execute!(
        io::stdout(),
        EnterAlternateScreen,
        EnableBracketedPaste,
        EnableMouseCapture,
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    )?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let mut app = App::default();
    let mut ticks = 0u64;
    let mut refresh = Instant::now();
    let mut needs_draw = true;
    let mut blink = 0u64;
    loop {
        for _ in 0..128 {
            match input.try_recv() {
                Ok(update) => {
                    if !app.apply(update) {
                        return Ok(());
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return Ok(()),
            }
        }
        needs_draw |= app.take_dirty();
        // The attention blink is the only state that changes with time alone.
        let phase = (ticks / 16) % 2;
        if !needs_draw && app.view.state.attention() && !app.view.reduced_motion && phase != blink {
            needs_draw = true;
        }
        if needs_draw {
            terminal.draw(|frame| render::draw(frame, &mut app, ticks))?;
            needs_draw = false;
            blink = phase;
        }
        if event::poll(Duration::from_millis(50))? && !app.handle(event::read()?, &output) {
            break;
        }
        ticks = ticks.wrapping_add(1);
        if refresh.elapsed() >= Duration::from_millis(750) {
            app.send(&output, Intent::Refresh);
            refresh = Instant::now();
        }
    }
    Ok(())
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
    }
}
