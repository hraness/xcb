//! Keyboard-oriented context, history and explicit managed-task controls.
use super::*;
use xcb_core::session::{Message, Role};

#[derive(Clone, PartialEq, Eq)]
pub(super) struct ComposerTarget {
    pub task: Id,
    pub conversation: Id,
    pub answer_revision: Option<u64>,
}

pub(super) struct PendingHabitat {
    pub operation: Id,
    pub context: Id,
    pub task: Option<Id>,
    pub text: String,
    pub target: Option<ComposerTarget>,
}

#[derive(Clone)]
pub(super) enum LivePicker {
    Tasks,
    Backlog { all: bool, attention: bool },
    Schedules { all: bool },
    Conversations,
    Sessions,
    Projects { all: bool },
}

pub(super) fn operation_id() -> Id {
    Id::new(format!("ui_{}", uuid::Uuid::new_v4().simple())).expect("generated operation id")
}

pub(super) fn item_matches(item: &PickItem, query: &str) -> bool {
    let query = query.to_lowercase();
    item.label.to_lowercase().contains(&query)
        || matches!(&item.action, PickAction::Text(text) if text.to_lowercase().contains(&query))
}

impl App {
    pub(super) fn habitat_pending(&self, intent: &Intent) -> Option<PendingHabitat> {
        let Intent::Habitat(command) = intent else {
            return None;
        };
        let (operation, task, text, revision) = match command {
            HabitatCommand::EnqueueIn { id, prompt, .. }
            | HabitatCommand::Enqueue { id, prompt, .. } => {
                (id.clone(), None, prompt.clone(), None)
            }
            HabitatCommand::Steer { task, event, text } => {
                (event.clone(), Some(task.clone()), text.clone(), None)
            }
            HabitatCommand::Reply {
                id,
                reply,
                text,
                expected_revision,
            } => (
                reply.clone(),
                Some(id.clone()),
                text.clone(),
                Some(*expected_revision),
            ),
            _ => return None,
        };
        let context = task
            .as_ref()
            .and_then(|id| {
                self.view
                    .backlog
                    .iter()
                    .find(|row| &row.id == id)
                    .map(|row| row.conversation.clone())
            })
            .or_else(|| self.view.conversation.clone())?;
        let target = task.as_ref().map(|task| ComposerTarget {
            task: task.clone(),
            conversation: context.clone(),
            answer_revision: revision,
        });
        Some(PendingHabitat {
            operation,
            context,
            task,
            text,
            target,
        })
    }

    pub(super) fn interaction_command(
        &mut self,
        command: &str,
        arguments: &str,
        output: &SyncSender<Intent>,
    ) -> bool {
        match command {
            "/detach" => {
                if arguments == "all" {
                    self.attachments.clear();
                    self.notice = "Attachments removed from this draft.".into();
                } else {
                    let index = if arguments.is_empty() {
                        self.attachments.len().checked_sub(1)
                    } else {
                        arguments
                            .parse::<usize>()
                            .ok()
                            .and_then(|n| n.checked_sub(1))
                    };
                    if let Some(index) = index.filter(|index| *index < self.attachments.len()) {
                        self.attachments.remove(index);
                        self.notice = "Attachment removed from this draft.".into();
                    } else {
                        self.notice = "Use /detach <attachment number> or /detach all.".into();
                    }
                }
            }
            "/history" => self.open_transcript(false),
            "/copy" => self.copy_answer(),
            "/clear" => self.clear_display(),
            "/editor" => self.external_editor_requested = true,
            "/rename" => self.rename_context(arguments, output),
            "/drafts" => self.open_recovery(),
            "/tools" => self.show_activity = !self.show_activity,
            "/thinking" => self.show_thinking = !self.show_thinking,
            "/task" => {
                self.composer_target = None;
                self.composer.set_text(arguments);
                self.notice = "New task. Enter sends; Tab queues.".into();
            }
            "/queue" if self.managed_mode() => {
                if arguments.is_empty() {
                    self.slash("/backlog", output);
                } else {
                    self.targeted_submit(arguments.to_owned(), true, output);
                }
            }
            "/cancel" if self.managed_mode() => {
                if arguments.is_empty() {
                    self.managed_cancel(output);
                } else if let Some(row) = self
                    .view
                    .backlog
                    .iter()
                    .find(|row| row.id.as_str() == arguments)
                {
                    self.cancel_selected(row.id.clone(), row.revision, output);
                } else {
                    self.notice =
                        "Task is outside the current view. Use /tasks or /backlog all.".into();
                }
            }
            "/status" => {
                let mut lines = vec![format!("State: {}", self.view.state.label())];
                if let Some(session) = &self.view.session {
                    lines.push(format!("Session: {} · {}", session.id, session.title));
                    lines.push(format!("Model: {}", session.model.label));
                }
                if let Some(context) = &self.view.conversation {
                    lines.push(format!("Conversation: {context}"));
                }
                if let Some(route) = &self.view.pending_route {
                    lines.push(format!(
                        "Route preview: {} · {} · {}",
                        route.provider, route.model, route.account
                    ));
                }
                lines.push(format!(
                    "{} visible tasks · {} need attention · {} inbox events",
                    self.view.tasks.len(),
                    self.view
                        .backlog
                        .iter()
                        .filter(|row| row.state.attention())
                        .count(),
                    self.view.inbox.len()
                ));
                if let Some(target) = self.composer_target_label() {
                    lines.push(target);
                }
                lines.push("/attention · /agents · /inbox · /project · /history".into());
                self.modal = Some(Modal::Inspect {
                    title: "Status".into(),
                    lines,
                    scroll: 0,
                });
            }
            _ => return false,
        }
        true
    }

    pub(super) fn interaction_key(
        &mut self,
        key: KeyEvent,
        output: &SyncSender<Intent>,
    ) -> Option<bool> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Char('t') if ctrl => self.open_transcript(false),
            KeyCode::Char('o') if ctrl => self.copy_answer(),
            KeyCode::Char('l') if ctrl => self.clear_display(),
            KeyCode::F(3) => self.open_transcript(true),
            KeyCode::F(2) => {
                self.slash("/attention", output);
            }
            KeyCode::F(4) => self.show_activity = !self.show_activity,
            KeyCode::Home if ctrl => {
                self.paused.set(true);
                self.scroll.set(0);
            }
            KeyCode::Up if alt && self.managed_mode() => self.recall_queued(output),
            KeyCode::Down if alt && self.managed_mode() => {
                self.slash("/attention", output);
            }
            KeyCode::Left | KeyCode::Right
                if alt
                    && self.managed_mode()
                    && self.composer.text().is_empty()
                    && self.attachments.is_empty()
                    && !self.pending_image =>
            {
                let current = self
                    .view
                    .conversations
                    .iter()
                    .position(|row| Some(&row.id) == self.view.conversation.as_ref());
                if let Some(index) = current.filter(|_| self.view.conversations.len() > 1) {
                    let len = self.view.conversations.len();
                    let next = if key.code == KeyCode::Left {
                        (index + len - 1) % len
                    } else {
                        (index + 1) % len
                    };
                    self.send(
                        output,
                        Intent::Conversation(self.view.conversations[next].id.clone()),
                    );
                }
            }
            KeyCode::Tab if !self.composer.text().starts_with('/') && self.managed_mode() => {
                let text = self.composer.text();
                self.targeted_submit(text, true, output);
            }
            _ => return None,
        }
        Some(true)
    }

    pub(super) fn record_live_picker(&mut self, command: &str, arguments: &str) {
        if !matches!(self.modal, Some(Modal::Picker { .. })) {
            return;
        }
        self.live_picker = match command {
            "/tasks" => Some(LivePicker::Tasks),
            "/attention" => Some(LivePicker::Backlog {
                all: true,
                attention: true,
            }),
            "/backlog" if arguments.is_empty() || arguments == "all" => Some(LivePicker::Backlog {
                all: arguments == "all",
                attention: false,
            }),
            "/schedule" if arguments.is_empty() || arguments == "all" => {
                Some(LivePicker::Schedules {
                    all: arguments == "all",
                })
            }
            "/project" if arguments.is_empty() || arguments == "all" => {
                Some(LivePicker::Projects {
                    all: arguments == "all",
                })
            }
            "/sessions" if self.managed_mode() => Some(LivePicker::Conversations),
            "/sessions" => Some(LivePicker::Sessions),
            _ => None,
        };
    }

    pub(super) fn refresh_live_surfaces(&mut self) {
        if let Some(kind) = &self.live_picker {
            if let Some(Modal::Picker {
                query,
                items,
                selected,
                ..
            }) = &mut self.modal
            {
                let previous = items
                    .iter()
                    .filter(|item| item_matches(item, query))
                    .nth(*selected)
                    .map(|item| item.action.clone());
                *items = kind.items(&self.view);
                let visible: Vec<_> = items
                    .iter()
                    .filter(|item| item_matches(item, query))
                    .collect();
                *selected = previous
                    .and_then(|action| visible.iter().position(|item| item.action == action))
                    .unwrap_or((*selected).min(visible.len().saturating_sub(1)));
            } else {
                self.live_picker = None;
            }
        }
        if let Some(action) = &self.live_inspect {
            if let Some(Modal::Inspect { scroll, .. }) = &self.modal {
                let scroll = *scroll;
                let next = match action {
                    PickAction::Task(id) => self
                        .view
                        .tasks
                        .iter()
                        .find(|row| &row.id == id)
                        .map(inspect_task),
                    PickAction::Backlog(id) => self
                        .view
                        .backlog
                        .iter()
                        .find(|row| &row.id == id)
                        .map(inspect_backlog),
                    PickAction::Schedule(id) => self
                        .view
                        .schedules
                        .iter()
                        .find(|row| &row.id == id)
                        .map(|row| Modal::Inspect {
                            title: format!("Schedule {}", row.id),
                            lines: vec![
                                row.prompt.clone(),
                                format!(
                                    "{} · every {}s · revision {}",
                                    if row.enabled { "enabled" } else { "paused" },
                                    row.interval_ms / 1000,
                                    row.revision
                                ),
                                format!("Next due {}", due_label(row.next_due_ms)),
                            ],
                            scroll: 0,
                        }),
                    PickAction::Project(id) => self
                        .view
                        .projects
                        .iter()
                        .find(|row| &row.conversation == id)
                        .map(|row| Modal::Inspect {
                            title: format!("Project {}", row.conversation),
                            lines: vec![
                                row.goal.clone(),
                                row.status.clone(),
                                format!(
                                    "{} tasks remaining · revision {}",
                                    row.remaining_tasks, row.revision
                                ),
                                format!("Expires {}", due_label(row.expires_at_ms)),
                            ],
                            scroll: 0,
                        }),
                    _ => None,
                };
                if matches!(
                    action,
                    PickAction::Task(_)
                        | PickAction::Backlog(_)
                        | PickAction::Schedule(_)
                        | PickAction::Project(_)
                ) {
                    self.modal = Some(next.unwrap_or(Modal::Inspect { title: "Outside current view".into(), lines: vec!["This record is outside the bounded view. Reopen its list to refresh.".into()], scroll: 0 }));
                    if let Some(Modal::Inspect {
                        scroll: next,
                        lines,
                        ..
                    }) = &mut self.modal
                    {
                        *next = scroll;
                        if matches!(action, PickAction::Task(_) | PickAction::Backlog(_)) {
                            lines.push(
                                "s guide · a answer question · x request cancellation · Esc close"
                                    .into(),
                            );
                        }
                    }
                }
            } else {
                self.live_inspect = None;
            }
        }
    }

    pub(super) fn inspector_action(&mut self, event: &Event, output: &SyncSender<Intent>) -> bool {
        if !matches!(self.modal, Some(Modal::Inspect { .. })) {
            return false;
        }
        let Some(PickAction::Task(id) | PickAction::Backlog(id)) = self.live_inspect.clone() else {
            return false;
        };
        let Event::Key(key) = event else {
            return false;
        };
        if key.kind == KeyEventKind::Release
            || key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return false;
        }
        match key.code {
            KeyCode::Char('s') => self.select_task_target(&id, false),
            KeyCode::Char('a') => self.select_task_target(&id, true),
            KeyCode::Char('x') => {
                if let Some(row) = self.view.backlog.iter().find(|row| row.id == id) {
                    self.cancel_selected(id, row.revision, output);
                } else if let Some(row) = self.view.tasks.iter().find(|row| row.id == id) {
                    self.cancel_selected(id, row.revision, output);
                } else {
                    self.notice =
                        "Task is outside the current view; cancellation was not requested.".into();
                }
            }
            _ => return false,
        }
        true
    }
    pub(super) fn select_task_target(&mut self, id: &Id, answer: bool) {
        let Some(row) = self.view.backlog.iter().find(|row| &row.id == id) else {
            self.notice = "Task is outside the current view. Refresh before selecting it.".into();
            return;
        };
        if answer && row.state != State::NeedsAnswer {
            self.notice = "This task is not waiting for an answer. Approval and recovery require their own controls.".into();
            return;
        }
        if matches!(
            row.state,
            State::Idle | State::Failed | State::Cancelled | State::Limited
        ) && !row.deferred
            && !row.status.starts_with("queued")
        {
            self.notice =
                "This task is settled. Start new work with /task or recall a prompt from history."
                    .into();
            return;
        }
        self.composer_target = Some(ComposerTarget {
            task: id.clone(),
            conversation: row.conversation.clone(),
            answer_revision: answer.then_some(row.revision),
        });
        self.modal = None;
        self.live_inspect = None;
        self.notice = if answer { "Answer the displayed question. A changed question rejects the reply." } else { "Enter guides this task at its next authorized turn. Tab queues a new task. /task returns to new work." }.into();
    }

    pub(super) fn targeted_submit(
        &mut self,
        text: String,
        queue: bool,
        output: &SyncSender<Intent>,
    ) {
        if text.trim().is_empty() {
            return;
        }
        if !self.recovery_capacity_available() {
            self.composer.set_text(&text);
            self.notice = "Recover or discard retained input before sending more work.".into();
            return;
        }
        if !self.attachments.is_empty() || self.pending_image {
            self.composer.set_text(&text);
            self.notice = "This control accepts text only; attachments and draft are retained. Use /task for ordinary submission.".into();
            return;
        }
        if self.pending_habitat.len() >= 16 || !self.recovery_capacity_available() {
            self.composer.set_text(&text);
            self.notice = "Waiting for earlier input acknowledgements; draft retained.".into();
            return;
        }
        let key = format!(
            "{}:{:?}:{}:{:?}:{}",
            if queue { "queue" } else { "target" },
            self.view.conversation,
            self.composer_target
                .as_ref()
                .map_or("", |target| target.task.as_str()),
            self.composer_target
                .as_ref()
                .and_then(|target| target.answer_revision),
            text
        );
        let operation = self.inbox_event_id("input", &key);
        let (command, context, task, target) = if queue {
            let Some(context) = self.view.conversation.clone() else {
                self.composer.set_text(&text);
                self.notice = "Open a managed conversation before queueing work.".into();
                return;
            };
            (
                HabitatCommand::EnqueueIn {
                    conversation: context.clone(),
                    id: operation.clone(),
                    prompt: text.clone(),
                    deferred: false,
                    priority: 5,
                },
                context,
                None,
                None,
            )
        } else {
            let Some(target) = self.composer_target.clone() else {
                self.composer.set_text(&text);
                return;
            };
            let command = if let Some(expected_revision) = target.answer_revision {
                HabitatCommand::Reply {
                    id: target.task.clone(),
                    expected_revision,
                    reply: operation.clone(),
                    text: text.clone(),
                }
            } else {
                HabitatCommand::Steer {
                    task: target.task.clone(),
                    event: operation.clone(),
                    text: text.clone(),
                }
            };
            (
                command,
                target.conversation.clone(),
                Some(target.task.clone()),
                Some(target),
            )
        };
        self.pending_habitat.push_back(PendingHabitat {
            operation,
            context,
            task,
            text: text.clone(),
            target,
        });
        self.composer.set_text("");
        if !self.flush_recovery(true) {
            self.pending_habitat.pop_back();
            self.composer.set_text(&text);
            return;
        }
        if self.try_send(output, Intent::Habitat(command)) {
            self.inbox_draft_event = None;
            self.composer.remember(&text);
            self.composer.set_text("");
            self.notice = "Saving input… delivery will be shown in the task inbox.".into();
        } else {
            self.pending_habitat.pop_back();
            self.composer.set_text(&text);
            self.flush_recovery(true);
        }
    }

    pub(super) fn habitat_outcome(
        &mut self,
        accepted: bool,
        _context: Id,
        task: Option<Id>,
        operation: Id,
        text: String,
    ) {
        let pending = self
            .pending_habitat
            .iter()
            .position(|entry| entry.operation == operation && entry.task == task)
            .and_then(|index| self.pending_habitat.remove(index));
        if accepted {
            self.notice = if task.is_some() {
                "Input saved. /inbox shows when it enters an authorized turn."
            } else {
                "Task queued. /backlog shows its progress."
            }
            .into();
            return;
        }
        // Only a matching outstanding operation can restore input.
        if pending.is_none() {
            return;
        }
        let context = pending.as_ref().expect("matched operation").context.clone();
        self.composer.remember(&text);
        if view_context(&self.view).as_ref() == Some(&context)
            && self.composer.text().is_empty()
            && self.attachments.is_empty()
            && !self.pending_image
            && self.composer_target == pending.as_ref().and_then(|entry| entry.target.clone())
        {
            self.composer.set_text(&text);
            if let Some(pending) = pending {
                self.composer_target = pending.target;
            }
        } else {
            self.retain_rejected(
                context,
                text,
                Vec::new(),
                pending.and_then(|entry| entry.target),
            );
        }
        self.notice =
            "Input was not admitted. Draft retained; inspect the task before retrying.".into();
    }

    pub(super) fn cancel_selected(&mut self, id: Id, revision: u64, output: &SyncSender<Intent>) {
        if self.try_send(
            output,
            Intent::Habitat(HabitatCommand::CancelTask {
                id,
                expected_revision: revision,
            }),
        ) {
            self.modal = None;
            self.notice =
                "Cancellation requested. The task remains held until its worker settles.".into();
        }
    }

    pub(super) fn managed_cancel(&mut self, output: &SyncSender<Intent>) {
        if let Some(target) = &self.composer_target {
            let task = self
                .view
                .tasks
                .iter()
                .find(|row| row.id == target.task)
                .map(|row| (row.id.clone(), row.revision))
                .or_else(|| {
                    self.view
                        .backlog
                        .iter()
                        .find(|row| row.id == target.task)
                        .map(|row| (row.id.clone(), row.revision))
                });
            if let Some((id, revision)) = task {
                self.cancel_selected(id, revision, output);
            } else {
                self.notice = "The selected task is outside the current view. Reopen /agents before cancelling it.".into();
            }
            return;
        }
        let tasks: Vec<_> = self
            .view
            .backlog
            .iter()
            .filter(|row| self.view.conversation.as_ref() == Some(&row.conversation))
            .filter(|row| row.state == State::Working || row.state.attention() || row.deferred)
            .collect();
        if tasks.len() == 1 {
            self.cancel_selected(tasks[0].id.clone(), tasks[0].revision, output);
        } else if tasks.is_empty() {
            self.notice = "No cancellable work in this conversation.".into();
        } else {
            let items = tasks
                .iter()
                .map(|row| PickItem {
                    label: format!("{} · {} · {}", row.title, row.status, row.id),
                    action: PickAction::CancelTask {
                        id: row.id.clone(),
                        revision: row.revision,
                    },
                })
                .collect();
            self.picker("Cancel which task? · Enter requests cancellation", items);
        }
    }

    pub(super) fn recall_queued(&mut self, output: &SyncSender<Intent>) {
        if self.pending_habitat.len() >= 16 || !self.recovery_capacity_available() {
            self.notice = "Waiting for earlier input acknowledgements.".into();
            return;
        }
        if !self.composer.text().is_empty() || !self.attachments.is_empty() {
            self.notice = "Clear or save the current draft before recalling queued input.".into();
            return;
        }
        let Some(row) = self
            .view
            .backlog
            .iter()
            .filter(|row| {
                self.view.conversation.as_ref() == Some(&row.conversation)
                    && row.status.starts_with("queued")
            })
            .max_by_key(|row| row.updated_at_ms)
        else {
            self.notice = "No queued task to recall in this conversation.".into();
            return;
        };
        let operation = operation_id();
        let context = row.conversation.clone();
        let id = row.id.clone();
        let revision = row.revision;
        if self.try_send(
            output,
            Intent::Habitat(HabitatCommand::RecallQueued {
                id: id.clone(),
                expected_revision: revision,
                operation: operation.clone(),
            }),
        ) {
            self.pending_habitat.push_back(PendingHabitat {
                operation,
                context,
                task: Some(id),
                text: String::new(),
                target: None,
            });
            self.notice = "Recalling queued input only if dispatch has not started…".into();
        }
    }

    pub(super) fn accept_queued_draft(&mut self, context: Id, id: Id, operation: Id, text: String) {
        let pending = self.pending_habitat.iter().position(|row| {
            row.context == context && row.task.as_ref() == Some(&id) && row.operation == operation
        });
        if pending.is_none() {
            return;
        }
        self.pending_habitat
            .remove(pending.expect("checked pending recall"));
        self.composer.remember(&text);
        if view_context(&self.view).as_ref() == Some(&context)
            && self.composer.text().is_empty()
            && self.attachments.is_empty()
            && !self.pending_image
            && self.composer_target.is_none()
        {
            self.composer_target = None;
            self.composer.set_text(&text);
        } else {
            self.retain_rejected(context, text, Vec::new(), None);
        }
        self.notice =
            "Queued task recalled before dispatch. Edit the draft and send when ready.".into();
    }

    pub(super) fn rename_context(&mut self, title: &str, output: &SyncSender<Intent>) {
        let current = if let Some(id) = &self.view.conversation {
            self.view
                .conversations
                .iter()
                .find(|row| &row.id == id)
                .map(|row| {
                    (
                        TranscriptContext::Conversation(id.clone()),
                        row.title.clone(),
                    )
                })
        } else {
            self.view.session.as_ref().map(|session| {
                (
                    TranscriptContext::Session(session.id.clone()),
                    session.title.clone(),
                )
            })
        };
        if let Some((context, expected_title)) = current {
            if title.is_empty() {
                self.composer.set_text(&format!("/rename {expected_title}"));
            } else {
                self.send(
                    output,
                    Intent::Rename {
                        context,
                        expected_title,
                        title: title.into(),
                    },
                );
            }
        } else {
            self.notice = "Open a conversation or session before renaming it.".into();
        }
    }

    pub fn composer_target_label(&self) -> Option<String> {
        self.composer_target.as_ref().map(|target| {
            let state = self.view.backlog.iter().find(|row| row.id == target.task);
            let purpose = if target.answer_revision.is_some() {
                "Answer"
            } else {
                "Guide"
            };
            format!(
                "{purpose} {} · {} · Tab queues new work",
                target.task,
                state.map_or("outside current view", |row| row.status.as_str())
            )
        })
    }

    pub fn transcript_messages(&self) -> impl Iterator<Item = &Message> {
        let start = self
            .transcript_clear_before
            .as_ref()
            .and_then(|id| {
                self.view
                    .messages
                    .iter()
                    .position(|message| &message.id == id)
            })
            .map_or(0, |index| index + 1);
        self.view.messages.iter().skip(start)
    }

    pub fn shortcut_lines(&self) -> Vec<String> {
        let mut lines = vec![
            "Compose".into(),
            "Enter  Send / guide selected task     Tab  Queue new work".into(),
            format!(
                "{}  New line     Ctrl-G  External editor",
                if self.keyboard_enhanced {
                    "Shift-Enter"
                } else {
                    "Alt-Enter / Ctrl-J"
                }
            ),
            "Ctrl-A/E  Line start/end     Ctrl-B/F  Left/right".into(),
            "Ctrl-P/N  Up/down or prompt history     Alt-B/F  Word left/right".into(),
            "Ctrl-U/K  Kill to start/end     Ctrl-W / Alt-Backspace  Kill word".into(),
            "Alt-D  Kill next word     Ctrl-Y  Yank     Ctrl-R  Search history".into(),
            "Ctrl-V  Paste image/text     /attach  Add file     /detach  Remove file".into(),
            String::new(),
            "Session and transcript".into(),
            "Esc  Close surface / return to latest / interrupt".into(),
            "Ctrl-C  Close surface / clear draft / stop work / quit".into(),
            "Ctrl-D  Delete forward; quit only with an empty composer".into(),
            "Ctrl-T  Transcript     F3  Find text     Ctrl-O  Copy last answer".into(),
            "PgUp/PgDn  Scroll     Ctrl-Home/End  Top/latest".into(),
            "Ctrl-L  Clear display     F4  Expand tools     Alt-Up  Recall queued work".into(),
            "? / F1  Shortcuts     /resume  Sessions     /agents  Tasks".into(),
            "F2 / Alt-Down  Attention     Alt-Left/Right  Switch conversation (empty draft)".into(),
            String::new(),
            "Commands".into(),
        ];
        lines.extend(
            SLASH_COMMANDS
                .iter()
                .map(|entry| format!("{} {}  {}", entry.name, entry.args, entry.summary)),
        );
        lines
    }

    pub(super) fn open_history_search(&mut self) {
        let original = self.composer.text();
        let matches = self.composer.history().cloned().collect();
        self.modal = Some(Modal::HistorySearch {
            query: String::new(),
            original,
            matches,
            selected: 0,
        });
    }

    pub(super) fn history_search_event(&mut self, event: &Event) -> bool {
        let Some(Modal::HistorySearch {
            query,
            original,
            matches,
            selected,
        }) = &mut self.modal
        else {
            return false;
        };
        let mut accept = None;
        let mut restore = false;
        let mut edited = false;
        match event {
            Event::Paste(text) => {
                let text = xcb_core::display_text(text, 1024).replace(['\r', '\n'], " ");
                if query.len() + text.len() <= 1024 {
                    query.push_str(&text);
                    edited = true;
                }
            }
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                match key.code {
                    KeyCode::Esc => restore = true,
                    KeyCode::Char('c') if ctrl => restore = true,
                    KeyCode::Enter => accept = matches.get(*selected).cloned(),
                    KeyCode::Up | KeyCode::Char('r') if key.code == KeyCode::Up || ctrl => {
                        if !matches.is_empty() {
                            *selected = (*selected + 1).min(matches.len() - 1);
                        }
                    }
                    KeyCode::Down | KeyCode::Char('s') if key.code == KeyCode::Down || ctrl => {
                        *selected = selected.saturating_sub(1)
                    }
                    KeyCode::Backspace => {
                        query.pop();
                        edited = true;
                    }
                    KeyCode::Char('u') if ctrl => {
                        query.clear();
                        edited = true;
                    }
                    KeyCode::Char(ch)
                        if !key
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                            && query.len() + ch.len_utf8() <= 1024 =>
                    {
                        query.push(ch);
                        edited = true;
                    }
                    _ => (),
                }
            }
            _ => (),
        }
        if edited {
            let needle = query.to_lowercase();
            *matches = self
                .composer
                .history()
                .filter(|text| text.to_lowercase().contains(&needle))
                .cloned()
                .collect();
            *selected = 0;
        }
        if restore {
            let original = original.clone();
            self.modal = None;
            self.composer.set_text(&original);
        } else if let Some(text) = accept {
            let index = self.composer.history().position(|entry| entry == &text);
            self.modal = None;
            if let Some(index) = index {
                self.composer.recall_history(index);
            }
        }
        true
    }

    pub(super) fn last_answer(&self) -> Option<String> {
        self.view
            .messages
            .iter()
            .rev()
            .find(|message| message.role == Role::Assistant && !message.text.trim().is_empty())
            .map(|message| message.text.clone())
    }

    pub(super) fn copy_answer(&mut self) {
        let Some(text) = self.last_answer() else {
            self.notice = "No assistant response to copy yet.".into();
            return;
        };
        self.notice = match arboard::Clipboard::new()
            .and_then(|mut clipboard| clipboard.set_text(text))
        {
            Ok(()) => "Copied the last assistant response.".into(),
            Err(_) => {
                "Clipboard unavailable. Open Ctrl-T to select the response in your terminal.".into()
            }
        };
    }

    pub(super) fn clear_display(&mut self) {
        self.transcript_clear_before = self.view.messages.last().map(|message| message.id.clone());
        self.stream.clear();
        self.thinking.clear();
        self.paused.set(false);
        self.scroll.set(0);
        self.notice = "Display cleared. Ctrl-T opens saved history.".into();
    }

    pub(super) fn open_transcript(&mut self, search: bool) {
        self.history_messages = self.view.messages.clone();
        self.history_context = self
            .view
            .transcript
            .as_ref()
            .map(|page| page.context.clone());
        self.history_first = self
            .view
            .transcript
            .as_ref()
            .and_then(|page| page.first_sequence);
        self.history_more = self
            .view
            .transcript
            .as_ref()
            .is_some_and(|page| page.has_older);
        self.history_request = None;
        self.modal = Some(Modal::Transcript {
            title: "Transcript".into(),
            lines: Vec::new(),
            query: String::new(),
            scroll: 0,
            matches: Vec::new(),
            selected: 0,
            search,
            has_more: self.history_more,
        });
        self.rebuild_transcript();
    }

    fn rebuild_transcript(&mut self) {
        let Some(Modal::Transcript {
            lines,
            query,
            matches,
            has_more,
            ..
        }) = &mut self.modal
        else {
            return;
        };
        let mut result = Vec::new();
        for message in &self.history_messages {
            result.push(
                match message.role {
                    Role::User => "› You",
                    Role::Assistant => "• Assistant",
                    Role::System => "· System",
                    Role::Thinking => "· Reasoning",
                    Role::Tool => "· Tool",
                }
                .into(),
            );
            result.extend(message.text.lines().map(str::to_owned));
            result.push(String::new());
        }
        if !self.stream.is_empty() {
            result.push("• Assistant · streaming".into());
            result.extend(self.stream.lines().map(str::to_owned));
        }
        *lines = result;
        *has_more = self.history_more;
        let needle = query.to_lowercase();
        *matches = if needle.is_empty() {
            Vec::new()
        } else {
            lines
                .iter()
                .enumerate()
                .filter(|(_, line)| line.to_lowercase().contains(&needle))
                .map(|(index, _)| index)
                .collect()
        };
    }

    pub(super) fn accept_history_page(&mut self, request: Id, page: xcb_core::ui::TranscriptPage) {
        if self.history_request.as_ref() != Some(&(request, page.context.clone()))
            || self.history_context.as_ref() != Some(&page.context)
            || self
                .view
                .transcript
                .as_ref()
                .map(|current| &current.context)
                != Some(&page.context)
            || !matches!(self.modal, Some(Modal::Transcript { .. }))
        {
            return;
        }
        self.history_request = None;
        let bytes: usize = self
            .history_messages
            .iter()
            .chain(page.messages.iter())
            .map(|message| message.text.len())
            .sum();
        if bytes > 16 * 1024 * 1024 || self.history_messages.len() + page.messages.len() > 2048 {
            self.notice = "Loaded history reached its 16 MiB / 2,048-message limit. Reopen to return to recent history.".into();
            return;
        }
        let mut older = page.messages;
        older.retain(|message| {
            !self
                .history_messages
                .iter()
                .any(|known| known.id == message.id)
        });
        older.append(&mut self.history_messages);
        self.history_messages = older;
        self.history_first = page.first_sequence;
        self.history_more = page.has_older;
        self.rebuild_transcript();
        if let Some(Modal::Transcript { scroll, .. }) = &mut self.modal {
            *scroll = 0;
        }
        self.dirty = true;
    }

    fn request_older(&mut self, output: &SyncSender<Intent>) {
        if self.history_request.is_some() {
            self.notice = "Loading older messages…".into();
            return;
        }
        if !self.history_more {
            self.notice = "Beginning of saved history.".into();
            return;
        }
        if let (Some(context), Some(before_sequence)) =
            (self.history_context.clone(), self.history_first)
        {
            let request = operation_id();
            if self.try_send(
                output,
                Intent::TranscriptPage {
                    context: context.clone(),
                    before_sequence,
                    request: request.clone(),
                },
            ) {
                self.history_request = Some((request, context));
            }
        }
    }

    pub(super) fn transcript_event(&mut self, event: &Event, output: &SyncSender<Intent>) -> bool {
        let Some(Modal::Transcript {
            query,
            scroll,
            matches,
            selected,
            search,
            ..
        }) = &mut self.modal
        else {
            return false;
        };
        let mut older = false;
        let mut close = false;
        let mut changed = false;
        let page = u32::from(self.viewport_height.get().saturating_sub(5).max(1));
        match event {
            Event::Mouse(mouse) => match mouse.kind {
                MouseEventKind::ScrollUp => *scroll = scroll.saturating_sub(3),
                MouseEventKind::ScrollDown => *scroll = scroll.saturating_add(3),
                _ => (),
            },
            Event::Paste(text) if *search => {
                if query.len() + text.len() <= 1024 {
                    query.push_str(&xcb_core::display_text(text, 1024).replace(['\r', '\n'], " "));
                    changed = true;
                }
            }
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                match key.code {
                    KeyCode::Esc | KeyCode::Char('c') if key.code == KeyCode::Esc || ctrl => {
                        if *search {
                            *search = false;
                        } else {
                            close = true;
                        }
                    }
                    KeyCode::Char('t') if ctrl => close = true,
                    KeyCode::F(3) => *search = true,
                    KeyCode::Char('/') if !*search => *search = true,
                    KeyCode::Enter | KeyCode::Char('n') | KeyCode::Char('p')
                        if *search && (key.code == KeyCode::Enter || ctrl) =>
                    {
                        if !matches.is_empty() {
                            let back = key.modifiers.contains(KeyModifiers::SHIFT)
                                || key.code == KeyCode::Char('p');
                            *selected = if back {
                                (*selected + matches.len() - 1) % matches.len()
                            } else {
                                (*selected + 1) % matches.len()
                            };
                            *scroll = matches[*selected] as u32;
                        }
                    }
                    KeyCode::Backspace if *search => {
                        query.pop();
                        changed = true;
                    }
                    KeyCode::Char('u') if *search && ctrl => {
                        query.clear();
                        changed = true;
                    }
                    KeyCode::Char(ch)
                        if *search
                            && !key
                                .modifiers
                                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                            && query.len() + ch.len_utf8() <= 1024 =>
                    {
                        query.push(ch);
                        changed = true;
                    }
                    KeyCode::Char('p') if !*search && !ctrl => older = true,
                    KeyCode::Home => *scroll = 0,
                    KeyCode::End => *scroll = u32::MAX,
                    KeyCode::Up | KeyCode::Char('k') => *scroll = scroll.saturating_sub(1),
                    KeyCode::Down | KeyCode::Char('j') => *scroll = scroll.saturating_add(1),
                    KeyCode::PageUp => {
                        if *scroll == 0 {
                            older = true;
                        } else {
                            *scroll = scroll.saturating_sub(page);
                        }
                    }
                    KeyCode::PageDown | KeyCode::Char(' ') => *scroll = scroll.saturating_add(page),
                    _ => (),
                }
            }
            _ => (),
        }
        if changed {
            self.rebuild_transcript();
            if let Some(Modal::Transcript {
                matches,
                selected,
                scroll,
                ..
            }) = &mut self.modal
            {
                *selected = 0;
                if let Some(first) = matches.first() {
                    *scroll = *first as u32;
                }
            }
        }
        if older {
            self.request_older(output);
        }
        if close {
            self.modal = None;
            self.history_request = None;
        }
        true
    }
}

impl LivePicker {
    fn items(&self, view: &View) -> Vec<PickItem> {
        match self {
            Self::Tasks => view
                .tasks
                .iter()
                .map(|row| PickItem {
                    label: format!(
                        "{} · {} · {} · {}",
                        row.title,
                        task_status(row),
                        row.id,
                        row.detail
                    ),
                    action: PickAction::Task(row.id.clone()),
                })
                .collect(),
            Self::Backlog { all, attention } => view
                .backlog
                .iter()
                .filter(|row| *all || view.conversation.as_ref() == Some(&row.conversation))
                .filter(|row| !*attention || row.state.attention())
                .map(|row| PickItem {
                    label: format!(
                        "{} · {} · P{} · {}",
                        row.title, row.status, row.priority, row.id
                    ),
                    action: PickAction::Backlog(row.id.clone()),
                })
                .collect(),
            Self::Schedules { all } => view
                .schedules
                .iter()
                .filter(|row| *all || view.conversation.as_ref() == Some(&row.conversation))
                .map(|row| PickItem {
                    label: format!(
                        "{} · {} · {}",
                        row.prompt,
                        if row.enabled { "enabled" } else { "paused" },
                        row.id
                    ),
                    action: PickAction::Schedule(row.id.clone()),
                })
                .collect(),
            Self::Projects { all } => view
                .projects
                .iter()
                .filter(|row| *all || view.conversation.as_ref() == Some(&row.conversation))
                .map(|row| PickItem {
                    label: format!(
                        "{} · {} · {} tasks · {}",
                        row.goal, row.status, row.remaining_tasks, row.conversation
                    ),
                    action: PickAction::Project(row.conversation.clone()),
                })
                .collect(),
            Self::Conversations => std::iter::once(PickItem {
                label: "＋ new conversation".into(),
                action: PickAction::NewConversation,
            })
            .chain(view.conversations.iter().map(|row| PickItem {
                label: format!("{} · {} msgs · {}", row.title, row.messages, row.workspace),
                action: PickAction::Conversation(row.id.clone()),
            }))
            .collect(),
            Self::Sessions => view
                .sessions
                .iter()
                .map(|row| PickItem {
                    label: format!(
                        "{} · {} · {}",
                        row.title,
                        row.model.label,
                        row.state.label()
                    ),
                    action: PickAction::Session(row.id.clone()),
                })
                .collect(),
        }
    }
}

fn inspect_backlog(row: &xcb_core::ui::BacklogRow) -> Modal {
    Modal::Inspect {
        title: format!("{} · {}", row.id, row.status),
        lines: vec![
            row.title.clone(),
            format!(
                "{} · P{} · revision {}",
                row.conversation, row.priority, row.revision
            ),
            format!("attention: {}", row.state.label()),
            String::new(),
            "Prompt".into(),
            row.prompt.clone(),
            String::new(),
            "Latest summary".into(),
            row.summary.clone(),
            String::new(),
            "Input cannot grant host or provider permission.".into(),
        ],
        scroll: 0,
    }
}
