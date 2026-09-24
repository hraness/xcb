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
    let base = root.path().canonicalize().unwrap();
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
    let other = f.managed.create_conversation(&f.workspace).await.unwrap();
    let other_task = f
        .managed
        .enqueue_backlog(&other.id, new_id("m"), "other project".into(), true, 5)
        .await
        .unwrap();
    let parent = state(&f, &enqueue(&f, "worker", false).await, TaskState::Running).await;
    let (result,_)=f.managed.habitat_worker_call(&parent,parent.session.as_ref().unwrap(),"cross","xcb_backlog_complete",&json!({"taskId":other_task.id,"expectedRevision":other_task.revision,"summary":"claim"})).await;
    assert!(result.is_err());
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
    assert_eq!(version, 5);
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
    let mut policy = policy_from(&tx, &f.conversation).unwrap().unwrap();
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
    let second = f.managed.create_conversation(&f.workspace).await.unwrap();
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
            "UPDATE project_policies SET payload='{bad' WHERE conversation=?1",
            [f.conversation.as_str()],
        )
        .unwrap();
    let policies = f.managed.project_policies().unwrap();
    assert_eq!(policies.len(), 1);
    assert_eq!(policies[0].conversation, second.id);
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
            "UPDATE project_policies SET payload=?1 WHERE conversation=?2",
            params![" ".repeat(65_537), f.conversation.as_str()],
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
    assert_eq!(version, 5);
    assert!(reopened.conversation(&f.conversation).unwrap().is_some());
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
