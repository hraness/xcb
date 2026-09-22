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
        .create_task(
            &conversation.id,
            new_id("input"),
            "finish the migration and its checks".into(),
            vec![],
            &workspace,
        )
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
