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

async fn prepared() -> Fixture {
    prepared_with_goal("finish the migration and its checks".into()).await
}

async fn prepared_with_goal(goal: String) -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let base = root.path().canonicalize().unwrap();
    let state = base.join("state");
    let workspace = private::directory(&base.join("work")).unwrap();
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
        .create_task(&conversation.id, new_id("input"), goal, vec![], &workspace)
        .await
        .unwrap();
    let task = managed
        .prepare(
            &task,
            session.id.clone(),
            "claude/fixture-model".into(),
            "fixture route".into(),
            0,
            String::new(),
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

struct DiagnosticProtocol {
    model: ModelChoice,
    result_event: bool,
    stale_catalog: bool,
}

impl crate::protocol::Protocol for DiagnosticProtocol {
    async fn initialize(
        &mut self,
        _: &mut crate::process::StreamProcess,
        _: &str,
    ) -> Result<Vec<ModelChoice>> {
        let mut model = self.model.clone();
        if self.stale_catalog {
            model.id = Id::new("fresh-model").unwrap();
        }
        Ok(vec![model])
    }

    async fn start(
        &mut self,
        process: &mut crate::process::StreamProcess,
        _: crate::protocol::Prompt,
    ) -> Result<()> {
        assert!(
            !self.stale_catalog,
            "stale selection must never reach start"
        );
        if self.result_event {
            process.send(&json!({"fixture":true})).await
        } else {
            Err(Error::Protocol("fixture initialization rejected"))
        }
    }

    async fn receive(
        &mut self,
        _: &mut crate::process::StreamProcess,
        _: &[u8],
    ) -> Result<Vec<crate::protocol::Event>> {
        Ok(vec![
            crate::protocol::Event::Ready,
            crate::protocol::Event::Diagnostic(
                serde_json::from_value(json!("d".repeat(512))).unwrap(),
            ),
            crate::protocol::Event::Result {
                terminal: Terminal::Failed,
                text: "x".repeat(xcb_core::MAX_TEXT_BYTES),
                models: vec![],
            },
        ])
    }

    async fn reply(
        &mut self,
        _: &mut crate::process::StreamProcess,
        _: &str,
        _: Value,
    ) -> Result<()> {
        unreachable!("diagnostic fixture has no tools")
    }
}

#[tokio::test]
async fn runner_diagnostic_survives_restart_and_bounded_managed_message() {
    for (result_event, stale_catalog) in [(false, false), (true, false), (false, true)] {
        let Fixture {
            _root,
            managed,
            store,
            task,
            session,
        } = prepared_with_goal("🦀".repeat(120)).await;
        let store = Arc::new(store);
        store
            .set_models(session.model.provider, std::slice::from_ref(&session.model))
            .unwrap();
        let message = Message {
            id: new_id("input"),
            role: Role::User,
            text: task.goal.clone(),
            attachments: vec![],
            at_ms: now_ms(),
            provenance: None,
        };
        let session = store
            .append_message(&session.id, session.revision, &message)
            .unwrap();
        assert_eq!(session.title.len(), 160);
        assert_eq!(store.messages(&session.id, 10).unwrap()[0].text, task.goal);
        let state_root = store.root().to_path_buf();
        let notices = Arc::new(std::sync::Mutex::new(Vec::new()));
        let observed = notices.clone();
        let (_cancel, cancellation) = tokio::sync::watch::channel(false);
        let outcome = crate::runner::run_prepared(
            store.clone(),
            crate::runner::RunInput {
                session: session.clone(),
                message,
                config: Config::default(),
                pane_generation: false,
            },
            cancellation,
            Arc::new(move |event| {
                if let Progress::Notice(text) = event {
                    observed.lock().unwrap().push(text);
                }
            }),
            crate::runner::Launch {
                command: tokio::process::Command::new("/bin/cat"),
                cwd: PathBuf::from(&task.workspace),
                bridge: None,
                artifacts: crate::runner::LaunchArtifacts::create(store.root()).unwrap(),
                prepared_run: None,
                codex_credentials: None,
            },
            DiagnosticProtocol {
                model: session.model,
                result_event,
                stale_catalog,
            },
            crate::broker::Workspace::open_with_coordination(
                Path::new(&task.workspace),
                &_root.path().join("coordination"),
            )
            .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(outcome.state, State::Failed);
        assert_eq!(outcome.facts.failure, Some(Failure::Unknown));
        assert!(outcome.facts.joined);
        assert_eq!(outcome.facts.effects, EffectState::None);
        let diagnostic = outcome.diagnostic.as_ref().unwrap();
        if stale_catalog {
            assert_eq!(
                store
                    .models()
                    .unwrap()
                    .iter()
                    .map(|model| model.id.as_str())
                    .collect::<Vec<_>>(),
                ["fresh-model"]
            );
            assert!(
                diagnostic
                    .as_str()
                    .contains("not in the fresh provider catalog")
            );
        }
        assert!(
            notices
                .lock()
                .unwrap()
                .iter()
                .any(|text| text == diagnostic.as_str())
        );
        assert!(store.unsettled_runs().unwrap().is_empty());
        assert_eq!(
            store
                .settled_outcome(&session.id, 0)
                .unwrap()
                .unwrap()
                .diagnostic,
            outcome.diagnostic
        );
        drop(managed);
        drop(store);

        let managed = ManagedStore::open(&state_root).unwrap();
        let store = Store::open(&state_root).unwrap();
        managed.reconcile_startup(&store).await.unwrap();
        let recovered = managed.task(&task.id).unwrap().unwrap();
        assert_eq!(recovered.state, TaskState::Failed);
        assert_eq!(recovered.attempts, 1);
        assert!(recovered.detail.ends_with(diagnostic.as_str()));
        assert_eq!(
            managed.verify_task(&task.id).await.unwrap()["verified"],
            true
        );
        let messages = managed.messages(&task.conversation, 10).unwrap();
        let response = messages.last().unwrap();
        assert!(response.text.starts_with(&format!("**{}** · ", task.title)));
        assert!(response.text.contains(diagnostic.as_str()));
        response.validate().unwrap();
        if result_event {
            assert_eq!(task.title.len(), 160);
            assert_eq!(task.goal, "🦀".repeat(120));
            assert_eq!(response.text.len(), xcb_core::MAX_TEXT_BYTES);
        }
    }
}

#[tokio::test]
async fn restart_recovers_exact_turn_limit_and_rejects_legacy_idle_inference() {
    for durable in [true, false] {
        let Fixture {
            _root,
            managed,
            store,
            task,
            session,
        } = prepared().await;
        let input = Message {
            id: new_id("input"),
            role: Role::User,
            text: task.goal.clone(),
            attachments: vec![],
            at_ms: now_ms(),
            provenance: None,
        };
        let session = store
            .append_message(&session.id, session.revision, &input)
            .unwrap();
        let run = store
            .prepare_run(&session.id, session.revision, now_ms())
            .unwrap();
        let outcome = Outcome {
            diagnostic: None,
            text: "Migration written; the turn limit interrupted the remaining checks".into(),
            facts: TurnFacts {
                terminal: Terminal::TurnLimit,
                joined: true,
                effects: EffectState::Settled,
                pending_attention: false,
                failure: None,
            },
            state: State::Idle,
        };
        let current = store.session(&session.id).unwrap().unwrap();
        store
            .append_message(
                &session.id,
                current.revision,
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
        if durable {
            store
                .settle_outcome(&run, &input.id, &outcome, now_ms())
                .unwrap();
        } else {
            store.settle(&run, State::Idle, now_ms()).unwrap();
        }
        let state = store.root().to_path_buf();
        drop(managed);
        drop(store);
        let managed = ManagedStore::open(&state).unwrap();
        let store = Store::open(&state).unwrap();
        managed.reconcile_startup(&store).await.unwrap();
        let recovered = managed.task(&task.id).unwrap().unwrap();
        assert_eq!(
            recovered.state,
            if durable {
                TaskState::Queued
            } else {
                TaskState::Uncertain
            }
        );
        assert_eq!(recovered.session, Some(session.id));
        assert_ne!(recovered.state, TaskState::Completed);
        assert!(store.unsettled_runs().unwrap().is_empty());
    }
}

#[tokio::test]
async fn paused_and_queued_tasks_preserve_sessions_until_terminal() {
    let Fixture {
        _root,
        managed,
        store,
        mut task,
        session,
    } = prepared().await;
    for state in [TaskState::NeedsInput, TaskState::Queued] {
        let mut next = task.clone();
        next.state = state;
        next.revision += 1;
        next.updated_at_ms = now_ms();
        task = managed.transition(&task, next, None).await.unwrap();
        assert!(store.remove_session(&session.id).is_err());
        assert!(
            !store
                .prune_candidates(now_ms() + 1_000, 100)
                .unwrap()
                .contains(&session.id)
        );
        assert!(store.session(&session.id).unwrap().is_some());
    }
    let mut next = task.clone();
    next.state = TaskState::Completed;
    next.revision += 1;
    next.updated_at_ms = now_ms();
    managed.transition(&task, next, None).await.unwrap();
    assert!(
        store
            .prune_candidates(now_ms() + 1_000, 100)
            .unwrap()
            .contains(&session.id)
    );
    assert!(store.remove_session(&session.id).unwrap());
}

// Second-wave depth and robustness: bounded retention, ephemeral progress,
// connection reuse, continuation deltas and orphan custody.

#[tokio::test]
async fn retention_removes_old_rows_and_keeps_new_history_readable() {
    let Fixture {
        _root,
        managed,
        task,
        ..
    } = prepared().await;
    let conversation = task.conversation.clone();
    // Settle the fixture task terminally, then backdate every row it owns
    // past the documented retention horizon.
    let mut done = task.clone();
    done.state = TaskState::Completed;
    done.revision += 1;
    done.updated_at_ms = now_ms();
    let done = managed
        .transition(
            &task,
            done,
            Some(ManagedStore::assistant(
                "settled".to_owned(),
                Some(&task.id),
                task.revision + 1,
            )),
        )
        .await
        .unwrap();
    let old = now_ms() - RETENTION_HORIZON_MS - 60_000;
    {
        let db = managed.db().unwrap();
        db.execute(
            "UPDATE tasks SET updated_at=?1 WHERE id=?2",
            params![sql(old).unwrap(), done.id.as_str()],
        )
        .unwrap();
        db.execute("UPDATE messages SET at_ms=?1", params![sql(old).unwrap()])
            .unwrap();
        db.execute(
            "INSERT INTO mailbox_messages(id,source_task,target_task,sequence,created_at,payload) VALUES('mb_old',?1,?1,1,?2,'{}')",
            params![done.id.as_str(), sql(old).unwrap()],
        )
        .unwrap();
    }
    // A fresh task whose rows must survive the pass.
    let fresh = managed
        .create_task(
            &conversation,
            new_id("input"),
            "new work".into(),
            vec![],
            Path::new(&done.workspace),
        )
        .await
        .unwrap();
    assert!(managed.task(&done.id).unwrap().is_some());
    let removed = managed.retain().unwrap();
    assert!(removed > 0);
    assert!(
        managed.task(&done.id).unwrap().is_none(),
        "terminal task past the horizon survived retention"
    );
    assert!(managed.task(&fresh.id).unwrap().is_some());
    let remaining = managed.messages(&conversation, 256).unwrap();
    assert!(!remaining.is_empty());
    assert!(
        remaining.iter().all(|message| message.at_ms > old),
        "messages past the horizon survived retention"
    );
    // The retained task's receipt chain still verifies.
    assert_eq!(
        managed.verify_task(&fresh.id).await.unwrap()["verified"],
        true
    );
}

#[tokio::test]
async fn per_conversation_cap_keeps_only_the_newest_messages() {
    let Fixture {
        _root,
        managed,
        task,
        ..
    } = prepared().await;
    let conversation = task.conversation.clone();
    let mut sequence: i64 = {
        let db = managed.db().unwrap();
        db.query_row(
            "SELECT COALESCE(max(sequence),0) FROM messages WHERE conversation=?1",
            [conversation.as_str()],
            |row| row.get(0),
        )
        .unwrap()
    };
    {
        let mut db = managed.db().unwrap();
        let tx = db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        for _ in 0..(RETENTION_CONVERSATION_MESSAGES + 32) {
            sequence += 1;
            let message = Message {
                id: new_id("m"),
                role: Role::Assistant,
                text: "bulk".into(),
                attachments: vec![],
                at_ms: now_ms(),
                provenance: None,
            };
            tx.execute(
                "INSERT INTO messages(id,conversation,sequence,task,payload,at_ms) VALUES(?1,?2,?3,NULL,?4,?5)",
                params![
                    message.id.as_str(),
                    conversation.as_str(),
                    sequence,
                    serde_json::to_string(&message).unwrap(),
                    sql(now_ms()).unwrap()
                ],
            )
            .unwrap();
        }
        tx.commit().unwrap();
    }
    managed.retain().unwrap();
    let kept: i64 = managed
        .db()
        .unwrap()
        .query_row(
            "SELECT count(*) FROM messages WHERE conversation=?1",
            [conversation.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(kept, RETENTION_CONVERSATION_MESSAGES);
    // The newest sequences are the ones kept.
    let messages = managed.messages(&conversation, 8).unwrap();
    assert_eq!(messages.len(), 8);
}

#[tokio::test]
async fn degraded_read_only_store_refuses_writes_but_reads() {
    let Fixture {
        _root,
        mut managed,
        task,
        ..
    } = prepared().await;
    // The fallback `open` applies after failed retention is simulated by
    // flagging this handle: writes must fail closed while reads continue.
    managed.read_only = true;
    assert!(managed.read_only());
    let error = managed
        .create_task(
            &task.conversation,
            new_id("input"),
            "later".into(),
            vec![],
            Path::new(&task.workspace),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(error, Error::Unavailable(_)),
        "read-only store accepted a task write: {error:?}"
    );
    assert!(managed.task(&task.id).unwrap().is_some());
    assert!(!managed.messages(&task.conversation, 8).unwrap().is_empty());
    assert_eq!(managed.retain().unwrap(), 0);
}

#[tokio::test]
async fn prune_candidates_scans_active_managed_tasks_once_per_pass() {
    let Fixture {
        _root,
        store,
        task,
        session,
        ..
    } = prepared().await;
    let workspace = Path::new(&task.workspace);
    // Direct sessions become prune candidates without their own scans.
    for _ in 0..3 {
        store
            .create_session(
                &session.account,
                session.model.clone(),
                workspace,
                now_ms() - 10_000,
            )
            .unwrap();
    }
    for expected in [1u64, 2] {
        let candidates = store.prune_candidates(now_ms() + 60_000, 100).unwrap();
        assert_eq!(candidates.len(), 3);
        assert!(!candidates.contains(&session.id));
        assert_eq!(
            store.managed_active_scans(),
            Some(expected),
            "prune pass did not reuse one active-task scan"
        );
    }
}

#[tokio::test]
async fn mailbox_migration_runs_only_when_the_table_is_missing() {
    let Fixture {
        _root,
        managed,
        task,
        ..
    } = prepared().await;
    // The fresh v1 schema already includes the table: nothing to migrate.
    assert_eq!(
        managed
            .mailbox_migrations
            .load(std::sync::atomic::Ordering::Relaxed),
        0
    );
    {
        let db = managed.db().unwrap();
        db.execute_batch("DROP TABLE mailbox_messages").unwrap();
    }
    let state = managed.root().parent().unwrap().to_path_buf();
    let reopened = ManagedStore::open(&state).unwrap();
    assert_eq!(
        reopened
            .mailbox_migrations
            .load(std::sync::atomic::Ordering::Relaxed),
        1,
        "missing mailbox table was not rebuilt"
    );
    let again = ManagedStore::open(&state).unwrap();
    assert_eq!(
        again
            .mailbox_migrations
            .load(std::sync::atomic::Ordering::Relaxed),
        0,
        "existing mailbox table was migrated again"
    );
    // The recreated table answers mailbox reads.
    assert!(again.mailbox(&task.id, 0, 8).unwrap().is_empty());
}

#[tokio::test]
async fn worker_bridge_reuses_one_managed_connection_per_run() {
    let Fixture {
        _root,
        managed,
        store,
        session,
        ..
    } = prepared().await;
    let mut bridge = None;
    let (first, _) = crate::runner::managed_tool_call(
        &store,
        &mut bridge,
        &session.id,
        "run:call-1",
        "xcb_swarm_status",
        &json!({}),
    )
    .await;
    assert!(first.is_ok(), "first swarm status failed: {first:?}");
    assert!(bridge.is_some());
    // Remove the managed database files: a fresh open would see an empty
    // store and fail the worker lookup, while the cached connection keeps
    // answering on its existing descriptor.
    for name in ["managed.sqlite", "managed.sqlite-wal", "managed.sqlite-shm"] {
        let _ = fs::remove_file(managed.root().join(name));
    }
    let (second, _) = crate::runner::managed_tool_call(
        &store,
        &mut bridge,
        &session.id,
        "run:call-2",
        "xcb_swarm_status",
        &json!({}),
    )
    .await;
    assert!(
        second.is_ok(),
        "bridge reopened instead of reusing the run connection: {second:?}"
    );
}

#[tokio::test]
async fn carried_continuation_prompt_sends_only_the_delta() {
    let Fixture { mut task, .. } = prepared().await;
    task.user_inputs = vec!["first follow-up".into(), "second follow-up".into()];
    task.delivered_inputs = 1;
    task.next_prompt = "continue from the confirmed checkpoint".into();
    let carried = worker_prompt(&task, &[], &[], true);
    assert!(carried.contains("second follow-up"));
    assert!(carried.contains("continue from the confirmed checkpoint"));
    assert!(!carried.contains("first follow-up"));
    assert!(!carried.contains(&task.goal));
    assert!(!carried.contains("managed-task contract"));
    let full = worker_prompt(&task, &[], &[], false);
    assert!(full.contains(&task.goal));
    assert!(full.contains("first follow-up"));
    assert!(full.contains("second follow-up"));
    assert!(full.contains("managed-task contract"));
}

#[tokio::test]
async fn ephemeral_progress_shows_in_detail_until_settlement() {
    let Fixture {
        _root,
        managed,
        store,
        task,
        ..
    } = prepared().await;
    let workspace = PathBuf::from(&task.workspace);
    let row_of = |managed: &ManagedStore| {
        managed_view(&store, managed, &task.conversation, &workspace)
            .unwrap()
            .tasks
            .into_iter()
            .find(|row| row.id == task.id)
            .unwrap()
            .detail
    };
    // A beat older than the last durable transition never surfaces.
    let mut beats = BTreeMap::new();
    beats.insert(
        task.id.clone(),
        ProgressBeat {
            at_ms: task.updated_at_ms,
            text: "stale heartbeat".into(),
        },
    );
    write_progress(managed.root(), &beats).unwrap();
    assert_eq!(row_of(&managed), task.detail);
    // A heartbeat newer than the task's last transition merges into detail.
    beats.insert(
        task.id.clone(),
        ProgressBeat {
            at_ms: task.updated_at_ms + 1,
            text: "running tool workspace_exec".into(),
        },
    );
    write_progress(managed.root(), &beats).unwrap();
    let detail = row_of(&managed);
    assert!(detail.contains("running tool workspace_exec"));
    assert!(detail.contains(&task.detail));
    // Settlement's durable transition supersedes the heartbeat.
    let mut settled = task.clone();
    settled.state = TaskState::Completed;
    settled.detail = "task complete".into();
    settled.revision += 1;
    settled.updated_at_ms = task.updated_at_ms + 2;
    let settled = managed.transition(&task, settled, None).await.unwrap();
    assert_eq!(row_of(&managed), settled.detail);
    // The task row itself is untouched: progress is never receipted state.
    assert_eq!(
        managed.task(&task.id).unwrap().unwrap().detail,
        "task complete"
    );
}

#[tokio::test]
async fn startup_sweeps_only_proven_marked_orphan_sessions() {
    let Fixture {
        _root,
        managed,
        store,
        task,
        session,
    } = prepared().await;
    let workspace = Path::new(&task.workspace);
    let now = now_ms();
    // Orphan: marked for the fixture task but never referenced by it.
    let orphan = store
        .create_managed_session(
            &session.account,
            session.model.clone(),
            workspace,
            now,
            &task.id,
        )
        .unwrap();
    // Referenced: a marked session prepared onto a second task.
    let referenced = store
        .create_managed_session(
            &session.account,
            session.model.clone(),
            workspace,
            now,
            &task.id,
        )
        .unwrap();
    let other = managed
        .create_task(
            &task.conversation,
            new_id("input"),
            "second task".into(),
            vec![],
            workspace,
        )
        .await
        .unwrap();
    managed
        .prepare(
            &other,
            referenced.id.clone(),
            "claude/fixture".into(),
            "test".into(),
            0,
            String::new(),
        )
        .await
        .unwrap();
    // Transcript: a marked orphan that may carry work stays.
    let with_message = store
        .create_managed_session(
            &session.account,
            session.model.clone(),
            workspace,
            now,
            &task.id,
        )
        .unwrap();
    let message = Message {
        id: new_id("input"),
        role: Role::User,
        text: "hello".into(),
        attachments: vec![],
        at_ms: now,
        provenance: None,
    };
    store
        .append_message(&with_message.id, with_message.revision, &message)
        .unwrap();
    // Custody: a marked orphan with an unsettled run stays.
    let with_run = store
        .create_managed_session(
            &session.account,
            session.model.clone(),
            workspace,
            now,
            &task.id,
        )
        .unwrap();
    let _run = store
        .prepare_run(&with_run.id, with_run.revision, now)
        .unwrap();
    // Unmarked direct sessions are never sweepable.
    let direct = store
        .create_session(&session.account, session.model.clone(), workspace, now)
        .unwrap();

    managed.reconcile_startup(&store).await.unwrap();

    assert!(
        store.session(&orphan.id).unwrap().is_none(),
        "marked orphan was not swept"
    );
    assert!(store.session(&referenced.id).unwrap().is_some());
    assert!(store.session(&with_message.id).unwrap().is_some());
    assert!(store.session(&with_run.id).unwrap().is_some());
    assert!(store.session(&direct.id).unwrap().is_some());
    assert!(store.session(&session.id).unwrap().is_some());
}
