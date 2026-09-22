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
