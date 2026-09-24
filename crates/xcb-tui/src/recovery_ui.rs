//! Recovery is explicit copying into an empty matching context, never submission or acknowledgement.
use super::*;
use input_recovery::{RecoveryEntryKind, RecoveryInput, RecoverySnapshot};
use std::{
    hash::{Hash, Hasher},
    path::PathBuf,
};

const MAX_RETAINED: usize = 32;

#[derive(Clone)]
pub(super) struct RetainedInput {
    pub context: Id,
    pub text: String,
    pub attachments: Vec<Attachment>,
    pub target: Option<interaction::ComposerTarget>,
    pub origin: Option<RecoveryInput>,
}
#[derive(Clone)]
pub(super) enum RecoveryChoice {
    Journal(usize),
    Input {
        journal: usize,
        input: Option<usize>,
    },
    ConfirmUnbound {
        journal: usize,
        input: Option<usize>,
        destination: Id,
    },
    History(usize),
    Retained(usize),
    DiscardJournal(usize),
    DiscardRetained(usize),
}

impl App {
    pub(super) fn configure_recovery(&mut self, directory: PathBuf) {
        self.recovery_directory = Some(directory.clone());
        match input_recovery::candidates(&directory) {
            Ok(entries) => {
                // Load history from at most one recent valid inactive journal. Drafts stay explicit.
                for entry in entries
                    .iter()
                    .filter(|entry| !entry.live && entry.kind != RecoveryEntryKind::Reservation)
                    .take(8)
                {
                    if let Ok(snapshot) = input_recovery::read(entry) {
                        let mut history = self.composer.history().cloned().collect::<Vec<_>>();
                        history.extend(snapshot.history);
                        self.composer.restore_history(history);
                        break;
                    }
                }
                let count = entries.iter().filter(|entry| !entry.live).count();
                self.recovery_entries = entries;
                if count > 0 {
                    self.notice = format!(
                        "{count} earlier terminal journals available with /drafts. Input is never sent automatically."
                    );
                }
            }
            Err(error) => self.notice = format!("Could not list recovered input: {error}"),
        }
        match input_recovery::RecoveryJournal::new(&directory) {
            Ok(journal) => self.recovery = Some(journal),
            Err(error) => self.notice = format!("Input recovery is not saving: {error}"),
        }
        self.flush_recovery(true);
    }

    /// Called after state changes/ticks and with `force` before normal terminal exit.
    pub(super) fn flush_recovery(&mut self, force: bool) -> bool {
        if self.recovery_directory.is_none() {
            return true;
        }
        if !force
            && self
                .recovery_saved
                .is_some_and(|saved| saved.elapsed() < Duration::from_millis(750))
        {
            return false;
        }
        if self.recovery.is_none() {
            if !force {
                return false;
            }
            let Some(directory) = self.recovery_directory.as_ref() else {
                return true;
            };
            match input_recovery::RecoveryJournal::new(directory) {
                Ok(journal) => self.recovery = Some(journal),
                Err(error) => {
                    self.notice = format!("Input recovery is not saving: {error}");
                    return false;
                }
            }
        }
        let snapshot = self.recovery_snapshot();
        let fingerprint = snapshot_fingerprint(&snapshot);
        if self.recovery_saved.is_some() && fingerprint == self.recovery_fingerprint {
            self.recovery_saved = Some(Instant::now());
            if self
                .recovery
                .as_ref()
                .expect("journal created")
                .verify_current()
                .is_ok()
            {
                return true;
            }
        }
        let result = self
            .recovery
            .as_mut()
            .expect("journal created")
            .save(&snapshot);
        self.recovery_saved = Some(Instant::now());
        match result {
            Ok(()) => {
                self.recovery_fingerprint = fingerprint;
                true
            }
            Err(error) => {
                self.notice =
                    format!("Input recovery could not save; current input is still here: {error}");
                self.dirty = true;
                false
            }
        }
    }

    fn recovery_snapshot(&self) -> RecoverySnapshot {
        let text = match &self.modal {
            Some(Modal::Editor {
                textarea,
                kind: EditorKind::Prompt,
                ..
            }) => textarea.lines().join("\n"),
            _ => self.composer.text(),
        };
        let mut other_inputs = self
            .drafts
            .iter()
            .map(|(id, draft)| {
                with_origin(
                    id,
                    &draft.text,
                    &draft.attachments,
                    draft.target.as_ref(),
                    draft.origin.as_ref(),
                )
            })
            .collect::<Vec<_>>();
        other_inputs.extend(self.recovery_extras.iter().map(|entry| {
            with_origin(
                &entry.context,
                &entry.text,
                &entry.attachments,
                entry.target.as_ref(),
                entry.origin.as_ref(),
            )
        }));
        other_inputs.extend(self.pending_echoes.iter().map(|entry| RecoveryInput {
            context: context_key(entry.session.as_ref()),
            text: entry.text.clone(),
            attachments: entry.recovery_attachments.clone(),
            uncertain_pending: true,
            operation: Some(entry.id.to_string()),
            task: None,
        }));
        other_inputs.extend(
            self.pending_habitat
                .iter()
                .filter(|entry| !entry.text.is_empty())
                .map(|entry| RecoveryInput {
                    context: context_key(Some(&entry.context)),
                    text: entry.text.clone(),
                    attachments: Vec::new(),
                    uncertain_pending: true,
                    operation: Some(entry.operation.to_string()),
                    task: entry.task.as_ref().map(ToString::to_string),
                }),
        );
        let context = context_key(view_context(&self.view).as_ref());
        let origin = self.recovery_composer_origin.as_ref().filter(|origin| {
            origin.context == context
                && origin.text == text
                && origin.attachments == self.attachments
        });
        RecoverySnapshot {
            text,
            history: self.composer.history().cloned().collect(),
            context,
            attachments: self.attachments.clone(),
            uncertain_pending: origin.is_some_and(|origin| origin.uncertain_pending),
            task: self
                .composer_target
                .as_ref()
                .map(|target| target.task.to_string())
                .or_else(|| origin.and_then(|origin| origin.task.clone())),
            operation: origin.and_then(|origin| origin.operation.clone()),
            other_inputs,
        }
    }

    pub(super) fn recovery_capacity_available(&self) -> bool {
        self.recovery_extras.len() + self.pending_echoes.len() + self.pending_habitat.len()
            < MAX_RETAINED
    }

    /// Call only for an exact rejected request or a draft being displaced, never an uncertain send.
    pub(super) fn retain_rejected(
        &mut self,
        context: Id,
        text: String,
        attachments: Vec<Attachment>,
        target: Option<interaction::ComposerTarget>,
    ) {
        self.retain_rejected_with_origin(context, text, attachments, target, None);
    }

    pub(super) fn retain_rejected_with_origin(
        &mut self,
        context: Id,
        text: String,
        attachments: Vec<Attachment>,
        target: Option<interaction::ComposerTarget>,
        origin: Option<RecoveryInput>,
    ) {
        if text.is_empty() && attachments.is_empty() {
            return;
        }
        if view_context(&self.view).as_ref() == Some(&context)
            && self.composer.text().is_empty()
            && self.attachments.is_empty()
            && !self.pending_image
            && self.composer_target == target
        {
            self.composer.set_text(&text);
            self.attachments = attachments;
            self.composer_target = target;
            self.recovery_composer_origin = origin;
            return;
        }
        // Submission admission reserves one retained slot for every outstanding request.
        // Preserve overflow in memory as well; serialization refuses oversized snapshots visibly.
        self.recovery_extras.push_back(RetainedInput {
            context,
            text,
            attachments,
            target,
            origin,
        });
        self.notice =
            "Earlier input is retained in /drafts; your current draft is unchanged.".into();
        self.flush_recovery(true);
    }

    pub(super) fn open_recovery(&mut self) {
        if self.initial_view_pending {
            self.notice =
                "Opening your conversation; /drafts will be available when it loads.".into();
            return;
        }
        self.recovery_choices.clear();
        let mut items = Vec::new();
        for (index, retained) in self.recovery_extras.iter().enumerate() {
            let label = format!(
                "This terminal · {} · {}",
                retained.context,
                preview(&retained.text)
            );
            let choice = self.recovery_choices.len();
            self.recovery_choices.push(RecoveryChoice::Retained(index));
            items.push(PickItem {
                label,
                action: PickAction::Recovery(choice),
            });
        }
        if let Some(directory) = &self.recovery_directory {
            match input_recovery::candidates(directory) {
                Ok(entries) => self.recovery_entries = entries,
                Err(error) => {
                    self.notice = format!("Could not list recovered input: {error}");
                    return;
                }
            }
        }
        for (index, entry) in self
            .recovery_entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| !entry.live)
        {
            let age = SystemTime::now()
                .duration_since(entry.saved_at)
                .unwrap_or_default()
                .as_secs();
            let kind = match entry.kind {
                RecoveryEntryKind::Snapshot => "Saved terminal",
                RecoveryEntryKind::Staged => "Interrupted save",
                RecoveryEntryKind::Reservation => "Inactive reservation",
            };
            let label = format!(
                "{kind} · {} · {age}s ago",
                entry
                    .path()
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
            );
            let choice = self.recovery_choices.len();
            self.recovery_choices.push(RecoveryChoice::Journal(index));
            items.push(PickItem {
                label,
                action: PickAction::Recovery(choice),
            });
        }
        if items.is_empty() {
            self.notice = "No inactive terminal journals or retained rejected input. Live terminals keep their own drafts.".into();
            return;
        }
        self.picker(
            "Input recovery · Enter opens · Ctrl-D discards · Esc closes",
            items,
        );
    }

    pub(super) fn recover_entry(&mut self, choice: usize) {
        let Some(choice) = self.recovery_choices.get(choice).cloned() else {
            return;
        };
        match choice {
            RecoveryChoice::Journal(journal) => {
                let Some(entry) = self.recovery_entries.get(journal) else {
                    return;
                };
                if entry.kind == RecoveryEntryKind::Reservation {
                    self.recovery_choices = vec![RecoveryChoice::DiscardJournal(journal)];
                    self.picker("Inactive reservation · Enter discards this file · Esc keeps it", vec![PickItem {
                        label: "No draft snapshot in this reservation. Discard this inactive reservation file to free its slot.".into(),
                        action: PickAction::Recovery(0),
                    }]);
                    return;
                }
                let snapshot = match input_recovery::read(entry) {
                    Ok(snapshot) => snapshot,
                    Err(error) => {
                        self.notice = format!(
                            "Could not read recovered input: {error}. File kept at {}. Open /drafts, select this file, then Ctrl-D to discard it.",
                            entry.path().display()
                        );
                        return;
                    }
                };
                self.recovery_choices.clear();
                let mut items = Vec::new();
                if !snapshot.text.is_empty() || !snapshot.attachments.is_empty() {
                    self.recovery_choices.push(RecoveryChoice::Input {
                        journal,
                        input: None,
                    });
                    items.push(PickItem {
                        label: input_label(&main_input(&snapshot)),
                        action: PickAction::Recovery(0),
                    });
                }
                for (input, value) in snapshot
                    .other_inputs
                    .iter()
                    .enumerate()
                    .filter(|(_, value)| !value.text.is_empty() || !value.attachments.is_empty())
                {
                    let choice = self.recovery_choices.len();
                    self.recovery_choices.push(RecoveryChoice::Input {
                        journal,
                        input: Some(input),
                    });
                    items.push(PickItem {
                        label: input_label(value),
                        action: PickAction::Recovery(choice),
                    });
                }
                let choice = self.recovery_choices.len();
                self.recovery_choices.push(RecoveryChoice::History(journal));
                items.push(PickItem {
                    label: format!(
                        "Add {} complete prompts to Ctrl-R history",
                        snapshot.history.len()
                    ),
                    action: PickAction::Recovery(choice),
                });
                self.picker(
                    "Saved input · matching context only · Ctrl-D discards this saved file",
                    items,
                );
            }
            RecoveryChoice::Input { journal, input } => {
                let Some(entry) = self.recovery_entries.get(journal) else {
                    return;
                };
                let snapshot = match input_recovery::read(entry) {
                    Ok(snapshot) => snapshot,
                    Err(error) => {
                        self.notice = format!("Could not read recovered input: {error}");
                        return;
                    }
                };
                let value = match input {
                    Some(index) => match snapshot.other_inputs.get(index) {
                        Some(value) => value.clone(),
                        None => return,
                    },
                    None => main_input(&snapshot),
                };
                if self.unbound_matches_loaded_workspace(&value)
                    && view_context(&self.view).is_some()
                {
                    if !self.composer.text().is_empty()
                        || !self.attachments.is_empty()
                        || self.pending_image
                    {
                        self.notice = "Current input is newer. Finish or clear it before recovering another draft.".into();
                        return;
                    }
                    let destination = view_context(&self.view).expect("checked loaded context");
                    self.recovery_choices = vec![RecoveryChoice::ConfirmUnbound {
                        journal,
                        input,
                        destination: destination.clone(),
                    }];
                    self.picker(
                        "Confirm draft copy · Enter copies for review · Esc keeps it saved",
                        vec![PickItem {
                            label: format!(
                                "Copy this workspace's unbound input into {destination}; {}",
                                if value.uncertain_pending {
                                    "it may already have been sent"
                                } else {
                                    "nothing will be sent"
                                }
                            ),
                            action: PickAction::Recovery(0),
                        }],
                    );
                    return;
                }
                if self.restore_recovered_input(&value) {
                    self.modal = None;
                    self.flush_recovery(true);
                }
            }
            RecoveryChoice::ConfirmUnbound {
                journal,
                input,
                destination,
            } => {
                if view_context(&self.view).as_ref() != Some(&destination) {
                    self.notice = "The destination changed. Open /drafts and choose again; nothing was copied.".into();
                    return;
                }
                let Some(entry) = self.recovery_entries.get(journal) else {
                    return;
                };
                let snapshot = match input_recovery::read(entry) {
                    Ok(snapshot) => snapshot,
                    Err(error) => {
                        self.notice = format!("Could not read recovered input: {error}");
                        return;
                    }
                };
                let mut value = match input {
                    Some(index) => match snapshot.other_inputs.get(index) {
                        Some(value) => value.clone(),
                        None => return,
                    },
                    None => main_input(&snapshot),
                };
                if !self.unbound_matches_loaded_workspace(&value) {
                    self.notice =
                        "The saved input does not match this workspace. Nothing was copied.".into();
                    return;
                }
                value.context = context_key(Some(&destination));
                if self.restore_recovered_input(&value) {
                    self.modal = None;
                    self.flush_recovery(true);
                }
            }
            RecoveryChoice::History(journal) => {
                let Some(entry) = self.recovery_entries.get(journal) else {
                    return;
                };
                match input_recovery::read(entry) {
                    Ok(snapshot) => {
                        let mut history = self.composer.history().cloned().collect::<Vec<_>>();
                        history.extend(snapshot.history);
                        self.composer.restore_history(history);
                        self.modal = None;
                        self.notice = "Prompt history added within the 200-prompt / 1 MiB limit. Current input is unchanged.".into();
                        self.flush_recovery(true);
                    }
                    Err(error) => {
                        self.notice = format!("Could not read recovered history: {error}")
                    }
                }
            }
            RecoveryChoice::Retained(index) => {
                let Some(retained) = self.recovery_extras.get(index).cloned() else {
                    return;
                };
                let value = with_origin(
                    &retained.context,
                    &retained.text,
                    &retained.attachments,
                    retained.target.as_ref(),
                    retained.origin.as_ref(),
                );
                if self.restore_recovered_input(&value) {
                    self.recovery_extras.remove(index);
                    self.modal = None;
                    self.flush_recovery(true);
                }
            }
            RecoveryChoice::DiscardJournal(index) => {
                let Some(entry) = self.recovery_entries.get(index) else {
                    return;
                };
                match input_recovery::remove(entry) {
                    Ok(()) => {
                        self.modal = None;
                        self.notice = "Selected inactive saved file discarded, including the input and history it contained.".into();
                        self.flush_recovery(true);
                    }
                    Err(error) => self.notice = format!("The journal was not discarded: {error}"),
                }
            }
            RecoveryChoice::DiscardRetained(index) => {
                if self.recovery_extras.remove(index).is_some() {
                    self.modal = None;
                    self.notice = "Selected retained input discarded.".into();
                    self.flush_recovery(true);
                }
            }
        }
    }

    fn restore_recovered_input(&mut self, input: &RecoveryInput) -> bool {
        let current = context_key(view_context(&self.view).as_ref());
        if current == "unbound:unavailable" {
            self.notice = "Open a conversation or session before recovering input; the current workspace could not be identified.".into();
            return false;
        }
        if input.context != current {
            self.notice = format!(
                "This input belongs to {}. Use /resume to open that context, then /drafts again.",
                input.context
            );
            return false;
        }
        if !self.composer.text().is_empty() || !self.attachments.is_empty() || self.pending_image {
            self.notice =
                "Current input is newer. Finish or clear it before recovering another draft."
                    .into();
            return false;
        }
        self.composer.set_text(&input.text);
        self.attachments = input.attachments.clone();
        self.composer_target = None;
        self.recovery_composer_origin = Some(input.clone());
        self.notice = if input.uncertain_pending {
            "Recovered a request whose send result is unknown. Check the conversation/task before sending again; nothing was sent now."
        } else if input.task.is_some() {
            "Draft recovered without a task target. Review it and choose the intended task before sending."
        } else {
            "Draft recovered for review. Nothing was sent; the original journal remains until explicitly discarded."
        }.into();
        true
    }

    fn unbound_matches_loaded_workspace(&self, input: &RecoveryInput) -> bool {
        if !input.context.starts_with("unbound:") {
            return false;
        }
        let workspace = if let Some(conversation) = &self.view.conversation {
            self.view
                .conversations
                .iter()
                .find(|row| &row.id == conversation)
                .map(|row| row.workspace.as_str())
        } else {
            self.view
                .session
                .as_ref()
                .map(|session| session.workspace.as_str())
        };
        workspace
            .and_then(|workspace| std::path::Path::new(workspace).canonicalize().ok())
            .and_then(|path| path.to_str().map(|path| format!("unbound:{path}")))
            .is_some_and(|workspace| workspace == input.context)
    }

    pub(super) fn recovery_event(&mut self, event: &Event) -> bool {
        let Event::Key(key) = event else {
            return false;
        };
        if key.kind == KeyEventKind::Release
            || key.code != KeyCode::Char('d')
            || !key.modifiers.contains(KeyModifiers::CONTROL)
        {
            return false;
        }
        let Some(Modal::Picker {
            query,
            items,
            selected,
            ..
        }) = &self.modal
        else {
            return false;
        };
        let Some(PickItem {
            action: PickAction::Recovery(index),
            ..
        }) = items
            .iter()
            .filter(|item| interaction::item_matches(item, query))
            .nth(*selected)
        else {
            return false;
        };
        let Some(choice) = self.recovery_choices.get(*index).cloned() else {
            return true;
        };
        let (choice, label) = match choice {
            RecoveryChoice::Journal(index)
            | RecoveryChoice::History(index)
            | RecoveryChoice::Input { journal: index, .. } => (
                RecoveryChoice::DiscardJournal(index),
                "Discard this inactive saved file, including every draft and history entry it contains"
                    .to_owned(),
            ),
            RecoveryChoice::Retained(index) => (
                RecoveryChoice::DiscardRetained(index),
                "Discard this retained input and its attachment metadata".to_owned(),
            ),
            RecoveryChoice::DiscardJournal(_)
            | RecoveryChoice::DiscardRetained(_)
            | RecoveryChoice::ConfirmUnbound { .. } => return true,
        };
        self.recovery_choices.clear();
        self.recovery_choices.push(choice);
        self.picker(
            "Confirm discard · Enter discards permanently · Esc keeps input",
            vec![PickItem {
                label,
                action: PickAction::Recovery(0),
            }],
        );
        true
    }
}

use std::time::SystemTime;
fn with_origin(
    context: &Id,
    text: &str,
    attachments: &[Attachment],
    target: Option<&interaction::ComposerTarget>,
    origin: Option<&RecoveryInput>,
) -> RecoveryInput {
    let context = context_key(Some(context));
    let mut input = origin
        .filter(|origin| {
            origin.context == context && origin.text == text && origin.attachments == attachments
        })
        .cloned()
        .unwrap_or_else(|| RecoveryInput {
            context,
            text: text.to_owned(),
            attachments: attachments.to_vec(),
            ..Default::default()
        });
    if let Some(target) = target {
        input.task = Some(target.task.to_string());
    }
    input
}
fn context_key(id: Option<&Id>) -> String {
    id.map_or_else(
        || {
            format!(
                "unbound:{}",
                std::env::current_dir()
                    .ok()
                    .and_then(|path| path.canonicalize().ok())
                    .and_then(|path| path.to_str().map(str::to_owned))
                    .unwrap_or_else(|| "unavailable".into())
            )
        },
        |id| format!("context:{id}"),
    )
}
fn main_input(snapshot: &RecoverySnapshot) -> RecoveryInput {
    RecoveryInput {
        context: snapshot.context.clone(),
        text: snapshot.text.clone(),
        attachments: snapshot.attachments.clone(),
        uncertain_pending: snapshot.uncertain_pending,
        task: snapshot.task.clone(),
        operation: snapshot.operation.clone(),
    }
}
fn preview(text: &str) -> String {
    xcb_core::display_text(text, 96).replace(['\n', '\t'], " ")
}
fn input_label(input: &RecoveryInput) -> String {
    format!(
        "{} · {}{} · {}",
        input.context,
        if input.uncertain_pending {
            "send result unknown"
        } else {
            "draft"
        },
        input
            .task
            .as_ref()
            .map_or(String::new(), |task| format!(" · task {task}")),
        preview(&input.text)
    )
}
fn snapshot_fingerprint(snapshot: &RecoverySnapshot) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    snapshot.context.hash(&mut hasher);
    snapshot.text.hash(&mut hasher);
    snapshot.history.hash(&mut hasher);
    snapshot.uncertain_pending.hash(&mut hasher);
    snapshot.task.hash(&mut hasher);
    snapshot.operation.hash(&mut hasher);
    hash_attachments(&snapshot.attachments, &mut hasher);
    for input in &snapshot.other_inputs {
        input.context.hash(&mut hasher);
        input.text.hash(&mut hasher);
        input.uncertain_pending.hash(&mut hasher);
        input.operation.hash(&mut hasher);
        input.task.hash(&mut hasher);
        hash_attachments(&input.attachments, &mut hasher);
    }
    hasher.finish()
}
fn hash_attachments(attachments: &[Attachment], hasher: &mut impl Hasher) {
    attachments.len().hash(hasher);
    for item in attachments {
        item.digest.hash(hasher);
        item.media_type.hash(hasher);
        item.bytes.hash(hasher);
        item.width.hash(hasher);
        item.height.hash(hasher);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct TestState(PathBuf);
    impl TestState {
        fn new() -> Self {
            Self(
                std::env::temp_dir().join(format!("xcb-recovery-ui-test-{}", uuid::Uuid::new_v4())),
            )
        }
    }
    impl Drop for TestState {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn id(value: &str) -> Id {
        Id::new(value).unwrap()
    }
    #[test]
    fn recovery_never_overwrites_or_crosses_contexts_or_restores_authority() {
        let mut app = App::default();
        app.view.conversation = Some(id("c_first"));
        let mut input = RecoveryInput {
            context: context_key(Some(&id("c_other"))),
            text: "recovered".into(),
            ..Default::default()
        };
        assert!(!app.restore_recovered_input(&input));
        input.context = context_key(Some(&id("c_first")));
        app.composer.set_text("newer");
        assert!(!app.restore_recovered_input(&input));
        assert_eq!(app.composer.text(), "newer");
        app.composer.set_text("");
        input.uncertain_pending = true;
        input.task = Some("task_original".into());
        input.operation = Some("op_original".into());
        assert!(app.restore_recovered_input(&input));
        assert_eq!(app.composer.text(), "recovered");
        assert!(app.composer_target.is_none());
        assert!(app.notice.contains("unknown"));
        assert!(app.pending_echoes.is_empty());
        let snapshot = app.recovery_snapshot();
        assert!(snapshot.uncertain_pending);
        assert_eq!(snapshot.operation.as_deref(), Some("op_original"));
        assert_eq!(snapshot.task.as_deref(), Some("task_original"));
    }
    #[test]
    fn snapshot_keeps_context_drafts_and_unacknowledged_inputs_separate() {
        let mut app = App::default();
        app.view.conversation = Some(id("c_current"));
        app.composer.set_text("current");
        app.save_draft(
            id("c_other"),
            SessionDraft {
                text: "other".into(),
                attachments: Vec::new(),
                target: None,
                origin: None,
            },
        );
        app.pending_echoes.push_back(PendingEcho {
            id: id("m_pending"),
            session: Some(id("c_current")),
            text: "possibly sent".into(),
            attachments: 0,
            recovery_attachments: Vec::new(),
        });
        app.retain_rejected(
            id("c_current"),
            "rejected but still needed".into(),
            Vec::new(),
            None,
        );
        let snapshot = app.recovery_snapshot();
        assert_eq!(snapshot.text, "current");
        assert_eq!(snapshot.other_inputs.len(), 3);
        assert!(
            snapshot
                .other_inputs
                .iter()
                .any(|input| input.context == "context:c_other" && input.text == "other")
        );
        assert!(snapshot.other_inputs.iter().any(
            |input| input.uncertain_pending && input.operation.as_deref() == Some("m_pending")
        ));
        assert_eq!(app.composer.text(), "current");
    }
    #[test]
    fn rejected_input_does_not_replace_a_newer_empty_task_selection() {
        let mut app = App::default();
        app.view.conversation = Some(id("c_current"));
        let old = interaction::ComposerTarget {
            task: id("task_old"),
            conversation: id("c_current"),
            answer_revision: None,
        };
        let newer = interaction::ComposerTarget {
            task: id("task_new"),
            conversation: id("c_current"),
            answer_revision: None,
        };
        app.composer_target = Some(newer.clone());
        app.retain_rejected(
            id("c_current"),
            "old rejected input".into(),
            Vec::new(),
            Some(old),
        );
        assert!(app.composer.text().is_empty());
        assert!(app.composer_target == Some(newer));
        assert_eq!(app.recovery_extras.len(), 1);
    }
    #[test]
    fn forced_flush_returns_false_when_unchanged_snapshot_is_no_longer_durable() {
        let state = TestState::new();
        let mut app = App::default();
        assert!(app.flush_recovery(true));
        app.configure_recovery(state.0.clone());
        assert!(app.flush_recovery(true));
        std::fs::remove_file(app.recovery.as_ref().unwrap().path()).unwrap();
        assert!(!app.flush_recovery(true));
    }
    #[test]
    fn stashed_and_retained_origins_preserve_unknown_send_identity() {
        let mut app = App::default();
        let origin = RecoveryInput {
            context: "context:c_other".into(),
            text: "still uncertain".into(),
            uncertain_pending: true,
            operation: Some("op_original".into()),
            task: Some("task_original".into()),
            ..Default::default()
        };
        app.save_draft(
            id("c_other"),
            SessionDraft {
                text: origin.text.clone(),
                attachments: Vec::new(),
                target: None,
                origin: Some(origin.clone()),
            },
        );
        app.retain_rejected_with_origin(
            id("c_other"),
            origin.text.clone(),
            Vec::new(),
            None,
            Some(origin),
        );
        let snapshot = app.recovery_snapshot();
        assert_eq!(snapshot.other_inputs.len(), 2);
        assert!(
            snapshot
                .other_inputs
                .iter()
                .all(|input| input.uncertain_pending
                    && input.operation.as_deref() == Some("op_original")
                    && input.task.as_deref() == Some("task_original"))
        );
    }
    #[test]
    fn unbound_workspace_copy_requires_confirmation_and_preserves_uncertainty() {
        let state = TestState::new();
        let mut previous = input_recovery::RecoveryJournal::new(&state.0).unwrap();
        previous
            .save(&RecoverySnapshot {
                context: context_key(None),
                text: "before context arrived".into(),
                uncertain_pending: true,
                task: Some("task_original".into()),
                operation: Some("op_original".into()),
                ..Default::default()
            })
            .unwrap();
        drop(previous);
        let mut app = App::default();
        app.view.conversation = Some(id("c_destination"));
        let workspace = std::env::current_dir()
            .unwrap()
            .canonicalize()
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        app.view.conversations.push(xcb_core::ui::ConversationRow {
            id: id("c_destination"),
            title: "Destination".into(),
            workspace: workspace.clone(),
            messages: 0,
            updated_at_ms: 0,
        });
        app.configure_recovery(state.0.clone());
        app.open_recovery();
        app.recover_entry(0);
        app.recover_entry(0);
        assert!(app.composer.text().is_empty());
        assert!(matches!(
            app.recovery_choices[0],
            RecoveryChoice::ConfirmUnbound { .. }
        ));
        app.view.conversations[0].workspace = state.0.to_str().unwrap().to_owned();
        app.recover_entry(0);
        assert!(app.composer.text().is_empty());
        app.view.conversations[0].workspace = workspace;
        app.view.conversation = Some(id("c_changed"));
        app.recover_entry(0);
        assert!(app.composer.text().is_empty());
        app.view.conversation = Some(id("c_destination"));
        app.composer.set_text("newer");
        app.recover_entry(0);
        assert_eq!(app.composer.text(), "newer");
        app.composer.set_text("");
        app.recover_entry(0);
        let snapshot = app.recovery_snapshot();
        assert_eq!(snapshot.text, "before context arrived");
        assert_eq!(snapshot.context, "context:c_destination");
        assert!(snapshot.uncertain_pending);
        assert_eq!(snapshot.operation.as_deref(), Some("op_original"));
        assert_eq!(snapshot.task.as_deref(), Some("task_original"));
        assert!(app.composer_target.is_none());
        let earlier = input_recovery::candidates(&state.0)
            .unwrap()
            .into_iter()
            .find(|entry| !entry.live)
            .unwrap();
        assert_eq!(
            input_recovery::read(&earlier).unwrap().context,
            context_key(None)
        );
    }
}
