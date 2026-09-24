use super::*;
use xcb_core::ui::TranscriptContext;

struct Fixture {
    _root: tempfile::TempDir,
    state: PathBuf,
    workspace: PathBuf,
    store: ManagedStore,
    conversation: Id,
}
async fn fixture() -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let base = root.path().canonicalize().unwrap();
    let state = private::directory(&base.join("state")).unwrap();
    let workspace = private::directory(&base.join("workspace")).unwrap();
    let store = ManagedStore::open(&state).unwrap();
    let conversation = store.create_conversation(&workspace).await.unwrap().id;
    Fixture {
        _root: root,
        state,
        workspace,
        store,
        conversation,
    }
}
async fn enqueue(f: &Fixture) -> ManagedTask {
    f.store
        .enqueue_backlog(
            &f.conversation,
            new_id("m"),
            "Inspect the tests".into(),
            true,
            0,
        )
        .await
        .unwrap()
}
async fn question(f: &Fixture, task: &ManagedTask, text: &str) -> ManagedTask {
    let mut next = task.clone();
    next.state = TaskState::NeedsInput;
    next.deferred = false;
    next.attention = Some(State::NeedsApproval);
    next.last_output = Some(text.into());
    next.revision += 1;
    next.updated_at_ms = now_ms().max(task.updated_at_ms);
    f.store.transition(task, next, None).await.unwrap()
}

#[tokio::test]
async fn ui_cancel_is_revision_checked_and_does_not_settle_or_retarget() {
    let f = fixture().await;
    let task = enqueue(&f).await;
    let other = enqueue(&f).await;
    let edited = f
        .store
        .edit_backlog(&task.id, task.revision, "Updated task".into(), 0)
        .await
        .unwrap();
    assert!(f.store.cancel_task(&task.id, task.revision).await.is_err());
    assert!(!f.store.task(&task.id).unwrap().unwrap().cancel_requested);
    let cancelled = f
        .store
        .cancel_task(&edited.id, edited.revision)
        .await
        .unwrap();
    assert_eq!(cancelled.state, TaskState::Queued);
    assert!(cancelled.cancel_requested);
    assert_eq!(cancelled.session, edited.session);
    assert!(!f.store.task(&other.id).unwrap().unwrap().cancel_requested);
    let settled = f.store.settle_unstarted_cancel(&cancelled).await.unwrap();
    assert_eq!(settled.state, TaskState::Cancelled);
    assert!(
        f.store
            .cancel_task(&settled.id, settled.revision)
            .await
            .is_err()
    );
    f.store.verify_task(&settled.id).await.unwrap();
}

#[tokio::test]
async fn ui_cancel_and_edit_race_has_one_winner() {
    let f = fixture().await;
    let task = enqueue(&f).await;
    let (cancel, edit) = tokio::join!(
        f.store.cancel_task(&task.id, task.revision),
        f.store
            .edit_backlog(&task.id, task.revision, "Different task".into(), 0),
    );
    assert_ne!(cancel.is_ok(), edit.is_ok());
    let current = f.store.task(&task.id).unwrap().unwrap();
    assert_eq!(current.revision, task.revision + 1);
    assert_eq!(current.cancel_requested, cancel.is_ok());
}

#[tokio::test]
async fn ui_cancel_never_follows_a_corrupt_payload_to_another_task() {
    let f = fixture().await;
    let task = enqueue(&f).await;
    let other = enqueue(&f).await;
    f.store
        .db()
        .unwrap()
        .execute(
            "UPDATE tasks SET payload=?1 WHERE id=?2",
            params![serde_json::to_string(&other).unwrap(), task.id.as_str()],
        )
        .unwrap();
    assert!(f.store.cancel_task(&task.id, task.revision).await.is_err());
    assert!(!f.store.task(&other.id).unwrap().unwrap().cancel_requested);
}

#[tokio::test]
async fn ui_reply_rejects_stale_questions_and_replays_only_original_input() {
    let f = fixture().await;
    let task = question(&f, &enqueue(&f).await, "First approval?").await;
    let operation = new_id("m");
    let answer = f
        .store
        .reply_to_task_checked(&task.id, task.revision, operation.clone(), "Yes".into())
        .await
        .unwrap();
    let replacement = question(&f, &answer, "A different approval?").await;
    assert!(
        f.store
            .reply_to_task_checked(&task.id, task.revision, new_id("m"), "Yes".into())
            .await
            .is_err()
    );
    let replay = f
        .store
        .reply_to_task_checked(&task.id, task.revision, operation.clone(), "Yes".into())
        .await
        .unwrap();
    assert_eq!(replay.revision, answer.revision);
    assert!(
        f.store
            .reply_to_task_checked(
                &task.id,
                replacement.revision,
                operation.clone(),
                "Yes".into()
            )
            .await
            .is_err()
    );
    assert!(
        f.store
            .reply_to_task_checked(&task.id, task.revision, operation, "No".into())
            .await
            .is_err()
    );
    let current = f.store.task(&task.id).unwrap().unwrap();
    assert_eq!(current.revision, replacement.revision);
    assert_eq!(current.attention, Some(State::NeedsApproval));
    assert_eq!(
        current.last_output.as_deref(),
        Some("A different approval?")
    );
    f.store.verify_task(&task.id).await.unwrap();
}

#[tokio::test]
async fn ui_reply_duplicate_race_records_one_answer_and_survives_reopen() {
    let f = fixture().await;
    let task = question(&f, &enqueue(&f).await, "Proceed?").await;
    let operation = new_id("m");
    let (a, b) = tokio::join!(
        f.store
            .reply_to_task_checked(&task.id, task.revision, operation.clone(), "Proceed".into()),
        f.store
            .reply_to_task_checked(&task.id, task.revision, operation.clone(), "Proceed".into()),
    );
    assert_eq!(a.unwrap().revision, b.unwrap().revision);
    let reopened = ManagedStore::open(&f.state).unwrap();
    assert_eq!(
        reopened
            .reply_to_task_checked(&task.id, task.revision, operation, "Proceed".into())
            .await
            .unwrap()
            .revision,
        task.revision + 1
    );
    let messages = reopened.messages(&f.conversation, 512).unwrap();
    assert_eq!(
        messages
            .iter()
            .filter(|message| message.text == "Proceed")
            .count(),
        1
    );
}

#[tokio::test]
async fn ui_enqueue_identity_includes_queue_options() {
    let f = fixture().await;
    let operation = new_id("m");
    let first = f
        .store
        .enqueue_backlog(&f.conversation, operation.clone(), "Task".into(), true, 2)
        .await
        .unwrap();
    let again = f
        .store
        .enqueue_backlog(&f.conversation, operation.clone(), "Task".into(), true, 2)
        .await
        .unwrap();
    assert_eq!(first.id, again.id);
    assert!(
        f.store
            .enqueue_backlog(&f.conversation, operation.clone(), "Task".into(), false, 2)
            .await
            .is_err()
    );
    assert!(
        f.store
            .enqueue_backlog(&f.conversation, operation, "Task".into(), true, 3)
            .await
            .is_err()
    );
    assert_eq!(f.store.tasks(16).unwrap().len(), 1);
}

#[tokio::test]
async fn ui_new_work_never_answers_cancels_or_continues_an_existing_task() {
    let f = fixture().await;
    let waiting = question(&f, &enqueue(&f).await, "Approve the next step?").await;
    for text in [
        "yes",
        "cancel",
        "continue",
        "new task: preserve this exact text",
    ] {
        f.store
            .submit_new(
                &f.conversation,
                new_id("m"),
                text.into(),
                vec![],
                &f.workspace,
            )
            .await
            .unwrap();
        let current = f.store.task(&waiting.id).unwrap().unwrap();
        assert_eq!(current.revision, waiting.revision);
        assert_eq!(current.attention, Some(State::NeedsApproval));
        assert!(!current.cancel_requested);
        assert!(
            f.store
                .tasks(16)
                .unwrap()
                .iter()
                .any(|task| task.goal == text && task.id != waiting.id)
        );
    }
    let image = Attachment {
        digest: "a".repeat(64),
        media_type: "image/png".into(),
        bytes: 512,
        width: 16,
        height: 16,
    };
    let submission = new_id("m");
    f.store
        .submit_new(
            &f.conversation,
            submission.clone(),
            String::new(),
            vec![image.clone()],
            &f.workspace,
        )
        .await
        .unwrap();
    f.store
        .submit_new(
            &f.conversation,
            submission.clone(),
            String::new(),
            vec![image.clone()],
            &f.workspace,
        )
        .await
        .unwrap();
    let saved = f
        .store
        .tasks(16)
        .unwrap()
        .into_iter()
        .find(|task| task.source_message == submission)
        .unwrap();
    assert_eq!(saved.attachments, vec![image.clone()]);
    assert!(saved.goal.is_empty());
    let mut changed = image;
    changed.digest = "b".repeat(64);
    assert!(
        f.store
            .submit_new(
                &f.conversation,
                submission,
                String::new(),
                vec![changed],
                &f.workspace
            )
            .await
            .is_err()
    );
    assert_eq!(f.store.tasks(16).unwrap().len(), 6);
}

#[tokio::test]
async fn ui_recall_and_dispatch_race_cannot_recall_running_work() {
    let f = fixture().await;
    let task = enqueue(&f).await;
    let mut dispatched = task.clone();
    dispatched.state = TaskState::Running;
    dispatched.deferred = false;
    dispatched.session = Some(new_id("s"));
    dispatched.attempts = 1;
    dispatched.revision += 1;
    dispatched.updated_at_ms = now_ms().max(task.updated_at_ms);
    let operation = new_id("m");
    let (recall, dispatch) = tokio::join!(
        f.store.recall_queued(&task.id, task.revision, &operation),
        f.store.transition(&task, dispatched, None),
    );
    assert_ne!(recall.is_ok(), dispatch.is_ok());
    let current = f.store.task(&task.id).unwrap().unwrap();
    if let Ok(recalled) = recall {
        assert_eq!(recalled.state, TaskState::Cancelled);
        assert!(!recalled.cancel_requested);
        assert_eq!(
            f.store
                .recall_queued(&task.id, task.revision, &operation)
                .await
                .unwrap()
                .revision,
            recalled.revision
        );
    } else {
        assert_eq!(current.state, TaskState::Running);
        assert!(
            f.store
                .recall_queued(&task.id, current.revision, &new_id("m"))
                .await
                .is_err()
        );
    }
    f.store.verify_task(&task.id).await.unwrap();
}

#[tokio::test]
async fn ui_recall_refuses_guidance_and_previous_worker_evidence() {
    let f = fixture().await;
    let task = enqueue(&f).await;
    f.store
        .steer_task(&task.id, new_id("event"), "Preserve this guidance".into())
        .unwrap();
    assert!(
        f.store
            .recall_queued(&task.id, task.revision, &new_id("m"))
            .await
            .is_err()
    );
    let task = enqueue(&f).await;
    let mut prior_worker = task.clone();
    prior_worker.worker_sessions.push(new_id("s"));
    prior_worker.revision += 1;
    prior_worker.updated_at_ms = now_ms().max(task.updated_at_ms);
    let task = f.store.transition(&task, prior_worker, None).await.unwrap();
    assert!(
        f.store
            .recall_queued(&task.id, task.revision, &new_id("m"))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn ui_transcript_cursor_handles_retention_gaps_and_concurrent_append() {
    let f = fixture().await;
    let other = f.store.create_conversation(&f.workspace).await.unwrap();
    for (conversation, sequences) in [
        (&f.conversation, vec![2, 5, 9, 20, 21, 37]),
        (&other.id, vec![1, 2]),
    ] {
        for sequence in sequences {
            let message = Message {
                id: new_id("m"),
                role: Role::Assistant,
                text: format!("{conversation}:{sequence}"),
                at_ms: now_ms(),
                attachments: vec![],
                provenance: None,
            };
            f.store.db().unwrap().execute("INSERT INTO messages(id,conversation,sequence,task,payload,at_ms) VALUES(?1,?2,?3,NULL,?4,?5)", params![message.id.as_str(),conversation.as_str(),sequence,serde_json::to_string(&message).unwrap(),sql(message.at_ms).unwrap()]).unwrap();
        }
    }
    let latest = f.store.transcript_page(&f.conversation, None, 3).unwrap();
    assert_eq!(latest.first_sequence, Some(20));
    assert!(latest.has_older);
    f.store
        .record_pair(
            &f.conversation,
            new_id("m"),
            "new input".into(),
            "new answer".into(),
        )
        .unwrap();
    let older = f
        .store
        .transcript_page(&f.conversation, latest.first_sequence, 3)
        .unwrap();
    assert_eq!(older.first_sequence, Some(2));
    assert!(!older.has_older);
    assert_eq!(
        older.context,
        TranscriptContext::Conversation(f.conversation.clone())
    );
    assert!(
        older
            .messages
            .iter()
            .all(|message| message.text.starts_with(f.conversation.as_str()))
    );
    assert!(
        older
            .messages
            .iter()
            .all(|message| !latest.messages.iter().any(|new| new.id == message.id))
    );
    assert_eq!(
        f.store
            .transcript_page(&other.id, None, 3)
            .unwrap()
            .messages
            .len(),
        2
    );
    assert!(
        f.store
            .transcript_page(&f.conversation, Some(0), 3)
            .is_err()
    );
    assert!(f.store.transcript_page(&f.conversation, None, 513).is_err());
}

#[tokio::test]
async fn ui_rename_preserves_task_custody_and_refreshes_each_changed_row() {
    let f = fixture().await;
    let task = enqueue(&f).await;
    let mut running = task.clone();
    running.state = TaskState::Running;
    running.deferred = false;
    running.session = Some(new_id("s"));
    running.revision += 1;
    running.updated_at_ms = now_ms().max(task.updated_at_ms);
    let running = f.store.transition(&task, running, None).await.unwrap();
    let old = f.store.conversation(&f.conversation).unwrap().unwrap();
    let stamp = f.store.view_stamp(&f.state, &f.conversation).unwrap();
    let renamed = f
        .store
        .rename_conversation(&f.conversation, &old.title, "  Review\n the tests  ")
        .unwrap();
    assert_eq!(renamed.title, "Review the tests");
    assert_ne!(
        stamp,
        f.store.view_stamp(&f.state, &f.conversation).unwrap()
    );
    assert!(
        f.store
            .rename_conversation(&f.conversation, &old.title, "Stale rename")
            .is_err()
    );
    let other = f.store.create_conversation(&f.workspace).await.unwrap();
    let stamp = f.store.view_stamp(&f.state, &f.conversation).unwrap();
    f.store
        .rename_conversation(&other.id, &other.title, "Other row")
        .unwrap();
    assert_ne!(
        stamp,
        f.store.view_stamp(&f.state, &f.conversation).unwrap()
    );
    assert_eq!(
        serde_json::to_value(f.store.task(&task.id).unwrap().unwrap()).unwrap(),
        serde_json::to_value(running).unwrap()
    );
    assert_eq!(
        ManagedStore::open(&f.state)
            .unwrap()
            .conversation(&f.conversation)
            .unwrap()
            .unwrap()
            .title,
        renamed.title
    );
    f.store.verify_task(&task.id).await.unwrap();
}

#[tokio::test]
async fn ui_recovery_and_pages_keep_identity_when_context_switches_under_backpressure() {
    use xcb_core::ui::HabitatCommand;
    let f = fixture().await;
    let other = f.store.create_conversation(&f.workspace).await.unwrap();
    let (commands, input) = std::sync::mpsc::sync_channel(8);
    let (output, display) = std::sync::mpsc::sync_channel(1);
    let enqueue = new_id("m");
    let steer = new_id("event");
    let missing = new_id("missing_task");
    let request = new_id("page");
    let rejected_page = new_id("page");
    let rejected_recall = new_id("recall");
    commands
        .send(Intent::Habitat(HabitatCommand::Enqueue {
            id: enqueue.clone(),
            prompt: "Save this queued draft".into(),
            deferred: true,
            priority: 0,
        }))
        .unwrap();
    commands
        .send(Intent::Habitat(HabitatCommand::Steer {
            task: missing.clone(),
            event: steer.clone(),
            text: "Keep this rejected guidance".into(),
        }))
        .unwrap();
    commands
        .send(Intent::TranscriptPage {
            context: TranscriptContext::Conversation(f.conversation.clone()),
            before_sequence: 1,
            request: request.clone(),
        })
        .unwrap();
    commands
        .send(Intent::Habitat(HabitatCommand::RecallQueued {
            id: missing.clone(),
            expected_revision: 1,
            operation: rejected_recall.clone(),
        }))
        .unwrap();
    commands.send(Intent::Conversation(other.id)).unwrap();
    commands
        .send(Intent::TranscriptPage {
            context: TranscriptContext::Conversation(f.conversation.clone()),
            before_sequence: 1,
            request: rejected_page.clone(),
        })
        .unwrap();
    let ui = tokio::spawn(serve_ui(
        Arc::new(Store::open(&f.state).unwrap()),
        f.conversation.clone(),
        input,
        output,
        PathBuf::from("/usr/bin/true"),
    ));
    // Keep the channel full while every command, including the switch, is handled.
    tokio::time::sleep(Duration::from_millis(600)).await;
    let mut accepted = false;
    let mut recovered = false;
    let mut page_received = false;
    let mut page_rejected = false;
    let mut recall_rejected = false;
    tokio::time::timeout(Duration::from_secs(4), async {
        while !(accepted && recovered && page_received && page_rejected && recall_rejected) {
            while let Ok(update) = display.try_recv() {
                match update {
                    Update::HabitatAccepted {
                        context,
                        task,
                        operation,
                        text,
                    } => {
                        assert_eq!(context, f.conversation);
                        assert_eq!(task, None);
                        assert_eq!(operation, enqueue);
                        assert_eq!(text, "Save this queued draft");
                        accepted = true;
                    }
                    Update::HabitatDraft {
                        context,
                        task,
                        operation,
                        text,
                    } => {
                        assert_eq!(context, f.conversation);
                        assert_eq!(task, Some(missing.clone()));
                        assert_eq!(operation, steer);
                        assert_eq!(text, "Keep this rejected guidance");
                        recovered = true;
                    }
                    Update::TranscriptPage {
                        request: returned,
                        page,
                    } => {
                        assert_eq!(returned, request);
                        assert_eq!(
                            page.context,
                            TranscriptContext::Conversation(f.conversation.clone())
                        );
                        page_received = true;
                    }
                    Update::TranscriptPageRejected {
                        context,
                        request,
                        reason,
                    } => {
                        assert_eq!(
                            context,
                            TranscriptContext::Conversation(f.conversation.clone())
                        );
                        assert_eq!(request, rejected_page);
                        assert!(reason.contains("context changed"));
                        page_rejected = true;
                    }
                    Update::QueuedRecallRejected {
                        context,
                        id,
                        operation,
                        reason,
                    } => {
                        assert_eq!(context, f.conversation);
                        assert_eq!(id, missing);
                        assert_eq!(operation, rejected_recall);
                        assert!(reason.contains("task not found"));
                        recall_rejected = true;
                    }
                    _ => (),
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    commands.send(Intent::Quit).unwrap();
    ui.await.unwrap().unwrap();
}

#[tokio::test]
async fn ui_navigation_burst_rejects_stale_submission_and_queue_contexts() {
    use xcb_core::ui::HabitatCommand;
    let f = fixture().await;
    let other = f.store.create_conversation(&f.workspace).await.unwrap();
    let (commands, input) = std::sync::mpsc::sync_channel(8);
    let (output, display) = std::sync::mpsc::sync_channel(1);
    let submission = new_id("m");
    let enqueue = new_id("m");
    let accepted = new_id("m");
    let image = Attachment {
        digest: "a".repeat(64),
        media_type: "image/png".into(),
        bytes: 512,
        width: 16,
        height: 16,
    };
    // Navigation and stale inputs arrive before the UI can see the new view.
    commands
        .send(Intent::Conversation(other.id.clone()))
        .unwrap();
    commands
        .send(Intent::SubmitTo {
            context: TranscriptContext::Conversation(f.conversation.clone()),
            id: submission.clone(),
            text: "Keep this in the original conversation".into(),
            attachments: vec![image.clone()],
        })
        .unwrap();
    commands
        .send(Intent::Habitat(HabitatCommand::EnqueueIn {
            conversation: f.conversation.clone(),
            id: enqueue.clone(),
            prompt: "Keep this queued draft in its original conversation".into(),
            deferred: true,
            priority: 0,
        }))
        .unwrap();
    commands
        .send(Intent::HabitatAt {
            conversation: f.conversation.clone(),
            command: HabitatCommand::ConfigureProject {
                expected_revision: None,
                goal: "Never authorize the newly selected project".into(),
                max_tasks: 5,
                expires_at_ms: now_ms() + 60_000,
                required_provider: None,
            },
        })
        .unwrap();
    commands
        .send(Intent::HabitatAt {
            conversation: f.conversation.clone(),
            command: HabitatCommand::Schedule {
                prompt: "Never schedule the other conversation".into(),
                interval_ms: 60_000,
            },
        })
        .unwrap();
    commands
        .send(Intent::SubmitTo {
            context: TranscriptContext::Conversation(other.id.clone()),
            id: accepted.clone(),
            text: "Correctly addressed new work".into(),
            attachments: vec![],
        })
        .unwrap();
    let ui = tokio::spawn(serve_ui(
        Arc::new(Store::open(&f.state).unwrap()),
        f.conversation.clone(),
        input,
        output,
        PathBuf::from("/usr/bin/true"),
    ));
    tokio::time::sleep(Duration::from_millis(600)).await;
    let mut rejected_submit = false;
    let mut rejected_queue = false;
    let mut accepted_submit = false;
    tokio::time::timeout(Duration::from_secs(4), async {
        while !(rejected_submit && rejected_queue && accepted_submit) {
            while let Ok(update) = display.try_recv() {
                match update {
                    Update::SubmitRejected {
                        id,
                        context,
                        text,
                        attachments,
                        reason,
                    } => {
                        assert_eq!(id, submission);
                        assert_eq!(
                            context,
                            Some(TranscriptContext::Conversation(f.conversation.clone()))
                        );
                        assert_eq!(text, "Keep this in the original conversation");
                        assert_eq!(attachments, vec![image.clone()]);
                        assert!(reason.contains("conversation changed"));
                        rejected_submit = true;
                    }
                    Update::HabitatDraft {
                        context,
                        task,
                        operation,
                        text,
                    } => {
                        assert_eq!(context, f.conversation);
                        assert_eq!(task, None);
                        assert_eq!(operation, enqueue);
                        assert_eq!(text, "Keep this queued draft in its original conversation");
                        rejected_queue = true;
                    }
                    Update::Submitted { id, context } => {
                        assert_eq!(id, accepted);
                        assert_eq!(context, TranscriptContext::Conversation(other.id.clone()));
                        accepted_submit = true;
                    }
                    Update::HabitatAccepted { .. } => panic!("stale queue was accepted"),
                    _ => (),
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    commands.send(Intent::Quit).unwrap();
    ui.await.unwrap().unwrap();
    assert!(f.store.messages(&f.conversation, 128).unwrap().is_empty());
    let tasks = f.store.tasks(16).unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].conversation, other.id);
    assert_eq!(tasks[0].source_message, accepted);
    assert!(f.store.project_policies().unwrap().is_empty());
    assert!(f.store.schedules(None).unwrap().is_empty());
}
