use super::*;
struct Fixture {
    _root: tempfile::TempDir,
    managed: ManagedStore,
    store: Store,
    conversation: Id,
    workspace: PathBuf,
}
async fn fixture() -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let base = xcb_core::canonical(root.path()).unwrap();
    let workspace = private::directory(&base.join("work")).unwrap();
    let state = base.join("state");
    let managed = ManagedStore::open(&state).unwrap();
    let store = Store::open(&state).unwrap();
    let conversation = managed.create_conversation(&workspace).await.unwrap().id;
    Fixture {
        _root: root,
        managed,
        store,
        conversation,
        workspace,
    }
}
/// A sibling project directory beside the fixture's workspace.
fn second(f: &Fixture) -> PathBuf {
    private::directory(&f.workspace.parent().unwrap().join("other")).unwrap()
}
fn explicit(path: &Path) -> IntakeCues {
    IntakeCues {
        origin: Origin::Cli,
        explicit: Some(path.to_owned()),
        target: None,
        focus: None,
        launch_hint: None,
        infer_only: false,
    }
}
/// A task in the global thread, bound explicitly to `workspace`.
async fn in_thread(f: &Fixture, workspace: &Path, prompt: &str) -> ManagedTask {
    match f
        .managed
        .submit_to_thread(new_id("m"), prompt.into(), vec![], explicit(workspace))
        .await
        .unwrap()
    {
        Intake::Accepted { task, .. } => task,
        Intake::Ask { reason, .. } => panic!("thread asked: {reason}"),
    }
}
async fn worker_call(
    f: &Fixture,
    source: &ManagedTask,
    call: &str,
    name: &str,
    args: &Value,
) -> Result<Value> {
    f.managed
        .habitat_worker_call(source, source.session.as_ref().unwrap(), call, name, args)
        .await
        .0
}
async fn enqueue(f: &Fixture, prompt: &str, deferred: bool) -> ManagedTask {
    f.managed
        .enqueue_backlog(&f.conversation, new_id("m"), prompt.into(), deferred, 5)
        .await
        .unwrap()
}
async fn state(f: &Fixture, task: &ManagedTask, state: TaskState) -> ManagedTask {
    let mut next = task.clone();
    next.state = state;
    next.revision += 1;
    next.updated_at_ms = now_ms().max(task.updated_at_ms);
    if state == TaskState::Running {
        next.session = Some(new_id("s"));
    }
    f.managed.transition(task, next, None).await.unwrap()
}
fn grant(f: &Fixture, max: u32) -> ProjectPolicy {
    f.managed
        .configure_project_policy(
            &f.conversation,
            None,
            "Maintain the project".into(),
            max,
            now_ms() + 7_200_000,
            Some(Provider::Codex),
        )
        .unwrap()
}
async fn propose(f: &Fixture, parent: &ManagedTask, call: &str, prompt: &str) -> ManagedTask {
    let (result, _) = f
        .managed
        .habitat_worker_call(
            parent,
            parent.session.as_ref().unwrap(),
            call,
            "xcb_backlog_add",
            &json!({"prompt":prompt,"priority":5}),
        )
        .await;
    let id = Id::new(result.unwrap()["id"].as_str().unwrap()).unwrap();
    f.managed.task(&id).unwrap().unwrap()
}
#[tokio::test]
async fn admission_waits_for_parent_and_excludes_user_backlog_and_is_atomic() {
    let f = fixture().await;
    grant(&f, 1);
    let user = enqueue(&f, "User idea", true).await;
    let parent = state(
        &f,
        &enqueue(&f, "Initial work", false).await,
        TaskState::Running,
    )
    .await;
    let first = propose(&f, &parent, "one", "Follow-up one").await;
    let second = propose(&f, &parent, "two", "Follow-up two").await;
    f.managed.tick_projects(now_ms()).await.unwrap();
    assert!(f.managed.task(&first.id).unwrap().unwrap().deferred);
    state(&f, &parent, TaskState::Completed).await;
    let (a, b) = tokio::join!(
        f.managed.tick_projects(now_ms()),
        f.managed.tick_projects(now_ms())
    );
    a.unwrap();
    b.unwrap();
    let tasks = [first.id, second.id].map(|id| f.managed.task(&id).unwrap().unwrap());
    assert_eq!(tasks.iter().filter(|t| !t.deferred).count(), 1);
    assert!(f.managed.task(&user.id).unwrap().unwrap().deferred);
    assert_eq!(
        f.managed
            .project_policy(&f.conversation)
            .unwrap()
            .unwrap()
            .admitted_tasks,
        1
    );
    let admitted = tasks.iter().find(|t| !t.deferred).unwrap();
    assert_eq!(
        f.managed.effective_route_preferences(admitted).unwrap(),
        (Some(Provider::Codex), true)
    );
}
#[tokio::test]
async fn pause_resume_preserves_budget_and_generation_and_blocks_dispatch() {
    let f = fixture().await;
    grant(&f, 2);
    let parent = state(
        &f,
        &enqueue(&f, "Initial work", false).await,
        TaskState::Running,
    )
    .await;
    let proposal = propose(&f, &parent, "one", "Follow-up").await;
    state(&f, &parent, TaskState::Completed).await;
    f.managed.tick_projects(now_ms()).await.unwrap();
    let admitted = f.managed.task(&proposal.id).unwrap().unwrap();
    let policy = f.managed.project_policy(&f.conversation).unwrap().unwrap();
    let paused = f
        .managed
        .set_project_policy_enabled(&f.conversation, policy.revision, false)
        .unwrap();
    assert_eq!(paused.admitted_tasks, 1);
    assert!(
        f.managed
            .project_dispatch_block(&admitted)
            .unwrap()
            .is_some()
    );
    let resumed = f
        .managed
        .set_project_policy_enabled(&f.conversation, paused.revision, true)
        .unwrap();
    assert_eq!(resumed.generation, policy.generation);
    assert_eq!(resumed.admitted_tasks, 1);
    assert!(
        f.managed
            .project_dispatch_block(&admitted)
            .unwrap()
            .is_none()
    );
    let replaced = f
        .managed
        .configure_project_policy(
            &f.conversation,
            Some(resumed.revision),
            "New goal".into(),
            3,
            now_ms() + 7_200_000,
            None,
        )
        .unwrap();
    assert_ne!(replaced.generation, policy.generation);
    assert!(
        f.managed
            .project_dispatch_block(&admitted)
            .unwrap()
            .is_some()
    );
    assert_eq!(
        f.managed.effective_route_preferences(&admitted).unwrap(),
        (Some(Provider::Codex), true)
    );
}
#[tokio::test]
async fn provider_conflict_and_uncertain_work_block_automatic_admission() {
    let f = fixture().await;
    grant(&f, 2);
    let parent = state(
        &f,
        &enqueue(&f, "Initial work", false).await,
        TaskState::Running,
    )
    .await;
    let conflict = propose(&f, &parent, "one", "Use Claude to continue").await;
    let good = propose(&f, &parent, "two", "Continue checks").await;
    state(&f, &parent, TaskState::Completed).await;
    let blocker = state(
        &f,
        &enqueue(&f, "Unresolved work", false).await,
        TaskState::Uncertain,
    )
    .await;
    f.managed.tick_projects(now_ms()).await.unwrap();
    assert!(f.managed.task(&good.id).unwrap().unwrap().deferred);
    state(&f, &blocker, TaskState::Failed).await;
    f.managed.tick_projects(now_ms()).await.unwrap();
    let question = f.managed.task(&conflict.id).unwrap().unwrap();
    assert_eq!(question.state, TaskState::NeedsInput);
    assert!(question.routing_question);
    assert!(f.managed.task(&good.id).unwrap().unwrap().deferred);
    f.managed
        .reply_to_task(&question.id, "Use Codex to continue".into())
        .await
        .unwrap();
    f.managed.tick_projects(now_ms()).await.unwrap();
    assert!(!f.managed.task(&question.id).unwrap().unwrap().deferred);
}
#[tokio::test]
async fn completed_backlog_is_audited_and_worker_cannot_close_another_project() {
    let f = fixture().await;
    let work = enqueue(&f, "Already addressed", true).await;
    let done = f
        .managed
        .complete_backlog(&work.id, work.revision, "Verified by the user".into())
        .await
        .unwrap();
    assert_eq!(done.state, TaskState::Completed);
    assert!(!done.deferred);
    assert!(
        f.managed
            .complete_backlog(&work.id, work.revision, "stale".into())
            .await
            .is_err()
    );
    assert_eq!(
        f.managed.verify_task(&done.id).await.unwrap()["revisions"],
        2
    );
    let parent = state(&f, &enqueue(&f, "worker", false).await, TaskState::Running).await;
    let complete = |task: &ManagedTask| json!({"taskId":task.id,"expectedRevision":task.revision,"summary":"claim"});
    // Another directory is another project, whatever the conversation.
    let other = f.managed.create_conversation(&second(&f)).await.unwrap();
    let other_task = f
        .managed
        .enqueue_backlog(&other.id, new_id("m"), "other project".into(), true, 5)
        .await
        .unwrap();
    let args = complete(&other_task);
    assert!(
        worker_call(&f, &parent, "cross", "xcb_backlog_complete", &args)
            .await
            .is_err()
    );
    // A second conversation over the same directory is the same project.
    let twin = f.managed.create_conversation(&f.workspace).await.unwrap();
    let twin_task = f
        .managed
        .enqueue_backlog(&twin.id, new_id("m"), "same project".into(), true, 5)
        .await
        .unwrap();
    let args = complete(&twin_task);
    worker_call(&f, &parent, "same", "xcb_backlog_complete", &args)
        .await
        .unwrap();
    assert_eq!(
        f.managed.task(&twin_task.id).unwrap().unwrap().state,
        TaskState::Completed
    );
}
#[tokio::test]
async fn uncertainty_cannot_be_cleared_by_absence_of_a_process_alone() {
    let f = fixture().await;
    let task = state(&f, &enqueue(&f, "Work", false).await, TaskState::Uncertain).await;
    assert!(
        f.managed
            .reconcile_uncertain(&f.store, &task.id, task.revision)
            .await
            .is_err()
    );
    assert_eq!(
        f.managed.task(&task.id).unwrap().unwrap().state,
        TaskState::Uncertain
    );
}
#[tokio::test]
async fn schema_upgrade_is_additive_and_repeat_open_keeps_grants() {
    let f = fixture().await;
    let policy = grant(&f, 4);
    let reopened = ManagedStore::open(f.managed.root().parent().unwrap()).unwrap();
    assert_eq!(
        reopened
            .project_policy(&f.conversation)
            .unwrap()
            .unwrap()
            .generation,
        policy.generation
    );
    let version: u32 = reopened
        .db()
        .unwrap()
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, 7);
}

fn planner() -> crate::managed_program::AdmittedProgram {
    crate::managed_program::AdmittedProgram::admit(json!({"contract":"algal.organism.v1","key":"organism:test-planner","name":"Planner","cells":[{"id":"report","kind":"const","outputs":{"value":{"type":"text","value":"Reviewed project state"}}},{"id":"proposal","kind":"const","outputs":{"value":{"type":"text","value":"Run the next project check"}}}],"edges":[],"interface":{"inputs":{},"outputs":{"summary":{"cell":"report","port":"value"},"prompt":{"cell":"proposal","port":"value"}}}}),json!({})).unwrap()
}
#[tokio::test]
async fn pure_scheduled_program_completes_without_provider_and_proposes_once() {
    let f = fixture().await;
    let mut config = Config::default();
    config.extensions.reflexes.settle = crate::config::ReflexMode::Active;
    config.save(f.store.root(), None).unwrap();
    grant(&f, 2);
    let now = now_ms();
    let schedule = f
        .managed
        .create_program_schedule(
            &f.conversation,
            "Project planner".into(),
            planner(),
            60_000,
            now,
        )
        .await
        .unwrap();
    f.managed.tick_schedules(now).await.unwrap();
    let schedule = f
        .managed
        .schedules(None)
        .unwrap()
        .into_iter()
        .find(|s| s.id == schedule.id)
        .unwrap();
    let task = f
        .managed
        .task(schedule.last_task.as_ref().unwrap())
        .unwrap()
        .unwrap();
    let managed = Arc::new(f.managed);
    let store = Arc::new(f.store);
    let mut supervisor = Supervisor::new(managed.clone(), store.clone());
    assert!(matches!(
        supervisor.launch(&task).await.unwrap(),
        Dispatch::Started
    ));
    let completion = supervisor.joins.join_next().await.unwrap().unwrap();
    supervisor.active.clear();
    supervisor.active_workspaces.clear();
    supervisor.record(completion, 0).await;
    let completed = managed.task(&task.id).unwrap().unwrap();
    assert_eq!(completed.state, TaskState::Completed);
    assert!(completed.program_receipt.is_some());
    assert!(completed.settle.is_none());
    assert!(completed.session.is_none());
    assert!(store.sessions(32).unwrap().is_empty());
    let backlog = managed.backlog(Some(&f.conversation), 64).unwrap();
    assert_eq!(backlog.len(), 2);
    let child = backlog.iter().find(|t| t.id != task.id).unwrap();
    assert!(child.deferred);
    assert_eq!(child.project_proposal.as_ref().unwrap().parent, task.id);
    let (_cancel, cancelled) = watch::channel(false);
    let report = planner().run(cancelled).await.unwrap();
    managed.finish_program(&task.id, &Ok(report)).await.unwrap();
    assert_eq!(managed.backlog(Some(&f.conversation), 64).unwrap().len(), 2);
    managed.tick_projects(now_ms()).await.unwrap();
    assert!(!managed.task(&child.id).unwrap().unwrap().deferred);
}
#[tokio::test]
async fn paused_project_preserves_due_schedule_until_resume() {
    let f = fixture().await;
    let policy = grant(&f, 2);
    let paused = f
        .managed
        .set_project_policy_enabled(&f.conversation, policy.revision, false)
        .unwrap();
    let due = now_ms();
    let schedule = f
        .managed
        .create_schedule(&f.conversation, "Scheduled review".into(), 60_000, due)
        .await
        .unwrap();
    f.managed.tick_schedules(due).await.unwrap();
    assert!(
        f.managed
            .backlog(Some(&f.conversation), 64)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        f.managed.schedules(None).unwrap()[0].next_due_ms,
        schedule.next_due_ms
    );
    f.managed
        .set_project_policy_enabled(&f.conversation, paused.revision, true)
        .unwrap();
    f.managed.tick_schedules(due).await.unwrap();
    assert_eq!(
        f.managed.backlog(Some(&f.conversation), 64).unwrap().len(),
        1
    );
}
#[tokio::test]
async fn cancellation_still_records_after_project_authority_is_paused() {
    let f = fixture().await;
    grant(&f, 2);
    let parent = state(
        &f,
        &enqueue(&f, "Initial work", false).await,
        TaskState::Running,
    )
    .await;
    let child = propose(&f, &parent, "child", "Follow up").await;
    state(&f, &parent, TaskState::Completed).await;
    f.managed.tick_projects(now_ms()).await.unwrap();
    let admitted = f.managed.task(&child.id).unwrap().unwrap();
    let running = state(&f, &admitted, TaskState::Running).await;
    let policy = f.managed.project_policy(&f.conversation).unwrap().unwrap();
    f.managed
        .set_project_policy_enabled(&f.conversation, policy.revision, false)
        .unwrap();
    let mut cancelled = running.clone();
    cancelled.cancel_requested = true;
    cancelled.revision += 1;
    cancelled.updated_at_ms = now_ms().max(running.updated_at_ms);
    assert!(
        f.managed
            .transition(&running, cancelled, None)
            .await
            .is_ok()
    );
}
#[tokio::test]
async fn exact_settled_outcome_can_reconcile_uncertainty_without_retry() {
    use xcb_core::models::{Mode, ModelChoice};
    let f = fixture().await;
    let account = f
        .store
        .add_account(Provider::Codex, "Fixture", now_ms(), None)
        .unwrap();
    let model = ModelChoice {
        provider: Provider::Codex,
        id: Id::new("fixture").unwrap(),
        label: "Fixture".into(),
        mode: Mode::Fixed,
        resolved: None,
        effort: None,
        observed_at_ms: now_ms(),
    };
    let session = f
        .store
        .create_session(&account.id, model, &f.workspace, now_ms())
        .unwrap();
    let task = enqueue(&f, "Actual work", false).await;
    let running = f
        .managed
        .prepare(
            &task,
            session.id.clone(),
            "fixture".into(),
            "fixture".into(),
            0,
            String::new(),
        )
        .await
        .unwrap();
    let uncertain = state(&f, &running, TaskState::Uncertain).await;
    let input = Message {
        id: new_id("input"),
        role: Role::User,
        text: task.goal.clone(),
        at_ms: now_ms(),
        attachments: vec![],
        provenance: None,
    };
    let current = f
        .store
        .append_message(&session.id, session.revision, &input)
        .unwrap();
    let run = f
        .store
        .prepare_run(&session.id, current.revision, now_ms())
        .unwrap();
    assert!(
        f.managed
            .reconcile_uncertain(&f.store, &uncertain.id, uncertain.revision)
            .await
            .is_err()
    );
    let outcome = Outcome {
        tool_calls: Some(0),
        text_attention: false,
        text: "Done with verified source receipt".into(),
        facts: xcb_core::policy::TurnFacts {
            terminal: Terminal::Completed,
            joined: true,
            effects: EffectState::Settled,
            pending_attention: false,
            failure: None,
        },
        state: State::Idle,
        diagnostic: None,
    };
    let current = f.store.session(&session.id).unwrap().unwrap();
    f.store
        .append_message(
            &session.id,
            current.revision,
            &Message {
                id: new_id("answer"),
                role: Role::Assistant,
                text: outcome.text.clone(),
                at_ms: now_ms(),
                attachments: vec![],
                provenance: None,
            },
        )
        .unwrap();
    f.store
        .settle_outcome(&run, &input.id, &outcome, now_ms())
        .unwrap();
    let reconciled = f
        .managed
        .reconcile_uncertain(&f.store, &uncertain.id, uncertain.revision)
        .await
        .unwrap();
    assert_eq!(reconciled.state, TaskState::Completed);
    assert_eq!(reconciled.attempts, uncertain.attempts);
    assert_eq!(
        reconciled.last_output.as_deref(),
        Some(outcome.text.as_str())
    );
    assert_eq!(
        f.managed.verify_task(&task.id).await.unwrap()["verified"],
        true
    );
}

#[tokio::test]
async fn queued_schedule_stops_at_project_pause_and_expiry_changes_view_stamp() {
    let f = fixture().await;
    let policy = grant(&f, 2);
    let now = now_ms();
    let schedule = f
        .managed
        .create_schedule(&f.conversation, "Scheduled work".into(), 60_000, now)
        .await
        .unwrap();
    f.managed.tick_schedules(now).await.unwrap();
    let schedule = f
        .managed
        .schedules(None)
        .unwrap()
        .into_iter()
        .find(|s| s.id == schedule.id)
        .unwrap();
    let task = f
        .managed
        .task(schedule.last_task.as_ref().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(task.schedule.as_ref(), Some(&schedule.id));
    assert!(f.managed.project_dispatch_block(&task).unwrap().is_none());
    let paused = f
        .managed
        .set_project_policy_enabled(&f.conversation, policy.revision, false)
        .unwrap();
    assert!(f.managed.project_dispatch_block(&task).unwrap().is_some());
    f.managed
        .set_project_policy_enabled(&f.conversation, paused.revision, true)
        .unwrap();
    assert!(f.managed.project_dispatch_block(&task).unwrap().is_none());
    let root = f.managed.root().parent().unwrap();
    let before = f.managed.view_stamp(root, &f.conversation).unwrap();
    let mut db = f.managed.write_db().unwrap();
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    let mut policy = policy_from(&tx, f.workspace.to_str().unwrap())
        .unwrap()
        .unwrap();
    policy.expires_at_ms = now_ms() - 1;
    write_policy(&tx, &policy).unwrap();
    tx.commit().unwrap();
    drop(db);
    let after = f.managed.view_stamp(root, &f.conversation).unwrap();
    assert_ne!(before, after);
    assert_eq!(
        f.managed
            .project_policy(&f.conversation)
            .unwrap()
            .unwrap()
            .status(),
        "expired"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn project_policy_bounds_and_corruption_are_isolated() {
    let f = fixture().await;
    for (tasks, expires) in [
        (0, now_ms() + 7_200_000),
        (101, now_ms() + 7_200_000),
        (1, now_ms() + 10_000),
        (1, now_ms() + 31 * 24 * 60 * 60 * 1000),
    ] {
        assert!(
            f.managed
                .configure_project_policy(
                    &f.conversation,
                    None,
                    "Goal".into(),
                    tasks,
                    expires,
                    None
                )
                .is_err()
        );
    }
    grant(&f, 1);
    // Grants are keyed by directory: the second project needs its own.
    let other = private::directory(&f.workspace.parent().unwrap().join("work2")).unwrap();
    let second = f.managed.create_conversation(&other).await.unwrap();
    f.managed
        .configure_project_policy(
            &second.id,
            None,
            "Other project".into(),
            2,
            now_ms() + 7_200_000,
            None,
        )
        .unwrap();
    f.managed
        .db()
        .unwrap()
        .execute(
            "UPDATE project_policies SET payload='{bad' WHERE workspace=?1",
            [f.workspace.to_str().unwrap()],
        )
        .unwrap();
    let policies = f.managed.project_policies().unwrap();
    assert_eq!(policies.len(), 1);
    assert_eq!(policies[0].workspace, other.to_str().unwrap());
}

#[tokio::test]
async fn scheduled_prompt_freezes_provider_and_asks_once_before_conflicting_dispatch() {
    let f = fixture().await;
    grant(&f, 2);
    let now = now_ms();
    let schedule = f
        .managed
        .create_schedule(
            &f.conversation,
            "Use Claude to inspect the project".into(),
            60_000,
            now,
        )
        .await
        .unwrap();
    f.managed.tick_schedules(now).await.unwrap();
    let saved = f
        .managed
        .schedules(None)
        .unwrap()
        .into_iter()
        .find(|s| s.id == schedule.id)
        .unwrap();
    let question = f
        .managed
        .task(saved.last_task.as_ref().unwrap())
        .unwrap()
        .unwrap();
    assert!(question.routing_question);
    assert_eq!(question.state, TaskState::NeedsInput);
    assert_eq!(question.provider_preference, Some(Provider::Codex));
    assert!(question.provider_required);
    f.managed.tick_schedules(now + 120_000).await.unwrap();
    assert_eq!(
        f.managed.backlog(Some(&f.conversation), 64).unwrap().len(),
        1
    );
    assert!(
        f.managed
            .reply_to_task(&question.id, "Use Claude to continue anyway".into())
            .await
            .is_err()
    );
    let replied = f
        .managed
        .reply_to_task(&question.id, "Inspect the project".into())
        .await
        .unwrap();
    assert!(!replied.routing_question);
    assert!(!replied.deferred);
    assert_eq!(replied.state, TaskState::Queued);
    assert_eq!(
        f.managed.effective_route_preferences(&replied).unwrap(),
        (Some(Provider::Codex), true)
    );
    assert_eq!(replied.goal, question.goal);
}

#[tokio::test]
async fn oversized_policy_is_invalid_not_absent_for_scheduled_dispatch() {
    let f = fixture().await;
    grant(&f, 1);
    let now = now_ms();
    let schedule = f
        .managed
        .create_schedule(&f.conversation, "Review".into(), 60_000, now)
        .await
        .unwrap();
    f.managed.tick_schedules(now).await.unwrap();
    let schedule = f
        .managed
        .schedules(None)
        .unwrap()
        .into_iter()
        .find(|s| s.id == schedule.id)
        .unwrap();
    let task = f
        .managed
        .task(schedule.last_task.as_ref().unwrap())
        .unwrap()
        .unwrap();
    f.managed
        .db()
        .unwrap()
        .execute(
            "UPDATE project_policies SET payload=?1 WHERE workspace=?2",
            params![" ".repeat(65_537), f.workspace.to_str().unwrap()],
        )
        .unwrap();
    assert!(f.managed.project_policy(&f.conversation).is_err());
    assert!(f.managed.project_dispatch_block(&task).is_err());
}

#[tokio::test]
async fn live_legacy_supervisor_blocks_schema_upgrade_without_mutation() {
    let f = fixture().await;
    let state = f.managed.root().parent().unwrap().to_owned();
    f.managed
        .db()
        .unwrap()
        .execute_batch(
            "DROP TABLE project_memory; DROP TABLE project_policies; PRAGMA user_version=2;",
        )
        .unwrap();
    let guard = managed_migration_guard(f.managed.root()).unwrap();
    assert!(matches!(
        ManagedStore::open(&state),
        Err(Error::Conflict(_))
    ));
    let version: u32 = f
        .managed
        .db()
        .unwrap()
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, 2);
    let added: bool = f
        .managed
        .db()
        .unwrap()
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='project_policies')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!added);
    drop(guard);
    let reopened = ManagedStore::open(&state).unwrap();
    let version: u32 = reopened
        .db()
        .unwrap()
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, 7);
    assert!(reopened.conversation(&f.conversation).unwrap().is_some());
    // The legacy-shaped tables the reset recreated are rebuilt keyed on the
    // project directory.
    let workspace = f.workspace.to_str().unwrap();
    assert!(reopened.project_policy_in(workspace).unwrap().is_none());
    assert!(reopened.memory_binding_in(workspace).unwrap().is_none());
    let policy = reopened
        .configure_project_policy_in(
            &f.workspace,
            None,
            "Maintain the project".into(),
            2,
            now_ms() + 7_200_000,
            None,
            0,
            0,
        )
        .unwrap();
    assert_eq!(
        reopened
            .project_policy_in(workspace)
            .unwrap()
            .unwrap()
            .generation,
        policy.generation
    );
    assert_eq!(
        reopened
            .project_policy(&f.conversation)
            .unwrap()
            .unwrap()
            .generation,
        policy.generation
    );
}

#[tokio::test]
async fn active_settle_continuation_defers_project_proposal_until_parent_finishes() {
    use xcb_core::models::{Mode, ModelChoice};
    let f = fixture().await;
    grant(&f, 2);
    let mut config = Config::default();
    config.extensions.reflexes.settle = crate::config::ReflexMode::Active;
    config.save(f.store.root(), None).unwrap();
    let account = f
        .store
        .add_account(Provider::Codex, "Fixture", now_ms(), None)
        .unwrap();
    let model = ModelChoice {
        provider: Provider::Codex,
        id: Id::new("fixture").unwrap(),
        label: "Fixture".into(),
        mode: Mode::Fixed,
        resolved: None,
        effort: None,
        observed_at_ms: now_ms(),
    };
    let session = f
        .store
        .create_session(&account.id, model, &f.workspace, now_ms())
        .unwrap();
    let task = enqueue(&f, "Update parser and its callers", false).await;
    let parent = f
        .managed
        .prepare(
            &task,
            session.id.clone(),
            "fixture".into(),
            "fixture".into(),
            0,
            String::new(),
        )
        .await
        .unwrap();
    let child = propose(&f, &parent, "next", "Review remaining documentation").await;
    let outcome = |text: &str| Outcome {
        tool_calls: Some(60),
        text_attention: false,
        text: text.into(),
        facts: xcb_core::policy::TurnFacts {
            terminal: Terminal::Completed,
            joined: true,
            effects: EffectState::Settled,
            pending_attention: false,
            failure: None,
        },
        state: State::Idle,
        diagnostic: None,
    };
    let continuing = f
        .managed
        .finish(
            &f.store,
            &parent.id,
            Ok(outcome("Schema migrated. Next, I'll update the callers:")),
        )
        .await
        .unwrap();
    assert_eq!(continuing.state, TaskState::Queued);
    assert_eq!(continuing.settle.as_deref(), Some("stopped_short"));
    f.managed.tick_projects(now_ms()).await.unwrap();
    assert!(f.managed.task(&child.id).unwrap().unwrap().deferred);
    assert_eq!(
        f.managed
            .project_policy(&f.conversation)
            .unwrap()
            .unwrap()
            .admitted_tasks,
        0
    );
    let running = f
        .managed
        .prepare(
            &continuing,
            session.id,
            "fixture".into(),
            "fixture".into(),
            0,
            String::new(),
        )
        .await
        .unwrap();
    let done = f
        .managed
        .finish(
            &f.store,
            &running.id,
            Ok(outcome("All callers updated and tests pass.")),
        )
        .await
        .unwrap();
    assert_eq!(done.state, TaskState::Completed);
    f.managed.tick_projects(now_ms()).await.unwrap();
    assert!(!f.managed.task(&child.id).unwrap().unwrap().deferred);
    assert_eq!(
        f.managed
            .project_policy(&f.conversation)
            .unwrap()
            .unwrap()
            .admitted_tasks,
        1
    );
}

fn grant_in(f: &Fixture, workspace: &Path, max: u32) -> ProjectPolicy {
    f.managed
        .configure_project_policy_in(
            workspace,
            None,
            "Maintain this directory".into(),
            max,
            now_ms() + 7_200_000,
            None,
            0,
            0,
        )
        .unwrap()
}
fn text(path: &Path) -> &str {
    path.to_str().unwrap()
}

#[tokio::test]
async fn grant_for_workspace_a_never_admits_backlog_or_children_in_b_from_thread() {
    let f = fixture().await;
    let (a, b) = (f.workspace.clone(), second(&f));
    grant_in(&f, &a, 4);
    // Both parents live in the one thread; only A holds a grant.
    let parent_b = state(
        &f,
        &in_thread(&f, &b, "Work in B").await,
        TaskState::Running,
    )
    .await;
    let child_b = worker_call(
        &f,
        &parent_b,
        "b",
        "xcb_backlog_add",
        &json!({"prompt":"Follow up in B"}),
    )
    .await
    .unwrap();
    let child_b = f
        .managed
        .task(&Id::new(child_b["id"].as_str().unwrap()).unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(child_b.workspace, text(&b));
    assert!(child_b.project_proposal.is_none());
    let thread = f.managed.global_thread().await.unwrap().id;
    let user_b = f
        .managed
        .enqueue_backlog_at(
            &thread,
            Some(&b),
            BindingOrigin::Cli,
            new_id("m"),
            "User idea in B".into(),
            true,
            5,
            None,
        )
        .await
        .unwrap();
    let parent_a = state(
        &f,
        &in_thread(&f, &a, "Work in A").await,
        TaskState::Running,
    )
    .await;
    let child_a = worker_call(
        &f,
        &parent_a,
        "a",
        "xcb_backlog_add",
        &json!({"prompt":"Follow up in A"}),
    )
    .await
    .unwrap();
    let child_a = Id::new(child_a["id"].as_str().unwrap()).unwrap();
    // B's running work neither blocks nor borrows A's grant.
    state(&f, &parent_a, TaskState::Completed).await;
    f.managed.tick_projects(now_ms()).await.unwrap();
    assert!(!f.managed.task(&child_a).unwrap().unwrap().deferred);
    state(&f, &parent_b, TaskState::Completed).await;
    f.managed.tick_projects(now_ms()).await.unwrap();
    assert!(f.managed.task(&child_b.id).unwrap().unwrap().deferred);
    assert!(f.managed.task(&user_b.id).unwrap().unwrap().deferred);
    assert_eq!(
        f.managed
            .project_policy_in(text(&a))
            .unwrap()
            .unwrap()
            .admitted_tasks,
        1
    );
    assert!(f.managed.project_policy_in(text(&b)).unwrap().is_none());
    // An entry-created thread task must name its directory.
    assert!(
        f.managed
            .enqueue_backlog(&thread, new_id("m"), "Unscoped".into(), true, 5)
            .await
            .is_err()
    );
}

#[cfg(unix)]
#[tokio::test]
async fn two_conversations_over_one_workspace_share_grant_backlog_working_memory_and_binding() {
    let f = fixture().await;
    let twin = f
        .managed
        .create_conversation(&f.workspace)
        .await
        .unwrap()
        .id;
    let policy = grant(&f, 2);
    assert_eq!(
        f.managed.project_policy(&twin).unwrap().unwrap().generation,
        policy.generation
    );
    let first = enqueue(&f, "Held in the first view", true).await;
    let second_view = f
        .managed
        .enqueue_backlog(
            &twin,
            new_id("m"),
            "Held in the second view".into(),
            true,
            5,
        )
        .await
        .unwrap();
    let shared: Vec<Id> = f
        .managed
        .backlog_in(text(&f.workspace), 64)
        .unwrap()
        .into_iter()
        .map(|task| task.id)
        .collect();
    assert!(shared.contains(&first.id) && shared.contains(&second_view.id));
    // Work finished in one view is working memory for the other.
    f.managed
        .complete_backlog(&first.id, first.revision, "Checked parser".into())
        .await
        .unwrap();
    assert!(
        f.managed
            .working_memory(&twin, 8)
            .unwrap()
            .iter()
            .any(|row| row.task == first.id)
    );
    // A worker in the second view proposes under the directory's grant, and
    // the grant admits it once the directory's other work is done.
    let parent = state(
        &f,
        &f.managed
            .enqueue_backlog(&twin, new_id("m"), "Work".into(), false, 5)
            .await
            .unwrap(),
        TaskState::Running,
    )
    .await;
    let proposal = propose(&f, &parent, "twin", "Follow-up").await;
    assert_eq!(
        proposal.project_proposal.as_ref().unwrap().generation,
        policy.generation
    );
    state(&f, &parent, TaskState::Completed).await;
    f.managed
        .complete_backlog(&second_view.id, second_view.revision, "Done".into())
        .await
        .unwrap();
    f.managed.tick_projects(now_ms()).await.unwrap();
    assert!(!f.managed.task(&proposal.id).unwrap().unwrap().deferred);
    assert_eq!(
        f.managed
            .project_policy(&f.conversation)
            .unwrap()
            .unwrap()
            .admitted_tasks,
        1
    );
    // One Wordcell binding serves both views.
    let (_tools, config) = wordcell_fixture(&f);
    let binding = f
        .managed
        .bind_memory(&f.conversation, None, config)
        .unwrap();
    assert_eq!(
        f.managed.memory_binding(&twin).unwrap().unwrap().revision,
        binding.revision
    );
}

#[tokio::test]
async fn grant_on_parent_does_not_cover_explicit_subdirectory() {
    let f = fixture().await;
    grant_in(&f, &f.workspace, 4);
    let sub = private::directory(&f.workspace.join("sub")).unwrap();
    let parent = state(
        &f,
        &in_thread(&f, &sub, "Work in sub").await,
        TaskState::Running,
    )
    .await;
    assert_eq!(parent.workspace, text(&sub));
    let child = worker_call(
        &f,
        &parent,
        "sub",
        "xcb_backlog_add",
        &json!({"prompt":"Follow up in sub"}),
    )
    .await
    .unwrap();
    let child = f
        .managed
        .task(&Id::new(child["id"].as_str().unwrap()).unwrap())
        .unwrap()
        .unwrap();
    assert!(child.project_proposal.is_none());
    state(&f, &parent, TaskState::Completed).await;
    f.managed.tick_projects(now_ms()).await.unwrap();
    assert!(f.managed.task(&child.id).unwrap().unwrap().deferred);
    assert!(f.managed.project_policy_in(text(&sub)).unwrap().is_none());
    assert_eq!(
        f.managed
            .project_policy_in(text(&f.workspace))
            .unwrap()
            .unwrap()
            .admitted_tasks,
        0
    );
}

/// A trusted fake Wordcell CLI that answers exact search and note creation.
#[cfg(unix)]
fn wordcell_fixture(f: &Fixture) -> (PathBuf, crate::wordcell::WordcellConfig) {
    use std::os::unix::fs::PermissionsExt;
    let tools = private::directory(&f.workspace.parent().unwrap().join("tools")).unwrap();
    let vault = private::directory(&tools.join("vault")).unwrap();
    let executable = tools.join("wordcell");
    std::fs::write(
        &executable,
        "#!/bin/sh\nif [ \"$1\" = search ]; then printf '{\"results\":[]}'; exit 0; fi\ntest \"$1\" = note && test \"$2\" = create || exit 2\nprintf '{\"changed\":true,\"path\":\"%s.md\",\"revision\":\"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"}' \"$3\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let config = crate::wordcell::WordcellConfig::admit(&executable, &vault).unwrap();
    (tools, config)
}

#[cfg(unix)]
#[tokio::test]
async fn wordcell_bind_search_promote_are_workspace_keyed_and_promotion_digest_keeps_conversation_provenance()
 {
    let f = fixture().await;
    let (a, b) = (f.workspace.clone(), second(&f));
    let (_tools, config) = wordcell_fixture(&f);
    let binding = f.managed.bind_memory_in(&a, None, config).unwrap();
    assert_eq!(binding.workspace, text(&a));
    assert_eq!(
        f.managed
            .search_memory_in(text(&a), "parser", 4)
            .await
            .unwrap(),
        json!({"results":[]})
    );
    // The view over A searches the same binding; B has none.
    assert!(
        f.managed
            .search_memory(&f.conversation, "parser", 4)
            .await
            .is_ok()
    );
    assert!(matches!(
        f.managed.search_memory_in(text(&b), "parser", 4).await,
        Err(Error::Unavailable(
            "project Wordcell memory is not configured"
        ))
    ));
    // A thread task in A promotes into A's binding, and its promotion keeps
    // the thread as conversation provenance in the request digest.
    let task = in_thread(&f, &a, "Decide the parser API").await;
    let note = "Keep the parser API stable.";
    let receipt = f.managed.promote_memory(&task.id, note).await.unwrap();
    assert_eq!(receipt.status, crate::wordcell::PromotionStatus::Completed);
    let promotion = crate::wordcell::Promotion {
        task_id: task.id.to_string(),
        conversation_id: GLOBAL_THREAD_ID.into(),
        summary: note.into(),
    };
    assert_eq!(
        receipt.request_digest,
        digest(
            serde_json::to_vec(&json!({
                "contract":"xcb.wordcell-promotion.v1",
                "config":binding.config,
                "promotion":promotion
            }))
            .unwrap()
        )
    );
    let other = in_thread(&f, &b, "Work in B").await;
    assert!(f.managed.promote_memory(&other.id, note).await.is_err());
}

#[tokio::test]
async fn projects_row_status_paused_by_upgrade() {
    let f = fixture().await;
    let policy = grant(&f, 2);
    let paused = f
        .managed
        .set_project_policy_enabled_in(&policy.workspace, policy.revision, false)
        .unwrap();
    let row = |f: &Fixture| f.managed.project_rows().unwrap().pop().unwrap();
    assert_eq!(row(&f).status, "paused");
    assert_eq!(row(&f).name, "work");
    f.managed
        .db()
        .unwrap()
        .execute(
            "INSERT INTO project_migration_conflicts(id,kind,workspace,conversation,disposition,stranded_tasks,payload,created_at,resolved_at) VALUES('pmc_test','grant',?1,'c_legacy','winner_paused',0,'{}',?2,NULL)",
            params![policy.workspace, sql(now_ms()).unwrap()],
        )
        .unwrap();
    assert_eq!(row(&f).status, "paused by upgrade");
    assert_eq!(
        f.managed.project_status(&paused).unwrap(),
        "paused by upgrade"
    );
    // Resuming deliberately resolves the upgrade conflict.
    f.managed
        .set_project_policy_enabled_in(&policy.workspace, paused.revision, true)
        .unwrap();
    assert_eq!(row(&f).status, "active");
    assert!(f.managed.migration_conflicts(true).unwrap().is_empty());
}

/// Fabricate `other` as a linked worktree of the fixture workspace's
/// repository: a plain `.git` dir on the workspace, a worktree gitdir under
/// it, and the `gitdir:` pointer file in `other`. No git binary needed.
fn repo_family(f: &Fixture, other: &Path) -> String {
    let gitdir = f.workspace.join(".git");
    let link = gitdir.join("worktrees").join("other");
    std::fs::create_dir_all(&link).unwrap();
    std::fs::write(link.join("commondir"), "../..\n").unwrap();
    std::fs::write(other.join(".git"), format!("gitdir: {}\n", link.display())).unwrap();
    crate::managed::workspace::repo_common_dir(&f.workspace).unwrap()
}

#[tokio::test]
async fn herd_dials_scale_under_revision_and_preserve_the_grant() {
    let f = fixture().await;
    let policy = f
        .managed
        .configure_project_policy_dialed(
            &f.conversation,
            None,
            "Maintain the project".into(),
            4,
            now_ms() + 7_200_000,
            Some(Provider::Codex),
            2,
            6,
        )
        .unwrap();
    assert_eq!((policy.max_active, policy.max_per_hour), (2, 6));
    let scaled = f
        .managed
        .update_project_throughput_in(&policy.workspace, policy.revision, 8, 0)
        .unwrap();
    assert_eq!((scaled.max_active, scaled.max_per_hour), (8, 0));
    assert_eq!(scaled.goal, policy.goal);
    assert_eq!(scaled.max_tasks, policy.max_tasks);
    assert_eq!(scaled.generation, policy.generation);
    assert!(matches!(
        f.managed
            .update_project_throughput_in(&policy.workspace, policy.revision, 1, 1),
        Err(Error::Conflict(_))
    ));
    // Bounds are enforced on write, not just at the CLI flag parser.
    assert!(
        f.managed
            .update_project_throughput_in(&policy.workspace, scaled.revision, 65, 0)
            .is_err()
    );
    assert!(
        f.managed
            .update_project_throughput_in(&policy.workspace, scaled.revision, 0, 513)
            .is_err()
    );
}

#[tokio::test]
async fn hourly_dial_holds_schedule_and_proposal_work_until_the_window_clears() {
    let f = fixture().await;
    f.managed
        .configure_project_policy_dialed(
            &f.conversation,
            None,
            "Maintain the project".into(),
            8,
            now_ms() + 7_200_000,
            Some(Provider::Codex),
            0,
            1,
        )
        .unwrap();
    // One automatic admission this window: a proposal the grant releases.
    let parent = state(
        &f,
        &enqueue(&f, "Initial work", false).await,
        TaskState::Running,
    )
    .await;
    let proposal = propose(&f, &parent, "one", "Follow-up").await;
    state(&f, &parent, TaskState::Completed).await;
    f.managed.tick_projects(now_ms()).await.unwrap();
    let admitted = f.managed.task(&proposal.id).unwrap().unwrap();
    assert!(!admitted.deferred);
    assert!(
        admitted
            .project_proposal
            .as_ref()
            .unwrap()
            .admitted_at_ms
            .is_some()
    );
    // Settle it so the outstanding-work gate is not the schedule's blocker.
    state(&f, &admitted, TaskState::Completed).await;
    let due = now_ms();
    let schedule = f
        .managed
        .create_schedule(&f.conversation, "Sweep".into(), 60_000, due)
        .await
        .unwrap();
    f.managed.tick_schedules(due).await.unwrap();
    // The spent hourly window defers the occurrence; nothing was dropped.
    let held = f
        .managed
        .schedules(None)
        .unwrap()
        .into_iter()
        .find(|s| s.id == schedule.id)
        .unwrap();
    assert!(held.last_task.is_none());
    assert_eq!(held.next_due_ms, schedule.next_due_ms);
    // The operator-facing view explains the hold.
    assert_eq!(
        f.managed.schedule_view(&held).unwrap().blocker.as_deref(),
        Some("project hourly start limit reached")
    );
    let later = due + super::ADMISSION_WINDOW_MS + 1;
    f.managed.tick_schedules(later).await.unwrap();
    let fired = f
        .managed
        .schedules(None)
        .unwrap()
        .into_iter()
        .find(|s| s.id == schedule.id)
        .unwrap();
    assert!(fired.last_task.is_some());
    assert!(fired.next_due_ms > schedule.next_due_ms);
}

#[tokio::test]
async fn herd_family_covers_linked_worktrees_and_operator_work_never_defers() {
    let f = fixture().await;
    let other = second(&f);
    let common = repo_family(&f, &other);
    let policy = f
        .managed
        .configure_project_policy_dialed(
            &f.conversation,
            None,
            "Maintain the project".into(),
            8,
            now_ms() + 7_200_000,
            Some(Provider::Codex),
            1,
            0,
        )
        .unwrap();
    assert_eq!(policy.repo.as_deref(), Some(common.as_str()));
    // The herd covers the linked worktree; an unrelated directory is outside.
    assert!(herd_covers(&policy, &f.workspace.display().to_string()));
    assert!(herd_covers(&policy, &other.display().to_string()));
    let strangers = private::directory(&f.workspace.parent().unwrap().join("strangers")).unwrap();
    assert!(!herd_covers(&policy, &strangers.display().to_string()));
    assert!(
        f.managed
            .herd_policy_in(&other.display().to_string())
            .unwrap()
            .is_some()
    );
    // An operator-started task in the linked worktree holds a family lane.
    let holder = state(
        &f,
        &in_thread(&f, &other, "Operator work").await,
        TaskState::Running,
    )
    .await;
    assert_eq!(f.managed.herd_lanes_in(&policy).unwrap().len(), 1);
    // It counts even though it is not automatic.
    assert!(!herd_automatic(&holder));
    // An automatic task in the herd's own checkout defers on the cap.
    let due = now_ms();
    let schedule = f
        .managed
        .create_schedule(&f.conversation, "Sweep".into(), 60_000, due)
        .await
        .unwrap();
    f.managed.tick_schedules(due).await.unwrap();
    let task = f
        .managed
        .task(
            f.managed
                .schedules(None)
                .unwrap()
                .into_iter()
                .find(|s| s.id == schedule.id)
                .unwrap()
                .last_task
                .as_ref()
                .unwrap(),
        )
        .unwrap()
        .unwrap();
    assert!(herd_automatic(&task));
    let operator = in_thread(&f, &f.workspace, "Operator lane").await;
    let managed = Arc::new(f.managed);
    let store = Arc::new(f.store);
    let supervisor = Supervisor::new(managed.clone(), store.clone());
    let reason = supervisor.herd_capacity_block(&task).unwrap().unwrap();
    assert!(reason.contains("1 of 1"), "{reason}");
    // Operator work is never held back by the herd's dial.
    assert!(supervisor.herd_capacity_block(&operator).unwrap().is_none());
    // Raising the dial releases the automatic lane.
    let scaled = managed
        .update_project_throughput_in(&policy.workspace, policy.revision, 2, 0)
        .unwrap();
    assert_eq!(scaled.max_active, 2);
    assert!(supervisor.herd_capacity_block(&task).unwrap().is_none());
    // Uncertain work keeps custody but does not hold a live lane.
    let mut uncertain = holder.clone();
    uncertain.state = TaskState::Uncertain;
    uncertain.revision += 1;
    uncertain.updated_at_ms = now_ms().max(holder.updated_at_ms);
    managed.transition(&holder, uncertain, None).await.unwrap();
    assert_eq!(managed.herd_lanes_in(&scaled).unwrap().len(), 0);
}

#[tokio::test]
async fn legacy_policy_and_proposal_payloads_decode_with_empty_dials() {
    let f = fixture().await;
    grant(&f, 2);
    let db = f.managed.db().unwrap();
    // A pre-dials policy payload must still decode with the new defaults.
    let row: String = db
        .query_row(
            "SELECT payload FROM project_policies WHERE workspace=?1",
            [f.workspace.display().to_string()],
            |row| row.get(0),
        )
        .unwrap();
    let mut value: Value = serde_json::from_str(&row).unwrap();
    let object = value.as_object_mut().unwrap();
    object.remove("repo");
    object.remove("max_active");
    object.remove("max_per_hour");
    let legacy: ProjectPolicy = serde_json::from_value(value).unwrap();
    assert_eq!(
        (legacy.max_active, legacy.max_per_hour, legacy.repo),
        (0, 0, None)
    );
    // Same for a proposal recorded before admission timestamps existed.
    let proposal: ProjectProposal = serde_json::from_value(json!({
        "parent": "m_one",
        "generation": "g_one",
        "required_provider": null,
        "admitted": false
    }))
    .unwrap();
    assert_eq!(proposal.admitted_at_ms, None);
}
