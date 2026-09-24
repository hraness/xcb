use super::*;

struct Fixture {
    _root: tempfile::TempDir,
    state: PathBuf,
    workspace: PathBuf,
    managed: Arc<ManagedStore>,
    store: Arc<Store>,
    conversation: Id,
}
async fn fixture() -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let base = root.path().canonicalize().unwrap();
    let workspace = private::directory(&base.join("work")).unwrap();
    let state = base.join("state");
    let managed = Arc::new(ManagedStore::open(&state).unwrap());
    let store = Arc::new(Store::open(&state).unwrap());
    let conversation = managed.create_conversation(&workspace).await.unwrap().id;
    managed
        .configure_project_policy(
            &conversation,
            None,
            "Review this project".into(),
            8,
            now_ms() + 7_200_000,
            Some(Provider::Codex),
        )
        .unwrap();
    Fixture {
        _root: root,
        state,
        workspace,
        managed,
        store,
        conversation,
    }
}
fn program(calls: u8) -> AdmittedProgram {
    let cells: Vec<Value> = (0..calls).map(|i| if i == 0 {
        json!({"id":format!("worker{i}"),"kind":"agent","prompt":"Review project state","output":{"kind":"text"}})
    } else {
        json!({"id":format!("worker{i}"),"kind":"agent","prompt":"Check prior report","inputs":{"report":"text"},"output":{"kind":"text"}})
    }).collect();
    let edges: Vec<Value> = (1..calls).map(|i| json!({"from":{"cell":format!("worker{}",i-1),"port":"out"},"to":{"cell":format!("worker{i}"),"port":"report"}})).collect();
    AdmittedProgram::admit_managed(json!({"contract":"algal.organism.v1","key":"organism:managed-test","name":"Managed test","cells":cells,"edges":edges,"interface":{"inputs":{},"outputs":{"summary":{"cell":format!("worker{}",calls-1),"port":"out"}}}}),json!({}),calls).unwrap()
}
async fn enqueue(f: &Fixture, calls: u8) -> ManagedTask {
    f.managed
        .enqueue_program(
            &f.conversation,
            new_id("m"),
            "Controller".into(),
            program(calls),
        )
        .await
        .unwrap()
}
async fn run_slice(f: &Fixture, task: &ManagedTask) -> (ManagedTask, ProgramSlice) {
    let input = f.managed.program_slice_input(task).unwrap();
    let mut next = task.clone();
    next.state = TaskState::Running;
    next.revision += 1;
    next.updated_at_ms = now_ms().max(task.updated_at_ms);
    let running = f.managed.transition(task, next, None).await.unwrap();
    let (_sender, cancel) = watch::channel(false);
    let slice = running
        .program
        .as_ref()
        .unwrap()
        .step(input.0, input.1, cancel)
        .await
        .unwrap();
    (running, slice)
}
async fn waiting(f: &Fixture, calls: u8) -> (ManagedTask, ManagedTask) {
    let parent = enqueue(f, calls).await;
    let (running, slice) = run_slice(f, &parent).await;
    let parent = f
        .managed
        .finish_program_slice(&running.id, running.revision, &Ok(slice))
        .await
        .unwrap();
    let status = f.managed.program_status(&parent.id).unwrap().unwrap();
    let child = f
        .managed
        .task(status.child.as_ref().unwrap())
        .unwrap()
        .unwrap();
    (parent, child)
}
async fn settle_child(f: &Fixture, child: &ManagedTask, text: &str) -> ManagedTask {
    use xcb_core::models::{Mode, ModelChoice};
    let account = f
        .store
        .add_account(Provider::Codex, "Fixture", now_ms(), None)
        .unwrap();
    let session = f
        .store
        .create_session(
            &account.id,
            ModelChoice {
                provider: Provider::Codex,
                id: Id::new("fixture").unwrap(),
                label: "Fixture".into(),
                mode: Mode::Fixed,
                resolved: None,
                effort: None,
                observed_at_ms: now_ms(),
            },
            &f.workspace,
            now_ms(),
        )
        .unwrap();
    let running = f
        .managed
        .prepare(
            child,
            session.id.clone(),
            "fixture".into(),
            "fixture".into(),
            0,
            String::new(),
        )
        .await
        .unwrap();
    let input = Message {
        id: new_id("input"),
        role: Role::User,
        text: child.goal.clone(),
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
    let outcome = Outcome {
        tool_calls: Some(0),
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
    let current = f.store.session(&session.id).unwrap().unwrap();
    f.store
        .append_message(
            &session.id,
            current.revision,
            &Message {
                id: new_id("answer"),
                role: Role::Assistant,
                text: text.into(),
                at_ms: now_ms(),
                attachments: vec![],
                provenance: None,
            },
        )
        .unwrap();
    f.store
        .settle_outcome(&run, &input.id, &outcome, now_ms())
        .unwrap();
    f.managed
        .finish(&f.store, &running.id, Ok(outcome))
        .await
        .unwrap()
}

#[tokio::test]
async fn publication_is_atomic_replayed_once_and_budgeted() {
    let f = fixture().await;
    let task = enqueue(&f, 2).await;
    let (running, slice) = run_slice(&f, &task).await;
    f.managed.db().unwrap().execute_batch("CREATE TRIGGER reject_program_call BEFORE INSERT ON program_calls BEGIN SELECT RAISE(ABORT,'injected publication failure'); END;").unwrap();
    assert!(
        f.managed
            .finish_program_slice(&running.id, running.revision, &Ok(slice.clone()))
            .await
            .is_err()
    );
    assert_eq!(
        f.managed.backlog(Some(&f.conversation), 64).unwrap().len(),
        1
    );
    assert!(
        read_execution(&f.managed.db().unwrap(), &task.id)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        f.managed
            .project_policy(&f.conversation)
            .unwrap()
            .unwrap()
            .admitted_tasks,
        0
    );
    assert_eq!(
        f.managed.task(&task.id).unwrap().unwrap().revision,
        running.revision
    );
    f.managed
        .db()
        .unwrap()
        .execute_batch("DROP TRIGGER reject_program_call;")
        .unwrap();
    let parent = f
        .managed
        .finish_program_slice(&running.id, running.revision, &Ok(slice.clone()))
        .await
        .unwrap();
    assert!(parent.program_waiting);
    f.managed
        .finish_program_slice(&running.id, running.revision, &Ok(slice))
        .await
        .unwrap();
    assert_eq!(
        f.managed.backlog(Some(&f.conversation), 64).unwrap().len(),
        2
    );
    assert_eq!(
        f.managed
            .project_policy(&f.conversation)
            .unwrap()
            .unwrap()
            .admitted_tasks,
        1
    );
    assert_eq!(
        f.managed.verify_task(&task.id).await.unwrap()["verified"],
        true
    );
}

#[tokio::test]
async fn restart_preserves_wait_and_consumes_exact_settled_result_once() {
    let f = fixture().await;
    let (parent, child) = waiting(&f, 1).await;
    let reopened = ManagedStore::open(&f.state).unwrap();
    reopened.reconcile_startup(&f.store).await.unwrap();
    assert!(reopened.task(&parent.id).unwrap().unwrap().program_waiting);
    assert_eq!(
        reopened.backlog(Some(&f.conversation), 64).unwrap().len(),
        2
    );
    settle_child(&f, &child, "Complete report, with exact provenance").await;
    reopened.tick_programs(&f.store, true).await.unwrap();
    let ready = reopened.task(&parent.id).unwrap().unwrap();
    assert!(!ready.program_waiting);
    let snapshot = reopened.program_slice_input(&ready).unwrap();
    assert_eq!(
        snapshot.1.as_ref().unwrap().summary,
        "Complete report, with exact provenance"
    );
    reopened.tick_programs(&f.store, true).await.unwrap();
    assert_eq!(
        reopened.task(&parent.id).unwrap().unwrap().revision,
        ready.revision
    );
    let (running, slice) = run_slice(&f, &ready).await;
    let complete = f
        .managed
        .finish_program_slice(&parent.id, running.revision, &Ok(slice))
        .await
        .unwrap();
    assert_eq!(complete.state, TaskState::Completed);
    assert_eq!(
        complete.last_output.as_deref(),
        Some("Complete report, with exact provenance")
    );
    assert_eq!(
        f.managed
            .project_policy(&f.conversation)
            .unwrap()
            .unwrap()
            .admitted_tasks,
        1
    );
    assert_eq!(
        f.managed.verify_task(&parent.id).await.unwrap()["verified"],
        true
    );
}

#[tokio::test]
async fn a_crash_during_a_pure_slice_replays_without_new_child_authority() {
    let f = fixture().await;
    let task = enqueue(&f, 1).await;
    let (running, _slice) = run_slice(&f, &task).await;
    let reopened = ManagedStore::open(&f.state).unwrap();
    reopened.reconcile_startup(&f.store).await.unwrap();
    let queued = reopened.task(&task.id).unwrap().unwrap();
    assert_eq!(queued.state, TaskState::Queued);
    assert!(queued.revision > running.revision);
    assert!(reopened.program_slice_input(&queued).unwrap().0.is_none());
    assert_eq!(
        reopened
            .project_policy(&f.conversation)
            .unwrap()
            .unwrap()
            .admitted_tasks,
        0
    );
    let (running, slice) = run_slice(&f, &queued).await;
    f.managed
        .finish_program_slice(&task.id, running.revision, &Ok(slice))
        .await
        .unwrap();
    assert_eq!(
        f.managed
            .project_policy(&f.conversation)
            .unwrap()
            .unwrap()
            .admitted_tasks,
        1
    );
}

#[tokio::test]
async fn parent_wait_releases_actual_supervisor_workspace_and_worker_slot() {
    let f = fixture().await;
    let task = enqueue(&f, 1).await;
    let mut supervisor = Supervisor::new(f.managed.clone(), f.store.clone());
    assert!(matches!(
        supervisor.launch(&task).await.unwrap(),
        Dispatch::Started
    ));
    for _ in 0..100 {
        tokio::time::sleep(Duration::from_millis(10)).await;
        supervisor.tick(false).await.unwrap();
        if f.managed.task(&task.id).unwrap().unwrap().program_waiting {
            break;
        }
    }
    assert!(f.managed.task(&task.id).unwrap().unwrap().program_waiting);
    assert!(supervisor.active.is_empty());
    assert!(supervisor.active_workspaces.is_empty());
    assert!(supervisor.active_accounts.is_empty());
    assert!(supervisor.joins.is_empty());
    assert!(f.managed.has_habitat_work().unwrap());
    let pure=AdmittedProgram::admit(json!({"contract":"algal.organism.v1","key":"organism:independent","cells":[{"id":"r","kind":"const","outputs":{"value":{"type":"text","value":"Independent pure result"}}}],"edges":[],"interface":{"inputs":{},"outputs":{"summary":{"cell":"r","port":"value"}}}}),json!({})).unwrap();
    let other = f
        .managed
        .enqueue_program(&f.conversation, new_id("m"), "Other pure work".into(), pure)
        .await
        .unwrap();
    assert!(matches!(
        supervisor.launch(&other).await.unwrap(),
        Dispatch::Started
    ));
    supervisor.shutdown().await;
}

#[tokio::test]
async fn child_attention_uncertainty_and_missing_receipts_never_resume() {
    let f = fixture().await;
    let (parent, child) = waiting(&f, 1).await;
    let mut next = child.clone();
    next.state = TaskState::NeedsInput;
    next.attention = Some(State::NeedsApproval);
    next.revision += 1;
    next.updated_at_ms = now_ms().max(child.updated_at_ms);
    let question = f.managed.transition(&child, next, None).await.unwrap();
    f.managed.tick_programs(&f.store, true).await.unwrap();
    assert!(f.managed.task(&parent.id).unwrap().unwrap().program_waiting);
    assert!(
        f.managed
            .attention(32)
            .unwrap()
            .iter()
            .any(|t| t.id == child.id && t.attention == Some(State::NeedsApproval))
    );
    let mut next = question.clone();
    next.state = TaskState::Uncertain;
    next.attention = None;
    next.revision += 1;
    next.updated_at_ms = now_ms().max(question.updated_at_ms);
    let uncertain = f.managed.transition(&question, next, None).await.unwrap();
    f.managed.tick_programs(&f.store, true).await.unwrap();
    assert!(f.managed.task(&parent.id).unwrap().unwrap().program_waiting);
    assert_eq!(
        f.managed.task(&child.id).unwrap().unwrap().revision,
        uncertain.revision
    );
    f.managed
        .db()
        .unwrap()
        .execute(
            "DELETE FROM program_calls WHERE parent=?1",
            [parent.id.as_str()],
        )
        .unwrap();
    f.managed.tick_programs(&f.store, true).await.unwrap();
    let held = f.managed.task(&parent.id).unwrap().unwrap();
    assert!(held.program_waiting);
    assert_eq!(held.habitat_ui_state(), State::NeedsAction);
    assert!(
        f.managed
            .attention(32)
            .unwrap()
            .iter()
            .any(|t| t.id == parent.id)
    );
}

#[tokio::test]
async fn cancellation_propagates_and_cannot_claim_uncertain_child_settlement() {
    let f = fixture().await;
    let (parent, child) = waiting(&f, 1).await;
    let mut next = parent.clone();
    next.cancel_requested = true;
    next.revision += 1;
    next.updated_at_ms = now_ms().max(parent.updated_at_ms);
    f.managed.transition(&parent, next, None).await.unwrap();
    f.managed.tick_programs(&f.store, true).await.unwrap();
    let cancelled = f.managed.task(&child.id).unwrap().unwrap();
    assert!(cancelled.cancel_requested);
    assert!(f.managed.task(&parent.id).unwrap().unwrap().program_waiting);
    f.managed.settle_unstarted_cancel(&cancelled).await.unwrap();
    f.managed.tick_programs(&f.store, true).await.unwrap();
    assert_eq!(
        f.managed.task(&parent.id).unwrap().unwrap().state,
        TaskState::Cancelled
    );
    assert_eq!(
        f.managed
            .project_policy(&f.conversation)
            .unwrap()
            .unwrap()
            .admitted_tasks,
        1
    );
    let f = fixture().await;
    let (parent, child) = waiting(&f, 1).await;
    let mut uncertain = child.clone();
    uncertain.state = TaskState::Uncertain;
    uncertain.revision += 1;
    uncertain.updated_at_ms = now_ms().max(child.updated_at_ms);
    f.managed.transition(&child, uncertain, None).await.unwrap();
    let mut cancelled = parent.clone();
    cancelled.cancel_requested = true;
    cancelled.revision += 1;
    cancelled.updated_at_ms = now_ms().max(parent.updated_at_ms);
    f.managed
        .transition(&parent, cancelled, None)
        .await
        .unwrap();
    f.managed.tick_programs(&f.store, true).await.unwrap();
    assert!(f.managed.task(&parent.id).unwrap().unwrap().program_waiting);
}

#[tokio::test]
async fn stale_slice_after_cancel_does_not_publish_and_grant_replacement_holds_children() {
    let f = fixture().await;
    let parent = enqueue(&f, 1).await;
    let (running, slice) = run_slice(&f, &parent).await;
    let mut cancel = running.clone();
    cancel.cancel_requested = true;
    cancel.revision += 1;
    cancel.updated_at_ms = now_ms().max(running.updated_at_ms);
    f.managed.transition(&running, cancel, None).await.unwrap();
    let finished = f
        .managed
        .finish_program_slice(&parent.id, running.revision, &Ok(slice))
        .await
        .unwrap();
    assert_eq!(finished.state, TaskState::Cancelled);
    assert_eq!(
        f.managed.backlog(Some(&f.conversation), 64).unwrap().len(),
        1
    );
    let f = fixture().await;
    let (parent, child) = waiting(&f, 1).await;
    let grant = f.managed.project_policy(&f.conversation).unwrap().unwrap();
    f.managed
        .configure_project_policy(
            &f.conversation,
            Some(grant.revision),
            "Replacement".into(),
            8,
            now_ms() + 7_200_000,
            Some(Provider::Codex),
        )
        .unwrap();
    assert!(f.managed.project_dispatch_block(&child).unwrap().is_some());
    f.managed.tick_programs(&f.store, true).await.unwrap();
    let held = f.managed.task(&parent.id).unwrap().unwrap();
    assert_eq!(held.habitat_ui_state(), State::NeedsAction);
    assert!(held.program_waiting);
}

#[tokio::test]
async fn failed_child_stops_controller_and_does_not_feed_previous_output() {
    let f = fixture().await;
    let (parent, child) = waiting(&f, 1).await;
    let mut failed = child.clone();
    failed.state = TaskState::Failed;
    failed.last_output = Some("Earlier partial output must not become success".into());
    failed.revision += 1;
    failed.updated_at_ms = now_ms().max(child.updated_at_ms);
    f.managed.transition(&child, failed, None).await.unwrap();
    f.managed.tick_programs(&f.store, true).await.unwrap();
    let stopped = f.managed.task(&parent.id).unwrap().unwrap();
    assert_eq!(stopped.state, TaskState::Failed);
    let execution = read_execution(&f.managed.db().unwrap(), &parent.id)
        .unwrap()
        .unwrap();
    assert!(execution.response.is_none());
}

#[tokio::test]
async fn retention_pins_live_children_and_registration_requires_current_grant() {
    let f = fixture().await;
    let (parent, child) = waiting(&f, 1).await;
    let completed = settle_child(&f, &child, "Retained completion").await;
    f.managed
        .db()
        .unwrap()
        .execute(
            "UPDATE tasks SET updated_at=0 WHERE id=?1",
            [child.id.as_str()],
        )
        .unwrap();
    f.managed.retain().unwrap();
    assert!(f.managed.task(&child.id).unwrap().is_some());
    assert_eq!(
        f.managed.task(&child.id).unwrap().unwrap().last_receipt,
        completed.last_receipt
    );
    assert!(f.managed.task(&parent.id).unwrap().unwrap().program_waiting);
    let grant = f.managed.project_policy(&f.conversation).unwrap().unwrap();
    f.managed
        .set_project_policy_enabled(&f.conversation, grant.revision, false)
        .unwrap();
    assert!(
        f.managed
            .enqueue_program(&f.conversation, new_id("m"), "Paused".into(), program(1))
            .await
            .is_err()
    );
    assert!(
        f.managed
            .create_program_schedule(
                &f.conversation,
                "Paused".into(),
                program(1),
                60_000,
                now_ms()
            )
            .await
            .is_err()
    );
}

#[tokio::test]
async fn admission_rejects_overlapping_controllers_but_preserves_deferred_work() {
    let f = fixture().await;
    f.managed
        .enqueue_backlog(
            &f.conversation,
            new_id("m"),
            "Deferred work".into(),
            true,
            5,
        )
        .await
        .unwrap();
    let parent = enqueue(&f, 1).await;
    assert!(
        f.managed
            .enqueue_program(
                &f.conversation,
                new_id("m"),
                "Concurrent controller".into(),
                program(1)
            )
            .await
            .is_err()
    );
    assert_eq!(
        f.managed.backlog(Some(&f.conversation), 64).unwrap().len(),
        2
    );
    let (running, slice) = run_slice(&f, &parent).await;
    f.managed
        .finish_program_slice(&parent.id, running.revision, &Ok(slice))
        .await
        .unwrap();
    assert_eq!(
        f.managed
            .project_policy(&f.conversation)
            .unwrap()
            .unwrap()
            .admitted_tasks,
        1
    );
}

#[tokio::test]
async fn complete_oversized_child_reports_stop_without_truncation_and_do_not_block_cancel() {
    for cancel in [false, true] {
        let f = fixture().await;
        let (parent, child) = waiting(&f, 1).await;
        let report = "a".repeat(MAX_SUMMARY_BYTES + 1);
        settle_child(&f, &child, &report).await;
        if cancel {
            let mut next = parent.clone();
            next.cancel_requested = true;
            next.revision += 1;
            next.updated_at_ms = now_ms().max(parent.updated_at_ms);
            f.managed.transition(&parent, next, None).await.unwrap();
        }
        f.managed.tick_programs(&f.store, true).await.unwrap();
        let terminal = f.managed.task(&parent.id).unwrap().unwrap();
        assert_eq!(
            terminal.state,
            if cancel {
                TaskState::Cancelled
            } else {
                TaskState::Failed
            }
        );
        if !cancel {
            assert!(terminal.detail.contains("report exceeds"));
        }
        let recorded = f.managed.task(&child.id).unwrap().unwrap();
        assert!(recorded.last_output.as_ref().unwrap().len() <= MAX_SUMMARY_BYTES);
        assert_eq!(
            f.store
                .settled_outcome(
                    recorded.session.as_ref().unwrap(),
                    recorded.message_count_before
                )
                .unwrap()
                .unwrap()
                .text,
            report
        );
        assert!(
            read_execution(&f.managed.db().unwrap(), &parent.id)
                .unwrap()
                .unwrap()
                .response
                .is_none()
        );
    }
}

#[tokio::test]
async fn pause_before_publication_is_rechecked_and_resume_rollback_preserves_exact_call() {
    let f = fixture().await;
    let parent = enqueue(&f, 1).await;
    let (running, slice) = run_slice(&f, &parent).await;
    let policy = f.managed.project_policy(&f.conversation).unwrap().unwrap();
    let paused = f
        .managed
        .set_project_policy_enabled(&f.conversation, policy.revision, false)
        .unwrap();
    let held = f
        .managed
        .finish_program_slice(&running.id, running.revision, &Ok(slice))
        .await
        .unwrap();
    assert_eq!(held.state, TaskState::Queued);
    assert!(held.detail.starts_with("project authority"));
    assert_eq!(
        f.managed.backlog(Some(&f.conversation), 64).unwrap().len(),
        1
    );
    assert_eq!(paused.admitted_tasks, 0);
    f.managed
        .set_project_policy_enabled(&f.conversation, paused.revision, true)
        .unwrap();
    let (running, slice) = run_slice(&f, &held).await;
    let waiting = f
        .managed
        .finish_program_slice(&running.id, running.revision, &Ok(slice))
        .await
        .unwrap();
    let child = f
        .managed
        .program_status(&parent.id)
        .unwrap()
        .unwrap()
        .child
        .unwrap();
    settle_child(
        &f,
        &f.managed.task(&child).unwrap().unwrap(),
        "Exact whole report",
    )
    .await;
    f.managed.db().unwrap().execute_batch("CREATE TRIGGER reject_resume BEFORE UPDATE ON program_executions BEGIN SELECT RAISE(ABORT,'injected resume failure'); END;").unwrap();
    assert!(
        f.managed
            .tick_program(&f.store, &waiting, true)
            .await
            .is_err()
    );
    assert!(f.managed.task(&parent.id).unwrap().unwrap().program_waiting);
    assert!(
        read_call(&f.managed.db().unwrap(), &parent.id, 1)
            .unwrap()
            .result
            .is_none()
    );
    f.managed
        .db()
        .unwrap()
        .execute_batch("DROP TRIGGER reject_resume;")
        .unwrap();
    f.managed.tick_programs(&f.store, true).await.unwrap();
    assert!(!f.managed.task(&parent.id).unwrap().unwrap().program_waiting);
    assert_eq!(
        f.managed
            .project_policy(&f.conversation)
            .unwrap()
            .unwrap()
            .admitted_tasks,
        1
    );
    assert_eq!(
        f.managed.backlog(Some(&f.conversation), 64).unwrap().len(),
        2
    );
}

#[tokio::test]
async fn stable_submission_replays_after_budget_or_grant_changes_but_not_changed_inputs() {
    let f = fixture().await;
    let submission = new_id("m");
    let admitted = program(1);
    let parent = f
        .managed
        .enqueue_program(
            &f.conversation,
            submission.clone(),
            "Controller".into(),
            admitted.clone(),
        )
        .await
        .unwrap();
    let (running, slice) = run_slice(&f, &parent).await;
    f.managed
        .finish_program_slice(&parent.id, running.revision, &Ok(slice))
        .await
        .unwrap();
    let policy = f.managed.project_policy(&f.conversation).unwrap().unwrap();
    f.managed
        .set_project_policy_enabled(&f.conversation, policy.revision, false)
        .unwrap();
    let replay = f
        .managed
        .enqueue_program(
            &f.conversation,
            submission.clone(),
            "Controller".into(),
            admitted.clone(),
        )
        .await
        .unwrap();
    assert_eq!(replay.id, parent.id);
    assert!(replay.program_waiting);
    assert!(
        f.managed
            .enqueue_program(&f.conversation, submission, "Changed goal".into(), admitted)
            .await
            .is_err()
    );
    assert_eq!(
        f.managed
            .project_policy(&f.conversation)
            .unwrap()
            .unwrap()
            .admitted_tasks,
        1
    );
    assert_eq!(
        f.managed.backlog(Some(&f.conversation), 64).unwrap().len(),
        2
    );
}

#[tokio::test]
async fn escaped_child_report_uses_encoding_bound_without_clipping_or_stalling() {
    let f = fixture().await;
    let (parent, child) = waiting(&f, 1).await;
    let report = "\"".repeat(MAX_SUMMARY_BYTES);
    settle_child(&f, &child, &report).await;
    f.managed.tick_programs(&f.store, true).await.unwrap();
    let stopped = f.managed.task(&parent.id).unwrap().unwrap();
    assert_eq!(stopped.state, TaskState::Failed);
    assert!(stopped.detail.contains("report exceeds"));
    assert_eq!(
        f.managed
            .task(&child.id)
            .unwrap()
            .unwrap()
            .last_output
            .as_deref(),
        Some(report.as_str())
    );
    assert!(
        read_execution(&f.managed.db().unwrap(), &parent.id)
            .unwrap()
            .unwrap()
            .response
            .is_none()
    );
}

#[tokio::test]
async fn sequential_children_keep_exact_history_and_consume_one_grant_unit_each() {
    let f = fixture().await;
    let (parent, first) = waiting(&f, 2).await;
    settle_child(&f, &first, "First checked report").await;
    f.managed.tick_programs(&f.store, true).await.unwrap();
    let ready = f.managed.task(&parent.id).unwrap().unwrap();
    let (running, slice) = run_slice(&f, &ready).await;
    f.managed
        .finish_program_slice(&parent.id, running.revision, &Ok(slice.clone()))
        .await
        .unwrap();
    f.managed
        .finish_program_slice(&parent.id, running.revision, &Ok(slice))
        .await
        .unwrap();
    let status = f.managed.program_status(&parent.id).unwrap().unwrap();
    assert_eq!(status.calls, 2);
    assert_ne!(status.child.as_ref(), Some(&first.id));
    let second = f
        .managed
        .task(status.child.as_ref().unwrap())
        .unwrap()
        .unwrap();
    assert!(second.goal.contains("First checked report"));
    settle_child(&f, &second, "Second checked report").await;
    f.managed.tick_programs(&f.store, true).await.unwrap();
    let ready = f.managed.task(&parent.id).unwrap().unwrap();
    let (running, slice) = run_slice(&f, &ready).await;
    let complete = f
        .managed
        .finish_program_slice(&parent.id, running.revision, &Ok(slice))
        .await
        .unwrap();
    assert_eq!(complete.state, TaskState::Completed);
    assert_eq!(
        complete.last_output.as_deref(),
        Some("Second checked report")
    );
    assert_eq!(
        f.managed
            .project_policy(&f.conversation)
            .unwrap()
            .unwrap()
            .admitted_tasks,
        2
    );
    assert_eq!(
        read_call(&f.managed.db().unwrap(), &parent.id, 1)
            .unwrap()
            .result
            .unwrap()
            .summary,
        "First checked report"
    );
    assert_eq!(
        f.managed.backlog(Some(&f.conversation), 64).unwrap().len(),
        3
    );
}

#[tokio::test]
async fn consumed_grant_budget_holds_next_call_without_spinning_or_reusing_authority() {
    let f = fixture().await;
    let grant = f.managed.project_policy(&f.conversation).unwrap().unwrap();
    f.managed
        .configure_project_policy(
            &f.conversation,
            Some(grant.revision),
            "One worker only".into(),
            1,
            now_ms() + 7_200_000,
            Some(Provider::Codex),
        )
        .unwrap();
    let (parent, child) = waiting(&f, 2).await;
    settle_child(&f, &child, "One completed worker").await;
    f.managed.tick_programs(&f.store, true).await.unwrap();
    let ready = f.managed.task(&parent.id).unwrap().unwrap();
    let (running, slice) = run_slice(&f, &ready).await;
    let held = f
        .managed
        .finish_program_slice(&parent.id, running.revision, &Ok(slice))
        .await
        .unwrap();
    assert_eq!(held.state, TaskState::Queued);
    assert_eq!(held.habitat_ui_state(), State::NeedsAction);
    assert!(
        f.managed
            .project_dispatch_block(&held)
            .unwrap()
            .unwrap()
            .contains("budget")
    );
    assert_eq!(
        f.managed.program_status(&parent.id).unwrap().unwrap().calls,
        1
    );
    assert_eq!(
        f.managed.backlog(Some(&f.conversation), 64).unwrap().len(),
        2
    );
}
