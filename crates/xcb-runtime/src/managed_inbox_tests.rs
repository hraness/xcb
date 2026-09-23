use super::*;
use xcb_core::{
    models::{Mode, ModelChoice},
    policy::TurnFacts,
    session::Session,
};

struct Fixture {
    _root: tempfile::TempDir,
    managed: ManagedStore,
    store: Store,
    task: ManagedTask,
    session: Session,
}
async fn fixture() -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let base = root.path().canonicalize().unwrap();
    let workspace = private::directory(&base.join("work")).unwrap();
    let state = base.join("state");
    let managed = ManagedStore::open(&state).unwrap();
    let store = Store::open(&state).unwrap();
    let account = store
        .add_account(Provider::Claude, "Fixture", now_ms(), None)
        .unwrap();
    let model = ModelChoice {
        provider: Provider::Claude,
        id: Id::new("fixture-model").unwrap(),
        label: "Fixture".into(),
        mode: Mode::Fixed,
        resolved: None,
        effort: None,
        observed_at_ms: now_ms(),
    };
    let session = store
        .create_session(&account.id, model, &workspace, now_ms())
        .unwrap();
    let conversation = managed.create_conversation(&workspace).await.unwrap();
    let task = managed
        .create_task(
            &conversation.id,
            new_id("m"),
            "Implement the agreed change and checks".into(),
            vec![],
            &workspace,
        )
        .await
        .unwrap();
    Fixture {
        _root: root,
        managed,
        store,
        task,
        session,
    }
}
fn guidance(f: &Fixture, text: &str) -> InboxEvent {
    f.managed
        .steer_task(&f.task.id, new_id("guidance"), text.into())
        .unwrap()
}
fn outcome() -> Outcome {
    Outcome {
        tool_calls: Some(0),
        diagnostic: None,
        text: "The work and checks are complete.".into(),
        state: State::Idle,
        facts: TurnFacts {
            terminal: Terminal::Completed,
            joined: true,
            effects: EffectState::Settled,
            pending_attention: false,
            failure: None,
        },
    }
}
async fn prepare(f: &Fixture, task: &ManagedTask) -> (ManagedTask, String) {
    let session = f.store.session(&f.session.id).unwrap().unwrap();
    let message_count = f.store.message_count(&session.id).unwrap();
    let events = f.managed.inbox_pending(task).unwrap();
    let mut prompt_task = task.clone();
    append_batch(&mut prompt_task, &events);
    let prompt = worker_prompt(&prompt_task, &[], &[], task.context_carried);
    let batch = Batch {
        events,
        session: session.id.clone(),
        message_count,
        input_count: prompt_task.user_inputs.len(),
        prompt_digest: digest(&prompt),
    };
    let task = f
        .managed
        .prepare_inbox(
            task,
            session.id,
            "claude/fixture-model".into(),
            "fixture".into(),
            message_count,
            String::new(),
            (!batch.events.is_empty()).then_some(&batch),
        )
        .await
        .unwrap();
    (task, prompt)
}
fn record(f: &Fixture, prompt: &str, outcome: &Outcome) {
    record_submission(f, prompt, outcome, Some(true));
}
fn record_submission(f: &Fixture, prompt: &str, outcome: &Outcome, submitted: Option<bool>) {
    let session = f.store.session(&f.session.id).unwrap().unwrap();
    let input = Message {
        id: new_id("input"),
        role: Role::User,
        text: prompt.into(),
        attachments: vec![],
        at_ms: now_ms(),
        provenance: None,
    };
    let session = f
        .store
        .append_message(&session.id, session.revision, &input)
        .unwrap();
    let run = f
        .store
        .prepare_run(&session.id, session.revision, now_ms())
        .unwrap();
    let session = f.store.session(&session.id).unwrap().unwrap();
    f.store
        .append_message(
            &session.id,
            session.revision,
            &Message {
                id: new_id("answer"),
                role: Role::Assistant,
                text: outcome.text.clone(),
                attachments: vec![],
                at_ms: now_ms(),
                provenance: None,
            },
        )
        .unwrap();
    f.store
        .settle_outcome_submitted(&run, &input.id, outcome, submitted, now_ms())
        .unwrap();
}
fn event(f: &Fixture, id: &Id) -> InboxEvent {
    event_from(&f.managed.db().unwrap(), id).unwrap().unwrap()
}

#[tokio::test]
async fn frozen_membership_delivers_only_prepared_events_and_late_input_wakes_once() {
    let f = fixture().await;
    let early = guidance(&f, "Preserve the existing API");
    let (running, prompt) = prepare(&f, &f.task).await;
    assert_eq!(event(&f, &early.id).status, "prepared");
    let late = guidance(&f, "Also check the rollback path");
    assert!(!prompt.contains(&late.text));
    record(&f, &prompt, &outcome());
    let next = f
        .managed
        .finish(&f.store, &running.id, Ok(outcome()))
        .await
        .unwrap();
    assert_eq!(next.state, TaskState::Queued);
    assert_eq!(next.attempts, 1);
    assert_eq!(next.delivered_inputs, 1);
    assert_eq!(event(&f, &early.id).status, "delivered");
    assert!(event(&f, &early.id).receipt.is_some());
    assert_eq!(event(&f, &late.id).status, "queued");
    let (running, prompt) = prepare(&f, &next).await;
    assert!(prompt.contains(&late.text));
    assert!(!prompt.contains(&early.text));
    record(&f, &prompt, &outcome());
    let done = f
        .managed
        .finish(&f.store, &running.id, Ok(outcome()))
        .await
        .unwrap();
    assert_eq!(done.state, TaskState::Completed);
    assert_eq!(done.attempts, 2);
    assert_eq!(event(&f, &late.id).status, "delivered");
}

#[tokio::test]
async fn late_event_after_empty_dispatch_requests_one_batched_turn() {
    let f = fixture().await;
    let (running, prompt) = prepare(&f, &f.task).await;
    assert!(f.managed.inbox_batch(&running.id).unwrap().is_none());
    for n in 0..8 {
        guidance(&f, &format!("Guidance {n}"));
    }
    record(&f, &prompt, &outcome());
    let next = f
        .managed
        .finish(&f.store, &running.id, Ok(outcome()))
        .await
        .unwrap();
    assert_eq!(next.state, TaskState::Queued);
    let (running, prompt) = prepare(&f, &next).await;
    assert_eq!(
        f.managed
            .inbox_batch(&running.id)
            .unwrap()
            .unwrap()
            .events
            .len(),
        8
    );
    record(&f, &prompt, &outcome());
    let done = f
        .managed
        .finish(&f.store, &running.id, Ok(outcome()))
        .await
        .unwrap();
    assert_eq!(done.state, TaskState::Completed);
    assert_eq!(done.attempts, 2);
    assert!(
        f.managed
            .inbox(Some(&done.id), None, None, 256)
            .unwrap()
            .iter()
            .all(|e| e.status == "delivered")
    );
}

#[tokio::test]
async fn arrival_during_finish_invalidates_the_atomic_snapshot() {
    let f = fixture().await;
    let (running, prompt) = prepare(&f, &f.task).await;
    record(&f, &prompt, &outcome());
    let stamp = f.managed.inbox_stamp(&running.id).unwrap();
    let late = guidance(&f, "Do the additional agreed check");
    let mut next = running.clone();
    next.state = TaskState::Completed;
    next.revision += 1;
    let changed = f
        .managed
        .transition_inbox(
            &running,
            next,
            None,
            &[],
            None,
            None,
            Some(&Change::Finish {
                delivered: false,
                unstarted: false,
                stamp: Some(stamp),
            }),
        )
        .await;
    assert!(matches!(
        changed,
        Err(Error::Conflict("managed task revision changed"))
    ));
    assert_eq!(
        f.managed.task(&running.id).unwrap().unwrap().state,
        TaskState::Running
    );
    assert_eq!(event(&f, &late.id).status, "queued");
    assert_eq!(
        f.managed
            .finish(&f.store, &running.id, Ok(outcome()))
            .await
            .unwrap()
            .state,
        TaskState::Queued
    );
}

#[tokio::test]
async fn failed_unadmitted_dispatch_rolls_back_only_its_staged_input() {
    let f = fixture().await;
    let accepted = guidance(&f, "Run the focused checks");
    let (running, _) = prepare(&f, &f.task).await;
    assert_eq!(running.user_inputs.len(), 1);
    let retry = f
        .managed
        .finish(
            &f.store,
            &running.id,
            Err(Error::Unavailable("fixture preflight failure")),
        )
        .await
        .unwrap();
    assert_eq!(retry.state, TaskState::Queued);
    assert!(retry.user_inputs.is_empty());
    assert!(!retry.context_carried);
    assert_eq!(event(&f, &accepted.id).status, "queued");
    assert!(f.managed.inbox_batch(&running.id).unwrap().is_none());
    let (running, prompt) = prepare(&f, &retry).await;
    assert_eq!(running.user_inputs.len(), 1);
    assert_eq!(prompt.matches(&accepted.text).count(), 1);
}

#[tokio::test]
async fn restart_before_admission_requeues_without_duplicate_input() {
    let f = fixture().await;
    let accepted = guidance(&f, "Preserve receipt provenance");
    let (running, _) = prepare(&f, &f.task).await;
    f.managed.reconcile_startup(&f.store).await.unwrap();
    let retry = f.managed.task(&running.id).unwrap().unwrap();
    assert_eq!(retry.state, TaskState::Queued);
    assert!(retry.user_inputs.is_empty());
    assert_eq!(event(&f, &accepted.id).status, "queued");
    assert!(f.managed.inbox_batch(&running.id).unwrap().is_none());
}

#[tokio::test]
async fn restart_after_exact_settlement_delivers_with_a_receipt_once() {
    let f = fixture().await;
    let accepted = guidance(&f, "Retain the test evidence");
    let (running, prompt) = prepare(&f, &f.task).await;
    record(&f, &prompt, &outcome());
    f.managed.reconcile_startup(&f.store).await.unwrap();
    let done = f.managed.task(&running.id).unwrap().unwrap();
    assert_eq!(done.state, TaskState::Completed);
    let delivered = event(&f, &accepted.id);
    assert_eq!(delivered.status, "delivered");
    assert_eq!(
        delivered.receipt.as_deref(),
        Some(done.last_receipt.as_str())
    );
    f.managed.reconcile_startup(&f.store).await.unwrap();
    assert_eq!(event(&f, &accepted.id), delivered);
}

#[tokio::test]
async fn no_settled_run_or_changed_prompt_never_claims_delivery() {
    for changed in [false, true] {
        let f = fixture().await;
        let accepted = guidance(&f, "Check the exact output");
        let (running, prompt) = prepare(&f, &f.task).await;
        if changed {
            record(&f, "A different prompt", &outcome());
        } else {
            let session = f.store.session(&f.session.id).unwrap().unwrap();
            f.store
                .append_message(
                    &session.id,
                    session.revision,
                    &Message {
                        id: new_id("input"),
                        role: Role::User,
                        text: prompt,
                        attachments: vec![],
                        at_ms: now_ms(),
                        provenance: None,
                    },
                )
                .unwrap();
        }
        let held = f
            .managed
            .finish(&f.store, &running.id, Ok(outcome()))
            .await
            .unwrap();
        assert_eq!(held.state, TaskState::Uncertain);
        assert_eq!(event(&f, &accepted.id).status, "held");
        assert!(event(&f, &accepted.id).receipt.is_none());
        assert!(f.managed.inbox_batch(&running.id).unwrap().is_some());
        assert!(
            f.managed
                .reconcile_uncertain(&f.store, &held.id, held.revision)
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn known_unsubmitted_input_requeues_but_unknown_submission_holds() {
    for submission in [Some(false), None] {
        let f = fixture().await;
        let accepted = guidance(&f, "Preserve exact delivery evidence");
        let (running, prompt) = prepare(&f, &f.task).await;
        let mut failed = outcome();
        failed.state = State::Failed;
        failed.facts.terminal = Terminal::Failed;
        failed.facts.effects = EffectState::None;
        failed.facts.failure = Some(Failure::Transport);
        record_submission(&f, &prompt, &failed, submission);
        let next = f
            .managed
            .finish(&f.store, &running.id, Ok(failed))
            .await
            .unwrap();
        if submission == Some(false) {
            assert_eq!(next.state, TaskState::Queued);
            assert_eq!(event(&f, &accepted.id).status, "queued");
            assert!(next.user_inputs.is_empty());
            assert!(!next.context_carried);
            assert!(f.managed.inbox_batch(&running.id).unwrap().is_none());
            let (_, prompt) = prepare(&f, &next).await;
            assert_eq!(prompt.matches(&accepted.text).count(), 1);
        } else {
            assert_eq!(next.state, TaskState::Uncertain);
            assert_eq!(event(&f, &accepted.id).status, "held");
            assert!(f.managed.inbox_batch(&running.id).unwrap().is_some());
        }
        assert!(event(&f, &accepted.id).receipt.is_none());
    }
}

#[tokio::test]
async fn exact_reconciliation_can_deliver_a_held_batch() {
    let f = fixture().await;
    let accepted = guidance(&f, "Use exact recovery evidence");
    let (running, prompt) = prepare(&f, &f.task).await;
    record(&f, &prompt, &outcome());
    let mut uncertain = outcome();
    uncertain.facts.effects = EffectState::Uncertain;
    let held = f
        .managed
        .finish(&f.store, &running.id, Ok(uncertain))
        .await
        .unwrap();
    assert_eq!(event(&f, &accepted.id).status, "held");
    let done = f
        .managed
        .reconcile_uncertain(&f.store, &held.id, held.revision)
        .await
        .unwrap();
    assert_eq!(done.state, TaskState::Completed);
    assert_eq!(event(&f, &accepted.id).status, "delivered");
    assert!(f.managed.inbox_batch(&running.id).unwrap().is_none());
}

#[tokio::test]
async fn exact_unsubmitted_reconciliation_preserves_guidance_for_explicit_retry() {
    let f = fixture().await;
    let accepted = guidance(&f, "Keep the original scope");
    let (running, prompt) = prepare(&f, &f.task).await;
    let mut failed = outcome();
    failed.state = State::Failed;
    failed.facts.terminal = Terminal::Failed;
    failed.facts.effects = EffectState::None;
    failed.facts.failure = Some(Failure::Transport);
    record_submission(&f, &prompt, &failed, Some(false));
    let mut uncertain = running.clone();
    uncertain.state = TaskState::Uncertain;
    uncertain.revision += 1;
    let uncertain = f
        .managed
        .transition(&running, uncertain, None)
        .await
        .unwrap();
    let reconciled = f
        .managed
        .reconcile_uncertain(&f.store, &uncertain.id, uncertain.revision)
        .await
        .unwrap();
    assert_eq!(reconciled.state, TaskState::NeedsInput);
    assert_eq!(reconciled.attention, Some(State::NeedsAction));
    assert!(reconciled.user_inputs.is_empty());
    assert_eq!(event(&f, &accepted.id).status, "held");
    assert!(event(&f, &accepted.id).receipt.is_none());
    assert!(f.managed.inbox_batch(&running.id).unwrap().is_none());
    let resumed = f
        .managed
        .reply_to_task(&running.id, "The route is repaired; retry".into())
        .await
        .unwrap();
    let (_, prompt) = prepare(&f, &resumed).await;
    assert_eq!(prompt.matches(&accepted.text).count(), 1);
}

#[tokio::test]
async fn approval_and_disabled_continuation_hold_late_guidance() {
    for approval in [false, true] {
        let f = fixture().await;
        let (running, prompt) = prepare(&f, &f.task).await;
        let late = guidance(&f, "Do not expand scope");
        let mut result = outcome();
        if approval {
            result.state = State::NeedsApproval;
            result.facts.pending_attention = true;
        } else {
            let (mut config, revision) = Config::load(f.store.root()).unwrap();
            config.extensions.auto_continue.enabled = false;
            config.save(f.store.root(), revision.as_deref()).unwrap();
        }
        record(&f, &prompt, &result);
        let held = f
            .managed
            .finish(&f.store, &running.id, Ok(result))
            .await
            .unwrap();
        assert_eq!(held.state, TaskState::NeedsInput);
        assert_eq!(held.attempts, 1);
        if approval {
            assert_eq!(held.attention, Some(State::NeedsApproval));
        }
        assert_eq!(event(&f, &late.id).status, "held");
    }
}

#[tokio::test]
async fn overflow_remains_queued_and_cancellation_closes_all_accepted_events() {
    let f = fixture().await;
    for n in 0..24 {
        guidance(&f, &format!("Guidance {n}"));
    }
    let (running, prompt) = prepare(&f, &f.task).await;
    assert_eq!(
        f.managed
            .inbox_batch(&running.id)
            .unwrap()
            .unwrap()
            .events
            .len(),
        16
    );
    let mut cancelled = running.clone();
    cancelled.cancel_requested = true;
    cancelled.revision += 1;
    let cancelled = f
        .managed
        .transition(&running, cancelled, None)
        .await
        .unwrap();
    record(&f, &prompt, &outcome());
    let done = f
        .managed
        .finish(&f.store, &cancelled.id, Ok(outcome()))
        .await
        .unwrap();
    assert_eq!(done.state, TaskState::Completed);
    let events = f.managed.inbox(Some(&done.id), None, None, 256).unwrap();
    assert_eq!(
        events.iter().filter(|e| e.status == "delivered").count(),
        16
    );
    assert_eq!(events.iter().filter(|e| e.status == "closed").count(), 8);
}

#[tokio::test]
async fn bounded_context_fits_whole_fifo_events_or_holds_them_intact() {
    let f = fixture().await;
    let mut task = f.task.clone();
    task.goal = "a".repeat(60_000);
    task.next_prompt = task.goal.clone();
    task.user_inputs = vec!["p".repeat(60_000)];
    let first = guidance(&f, "First short instruction");
    let second = guidance(&f, &"x".repeat(8192));
    let (chosen, prompt_task) = fit(&task, vec![first.clone(), second.clone()], &[]);
    assert_eq!(chosen.len(), 1);
    let prompt = worker_prompt(&prompt_task, &[], &[], false);
    assert!(prompt.len() <= xcb_core::MAX_TEXT_BYTES);
    assert!(prompt.contains(&first.text));
    assert!(!prompt.contains(&second.text));
    task.user_inputs = vec!["p".repeat(65_500)];
    let (chosen, _) = fit(&task, vec![second.clone()], &[]);
    assert!(chosen.is_empty());
    assert_eq!(event(&f, &second.id).status, "queued");
}

#[tokio::test]
async fn no_fitting_event_surfaces_attention_without_clipping_or_closing_input() {
    let f = fixture().await;
    let mut task = f.task.clone();
    task.user_inputs = vec!["Prior explicit input".into(); 64];
    task.revision += 1;
    let task = f.managed.transition(&f.task, task, None).await.unwrap();
    let accepted = f
        .managed
        .steer_task(&task.id, new_id("e"), "y".repeat(8192))
        .unwrap();
    let managed = Arc::new(ManagedStore::open(f.store.root()).unwrap());
    let store = Arc::new(Store::open(f.store.root()).unwrap());
    let mut supervisor = Supervisor::new(managed.clone(), store);
    assert!(matches!(
        supervisor.launch(&task).await.unwrap(),
        Dispatch::Settled
    ));
    let held = managed.task(&task.id).unwrap().unwrap();
    assert_eq!(held.state, TaskState::NeedsInput);
    assert_eq!(held.attention, Some(State::NeedsAction));
    assert!(
        managed
            .attention(256)
            .unwrap()
            .iter()
            .any(|t| t.id == held.id)
    );
    let event = managed
        .inbox(Some(&task.id), None, None, 256)
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(event.id, accepted.id);
    assert_eq!(event.text.len(), 8192);
    assert_eq!(event.status, "held");
    assert!(event.receipt.is_none());
}

#[tokio::test]
async fn prepared_record_retains_headroom_for_maximal_escaped_settlement() {
    for approval in [false, true] {
        let f = fixture().await;
        let mut task = f
            .managed
            .create_task(
                &f.task.conversation,
                new_id("m"),
                "x".repeat(74_000),
                vec![],
                Path::new(&f.task.workspace),
            )
            .await
            .unwrap();
        let mut next = task.clone();
        next.user_inputs.push("\"".repeat(22_000));
        next.revision += 1;
        task = f.managed.transition(&task, next, None).await.unwrap();
        let accepted = f
            .managed
            .steer_task(&task.id, new_id("e"), "\\".repeat(512))
            .unwrap();
        let (events, projected) = fit(&task, f.managed.inbox_pending(&task).unwrap(), &[]);
        assert_eq!(events.len(), 1);
        let projected_bytes = serde_json::to_vec(&projected).unwrap().len();
        assert!(projected_bytes > 185 * 1024 && projected_bytes <= 192 * 1024);
        let (running, prompt) = prepare(&f, &task).await;
        let mut metadata = running.clone();
        metadata.route_reason = Some("\\".repeat(4096));
        metadata.revision += 1;
        let running = f
            .managed
            .transition(&running, metadata, None)
            .await
            .unwrap();
        let mut result = outcome();
        result.text = "\"".repeat(8192);
        if approval {
            result.state = State::NeedsApproval;
            result.facts.pending_attention = true;
        } else {
            result.facts.terminal = Terminal::TurnLimit;
        }
        record(&f, &prompt, &result);
        let next = f
            .managed
            .finish(&f.store, &running.id, Ok(result))
            .await
            .unwrap();
        assert_eq!(
            next.state,
            if approval {
                TaskState::NeedsInput
            } else {
                TaskState::Queued
            }
        );
        assert_eq!(next.last_output.as_ref().unwrap().len(), 8192);
        assert!(serde_json::to_vec(&next).unwrap().len() < 256 * 1024);
        assert_eq!(event(&f, &accepted.id).status, "delivered");
    }
}

#[tokio::test]
async fn target_inbox_capacity_rejection_has_no_mailbox_effect() {
    let f = fixture().await;
    let target = f
        .managed
        .create_task(
            &f.task.conversation,
            new_id("m"),
            "Other work".into(),
            vec![],
            Path::new(&f.task.workspace),
        )
        .await
        .unwrap();
    for n in 0..MAX_TASK_EVENTS {
        f.managed
            .steer_task(&target.id, new_id("e"), format!("Item {n}"))
            .unwrap();
    }
    let (source, _) = prepare(&f, &f.task).await;
    let (result, effects) = f.managed.send_mailbox(
        &source,
        &target,
        &f.session.id,
        Provider::Claude,
        "capacity_call",
        "Extra message".into(),
    );
    assert!(result.is_err());
    assert_eq!(effects, EffectState::None);
    assert!(f.managed.mailbox(&target.id, 0, 64).unwrap().is_empty());
    assert_eq!(
        f.managed
            .inbox(Some(&target.id), None, None, 256)
            .unwrap()
            .len(),
        256
    );
}

#[tokio::test]
async fn schema_three_mailbox_migration_is_custody_guarded_and_imports_once() {
    let f = fixture().await;
    let target = f
        .managed
        .enqueue_backlog(
            &f.task.conversation,
            new_id("m"),
            "Deferred target".into(),
            true,
            5,
        )
        .await
        .unwrap();
    let (source, _) = prepare(&f, &f.task).await;
    let mailbox = f
        .managed
        .send_mailbox(
            &source,
            &target,
            &f.session.id,
            Provider::Claude,
            "legacy",
            "Old message".into(),
        )
        .0
        .unwrap();
    f.managed.db().unwrap().execute_batch("DROP TABLE inbox_batches; DROP TABLE inbox_watches; DROP TABLE inbox_events; PRAGMA user_version=3;").unwrap();
    let state = f.store.root();
    let lock = managed_migration_guard(f.managed.root()).unwrap();
    assert!(ManagedStore::open(state).is_err());
    let version: u32 = f
        .managed
        .db()
        .unwrap()
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, 3);
    drop(lock);
    let reopened = ManagedStore::open(state).unwrap();
    let events = reopened.inbox(Some(&target.id), None, None, 256).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].id.as_str(),
        format!("im_{}", digest(mailbox.id.as_str()))
    );
    assert!(events[0].text.contains("Old message"));
    assert_eq!(events[0].status, "held");
    assert!(events[0].receipt.is_none());
    drop(reopened);
    assert_eq!(
        ManagedStore::open(state)
            .unwrap()
            .inbox(Some(&target.id), None, None, 256)
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn uncertain_watched_source_keeps_reserved_report_waiting_until_conclusive() {
    let f = fixture().await;
    let source = f
        .managed
        .create_task(
            &f.task.conversation,
            new_id("m"),
            "Source work".into(),
            vec![],
            Path::new(&f.task.workspace),
        )
        .await
        .unwrap();
    let watch = f
        .managed
        .watch_task(&f.task.id, &source.id, new_id("w"))
        .unwrap();
    let mut uncertain = source.clone();
    uncertain.state = TaskState::Uncertain;
    uncertain.revision += 1;
    let uncertain = f
        .managed
        .transition(&source, uncertain, None)
        .await
        .unwrap();
    let waiting = event(&f, watch.event.as_ref().unwrap());
    assert_eq!(waiting.status, "waiting");
    assert!(!waiting.text.contains("settled as uncertain"));
    let mut done = uncertain.clone();
    done.state = TaskState::Completed;
    done.revision += 1;
    done.last_output = Some("Recovered exact evidence".into());
    f.managed.transition(&uncertain, done, None).await.unwrap();
    let report = event(&f, watch.event.as_ref().unwrap());
    assert_eq!(report.status, "queued");
    assert!(report.text.contains("Recovered exact evidence"));
}

#[tokio::test]
async fn retention_keeps_closed_watch_target_and_recent_late_report() {
    let f = fixture().await;
    let source = f
        .managed
        .create_task(
            &f.task.conversation,
            new_id("m"),
            "Long running source".into(),
            vec![],
            Path::new(&f.task.workspace),
        )
        .await
        .unwrap();
    let watch = f
        .managed
        .watch_task(&f.task.id, &source.id, new_id("w"))
        .unwrap();
    let mut closed = f.task.clone();
    closed.state = TaskState::Completed;
    closed.revision += 1;
    let mut closed = f.managed.transition(&f.task, closed, None).await.unwrap();
    let old = now_ms().saturating_sub(RETENTION_HORIZON_MS + 1000);
    closed.created_at_ms = old;
    closed.updated_at_ms = old;
    f.managed
        .db()
        .unwrap()
        .execute(
            "UPDATE tasks SET updated_at=?1,payload=?2 WHERE id=?3",
            params![
                sql(old).unwrap(),
                serde_json::to_string(&closed).unwrap(),
                closed.id.as_str()
            ],
        )
        .unwrap();
    f.managed.retain().unwrap();
    assert!(f.managed.task(&closed.id).unwrap().is_some());
    let mut done = source.clone();
    done.state = TaskState::Completed;
    done.revision += 1;
    done.last_output = Some("Late report retained".into());
    f.managed.transition(&source, done, None).await.unwrap();
    f.managed.retain().unwrap();
    assert!(f.managed.task(&closed.id).unwrap().is_some());
    let report = event(&f, watch.event.as_ref().unwrap());
    assert_eq!(report.status, "closed");
    assert!(report.text.contains("Late report retained"));
}

#[tokio::test]
async fn inbox_wake_preserves_confirm_veto_and_uses_neutral_prompt() {
    let safe = "The fix is ready on the branch. Should I open the PR and merge it?";
    let risky = "The old tables are unused. Should I drop the production database tables now?";
    for (mode, text, expected) in [
        (ReflexMode::Observe, safe, TaskState::NeedsInput),
        (ReflexMode::Active, risky, TaskState::NeedsInput),
        (ReflexMode::Active, safe, TaskState::Queued),
    ] {
        let f = fixture().await;
        let mut config = Config::default();
        config.extensions.reflexes.settle = ReflexMode::Active;
        config.extensions.reflexes.confirm = mode;
        config.save(f.store.root(), None).unwrap();
        let (running, prompt) = prepare(&f, &f.task).await;
        let event_id = guidance(&f, "Also summarize the validation results").id;
        let finished = Outcome {
            text: text.into(),
            tool_calls: Some(12),
            ..outcome()
        };
        record(&f, &prompt, &finished);
        let next = f
            .managed
            .finish(&f.store, &running.id, Ok(finished))
            .await
            .unwrap();
        assert_eq!(next.settle.as_deref(), Some("confirm"), "{text}");
        assert_eq!(next.state, expected, "{mode:?}: {text}");
        assert!(!next.next_prompt.contains("Yes, go ahead"));
        let input = event(&f, &event_id);
        assert!(input.receipt.is_none());
        if expected == TaskState::Queued {
            assert!(next.inbox_continuation);
            assert!(next.next_prompt.contains("do not answer approvals"));
            assert_eq!(input.status, "queued");
        } else {
            assert_eq!(next.attention, Some(State::NeedsAnswer));
            assert_eq!(input.status, "held");
        }
    }
}

async fn reflex_continuation(f: &Fixture) -> ManagedTask {
    let mut config = Config::default();
    config.extensions.reflexes.settle = ReflexMode::Active;
    config.save(f.store.root(), None).unwrap();
    let (running, prompt) = prepare(f, &f.task).await;
    let stopped = Outcome {
        text: "Schema migrated. Next, I'll update the callers:".into(),
        tool_calls: Some(60),
        ..outcome()
    };
    record(f, &prompt, &stopped);
    let next = f
        .managed
        .finish(&f.store, &running.id, Ok(stopped))
        .await
        .unwrap();
    assert_eq!(next.state, TaskState::Queued);
    assert_eq!(next.settle.as_deref(), Some("stopped_short"));
    assert!(!next.inbox_continuation);
    next
}

#[tokio::test]
async fn inbox_steered_work_does_not_train_reflex_continuation() {
    let f = fixture().await;
    let next = reflex_continuation(&f).await;
    guidance(&f, "Check the callers and report the results");
    let (running, prompt) = prepare(&f, &next).await;
    assert!(running.inbox_continuation);
    // The cause survives reload/recovery rather than existing only in memory.
    assert!(
        f.managed
            .task(&running.id)
            .unwrap()
            .unwrap()
            .inbox_continuation
    );
    let completed = Outcome {
        tool_calls: Some(14),
        ..outcome()
    };
    record(&f, &prompt, &completed);
    let done = f
        .managed
        .finish(&f.store, &running.id, Ok(completed))
        .await
        .unwrap();
    assert_eq!(done.state, TaskState::Completed);
    let status = reflex::ReflexStore::open(f.store.root())
        .unwrap()
        .status(Reflex::Settle, ReflexMode::Active, true)
        .unwrap();
    assert_eq!(status.observations, 2);
    assert_eq!(status.heads["unfinished"].labeled, 0);
}

#[tokio::test]
async fn cancelling_pending_or_prepared_inbox_work_does_not_train_reflex() {
    for prepared in [false, true] {
        let f = fixture().await;
        let next = reflex_continuation(&f).await;
        guidance(&f, "Use the updated requirements instead");
        if prepared {
            let (running, _) = prepare(&f, &next).await;
            assert!(running.inbox_continuation);
        }
        f.managed
            .submit(
                &f.task.conversation,
                new_id("cancel"),
                format!("cancel {}", f.task.id),
                vec![],
                Path::new(&f.task.workspace),
            )
            .await
            .unwrap();
        assert!(
            f.managed
                .task(&f.task.id)
                .unwrap()
                .unwrap()
                .cancel_requested
        );
        let status = reflex::ReflexStore::open(f.store.root())
            .unwrap()
            .status(Reflex::Settle, ReflexMode::Active, true)
            .unwrap();
        assert_eq!(status.heads["unfinished"].labeled, 0, "prepared={prepared}");
    }
}

#[tokio::test]
async fn late_guidance_neutralizes_queued_confirm_but_preserves_explicit_context() {
    let f = fixture().await;
    let mut config = Config::default();
    config.extensions.reflexes.settle = ReflexMode::Active;
    config.extensions.reflexes.confirm = ReflexMode::Active;
    config.save(f.store.root(), None).unwrap();
    let (running, prompt) = prepare(&f, &f.task).await;
    let ask = Outcome {
        text: "The fix is ready on the branch. Should I open the PR and merge it?".into(),
        tool_calls: Some(12),
        ..outcome()
    };
    record(&f, &prompt, &ask);
    let queued = f
        .managed
        .finish(&f.store, &running.id, Ok(ask))
        .await
        .unwrap();
    assert!(queued.next_prompt.contains("Yes, go ahead"));
    let accepted = guidance(&f, "Actually, stop at the review and report what is ready");
    let (events, projected) = fit(&queued, f.managed.inbox_pending(&queued).unwrap(), &[]);
    assert_eq!(projected.next_prompt, CONTINUATION_PROMPT);
    assert!(!worker_prompt(&projected, &[], &[], false).contains("Yes, go ahead"));
    let (running, prompt) = prepare(&f, &queued).await;
    assert_eq!(running.next_prompt, projected.next_prompt);
    assert!(running.inbox_continuation);
    assert!(prompt.contains(&accepted.text));
    assert!(!prompt.contains("Yes, go ahead"));
    assert_eq!(
        digest(&prompt),
        f.managed
            .inbox_batch(&running.id)
            .unwrap()
            .unwrap()
            .prompt_digest
    );
    for (attempts, checkpoint) in [
        (0, "Yes, go ahead with the tests I explicitly approved."),
        (
            1,
            "Continue the original task on a new eligible route. Previous settled route report: preserve these changes.",
        ),
    ] {
        let mut retained = queued.clone();
        retained.attempts = attempts;
        retained.next_prompt = checkpoint.into();
        let (_, projected) = fit(&retained, events.clone(), &[]);
        assert_eq!(projected.next_prompt, checkpoint);
    }
}
