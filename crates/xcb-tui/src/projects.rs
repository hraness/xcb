//! The thread's project controls: focus, the authority ladder, `/workspace`,
//! the which-project picker and held-task chips.
use super::*;

impl App {
    /// The selected task's directory: the task the composer is addressing.
    fn selected_task_workspace(&self) -> Option<String> {
        let target = self.composer_target.as_ref()?;
        self.view
            .tasks
            .iter()
            .find(|row| row.id == target.task)
            .map(|row| row.workspace.clone())
            .or_else(|| {
                self.view
                    .backlog
                    .iter()
                    .find(|row| row.id == target.task)
                    .map(|row| row.workspace.clone())
            })
            .filter(|workspace| !workspace.is_empty())
    }

    /// The directory an authority command acts on without an explicit
    /// argument (I7): the project view's directory, then the thread's focus,
    /// then the selected task's directory. Nothing is ever inferred.
    pub(crate) fn authority_workspace(&self) -> Option<String> {
        open_workspace(&self.view)
            .map(str::to_owned)
            .or_else(|| self.view.focus.clone().filter(|_| in_thread(&self.view)))
            .or_else(|| self.selected_task_workspace())
    }

    /// The directory a thread schedule or backlog item is fixed to at
    /// keystroke time. Views leave it to their own directory; in the thread
    /// `None` makes the runtime open the picker and write nothing.
    pub(crate) fn standing_workspace(&self) -> Option<String> {
        in_thread(&self.view)
            .then(|| self.authority_workspace())
            .flatten()
    }

    /// A list row's project: the directory name, or the conversation id
    /// when a legacy row has no directory.
    pub(crate) fn project_label(&self, workspace: &str, conversation: &Id) -> String {
        if workspace.is_empty() {
            conversation.as_str().to_owned()
        } else {
            workspace_name(&self.view, workspace)
        }
    }

    /// Thread tasks still waiting out an uncertain binding, with the whole
    /// seconds left before their first dispatch.
    pub fn held_tasks(&self, now: u64) -> Vec<(&xcb_core::ui::TaskRow, u64)> {
        if !in_thread(&self.view) {
            return Vec::new();
        }
        self.view
            .tasks
            .iter()
            .filter(|task| task.route.is_none() && task_queued(task))
            .filter_map(|task| {
                let until = task.hold_until_ms?;
                (until > now).then(|| (task, (until - now).div_ceil(1000)))
            })
            .collect()
    }

    /// The directory a thread message was bound to, for its transcript chip.
    pub fn message_workspace(&self, message: &Id) -> Option<&str> {
        if !in_thread(&self.view) {
            return None;
        }
        let page = self.view.transcript.as_ref()?;
        let index = page
            .messages
            .iter()
            .rposition(|candidate| &candidate.id == message)?;
        page.workspace_of(index)
    }

    /// `/workspace` and its subcommands. Directory arguments resolve against
    /// the known projects here; paths and containers are checked again by
    /// the runtime, which echoes what it did.
    pub(crate) fn workspace_command(
        &mut self,
        action: &str,
        tail: &str,
        output: &SyncSender<Intent>,
    ) {
        if !in_thread(&self.view) {
            self.notice = "Focus applies to the thread. Open it with /sessions.".into();
            return;
        }
        match action {
            "" => {
                let items = workspace_items(&self.view.workspaces, false);
                if items.is_empty() {
                    self.notice = "No known projects yet. /workspace add <dir> admits one.".into();
                } else {
                    self.picker("Focus a project", items);
                }
            }
            "clear" | "all" if tail.is_empty() => {
                self.send(output, Intent::Focus(None));
            }
            "add" if !tail.is_empty() => {
                self.send(output, Intent::AddWorkspace { path: tail.into() });
            }
            "add" => self.notice = "Use /workspace add <dir>.".into(),
            "go" => {
                let held = if tail.is_empty() {
                    let now = display_now_ms();
                    let held = self.held_tasks(now);
                    held.iter()
                        .find(|(task, _)| Some(&task.id) == self.last_bound.as_ref())
                        .or_else(|| held.first().filter(|_| held.len() == 1))
                        .map(|(task, _)| (task.id.clone(), task.revision))
                } else {
                    self.view
                        .tasks
                        .iter()
                        .find(|task| task.id.as_str() == tail)
                        .map(|task| (task.id.clone(), task.revision))
                };
                match held {
                    Some((task, revision)) => {
                        self.send(output, Intent::ReleaseHold { task, revision })
                    }
                    None => {
                        self.notice =
                            "No held task to start. /workspace go <task> names one.".into()
                    }
                }
            }
            "move" => {
                let (task, target) = tail.split_once(' ').unwrap_or((tail, ""));
                let target = target.trim();
                match self.view.tasks.iter().find(|row| row.id.as_str() == task) {
                    Some(row) if !target.is_empty() => {
                        let target = resolve_project(&self.view, target)
                            .unwrap_or_else(|_| target.to_owned());
                        let intent = Intent::MoveTask {
                            task: row.id.clone(),
                            revision: row.revision,
                            target,
                        };
                        self.send(output, intent);
                    }
                    _ => {
                        self.notice =
                            "Use /workspace move <task> <name|path>; only unstarted tasks move."
                                .into()
                    }
                }
            }
            _ => {
                let value = if tail.is_empty() {
                    action.to_owned()
                } else {
                    format!("{action} {tail}")
                };
                // A known name resolves here so an ambiguous one never
                // reaches the runtime; paths snap there.
                let target = match resolve_project(&self.view, &value) {
                    Ok(path) => path,
                    Err(_) if value.contains('/') || value.starts_with('~') => value,
                    Err(reason) => {
                        self.notice = reason;
                        return;
                    }
                };
                if let Some(task) = self.last_bound.as_ref().and_then(|id| {
                    self.view.tasks.iter().find(|task| {
                        &task.id == id
                            && task_queued(task)
                            && task.route.is_none()
                            && task.workspace != target
                    })
                }) {
                    let intent = Intent::MoveTask {
                        task: task.id.clone(),
                        revision: task.revision,
                        target: target.clone(),
                    };
                    self.send(output, intent);
                }
                self.send(output, Intent::Focus(Some(target)));
            }
        }
    }

    /// A picked project: admit a new one, focus it, then resend the draft
    /// the which-project question kept.
    pub(crate) fn pick_workspace(
        &mut self,
        path: String,
        new: bool,
        resubmit: bool,
        output: &SyncSender<Intent>,
    ) {
        if new && !self.try_send(output, Intent::AddWorkspace { path: path.clone() }) {
            return;
        }
        if !self.try_send(output, Intent::Focus(Some(path))) {
            return;
        }
        if resubmit && (!self.composer.text().trim().is_empty() || !self.attachments.is_empty()) {
            self.handle(
                Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
                output,
            );
        }
    }

    /// Alt-←/→ in the thread: move the focus along the overview's project
    /// order, wrapping through "all projects".
    pub(crate) fn cycle_focus(&mut self, forward: bool, output: &SyncSender<Intent>) {
        let mut order: Vec<String> = Vec::new();
        for row in agent_grid::project_order(self) {
            if !order.contains(&row) {
                order.push(row);
            }
        }
        if order.is_empty() {
            self.notice = "No projects in the overview yet.".into();
            return;
        }
        let current = self
            .view
            .focus
            .as_ref()
            .and_then(|focus| order.iter().position(|row| row == focus));
        let len = order.len();
        let next = match (current, forward) {
            (None, true) => Some(0),
            (None, false) => Some(len - 1),
            (Some(index), true) => (index + 1 < len).then_some(index + 1),
            (Some(index), false) => index.checked_sub(1),
        };
        self.send(
            output,
            Intent::Focus(next.map(|index| order[index].clone())),
        );
    }
}
