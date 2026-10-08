use super::*;

struct Fixture {
    _root: tempfile::TempDir,
    managed: ManagedStore,
    conversation: Id,
    workspace: PathBuf,
}
async fn fixture() -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let physical_root = xcb_core::canonical(root.path()).unwrap();
    let state = private::directory(&physical_root.join("state")).unwrap();
    let workspace = private::directory(&physical_root.join("project")).unwrap();
    let managed = ManagedStore::open(&state).unwrap();
    let conversation = managed.create_conversation(&workspace).await.unwrap().id;
    Fixture {
        _root: root,
        managed,
        conversation,
        workspace,
    }
}
/// A sibling project directory beside the fixture's workspace.
fn second(f: &Fixture) -> PathBuf {
    private::directory(&f.workspace.parent().unwrap().join("other")).unwrap()
}
/// A task in the global thread, bound explicitly to `workspace`.
async fn in_thread(managed: &ManagedStore, workspace: &Path, prompt: &str) -> ManagedTask {
    let cues = IntakeCues {
        origin: Origin::Cli,
        explicit: Some(workspace.to_owned()),
        target: None,
        focus: None,
        launch_hint: None,
        infer_only: false,
    };
    match managed
        .submit_to_thread(new_id("m"), prompt.into(), vec![], cues)
        .await
        .unwrap()
    {
        Intake::Accepted { task, .. } => task,
        Intake::Ask { reason, .. } => panic!("thread asked: {reason}"),
    }
}
async fn enqueue(f: &Fixture, prompt: &str, deferred: bool) -> ManagedTask {
    f.managed
        .enqueue_backlog(&f.conversation, new_id("m"), prompt.into(), deferred, 0)
        .await
        .unwrap()
}
async fn set_state(f: &Fixture, task: &ManagedTask, state: TaskState) -> ManagedTask {
    let mut next = task.clone();
    next.state = state;
    next.deferred = false;
    next.revision += 1;
    next.updated_at_ms = now_ms().max(task.updated_at_ms);
    if state == TaskState::Running {
        next.session = Some(new_id("s"));
    }
    f.managed.transition(task, next, None).await.unwrap()
}

#[tokio::test]
async fn deferred_backlog_edit_release_preserves_receipts_and_latest_prompt() {
    let f = fixture().await;
    let task = enqueue(&f, "Use Claude to inspect a typo", true).await;
    assert!(!f.managed.has_habitat_work().unwrap());
    assert_eq!(task.habitat_status(), "backlog");
    let prompt = "Use Codex to design an architecture";
    let edited = f
        .managed
        .edit_backlog(&task.id, task.revision, prompt.into(), 9)
        .await
        .unwrap();
    assert_eq!(edited.goal, task.goal);
    assert_eq!(edited.backlog_prompt.as_deref(), Some(prompt));
    let worker = worker_prompt(&edited, &[], &[], false);
    assert!(worker.contains(prompt));
    assert!(!worker.contains(&task.goal));
    assert_eq!(
        f.managed.effective_route_preferences(&edited).unwrap(),
        (Some(Provider::Codex), true)
    );
    assert!(
        f.managed
            .edit_backlog(&task.id, task.revision, "stale overwrite".into(), 0)
            .await
            .is_err()
    );
    let released = f
        .managed
        .release_backlog(&edited.id, edited.revision)
        .await
        .unwrap();
    assert!(!released.deferred);
    assert!(f.managed.has_habitat_work().unwrap());
    assert!(
        f.managed
            .edit_backlog(&released.id, released.revision, "late edit".into(), 0)
            .await
            .is_err()
    );
    assert_eq!(
        f.managed.verify_task(&task.id).await.unwrap()["revisions"],
        3
    );
}

#[tokio::test]
async fn schedule_coalesces_downtime_is_idempotent_and_never_overlaps() {
    let f = fixture().await;
    let now = now_ms();
    let schedule = f
        .managed
        .create_schedule(
            &f.conversation,
            "inspect project progress".into(),
            60_000,
            now - 180_000,
        )
        .await
        .unwrap();
    let (a, b) = tokio::join!(f.managed.tick_schedules(now), f.managed.tick_schedules(now));
    a.unwrap();
    b.unwrap();
    assert_eq!(
        f.managed.backlog(Some(&f.conversation), 64).unwrap().len(),
        1
    );
    let advanced = f.managed.schedules(None).unwrap().remove(0);
    assert_eq!(advanced.next_due_ms, now + 60_000);
    let first = f
        .managed
        .task(advanced.last_task.as_ref().unwrap())
        .unwrap()
        .unwrap();
    f.managed.tick_schedules(now + 600_000).await.unwrap();
    assert_eq!(f.managed.backlog(None, 64).unwrap().len(), 1);
    set_state(&f, &first, TaskState::Completed).await;
    f.managed.tick_schedules(now + 600_000).await.unwrap();
    assert_eq!(f.managed.backlog(None, 64).unwrap().len(), 2);
    let next = f.managed.schedules(None).unwrap().remove(0);
    assert_eq!(next.next_due_ms, now + 660_000);
    assert_ne!(next.last_task, advanced.last_task);
    let paused = f
        .managed
        .set_schedule_enabled(&schedule.id, next.revision, false)
        .unwrap();
    assert!(!paused.enabled);
    f.managed.tick_schedules(now + 900_000).await.unwrap();
    assert_eq!(f.managed.backlog(None, 64).unwrap().len(), 2);
}

#[tokio::test]
async fn pause_race_cannot_publish_a_schedule_occurrence() {
    let f = fixture().await;
    let now = now_ms();
    let schedule = f
        .managed
        .create_schedule(&f.conversation, "check status".into(), 60_000, now)
        .await
        .unwrap();
    let occurrence = Occurrence {
        schedule: schedule.clone(),
        now,
    };
    f.managed
        .set_schedule_enabled(&schedule.id, schedule.revision, false)
        .unwrap();
    let result = f
        .managed
        .create_habitat_task(
            &f.conversation,
            new_id("m"),
            schedule.prompt,
            vec![],
            &f.workspace,
            CreateOptions {
                occurrence: Some(&occurrence),
                ..CreateOptions::default()
            },
        )
        .await;
    assert!(matches!(result, Err(Error::Conflict(_))));
    assert!(f.managed.backlog(None, 64).unwrap().is_empty());
}

#[tokio::test]
async fn schedule_edit_delete_and_view_project_blocker_and_outcome() {
    let f = fixture().await;
    let now = now_ms();
    let schedule = f
        .managed
        .create_schedule(&f.conversation, "sweep".into(), 60_000, now)
        .await
        .unwrap();
    // Due and enabled with open work: the view names the blocker.
    let open = enqueue(&f, "in progress", false).await;
    let view = f.managed.schedule_view(&schedule).unwrap();
    assert_eq!(view.workspace.as_deref(), f.workspace.to_str());
    assert_eq!(view.blocker.as_deref(), Some("1 open task in this project"));
    assert!(view.last_task_state.is_none());
    // Once the work settles the view is clear.
    set_state(&f, &open, TaskState::Completed).await;
    assert!(
        f.managed
            .schedule_view(&schedule)
            .unwrap()
            .blocker
            .is_none()
    );
    // Edits carry the revision check; a stale handle cannot overwrite.
    let edited = f
        .managed
        .update_schedule(
            &schedule.id,
            schedule.revision,
            Some("deep sweep".into()),
            Some(120_000),
            Some(now + 5_000),
        )
        .unwrap();
    assert_eq!(edited.prompt, "deep sweep");
    assert_eq!(edited.interval_ms, 120_000);
    assert_eq!(edited.next_due_ms, now + 5_000);
    assert!(
        f.managed
            .update_schedule(
                &schedule.id,
                schedule.revision,
                Some("stale".into()),
                None,
                None
            )
            .is_err()
    );
    assert!(
        f.managed
            .update_schedule(&schedule.id, edited.revision, None, None, None)
            .is_err()
    );
    // One dispatch stamps the last outcome into the view.
    f.managed.tick_schedules(now + 5_000).await.unwrap();
    let current = f.managed.schedule(&schedule.id).unwrap().unwrap();
    assert!(current.last_task.is_some());
    let view = f.managed.schedule_view(&current).unwrap();
    assert_eq!(view.last_task_state, Some(TaskState::Queued));
    assert!(view.last_task_detail.is_some());
    // Deletion is revision-checked and leaves no row behind.
    assert!(f.managed.delete_schedule(&schedule.id, 1).is_err());
    f.managed
        .delete_schedule(&schedule.id, current.revision)
        .unwrap();
    assert!(f.managed.schedule(&schedule.id).unwrap().is_none());
    assert!(
        f.managed
            .schedule_views(Some(&f.conversation))
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn unresolved_attention_and_uncertainty_block_timer_work_and_survive_retention() {
    let f = fixture().await;
    let task = enqueue(&f, "existing work", false).await;
    let mut next = task.clone();
    next.state = TaskState::NeedsInput;
    next.attention = Some(State::NeedsApproval);
    next.last_output = Some("Please approve the requested action".into());
    next.revision += 1;
    let next = f.managed.transition(&task, next, None).await.unwrap();
    let now = now_ms();
    f.managed
        .create_schedule(&f.conversation, "daily work".into(), 60_000, now)
        .await
        .unwrap();
    f.managed.tick_schedules(now + 600_000).await.unwrap();
    assert_eq!(f.managed.backlog(None, 64).unwrap().len(), 1);
    assert_eq!(
        f.managed.attention(1).unwrap()[0].habitat_ui_state(),
        State::NeedsApproval
    );
    let uncertain = set_state(&f, &next, TaskState::Uncertain).await;
    f.managed
        .db()
        .unwrap()
        .execute(
            "UPDATE tasks SET updated_at=0 WHERE id=?1",
            [uncertain.id.as_str()],
        )
        .unwrap();
    f.managed.retain().unwrap();
    assert!(f.managed.task(&uncertain.id).unwrap().is_some());
    f.managed.tick_schedules(now + 900_000).await.unwrap();
    assert_eq!(f.managed.backlog(None, 64).unwrap().len(), 1);
}

#[tokio::test]
async fn worker_backlog_mutations_are_deferred_scoped_and_replay_exact_calls() {
    let f = fixture().await;
    let source = enqueue(&f, "work on project", false).await;
    let source = set_state(&f, &source, TaskState::Running).await;
    let session = source.session.as_ref().unwrap();
    let args = json!({"prompt":"follow up on the parser","priority":4});
    let (first, effects) = f
        .managed
        .habitat_worker_call(&source, session, "call-add", "xcb_backlog_add", &args)
        .await;
    assert_eq!(effects, EffectState::Settled);
    let first = first.unwrap();
    assert_eq!(first["deferred"], true);
    assert!(first.get("goal").is_none(), "tool output must be compact");
    let (again, effects) = f
        .managed
        .habitat_worker_call(&source, session, "call-add", "xcb_backlog_add", &args)
        .await;
    assert_eq!(again.unwrap(), first);
    assert_eq!(effects, EffectState::Settled);
    let changed = json!({"prompt":"different instruction","priority":4});
    assert!(
        f.managed
            .habitat_worker_call(&source, session, "call-add", "xcb_backlog_add", &changed)
            .await
            .0
            .is_err()
    );
    let target = Id::new(first["id"].as_str().unwrap()).unwrap();
    let edit = json!({"taskId":target,"expectedRevision":1,"prompt":"follow up on parser tests","priority":5});
    let (updated, _) = f
        .managed
        .habitat_worker_call(&source, session, "call-edit", "xcb_backlog_update", &edit)
        .await;
    assert_eq!(updated.unwrap()["revision"], 2);
    assert_eq!(
        f.managed
            .habitat_worker_call(&source, session, "call-edit", "xcb_backlog_update", &edit)
            .await
            .0
            .unwrap()["revision"],
        2
    );
    // Another directory is another project.
    let other = f.managed.create_conversation(&second(&f)).await.unwrap();
    let foreign = f
        .managed
        .enqueue_backlog(&other.id, new_id("m"), "another project".into(), true, 0)
        .await
        .unwrap();
    let edit =
        json!({"taskId":foreign.id,"expectedRevision":1,"prompt":"must not cross","priority":0});
    assert!(
        f.managed
            .habitat_worker_call(&source, session, "foreign", "xcb_backlog_update", &edit)
            .await
            .0
            .is_err()
    );
    assert_eq!(
        f.managed.verify_task(&target).await.unwrap()["revisions"],
        2
    );
    // Another conversation over the same directory is the same project, and
    // its edit replays exactly.
    let twin = f.managed.create_conversation(&f.workspace).await.unwrap();
    let shared = f
        .managed
        .enqueue_backlog(&twin.id, new_id("m"), "same project".into(), true, 0)
        .await
        .unwrap();
    let edit = json!({"taskId":shared.id,"expectedRevision":1,"prompt":"edited across views","priority":3});
    let (first, _) = f
        .managed
        .habitat_worker_call(&source, session, "twin", "xcb_backlog_update", &edit)
        .await;
    let first = first.unwrap();
    assert_eq!(first["revision"], 2);
    assert_eq!(first["conversation"], json!(twin.id));
    assert_eq!(
        f.managed
            .habitat_worker_call(&source, session, "twin", "xcb_backlog_update", &edit)
            .await
            .0
            .unwrap(),
        first
    );
}

#[tokio::test]
async fn worker_cancellation_and_turn_changes_are_rechecked_before_publication() {
    let f = fixture().await;
    let source = enqueue(&f, "work", false).await;
    let source = set_state(&f, &source, TaskState::Running).await;
    let session = source.session.as_ref().unwrap();
    let mutation = WorkerMutation::new(
        &source,
        session,
        "late",
        "xcb_backlog_add",
        &json!({"prompt":"late write"}),
    )
    .unwrap();
    let mut cancelled = source.clone();
    cancelled.cancel_requested = true;
    cancelled.revision += 1;
    f.managed
        .transition(&source, cancelled, None)
        .await
        .unwrap();
    let result = f
        .managed
        .create_habitat_task(
            &f.conversation,
            mutation.call.clone(),
            "late write".into(),
            vec![],
            &f.workspace,
            CreateOptions {
                deferred: true,
                worker: Some(&mutation),
                ..CreateOptions::default()
            },
        )
        .await;
    assert!(matches!(result, Err(Error::Conflict(_))));
    assert_eq!(f.managed.backlog(None, 64).unwrap().len(), 1);
}

#[tokio::test]
async fn recent_work_memory_is_bounded_and_bad_rows_do_not_break_other_work() {
    let f = fixture().await;
    let task = enqueue(&f, "finished task", false).await;
    let mut done = task.clone();
    done.state = TaskState::Completed;
    done.last_output = Some("Worker evidence. ".repeat(1000));
    done.revision += 1;
    f.managed.transition(&task, done, None).await.unwrap();
    let memory = f.managed.working_memory(&f.conversation, 32).unwrap();
    assert_eq!(memory.len(), 1);
    assert!(memory[0].summary.len() <= 2048);
    assert!(f.managed.working_memory(&f.conversation, 33).is_err());
    let context = f
        .managed
        .working_memory_context(&f.conversation, None)
        .unwrap();
    assert!(context.contains("worker-reported evidence"));
    assert!(context.contains(task.id.as_str()));
    assert!(context.len() < 2048);
    let bad = enqueue(&f, "bad row", true).await;
    f.managed
        .db()
        .unwrap()
        .execute(
            "UPDATE tasks SET payload='{}' WHERE id=?1",
            [bad.id.as_str()],
        )
        .unwrap();
    assert_eq!(f.managed.backlog(None, 64).unwrap().len(), 1);
    assert_eq!(f.managed.unreadable_tasks(), 1);
    let now = now_ms();
    let bad_schedule = f
        .managed
        .create_schedule(&f.conversation, "bad schedule".into(), 60_000, now)
        .await
        .unwrap();
    f.managed
        .db()
        .unwrap()
        .execute(
            "UPDATE habitat_schedules SET payload='{}' WHERE id=?1",
            [bad_schedule.id.as_str()],
        )
        .unwrap();
    assert!(f.managed.schedules(None).unwrap().is_empty());
    assert!(
        supervisor_fault(f.managed.root())
            .unwrap()
            .contains("could not be decoded")
    );
}

#[tokio::test]
async fn attention_includes_missing_account_and_schedules_have_explicit_bounds() {
    let f = fixture().await;
    let task = enqueue(&f, "needs a route", false).await;
    let mut blocked = task.clone();
    blocked.detail = NO_ACCOUNT_DETAIL.into();
    blocked.revision += 1;
    f.managed.transition(&task, blocked, None).await.unwrap();
    assert_eq!(
        f.managed.attention(1).unwrap()[0].habitat_ui_state(),
        State::NeedsAction
    );
    let current = f.managed.task(&task.id).unwrap().unwrap();
    let mut quota_blocked = current.clone();
    quota_blocked.detail = routing::NO_QUOTA_AVAILABLE_ROUTE.into();
    quota_blocked.revision += 1;
    f.managed
        .transition(&current, quota_blocked, None)
        .await
        .unwrap();
    let attention = f.managed.attention(1).unwrap();
    assert_eq!(attention.len(), 1);
    assert_eq!(attention[0].habitat_ui_state(), State::NeedsAction);
    assert_eq!(attention[0].detail, routing::NO_QUOTA_AVAILABLE_ROUTE);
    assert!(
        f.managed
            .create_schedule(&f.conversation, "too fast".into(), 59_999, now_ms())
            .await
            .is_err()
    );
    assert!(
        f.managed
            .create_schedule(
                &f.conversation,
                "too slow".into(),
                MAX_INTERVAL_MS + 1,
                now_ms()
            )
            .await
            .is_err()
    );
    assert!(f.managed.schedules(None).unwrap().is_empty());
}

#[tokio::test]
async fn legacy_task_receipts_survive_additive_habitat_migration() {
    let f = fixture().await;
    let task = enqueue(&f, "legacy task", false).await;
    let mut legacy = serde_json::to_value(&task).unwrap();
    for field in ["deferred", "priority", "attention", "backlog_prompt"] {
        legacy.as_object_mut().unwrap().remove(field);
    }
    legacy["last_receipt"] = json!("sha256:pending");
    let (_, receipt, payload) = ManagedStore::algal_receipt(&legacy).await.unwrap();
    legacy["last_receipt"] = json!(receipt);
    {
        let mut db = f.managed.db().unwrap();
        let tx = db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        tx.execute(
            "UPDATE tasks SET payload=?1 WHERE id=?2",
            params![serde_json::to_string(&legacy).unwrap(), task.id.as_str()],
        )
        .unwrap();
        tx.execute("DELETE FROM receipts WHERE task=?1", [task.id.as_str()])
            .unwrap();
        tx.execute(
            "INSERT INTO receipts(digest,task,revision,payload) VALUES(?1,?2,1,?3)",
            params![receipt, task.id.as_str(), payload],
        )
        .unwrap();
        tx.execute_batch(
            "DROP TABLE habitat_calls; DROP TABLE habitat_schedules; PRAGMA user_version=1;",
        )
        .unwrap();
        tx.commit().unwrap();
    }
    let state = f.managed.root().parent().unwrap().to_owned();
    drop(f.managed);
    let reopened = ManagedStore::open(&state).unwrap();
    assert_eq!(
        reopened.verify_task(&task.id).await.unwrap()["verified"],
        true
    );
    assert!(!reopened.task(&task.id).unwrap().unwrap().deferred);
    let version: u32 = reopened
        .db()
        .unwrap()
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(
        version, 7,
        "older binaries must refuse the new writer schema"
    );
}

#[tokio::test]
async fn worker_can_read_full_backlog_prompt_only_inside_its_workspace() {
    let f = fixture().await;
    let source = enqueue(&f, "active work", false).await;
    let source = set_state(&f, &source, TaskState::Running).await;
    let prompt = format!(
        "Implement parser\n{}\nPreserve this final constraint.",
        "detailed requirement\n".repeat(200)
    );
    let target = enqueue(&f, &prompt, true).await;
    let (read, effects) = f
        .managed
        .habitat_worker_call(
            &source,
            source.session.as_ref().unwrap(),
            "read",
            "xcb_backlog_get",
            &json!({"taskId":target.id}),
        )
        .await;
    assert_eq!(effects, EffectState::None);
    assert_eq!(read.unwrap()["prompt"], prompt);
    let other = f.managed.create_conversation(&second(&f)).await.unwrap();
    let foreign = f
        .managed
        .enqueue_backlog(
            &other.id,
            new_id("m"),
            "private other project".into(),
            true,
            0,
        )
        .await
        .unwrap();
    assert!(
        f.managed
            .habitat_worker_call(
                &source,
                source.session.as_ref().unwrap(),
                "foreign-read",
                "xcb_backlog_get",
                &json!({"taskId":foreign.id})
            )
            .await
            .0
            .is_err()
    );
    let twin = f.managed.create_conversation(&f.workspace).await.unwrap();
    let shared = f
        .managed
        .enqueue_backlog(&twin.id, new_id("m"), "shared project".into(), true, 0)
        .await
        .unwrap();
    let (read, _) = f
        .managed
        .habitat_worker_call(
            &source,
            source.session.as_ref().unwrap(),
            "twin-read",
            "xcb_backlog_get",
            &json!({"taskId":shared.id}),
        )
        .await;
    let read = read.unwrap();
    assert_eq!(read["prompt"], "shared project");
    assert_eq!(read["workspace"], json!(f.workspace));
    assert!(
        f.managed
            .habitat_worker_call(
                &source,
                source.session.as_ref().unwrap(),
                "invalid-read",
                "xcb_backlog_get",
                &json!({"taskId":target.id,"all":true})
            )
            .await
            .0
            .is_err()
    );
}

#[tokio::test]
async fn empty_worker_reports_keep_a_useful_history_summary() {
    let f = fixture().await;
    let task = enqueue(&f, "cancelled task", false).await;
    let mut stopped = task.clone();
    stopped.state = TaskState::Cancelled;
    stopped.detail = "worker cancellation settled".into();
    stopped.last_output = Some("  ".into());
    stopped.revision += 1;
    let stopped = f.managed.transition(&task, stopped, None).await.unwrap();
    assert_eq!(
        f.managed.working_memory(&f.conversation, 1).unwrap()[0].summary,
        "worker cancellation settled"
    );
    assert_eq!(
        compact_task(&stopped)["summary"],
        "worker cancellation settled"
    );
    assert_eq!(backlog_row(&stopped).summary, "worker cancellation settled");
}

#[tokio::test]
async fn future_schedules_keep_host_alive_without_decoding_prompts_or_creating_work() {
    let f = fixture().await;
    let now = now_ms();
    let schedule = f
        .managed
        .create_schedule(
            &f.conversation,
            "large future prompt ".repeat(1000),
            60_000,
            now + 60_000,
        )
        .await
        .unwrap();
    assert!(f.managed.has_habitat_work().unwrap());
    assert!(f.managed.due_schedules(now).unwrap().is_empty());
    f.managed.tick_schedules(now).await.unwrap();
    assert!(f.managed.backlog(None, 64).unwrap().is_empty());
    // The due/keepalive probes consult the indexed host fields, not every
    // future prompt. An undecodable future row is surfaced when inspected.
    f.managed
        .db()
        .unwrap()
        .execute(
            "UPDATE habitat_schedules SET payload='{}' WHERE id=?1",
            [schedule.id.as_str()],
        )
        .unwrap();
    assert!(f.managed.has_habitat_work().unwrap());
    assert!(f.managed.due_schedules(now).unwrap().is_empty());
    assert!(supervisor_fault(f.managed.root()).is_none());
}

fn text(path: &Path) -> &str {
    path.to_str().unwrap()
}

async fn thread(managed: &ManagedStore) -> Id {
    managed.global_thread().await.unwrap().id
}

#[cfg_attr(windows, allow(dead_code))]
async fn running(managed: &ManagedStore, task: &ManagedTask) -> ManagedTask {
    let mut next = task.clone();
    next.state = TaskState::Running;
    next.session = Some(new_id("s"));
    next.revision += 1;
    next.updated_at_ms = now_ms().max(task.updated_at_ms);
    managed.transition(task, next, None).await.unwrap()
}

#[cfg_attr(windows, allow(dead_code))]
async fn settle(managed: &ManagedStore, task: &ManagedTask, state: TaskState) -> ManagedTask {
    let mut next = task.clone();
    next.state = state;
    next.session = None;
    next.deferred = false;
    next.revision += 1;
    next.updated_at_ms = now_ms().max(task.updated_at_ms);
    managed.transition(task, next, None).await.unwrap()
}

fn inherited(origin: BindingOrigin, reason: String) -> Option<WorkspaceBinding> {
    Some(WorkspaceBinding {
        source: BindingSource::Inherited,
        confidence: BindingConfidence::High,
        origin,
        reason,
        alternatives: vec![],
    })
}

#[tokio::test]
async fn schedule_in_thread_requires_workspace_and_waits_only_on_its_workspace() {
    let f = fixture().await;
    let (a, b) = (f.workspace.clone(), second(&f));
    let thread = thread(&f.managed).await;
    let now = now_ms();
    assert!(
        f.managed
            .create_schedule(&thread, "Check".into(), 60_000, now)
            .await
            .is_err()
    );
    assert!(
        f.managed
            .create_schedule_at(&thread, Some(&a.join(".")), "Check".into(), 60_000, now)
            .await
            .is_err(),
        "a thread schedule's directory must already be canonical"
    );
    let schedule = f
        .managed
        .create_schedule_at(&thread, Some(&a), "Check A".into(), 60_000, now)
        .await
        .unwrap();
    assert_eq!(schedule.workspace.as_deref(), Some(text(&a)));
    // Undeferred work in B never holds A's schedule.
    let busy = in_thread(&f.managed, &b, "Busy in B").await;
    assert!(!busy.deferred);
    f.managed.tick_schedules(now).await.unwrap();
    let fired = f.managed.schedules(Some(&thread)).unwrap().pop().unwrap();
    let occurrence = f
        .managed
        .task(fired.last_task.as_ref().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(occurrence.workspace, text(&a));
    assert_eq!(occurrence.conversation, thread);
    assert_eq!(
        occurrence.binding,
        inherited(BindingOrigin::Schedule, format!("schedule {}", schedule.id))
    );
    // The occurrence itself is A's outstanding work: the next wake waits.
    f.managed.tick_schedules(fired.next_due_ms).await.unwrap();
    let waited = f.managed.schedules(Some(&thread)).unwrap().pop().unwrap();
    assert_eq!(waited.last_task, fired.last_task);
    assert_eq!(waited.revision, fired.revision);
}

/// A trusted fake Wordcell CLI that answers exact search.
#[cfg(unix)]
fn wordcell_fixture(base: &Path) -> crate::wordcell::WordcellConfig {
    use std::os::unix::fs::PermissionsExt;
    let tools = private::directory(&base.join("tools")).unwrap();
    let vault = private::directory(&tools.join("vault")).unwrap();
    let executable = tools.join("wordcell");
    std::fs::write(
        &executable,
        "#!/bin/sh\ntest \"$1\" = search || exit 2\nprintf '{\"results\":[]}'\n",
    )
    .unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    crate::wordcell::WordcellConfig::admit(&executable, &vault).unwrap()
}

#[cfg(unix)]
#[tokio::test]
async fn worker_in_a_cannot_get_update_complete_or_search_memory_of_b() {
    let f = fixture().await;
    let (a, b) = (f.workspace.clone(), second(&f));
    let thread = thread(&f.managed).await;
    let source = running(&f.managed, &in_thread(&f.managed, &a, "Work in A").await).await;
    let session = source.session.clone().unwrap();
    let held_b = f
        .managed
        .enqueue_backlog_at(
            &thread,
            Some(&b),
            BindingOrigin::Cli,
            new_id("m"),
            "Held in B".into(),
            true,
            0,
            None,
        )
        .await
        .unwrap();
    let call = |name: &'static str, args: Value| {
        let (managed, source, session) = (&f.managed, &source, &session);
        async move {
            managed
                .habitat_worker_call(source, session, &format!("{name}-b"), name, &args)
                .await
                .0
        }
    };
    assert!(
        call("xcb_backlog_get", json!({"taskId":held_b.id}))
            .await
            .is_err()
    );
    assert!(
        call(
            "xcb_backlog_update",
            json!({"taskId":held_b.id,"expectedRevision":held_b.revision,"prompt":"crossed"})
        )
        .await
        .is_err()
    );
    assert!(
        call(
            "xcb_backlog_complete",
            json!({"taskId":held_b.id,"expectedRevision":held_b.revision,"summary":"crossed"})
        )
        .await
        .is_err()
    );
    assert_eq!(
        f.managed.task(&held_b.id).unwrap().unwrap().revision,
        held_b.revision
    );
    // Listings stay inside A.
    let listed = call("xcb_backlog_list", json!({})).await.unwrap();
    assert_eq!(listed["workspace"], json!(text(&a)));
    assert!(
        listed["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .all(|task| task["workspace"] == json!(text(&a)))
    );
    // A provider may ignore the advertised schema bound. The worker clamps
    // the request and keeps the unattended turn alive instead of returning a
    // harmless validation error.
    let oversized = call("xcb_backlog_list", json!({"limit": 1_000_000}))
        .await
        .unwrap();
    assert!(oversized["tasks"].as_array().unwrap().len() <= 64);
    let done_b = f
        .managed
        .enqueue_backlog_at(
            &thread,
            Some(&b),
            BindingOrigin::Cli,
            new_id("m"),
            "Done in B".into(),
            true,
            0,
            None,
        )
        .await
        .unwrap();
    f.managed
        .complete_backlog(&done_b.id, done_b.revision, "Finished B".into())
        .await
        .unwrap();
    let recent = call("xcb_memory_recent", json!({})).await.unwrap();
    assert!(
        recent["memory"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["task"] != json!(done_b.id))
    );
    // B's Wordcell binding is never searched from A.
    f.managed
        .bind_memory_in(&b, None, wordcell_fixture(a.parent().unwrap()))
        .unwrap();
    assert!(matches!(
        call("xcb_memory_search", json!({"query":"parser"})).await,
        Err(Error::Unavailable(
            "project Wordcell memory is not configured"
        ))
    ));
    let worker_b = running(&f.managed, &in_thread(&f.managed, &b, "Work in B").await).await;
    assert_eq!(
        f.managed
            .habitat_worker_call(
                &worker_b,
                worker_b.session.as_ref().unwrap(),
                "search",
                "xcb_memory_search",
                &json!({"query":"parser"})
            )
            .await
            .0
            .unwrap(),
        json!({"results":[]})
    );
}

// Daemons run provider agents, which Windows refuses.

#[cfg(unix)]
#[tokio::test]
async fn thread_children_carry_inherited_bindings_and_replay_exactly() {
    let root = tempfile::tempdir().unwrap();
    let base = xcb_core::canonical(root.path()).unwrap();
    let state = private::directory(&base.join("state")).unwrap();
    let a = private::directory(&base.join("project")).unwrap();
    let managed = Arc::new(ManagedStore::open(&state).unwrap());
    let store = Store::open(&state).unwrap();
    let thread = thread(&managed).await;
    managed
        .configure_project_policy_in(
            &a,
            None,
            "Maintain A".into(),
            8,
            now_ms() + 7_200_000,
            None,
            0,
            0,
        )
        .unwrap();

    // Schedule occurrence.
    let now = now_ms();
    let schedule = managed
        .create_schedule_at(&thread, Some(&a), "Check A".into(), 60_000, now)
        .await
        .unwrap();
    managed.tick_schedules(now).await.unwrap();
    managed.tick_schedules(now).await.unwrap();
    let fired = managed.schedules(Some(&thread)).unwrap().pop().unwrap();
    let occurrence = managed
        .task(fired.last_task.as_ref().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(fired.revision, schedule.revision + 1);
    assert_eq!(
        occurrence.binding,
        inherited(BindingOrigin::Schedule, format!("schedule {}", schedule.id))
    );
    managed.verify_task(&occurrence.id).await.unwrap();
    settle(&managed, &occurrence, TaskState::Completed).await;

    // Worker `xcb_backlog_add`.
    let source = running(&managed, &in_thread(&managed, &a, "Work in A").await).await;
    let args = json!({"prompt":"Follow up in A"});
    let add = || {
        managed.habitat_worker_call(
            &source,
            source.session.as_ref().unwrap(),
            "add",
            "xcb_backlog_add",
            &args,
        )
    };
    let first = add().await.0.unwrap();
    assert_eq!(add().await.0.unwrap(), first);
    let proposal = managed
        .task(&Id::new(first["id"].as_str().unwrap()).unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(proposal.workspace, text(&a));
    assert_eq!(
        proposal.binding,
        inherited(BindingOrigin::Worker, format!("from {}", source.id))
    );
    managed.verify_task(&proposal.id).await.unwrap();
    settle(&managed, &source, TaskState::Completed).await;

    // Program child.
    let program = crate::managed_program::AdmittedProgram::admit_managed(
        json!({"contract":"algal.organism.v1","key":"organism:thread-child","name":"Thread child",
               "cells":[{"id":"worker","kind":"agent","prompt":"Review project state","output":{"kind":"text"}}],
               "edges":[],"interface":{"inputs":{},"outputs":{"summary":{"cell":"worker","port":"out"}}}}),
        json!({}),
        1,
    )
    .unwrap();
    let operation = new_id("m");
    let parent = managed
        .enqueue_program_at(
            &thread,
            Some(&a),
            BindingOrigin::Cli,
            operation.clone(),
            "Controller".into(),
            program.clone(),
        )
        .await
        .unwrap();
    assert_eq!(parent.binding.as_ref().unwrap().origin, BindingOrigin::Cli);
    let input = managed.program_slice_input(&parent).unwrap();
    let mut next = parent.clone();
    next.state = TaskState::Running;
    next.revision += 1;
    let started = managed.transition(&parent, next, None).await.unwrap();
    let (_sender, cancel) = watch::channel(false);
    let slice = started
        .program
        .as_ref()
        .unwrap()
        .step(input.0, input.1, cancel)
        .await
        .unwrap();
    let waiting = managed
        .finish_program_slice(&started.id, started.revision, &Ok(slice))
        .await
        .unwrap();
    let child = managed
        .task(
            managed
                .program_status(&waiting.id)
                .unwrap()
                .unwrap()
                .child
                .as_ref()
                .unwrap(),
        )
        .unwrap()
        .unwrap();
    assert_eq!(child.workspace, text(&a));
    assert_eq!(
        child.binding,
        inherited(BindingOrigin::Program, format!("from {}", parent.id))
    );
    managed.verify_task(&child.id).await.unwrap();
    // A retried submission replays the committed controller.
    assert_eq!(
        managed
            .enqueue_program_at(
                &thread,
                Some(&a),
                BindingOrigin::Cli,
                operation,
                "Controller".into(),
                program,
            )
            .await
            .unwrap()
            .id,
        parent.id
    );

    // `finish_program` follow-up.
    let planner = crate::managed_program::AdmittedProgram::admit(
        json!({"contract":"algal.organism.v1","key":"organism:thread-planner","name":"Planner",
               "cells":[{"id":"report","kind":"const","outputs":{"value":{"type":"text","value":"Reviewed"}}},
                        {"id":"proposal","kind":"const","outputs":{"value":{"type":"text","value":"Run the next check"}}}],
               "edges":[],"interface":{"inputs":{},"outputs":{"summary":{"cell":"report","port":"value"},"prompt":{"cell":"proposal","port":"value"}}}}),
        json!({}),
    )
    .unwrap();
    let planned = managed
        .enqueue_program_at(
            &thread,
            Some(&a),
            BindingOrigin::Cli,
            new_id("m"),
            "Planner".into(),
            planner.clone(),
        )
        .await
        .unwrap();
    let mut next = planned.clone();
    next.state = TaskState::Running;
    next.revision += 1;
    managed.transition(&planned, next, None).await.unwrap();
    let (_sender, cancel) = watch::channel(false);
    let report = planner.run(cancel).await.unwrap();
    managed
        .finish_program(&planned.id, &Ok(report))
        .await
        .unwrap();
    let follow_up = managed
        .backlog_in(text(&a), 64)
        .unwrap()
        .into_iter()
        .find(|task| task.goal == "Run the next check")
        .unwrap();
    assert_eq!(
        follow_up.binding,
        inherited(BindingOrigin::Program, format!("from {}", planned.id))
    );
    managed.verify_task(&follow_up.id).await.unwrap();

    // Daemon child.
    let daemon = AdmittedDaemon::admit(
        json!({"contract":"algal.organism.v1","key":"organism:daemon-worker","name":"Daemon worker",
               "cells":[{"id":"work","kind":"agent","prompt":"Do a bounded project task","output":{"kind":"text"}}],
               "interface":{"inputs":{},"outputs":{"summary":{"cell":"work","port":"out"}}}}),
        json!({}),
        1,
        4,
    )
    .unwrap();
    managed
        .enqueue_daemon_at(&thread, Some(&a), "worker", &daemon)
        .unwrap();
    for _ in 0..4 {
        managed.tick_daemons(&store, true).await.unwrap();
    }
    let status = managed.daemon_status_for("worker").unwrap().unwrap();
    assert_eq!(status.conversation, thread);
    let daemon_child = managed
        .task(status.pending_child.as_ref().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(daemon_child.workspace, text(&a));
    assert_eq!(
        daemon_child.binding,
        inherited(BindingOrigin::Daemon, "from daemon worker".into())
    );
    managed.verify_task(&daemon_child.id).await.unwrap();
    assert_eq!(managed.project_dispatch_block(&daemon_child).unwrap(), None);
}

fn pin_model(provider: Provider, id: &str, effort: Option<&str>) -> xcb_core::models::ModelChoice {
    xcb_core::models::ModelChoice {
        provider,
        id: Id::new(id).unwrap(),
        label: id.into(),
        mode: xcb_core::models::Mode::Fixed,
        resolved: None,
        effort: effort.map(|effort| Id::new(effort).unwrap()),
        observed_at_ms: 1,
    }
}

#[tokio::test]
async fn backlog_add_resolves_a_model_pin_to_its_observed_key() {
    let f = fixture().await;
    let xcb = Store::open(f.managed.root().parent().unwrap()).unwrap();
    xcb.set_models(
        Provider::Codex,
        &[
            pin_model(Provider::Codex, "gpt-6-sol", Some("ultra")),
            pin_model(Provider::Codex, "gpt-6-sol", Some("low")),
        ],
    )
    .unwrap();
    let task = f
        .managed
        .enqueue_backlog_at(
            &f.conversation,
            None,
            BindingOrigin::Cli,
            new_id("m"),
            "Pinned work".into(),
            true,
            0,
            Some("codex/gpt-6-sol/ultra".into()),
        )
        .await
        .unwrap();
    assert_eq!(
        task.required_model.as_deref(),
        Some("codex/gpt-6-sol/ultra")
    );
    assert_eq!(task.provider_preference, Some(Provider::Codex));
    assert!(task.provider_required);
    assert_eq!(task.route.as_deref(), Some("codex/gpt-6-sol/ultra"));
    assert_eq!(
        task.route_reason.as_deref(),
        Some("user required codex/gpt-6-sol/ultra")
    );
    f.managed.verify_task(&task.id).await.unwrap();
}

#[tokio::test]
async fn backlog_add_refuses_unresolvable_or_ambiguous_model_pins() {
    let f = fixture().await;
    let xcb = Store::open(f.managed.root().parent().unwrap()).unwrap();
    xcb.set_models(
        Provider::Codex,
        &[
            pin_model(Provider::Codex, "gpt-6-sol", Some("ultra")),
            pin_model(Provider::Codex, "gpt-6-sol", Some("low")),
        ],
    )
    .unwrap();
    assert!(
        f.managed
            .enqueue_backlog_at(
                &f.conversation,
                None,
                BindingOrigin::Cli,
                new_id("m"),
                "Pinned work".into(),
                true,
                0,
                Some("codex/gpt-9-missing/ultra".into()),
            )
            .await
            .is_err_and(|error| error.to_string().contains("not observed"))
    );
    // A bare id matching two efforts names no single route.
    assert!(
        f.managed
            .enqueue_backlog_at(
                &f.conversation,
                None,
                BindingOrigin::Cli,
                new_id("m"),
                "Pinned work".into(),
                true,
                0,
                Some("gpt-6-sol".into()),
            )
            .await
            .is_err_and(|error| error.to_string().contains("ambiguous"))
    );
}

#[tokio::test]
async fn backlog_add_refuses_a_never_excluded_pin() {
    let f = fixture().await;
    let xcb = Store::open(f.managed.root().parent().unwrap()).unwrap();
    xcb.set_models(
        Provider::Codex,
        &[pin_model(Provider::Codex, "gpt-6-sol", Some("ultra"))],
    )
    .unwrap();
    let (mut config, revision) = Config::load(f.managed.root().parent().unwrap()).unwrap();
    config.routing.never.push("codex/gpt-*-sol/*".into());
    config
        .save(f.managed.root().parent().unwrap(), revision.as_deref())
        .unwrap();
    assert!(
        f.managed
            .enqueue_backlog_at(
                &f.conversation,
                None,
                BindingOrigin::Cli,
                new_id("m"),
                "Pinned work".into(),
                true,
                0,
                Some("codex/gpt-6-sol/ultra".into()),
            )
            .await
            .is_err_and(|error| error.to_string().contains("routing.never"))
    );
}

#[tokio::test]
async fn backlog_add_refuses_a_pin_that_contradicts_a_required_provider() {
    let f = fixture().await;
    let xcb = Store::open(f.managed.root().parent().unwrap()).unwrap();
    xcb.set_models(
        Provider::Codex,
        &[pin_model(Provider::Codex, "gpt-6-sol", Some("ultra"))],
    )
    .unwrap();
    assert!(
        f.managed
            .enqueue_backlog_at(
                &f.conversation,
                None,
                BindingOrigin::Cli,
                new_id("m"),
                "Use Claude. Pinned work".into(),
                true,
                0,
                Some("codex/gpt-6-sol/ultra".into()),
            )
            .await
            .is_err_and(|error| error.to_string().contains("required provider"))
    );
    // The same pin on a matching directive is admitted.
    let task = f
        .managed
        .enqueue_backlog_at(
            &f.conversation,
            None,
            BindingOrigin::Cli,
            new_id("m"),
            "Use Codex. Pinned work".into(),
            true,
            0,
            Some("codex/gpt-6-sol/ultra".into()),
        )
        .await
        .unwrap();
    assert_eq!(
        task.required_model.as_deref(),
        Some("codex/gpt-6-sol/ultra")
    );
    assert!(task.provider_required);
}

#[tokio::test]
async fn a_schedule_dismisses_released_uncertainty_only_when_the_owner_opted_in() {
    let f = fixture().await;
    let store = Store::open(f.managed.root().parent().unwrap()).unwrap();
    let schedule = f
        .managed
        .create_schedule(&f.conversation, "herd".into(), 60_000, now_ms())
        .await
        .unwrap();
    let open = enqueue(&f, "unprovable", false).await;
    let uncertain = set_state(&f, &open, TaskState::Uncertain).await;
    // Off by default: a timer never infers that uncertainty settled.
    f.managed.tick_schedule_dismissals(&store).await.unwrap();
    assert_eq!(
        f.managed.task(&uncertain.id).unwrap().unwrap().state,
        TaskState::Uncertain
    );
    assert_eq!(
        f.managed
            .schedule_view(&schedule)
            .unwrap()
            .blocker
            .as_deref(),
        Some("1 open task in this project")
    );
    let opted = f
        .managed
        .set_schedule_dismissal(&schedule.id, schedule.revision, true)
        .unwrap();
    assert!(opted.dismiss_released_uncertainty);
    assert!(
        f.managed
            .set_schedule_dismissal(&schedule.id, schedule.revision, false)
            .is_err()
    );
    f.managed.tick_schedule_dismissals(&store).await.unwrap();
    let dismissed = f.managed.task(&uncertain.id).unwrap().unwrap();
    assert_eq!(dismissed.state, TaskState::Failed);
    assert!(dismissed.dismissed);
    assert_eq!(dismissed.detail, SCHEDULE_DISMISSED_DETAIL);
    assert!(f.managed.schedule_view(&opted).unwrap().blocker.is_none());
}
