use super::*;

struct Fixture {
    _root: tempfile::TempDir,
    managed: ManagedStore,
    conversation: ManagedConversation,
    task: ManagedTask,
}

async fn fixture() -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let base = root.path().canonicalize().unwrap();
    let workspace = private::directory(&base.join("work")).unwrap();
    let managed = ManagedStore::open(&base.join("state")).unwrap();
    let conversation = managed.create_conversation(&workspace).await.unwrap();
    let task = managed
        .enqueue_backlog(
            &conversation.id,
            new_id("m"),
            "Review the parser".into(),
            false,
            5,
        )
        .await
        .unwrap();
    Fixture {
        _root: root,
        managed,
        conversation,
        task,
    }
}

fn put_task(f: &Fixture, task: &ManagedTask) {
    task.validate().unwrap();
    f.managed.db().unwrap().execute(
        "INSERT OR REPLACE INTO tasks(id,operation,source_message,conversation,state,revision,updated_at,payload) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
        params![task.id.as_str(), task.operation.as_str(), task.source_message.as_str(), task.conversation.as_str(), task.state.as_str(), sql(task.revision).unwrap(), sql(task.updated_at_ms).unwrap(), serde_json::to_string(task).unwrap()],
    ).unwrap();
}

fn another_task(f: &Fixture, state: TaskState, updated: u64) -> ManagedTask {
    let mut task = f.task.clone();
    task.id = new_id("t");
    task.operation = new_id("op");
    task.source_message = new_id("m");
    task.state = state;
    task.updated_at_ms = task.created_at_ms.max(updated);
    task
}

fn rows(f: &Fixture) -> Vec<AgentRow> {
    f.managed
        .agent_overview(&f.conversation.id, &BTreeMap::new(), now_ms())
        .unwrap()
}

#[tokio::test]
async fn overview_uses_task_response_and_observed_route_not_acknowledgments_or_preferences() {
    let f = fixture().await;
    let mut task = f.task.clone();
    task.route = Some("codex".into());
    task.detail = "Host status is not a response".into();
    put_task(&f, &task);
    let row = rows(&f).remove(0);
    assert_eq!(
        row.context,
        TranscriptContext::Conversation(f.conversation.id.clone())
    );
    assert_eq!(row.title, f.conversation.title);
    assert_eq!(row.task, Some(task.id.clone()));
    assert!(row.response.is_empty());
    assert!(row.model.is_none());

    task.state = TaskState::Completed;
    task.session = Some(new_id("s"));
    task.route = Some("codex/gpt-fixture · account".into());
    task.last_output = Some("é".repeat(1400));
    task.settle = Some("done".into());
    put_task(&f, &task);
    let row = rows(&f).remove(0);
    assert_eq!(row.response.len(), 2048);
    assert_eq!(row.response, "é".repeat(1024));
    assert_eq!(row.category.as_deref(), Some("done"));
    assert_eq!(row.model, task.route);
}

#[tokio::test]
async fn overview_new_queued_task_does_not_borrow_an_older_response_or_model() {
    let f = fixture().await;
    let mut previous = f.task.clone();
    previous.state = TaskState::Completed;
    previous.session = Some(new_id("s"));
    previous.route = Some("previous/model".into());
    previous.last_output = Some("Previous work is done".into());
    previous.settle = Some("done".into());
    put_task(&f, &previous);
    let queued = another_task(&f, TaskState::Queued, previous.updated_at_ms + 1);
    put_task(&f, &queued);
    let row = rows(&f).remove(0);
    assert_eq!(row.task, Some(queued.id));
    assert!(row.response.is_empty());
    assert!(row.model.is_none());
    assert_eq!(row.activity, TaskState::Queued.label());
}

#[tokio::test]
async fn overview_attention_wins_within_a_conversation_without_duplicate_cells() {
    let f = fixture().await;
    let mut question = f.task.clone();
    question.state = TaskState::NeedsInput;
    question.attention = Some(State::NeedsApproval);
    question.last_output = Some("May I publish?".into());
    question.settle = Some("blocked".into());
    put_task(&f, &question);
    let running = another_task(&f, TaskState::Running, question.updated_at_ms + 100);
    put_task(&f, &running);
    let result = rows(&f);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].task, Some(question.id));
    assert_eq!(result[0].state, State::NeedsApproval);
    assert_eq!(result[0].response, "May I publish?");
}

#[tokio::test]
async fn overview_bounds_rows_but_keeps_old_focused_and_attention_conversations() {
    let f = fixture().await;
    let mut closed = f.task.clone();
    closed.state = TaskState::Completed;
    put_task(&f, &closed);
    let mut attention_id = None;
    for index in 0..135 {
        let mut conversation = f.conversation.clone();
        conversation.id = new_id("c");
        conversation.updated_at_ms += index + 1;
        f.managed
            .db()
            .unwrap()
            .execute(
                "INSERT INTO conversations(id,updated_at,payload) VALUES(?1,?2,?3)",
                params![
                    conversation.id.as_str(),
                    sql(conversation.updated_at_ms).unwrap(),
                    serde_json::to_string(&conversation).unwrap()
                ],
            )
            .unwrap();
        if index == 0 {
            let mut task = another_task(&f, TaskState::NeedsInput, conversation.updated_at_ms);
            task.conversation = conversation.id.clone();
            task.attention = Some(State::NeedsAnswer);
            put_task(&f, &task);
            attention_id = Some(conversation.id);
        }
    }
    let result = rows(&f);
    assert_eq!(result.len(), MAX_AGENTS);
    assert_eq!(
        result[0].context,
        TranscriptContext::Conversation(attention_id.unwrap())
    );
    assert!(
        result
            .iter()
            .any(|row| row.context == TranscriptContext::Conversation(f.conversation.id.clone()))
    );
}

#[tokio::test]
async fn overview_phases_are_generic_and_stale_future_or_settled_beats_are_ignored() {
    let f = fixture().await;
    let mut task = f.task.clone();
    task.state = TaskState::Running;
    let thinking = progress_label(Progress::Text {
        thinking: true,
        text: "private reasoning".into(),
    });
    assert_eq!(thinking, "thinking");
    assert_eq!(
        progress_label(Progress::Text {
            thinking: false,
            text: "response body".into()
        }),
        "writing response"
    );
    let beat = ProgressBeat {
        at_ms: task.updated_at_ms + 1,
        text: thinking,
    };
    assert_eq!(activity(&task, Some(&beat), beat.at_ms), "thinking");
    assert_eq!(
        activity(&task, Some(&beat), beat.at_ms + PROGRESS_FRESH_MS + 1),
        "running"
    );
    assert_eq!(activity(&task, Some(&beat), beat.at_ms - 1), "running");
    task.updated_at_ms = beat.at_ms;
    assert_eq!(activity(&task, Some(&beat), beat.at_ms), "running");
    task.updated_at_ms -= 1;
    task.state = TaskState::Completed;
    assert_eq!(activity(&task, Some(&beat), beat.at_ms), "completed");
}

#[tokio::test]
async fn overview_progress_updates_invalidate_the_view_stamp() {
    let f = fixture().await;
    let before = f
        .managed
        .view_stamp(f.managed.root().parent().unwrap(), &f.conversation.id)
        .unwrap();
    let beats = BTreeMap::from([(
        f.task.id.clone(),
        ProgressBeat {
            at_ms: now_ms(),
            text: "thinking".into(),
        },
    )]);
    write_progress(f.managed.root(), &beats).unwrap();
    let after = f
        .managed
        .view_stamp(f.managed.root().parent().unwrap(), &f.conversation.id)
        .unwrap();
    assert_ne!(before, after);
    assert!(after.progress_time.is_some());
}

#[tokio::test]
async fn overview_idle_direct_database_does_not_invalidate_on_time_boundaries() {
    let f = fixture().await;
    let root = f.managed.root().parent().unwrap();
    let store = Store::open(root).unwrap();
    let before = f
        .managed
        .view_stamp_at(root, &f.conversation.id, 14_999)
        .unwrap();
    let after = f
        .managed
        .view_stamp_at(root, &f.conversation.id, 30_000)
        .unwrap();
    assert_eq!(before, after);
    let mut liveness = SessionLiveness::default();
    let now = Instant::now();
    assert!(!liveness.changed(&store, now, false).unwrap());
    assert!(
        !liveness
            .changed(&store, now + Duration::from_secs(15), false)
            .unwrap()
    );
}

#[tokio::test]
async fn overview_owner_exit_invalidates_once_without_a_database_write() {
    use xcb_core::models::{Mode, ModelChoice};

    struct ChildGuard(std::process::Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    let f = fixture().await;
    let root = f.managed.root().parent().unwrap();
    let store = Store::open(root).unwrap();
    let account = store
        .add_account(Provider::Codex, "Synthetic", 1, None)
        .unwrap();
    let model = ModelChoice {
        provider: Provider::Codex,
        id: Id::new("fixture-model").unwrap(),
        label: "Fixture".into(),
        mode: Mode::Fixed,
        resolved: None,
        effort: None,
        observed_at_ms: 1,
    };
    let session = store
        .create_session(&account.id, model, Path::new(&f.conversation.workspace), 2)
        .unwrap();
    let mut run = store.prepare_run(&session.id, session.revision, 3).unwrap();
    let mut child = ChildGuard(
        Command::new("/bin/sleep")
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    run.owner.as_mut().unwrap().pid = child.0.id();
    let db = Connection::open(root.join("xcb.sqlite")).unwrap();
    db.execute(
        "UPDATE runs SET payload=?1 WHERE id=?2",
        params![serde_json::to_string(&run).unwrap(), run.id.as_str()],
    )
    .unwrap();
    let before = f
        .managed
        .view_stamp_at(root, &f.conversation.id, 1)
        .unwrap();
    let mut liveness = SessionLiveness::default();
    let now = Instant::now();
    assert!(!liveness.changed(&store, now, false).unwrap());
    assert!(liveness.live.contains(&session.id));
    assert!(
        !liveness
            .changed(&store, now + Duration::from_secs(15), false)
            .unwrap()
    );
    child.0.kill().unwrap();
    child.0.wait().unwrap();
    assert!(
        liveness
            .changed(&store, now + Duration::from_secs(30), false)
            .unwrap()
    );
    assert!(
        !liveness
            .changed(&store, now + Duration::from_secs(45), false)
            .unwrap()
    );
    assert_eq!(
        before,
        f.managed
            .view_stamp_at(root, &f.conversation.id, 1)
            .unwrap()
    );
    assert_eq!(
        store.agent_overview(None).unwrap()[0].state,
        State::Uncertain
    );
    assert_eq!(
        store.session(&session.id).unwrap().unwrap().state,
        State::Working
    );
    assert_eq!(store.unsettled_runs().unwrap().len(), 1);
}

#[tokio::test]
async fn overview_corrupt_task_identity_never_exposes_another_conversation_response() {
    let f = fixture().await;
    let mut corrupted = f.task.clone();
    corrupted.conversation = new_id("other");
    corrupted.last_output = Some("Unrelated response".into());
    f.managed
        .db()
        .unwrap()
        .execute(
            "UPDATE tasks SET payload=?1 WHERE id=?2",
            params![
                serde_json::to_string(&corrupted).unwrap(),
                f.task.id.as_str()
            ],
        )
        .unwrap();
    let row = rows(&f).remove(0);
    assert_eq!(row.state, State::Uncertain);
    assert!(row.response.is_empty());
    assert!(row.task.is_none());
    assert_eq!(f.managed.unreadable_tasks(), 1);
}

#[tokio::test]
async fn overview_global_attention_failures_and_running_survive_current_workspace_volume() {
    let f = fixture().await;
    let mut closed = f.task.clone();
    closed.state = TaskState::Completed;
    put_task(&f, &closed);
    let mut prioritized = Vec::new();
    for index in 0..135 {
        let mut conversation = f.conversation.clone();
        conversation.id = new_id("c");
        // Conversation creation predates task creation. Start after the task
        // so another_task's creation-time clamp cannot tie these timestamps.
        conversation.updated_at_ms = f.task.updated_at_ms + index + 1;
        if index < 3 {
            conversation.workspace = "/another/workspace".into();
        }
        f.managed
            .db()
            .unwrap()
            .execute(
                "INSERT INTO conversations(id,updated_at,payload) VALUES(?1,?2,?3)",
                params![
                    conversation.id.as_str(),
                    sql(conversation.updated_at_ms).unwrap(),
                    serde_json::to_string(&conversation).unwrap()
                ],
            )
            .unwrap();
        if index < 3 {
            let state = match index {
                0 => TaskState::NeedsInput,
                1 => TaskState::Running,
                _ => TaskState::Failed,
            };
            let mut task = another_task(&f, state, conversation.updated_at_ms);
            task.conversation = conversation.id.clone();
            task.workspace = conversation.workspace;
            if index == 0 {
                task.attention = Some(State::NeedsAnswer);
            }
            put_task(&f, &task);
            prioritized.push(TranscriptContext::Conversation(conversation.id));
        }
    }
    let result = rows(&f);
    assert_eq!(result.len(), MAX_AGENTS);
    assert_eq!(result[0].context, prioritized[2]);
    assert_eq!(result[0].state, State::Failed);
    assert_eq!(result[1].context, prioritized[0]);
    assert_eq!(result[1].state, State::NeedsAnswer);
    assert_eq!(result[2].context, prioritized[1]);
    assert_eq!(result[2].state, State::Working);
    assert!(
        result
            .iter()
            .any(|row| row.context == TranscriptContext::Conversation(f.conversation.id.clone()))
    );
}

#[tokio::test]
async fn overview_deferred_work_without_a_response_has_no_completion_category() {
    let f = fixture().await;
    let mut task = f.task.clone();
    task.deferred = true;
    put_task(&f, &task);
    let row = rows(&f).remove(0);
    assert_eq!(row.state, State::Idle);
    assert_eq!(row.activity, "backlog");
    assert!(row.response.is_empty());
    assert!(row.category.is_none());
}

#[tokio::test]
async fn overview_running_work_is_visible_over_a_newer_queued_sibling() {
    let f = fixture().await;
    let mut running = f.task.clone();
    running.state = TaskState::Running;
    put_task(&f, &running);
    let queued = another_task(&f, TaskState::Queued, running.updated_at_ms + 100);
    put_task(&f, &queued);
    let row = rows(&f).remove(0);
    assert_eq!(row.task, Some(running.id));
    assert_eq!(row.activity, "running");
}

#[tokio::test]
async fn overview_running_work_is_visible_over_a_previous_failure() {
    let f = fixture().await;
    let mut failed = f.task.clone();
    failed.state = TaskState::Failed;
    put_task(&f, &failed);
    let running = another_task(&f, TaskState::Running, failed.updated_at_ms + 1);
    put_task(&f, &running);
    let row = rows(&f).remove(0);
    assert_eq!(row.task, Some(running.id));
    assert_eq!(row.state, State::Working);
}

#[tokio::test]
async fn managed_view_combines_direct_and_managed_sessions_without_worker_duplicates() {
    use xcb_core::models::{Mode, ModelChoice};

    let f = fixture().await;
    let root = f.managed.root().parent().unwrap();
    let before = f.managed.view_stamp(root, &f.conversation.id).unwrap();
    let store = Store::open(root).unwrap();
    let account = store
        .add_account(Provider::Codex, "Synthetic", 1, None)
        .unwrap();
    let model = ModelChoice {
        provider: Provider::Codex,
        id: Id::new("fixture-model").unwrap(),
        label: "Fixture".into(),
        mode: Mode::Fixed,
        resolved: None,
        effort: None,
        observed_at_ms: 1,
    };
    let workspace = Path::new(&f.conversation.workspace);
    let session = store
        .create_session(&account.id, model.clone(), workspace, 2)
        .unwrap();
    let worker = store
        .create_managed_session(&account.id, model, workspace, 3, &f.task.id)
        .unwrap();
    let after = f.managed.view_stamp(root, &f.conversation.id).unwrap();
    assert_ne!(before, after);
    assert!(after.direct_database.is_some());
    let view = managed_view(&store, &f.managed, &f.conversation.id, workspace).unwrap();
    assert_eq!(view.agents.len(), 2);
    assert!(
        view.agents.iter().any(|row| {
            row.context == TranscriptContext::Conversation(f.conversation.id.clone())
        })
    );
    assert!(
        view.agents
            .iter()
            .any(|row| row.context == TranscriptContext::Session(session.id.clone()))
    );
    assert!(
        view.agents
            .iter()
            .all(|row| row.context != TranscriptContext::Session(worker.id.clone()))
    );
}
