use super::*;

struct Fixture {
    _root: tempfile::TempDir,
    managed: ManagedStore,
    conversation: Id,
    workspace: PathBuf,
}
async fn fixture() -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let physical_root = root.path().canonicalize().unwrap();
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
    let other = f.managed.create_conversation(&f.workspace).await.unwrap();
    let foreign = f
        .managed
        .enqueue_backlog(
            &other.id,
            new_id("m"),
            "another conversation".into(),
            true,
            0,
        )
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
        version, 6,
        "older binaries must refuse the new writer schema"
    );
}

#[tokio::test]
async fn worker_can_read_full_backlog_prompt_only_inside_its_conversation() {
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
    let other = f.managed.create_conversation(&f.workspace).await.unwrap();
    let foreign = f
        .managed
        .enqueue_backlog(
            &other.id,
            new_id("m"),
            "private other conversation".into(),
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
