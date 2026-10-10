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
    let base = xcb_core::canonical(root.path()).unwrap();
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
    let completed = xcb_core::policy::TurnFacts {
        terminal: Terminal::Completed,
        joined: true,
        effects: EffectState::Settled,
        pending_attention: false,
        failure: None,
    };
    settle_child_turn(f, child, text, completed, State::Idle).await
}
async fn settle_child_turn(
    f: &Fixture,
    child: &ManagedTask,
    text: &str,
    facts: xcb_core::policy::TurnFacts,
    state: State,
) -> ManagedTask {
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
        text_attention: false,
        text: text.into(),
        facts,
        state,
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

async fn context_worker(f: &Fixture, child: &ManagedTask) -> ManagedTask {
    use xcb_core::models::{Mode, ModelChoice};
    let account = f
        .store
        .add_account(Provider::Codex, "Context fixture", now_ms(), None)
        .unwrap();
    let session = f
        .store
        .create_session(
            &account.id,
            ModelChoice {
                provider: Provider::Codex,
                id: Id::new("context-fixture").unwrap(),
                label: "Context fixture".into(),
                mode: Mode::Fixed,
                resolved: None,
                effort: None,
                observed_at_ms: now_ms(),
            },
            &f.workspace,
            now_ms(),
        )
        .unwrap();
    f.managed
        .prepare(
            child,
            session.id,
            "fixture".into(),
            "fixture".into(),
            0,
            String::new(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn exact_context_is_available_through_the_worker_broker_after_restart() {
    let f = fixture().await;
    let exact = "Original input 🐚\nKeep these exact bytes.\u{feff}";
    let mut manifest = program(1).manifest;
    manifest["cells"][0]["inputs"] = json!({"request":"text"});
    manifest["cells"].as_array_mut().unwrap().insert(
        0,
        json!({"id":"original","kind":"input","outputs":{"value":{"type":"text"}}}),
    );
    manifest["edges"] = json!([{"from":{"cell":"original","port":"value"},"to":{"cell":"worker0","port":"request"}}]);
    manifest["interface"]["inputs"] = json!({"request":{"cell":"original","port":"value"}});
    let program = AdmittedProgram::admit_managed(manifest, json!({"request":exact}), 1).unwrap();
    let task = f
        .managed
        .enqueue_program(
            &f.conversation,
            new_id("m"),
            "Exact original task".into(),
            program.clone(),
        )
        .await
        .unwrap();
    let (running, slice) = run_slice(&f, &task).await;
    let parent = f
        .managed
        .finish_program_slice(&running.id, running.revision, &Ok(slice))
        .await
        .unwrap();
    let id = f
        .managed
        .program_status(&parent.id)
        .unwrap()
        .unwrap()
        .child
        .unwrap();
    let child = f.managed.task(&id).unwrap().unwrap();
    let child = context_worker(&f, &child).await;
    let reference = child
        .program_child
        .as_ref()
        .unwrap()
        .context
        .as_ref()
        .unwrap();
    assert!(worker_prompt(&child, &[], &[], false).contains(&reference.snapshot));
    assert!(worker_prompt(&child, &[], &[], true).contains("xcb_context_query"));
    let reopened = ManagedStore::open(&f.state).unwrap();
    let (catalog, effects) = reopened
        .worker_call(
            &f.store,
            child.session.as_ref().unwrap(),
            "context-inspect",
            "xcb_context_query",
            &json!({"op":"inspect"}),
        )
        .await;
    assert_eq!(effects, EffectState::None);
    let catalog = catalog.unwrap();
    assert_eq!(catalog["snapshot"], reference.snapshot);
    let input = catalog["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["label"] == "effect-context")
        .unwrap()["index"]
        .as_u64()
        .unwrap();
    let (read, effects) = reopened
        .worker_call(
            &f.store,
            child.session.as_ref().unwrap(),
            "context-read",
            "xcb_context_query",
            &json!({"op":"read","index":input}),
        )
        .await;
    assert_eq!(effects, EffectState::None);
    assert_eq!(
        read.unwrap()["text"],
        algal::canonical::canonical(&json!({"inputs":program.inputs,"turn":0})).unwrap()
    );
    let (search, _) = reopened
        .worker_call(
            &f.store,
            child.session.as_ref().unwrap(),
            "context-search",
            "xcb_context_query",
            &json!({"op":"search","query":"🐚"}),
        )
        .await;
    assert!(!search.unwrap()["matches"].as_array().unwrap().is_empty());
    for query in [
        json!({"op":"read","index":95}),
        json!({"op":"inspect","taskId":parent.id}),
        json!({"op":"inspect","snapshot":reference.snapshot}),
        json!({"op":"search","query":"x","maxResults":33}),
        json!({"op":"search","query":"x","maxResults":null}),
    ] {
        assert!(
            reopened
                .worker_call(
                    &f.store,
                    child.session.as_ref().unwrap(),
                    "context-invalid",
                    "xcb_context_query",
                    &query
                )
                .await
                .0
                .is_err()
        );
    }
    assert!(
        reopened
            .worker_call(
                &f.store,
                &new_id("unbound"),
                "context-other",
                "xcb_context_query",
                &json!({"op":"inspect"})
            )
            .await
            .0
            .is_err()
    );
}

#[tokio::test]
async fn next_child_retains_exact_declared_history_and_request_lineage() {
    let f = fixture().await;
    let (parent, first) = waiting(&f, 2).await;
    let exact = "First child exact result é🐚\nEvidence stays here.";
    let first = settle_child(&f, &first, exact).await;
    f.managed.tick_programs(&f.store, true).await.unwrap();
    let parent = f.managed.task(&parent.id).unwrap().unwrap();
    let (running, slice) = run_slice(&f, &parent).await;
    let parent = f
        .managed
        .finish_program_slice(&running.id, running.revision, &Ok(slice))
        .await
        .unwrap();
    let id = f
        .managed
        .program_status(&parent.id)
        .unwrap()
        .unwrap()
        .child
        .unwrap();
    let second = f.managed.task(&id).unwrap().unwrap();
    let second = context_worker(&f, &second).await;
    let catalog = f
        .managed
        .program_context_query(&second, &json!({"op":"inspect"}))
        .unwrap();
    let rows = catalog["entries"].as_array().unwrap();
    let result = rows
        .iter()
        .find(|entry| entry["label"] == "effect-context")
        .unwrap()["index"]
        .as_u64()
        .unwrap();
    let context = f
        .managed
        .program_context_query(&second, &json!({"op":"read","index":result}))
        .unwrap();
    let context: Value = serde_json::from_str(context["text"].as_str().unwrap()).unwrap();
    assert_eq!(context["inputs"]["report"], exact);
    let lineage = rows
        .iter()
        .find(|entry| entry["label"] == "current-lineage")
        .unwrap()["index"]
        .as_u64()
        .unwrap();
    let metadata = f
        .managed
        .program_context_query(&second, &json!({"op":"read","index":lineage}))
        .unwrap();
    let metadata: Value = serde_json::from_str(metadata["text"].as_str().unwrap()).unwrap();
    assert_eq!(metadata["parent"], parent.id.as_str());
    assert_eq!(metadata["call"], 2);
    assert_eq!(metadata["cellId"], "worker1");
    assert_eq!(
        metadata["requestDigest"],
        second.program_child.as_ref().unwrap().request_digest
    );
    // An old session and another child's copied reference cannot select this history.
    assert!(
        f.managed
            .worker_call(
                &f.store,
                first.session.as_ref().unwrap(),
                "old-session",
                "xcb_context_query",
                &json!({"op":"inspect"})
            )
            .await
            .0
            .is_err()
    );
    let mut forged = second.clone();
    forged.program_child.as_mut().unwrap().context = first.program_child.unwrap().context;
    assert!(
        f.managed
            .program_context_query(&forged, &json!({"op":"inspect"}))
            .is_err()
    );
}

#[tokio::test]
async fn legacy_children_remain_dispatchable_without_inventing_context() {
    let f = fixture().await;
    let (parent, child) = waiting(&f, 1).await;
    let mut old_child = serde_json::to_value(&child).unwrap();
    old_child["program_child"]
        .as_object_mut()
        .unwrap()
        .remove("context");
    let old_child: ManagedTask = serde_json::from_value(old_child).unwrap();
    old_child.validate().unwrap();
    let mut db = f.managed.db().unwrap();
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    let mut call = read_call(&tx, &parent.id, 1).unwrap();
    call.context = None;
    call.request.request = None;
    write_call(&tx, &call).unwrap();
    tx.execute(
        "UPDATE tasks SET payload=?1 WHERE id=?2",
        params![
            serde_json::to_string(&old_child).unwrap(),
            old_child.id.as_str()
        ],
    )
    .unwrap();
    tx.commit().unwrap();
    drop(db);
    check_dispatch(&f.managed.db().unwrap(), &old_child, now_ms()).unwrap();
    assert!(!worker_prompt(&old_child, &[], &[], false).contains("xcb_context_query"));
    assert!(matches!(
        f.managed
            .program_context_query(&old_child, &json!({"op":"inspect"})),
        Err(Error::Unavailable(
            "this older program child has no retained exact context"
        ))
    ));
}

#[tokio::test]
async fn exact_context_preserves_same_program_hidden_input_and_private_sibling_scope() {
    let f = fixture().await;
    let secret = "PRIVATE-HOLDOUT-INPUT-should-stay-hidden";
    let private_report = "PRIVATE-SIBLING-OUTPUT-should-stay-hidden";
    let private_goal = "PRIVATE-PARENT-GOAL-should-stay-hidden";
    let mut manifest = program(2).manifest;
    manifest["cells"][0]["prompt"] = json!("Private sibling instruction");
    manifest["cells"][0]["inputs"] = json!({"secret":"text"});
    manifest["cells"][1]
        .as_object_mut()
        .unwrap()
        .remove("inputs");
    manifest["cells"].as_array_mut().unwrap().insert(
        0,
        json!({"id":"hidden","kind":"input","outputs":{"value":{"type":"text"}}}),
    );
    manifest["edges"] =
        json!([{"from":{"cell":"hidden","port":"value"},"to":{"cell":"worker0","port":"secret"}}]);
    manifest["interface"]["inputs"] = json!({"secret":{"cell":"hidden","port":"value"}});
    let program = AdmittedProgram::admit_managed(manifest, json!({"secret":secret}), 2).unwrap();
    let parent = f
        .managed
        .enqueue_program(&f.conversation, new_id("m"), private_goal.into(), program)
        .await
        .unwrap();
    let (running, slice) = run_slice(&f, &parent).await;
    let parent = f
        .managed
        .finish_program_slice(&running.id, running.revision, &Ok(slice))
        .await
        .unwrap();
    let first_id = f
        .managed
        .program_status(&parent.id)
        .unwrap()
        .unwrap()
        .child
        .unwrap();
    let first = f.managed.task(&first_id).unwrap().unwrap();
    assert!(first.goal.contains(secret));
    settle_child(&f, &first, private_report).await;
    f.managed.tick_programs(&f.store, true).await.unwrap();
    let parent = f.managed.task(&parent.id).unwrap().unwrap();
    let (running, slice) = run_slice(&f, &parent).await;
    let parent = f
        .managed
        .finish_program_slice(&running.id, running.revision, &Ok(slice))
        .await
        .unwrap();
    let second_id = f
        .managed
        .program_status(&parent.id)
        .unwrap()
        .unwrap()
        .child
        .unwrap();
    let second = f.managed.task(&second_id).unwrap().unwrap();
    let second = context_worker(&f, &second).await;
    let catalog = f
        .managed
        .program_context_query(&second, &json!({"op":"inspect"}))
        .unwrap();
    for row in catalog["entries"].as_array().unwrap() {
        let value = f
            .managed
            .program_context_query(&second, &json!({"op":"read","index":row["index"]}))
            .unwrap();
        let text = value["text"].as_str().unwrap();
        for hidden in [
            secret,
            private_report,
            private_goal,
            "Private sibling instruction",
        ] {
            assert!(
                !text.contains(hidden),
                "hidden program data crossed a cell view"
            );
        }
    }
    let found = f
        .managed
        .program_context_query(&second, &json!({"op":"search","query":"PRIVATE"}))
        .unwrap();
    assert!(found["matches"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn self_consistent_substituted_context_fails_even_with_recomputed_snapshot_and_row_hashes() {
    let f = fixture().await;
    let (parent, child) = waiting(&f, 1).await;
    let child = context_worker(&f, &child).await;
    let db = f.managed.db().unwrap();
    let mut call = read_call(&db, &parent.id, 1).unwrap();
    let context = call.context.as_mut().unwrap();
    context.entries[0].text = "tampered instruction".into();
    let mut substituted = algal::store::Store::default();
    let snapshot = put_agent_context(&mut substituted, &context.entries).unwrap();
    context.reference = AgentContextHost::new(&substituted)
        .grant(
            &snapshot,
            None,
            Some(&serde_json::from_str(CONTEXT_LIMITS).unwrap()),
        )
        .unwrap();
    context.store().unwrap(); // Internally valid CAS is not proof of this call's source.
    let payload = serde_json::to_string(&call).unwrap();
    db.execute(
        "UPDATE program_calls SET payload=?1,digest=?2 WHERE parent=?3 AND call_index=1",
        params![payload, digest(&payload), parent.id.as_str()],
    )
    .unwrap();
    assert!(matches!(
        read_call(&db, &parent.id, 1),
        Err(Error::Conflict(
            "program context differs from exact effect source"
        ))
    ));
    drop(db);
    assert!(
        f.managed
            .program_context_query(&child, &json!({"op":"inspect"}))
            .is_err()
    );
    assert!(f.managed.task(&parent.id).unwrap().unwrap().program_waiting);
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
    assert!(f.managed.program_record(&parent.id).is_err());
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
    let record = f.managed.program_record(&parent.id).unwrap();
    assert_eq!(record["program"], serde_json::to_value(program(1)).unwrap());
    assert_eq!(
        record["results"][0]["summary"],
        "Complete report, with exact provenance"
    );
    assert_eq!(
        record["receiptDigest"],
        complete.program_receipt.clone().unwrap()
    );
    assert_eq!(record["children"][0], child.id.as_str());
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
    let pure=AdmittedProgram::admit(json!({"contract":"algal.organism.v1","key":"organism:independent","name":"Independent pure work","cells":[{"id":"r","kind":"const","outputs":{"value":{"type":"text","value":"Independent pure result"}}}],"edges":[],"interface":{"inputs":{},"outputs":{"summary":{"cell":"r","port":"value"}}}}),json!({})).unwrap();
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
async fn complete_oversized_child_reports_are_compacted_without_stranding_the_parent() {
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
        f.managed.tick_programs(&f.store, true).await.unwrap();
        let terminal = f.managed.task(&parent.id).unwrap().unwrap();
        if cancel {
            assert_eq!(terminal.state, TaskState::Cancelled);
        } else {
            // The compacted report is a durable continuation input. The
            // supervisor dispatches this queued resume on its next tick.
            assert_eq!(terminal.state, TaskState::Queued);
            assert!(!terminal.program_waiting);
            assert!(terminal.detail.contains("resuming pinned ALGAL program"));
            assert!(!terminal.next_prompt.is_empty());
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
        let response = read_execution(&f.managed.db().unwrap(), &parent.id)
            .unwrap()
            .unwrap()
            .response;
        assert_eq!(response.is_some(), !cancel);
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
    assert_eq!(stopped.state, TaskState::Queued);
    assert!(!stopped.program_waiting);
    assert!(stopped.detail.contains("resuming pinned ALGAL program"));
    assert!(!stopped.next_prompt.is_empty());
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
            .is_some()
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

#[tokio::test]
async fn live_controller_pins_completed_child_sessions_until_controller_settles() {
    let f = fixture().await;
    let (parent, child) = waiting(&f, 1).await;
    let completed = settle_child(&f, &child, "Pinned exact response").await;
    let session = completed.session.unwrap();
    assert!(f.managed.has_active_session(&session).unwrap());
    assert!(f.managed.active_session_ids().unwrap().contains(&session));
    f.managed.tick_programs(&f.store, true).await.unwrap();
    let ready = f.managed.task(&parent.id).unwrap().unwrap();
    let (running, slice) = run_slice(&f, &ready).await;
    f.managed
        .finish_program_slice(&parent.id, running.revision, &Ok(slice))
        .await
        .unwrap();
    assert!(!f.managed.has_active_session(&session).unwrap());
    assert!(!f.managed.active_session_ids().unwrap().contains(&session));
}

#[tokio::test]
async fn program_registered_in_a_cannot_publish_children_in_b() {
    let f = fixture().await;
    let a = f.workspace.clone();
    let b = private::directory(&a.parent().unwrap().join("other")).unwrap();
    let thread = f.managed.global_thread().await.unwrap().id;
    let enqueue_in = |workspace: PathBuf| {
        let managed = f.managed.clone();
        let thread = thread.clone();
        async move {
            managed
                .enqueue_program_at(
                    &thread,
                    Some(&workspace),
                    crate::workspace_infer::BindingOrigin::Cli,
                    new_id("m"),
                    "Controller".into(),
                    program(1),
                )
                .await
        }
    };
    // Without a grant in B a managed program cannot even register there,
    // although A holds one.
    assert!(enqueue_in(b.clone()).await.is_err());
    f.managed
        .configure_project_policy_in(
            &b,
            None,
            "Maintain B".into(),
            8,
            now_ms() + 7_200_000,
            None,
            0,
            0,
        )
        .unwrap();
    // Registered in A, it publishes only under A's grant: pausing A holds it
    // even though B's grant is active.
    let parent = enqueue_in(a.clone()).await.unwrap();
    assert_eq!(parent.workspace, a.to_str().unwrap());
    let (running, slice) = run_slice(&f, &parent).await;
    let policy = f
        .managed
        .project_policy_in(a.to_str().unwrap())
        .unwrap()
        .unwrap();
    let paused = f
        .managed
        .set_project_policy_enabled_in(&policy.workspace, policy.revision, false)
        .unwrap();
    let held = f
        .managed
        .finish_program_slice(&running.id, running.revision, &Ok(slice))
        .await
        .unwrap();
    assert_eq!(held.state, TaskState::Queued);
    assert!(held.detail.starts_with("project authority"));
    assert!(
        f.managed
            .program_status(&parent.id)
            .unwrap()
            .unwrap()
            .child
            .is_none()
    );
    let b_policy = f
        .managed
        .project_policy_in(b.to_str().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(b_policy.admitted_tasks, 0);
    // Resumed, the child lands in A and consumes A's budget only.
    f.managed
        .set_project_policy_enabled_in(&policy.workspace, paused.revision, true)
        .unwrap();
    let (running, slice) = run_slice(&f, &held).await;
    f.managed
        .finish_program_slice(&running.id, running.revision, &Ok(slice))
        .await
        .unwrap();
    let child = f
        .managed
        .task(
            &f.managed
                .program_status(&parent.id)
                .unwrap()
                .unwrap()
                .child
                .unwrap(),
        )
        .unwrap()
        .unwrap();
    assert_eq!(child.workspace, a.to_str().unwrap());
    assert_eq!(child.conversation, thread);
    assert_eq!(
        f.managed
            .project_policy_in(a.to_str().unwrap())
            .unwrap()
            .unwrap()
            .admitted_tasks,
        1
    );
    assert_eq!(
        f.managed
            .project_policy_in(b.to_str().unwrap())
            .unwrap()
            .unwrap()
            .admitted_tasks,
        0
    );
}

#[tokio::test]
async fn hourly_limit_holds_program_child_without_charging_and_resumes_once() {
    let f = fixture().await;
    let policy = f.managed.project_policy(&f.conversation).unwrap().unwrap();
    f.managed
        .update_project_throughput_in(&policy.workspace, policy.revision, 0, 1)
        .unwrap();
    let (parent, child) = waiting(&f, 2).await;
    settle_child(&f, &child, "Retained first report").await;
    f.managed.tick_programs(&f.store, true).await.unwrap();
    let ready = f.managed.task(&parent.id).unwrap().unwrap();
    let (running, slice) = run_slice(&f, &ready).await;
    let held = f
        .managed
        .finish_program_slice(&parent.id, running.revision, &Ok(slice))
        .await
        .unwrap();
    assert_eq!(held.state, TaskState::Queued);
    assert!(!held.program_waiting);
    assert_eq!(held.detail, "project hourly start limit reached");
    assert_eq!(
        f.managed.project_dispatch_block(&held).unwrap(),
        Some("project hourly start limit reached")
    );
    assert_eq!(
        f.managed.program_status(&parent.id).unwrap().unwrap().calls,
        1
    );
    let policy = f.managed.project_policy(&f.conversation).unwrap().unwrap();
    assert_eq!(policy.admitted_tasks, 1);
    assert_eq!(
        f.managed.backlog(Some(&f.conversation), 64).unwrap().len(),
        2
    );
    {
        let db = f.managed.db().unwrap();
        assert!(
            project::check_admission_window(&db, &policy, child.created_at_ms + 3_600_000 - 1)
                .is_err()
        );
        assert!(
            project::check_admission_window(&db, &policy, child.created_at_ms + 3_600_000).is_ok()
        );
    }
    f.managed
        .update_project_throughput_in(&policy.workspace, policy.revision, 0, 2)
        .unwrap();
    assert!(f.managed.project_dispatch_block(&held).unwrap().is_none());
    let (running, slice) = run_slice(&f, &held).await;
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
    let second = f
        .managed
        .task(status.child.as_ref().unwrap())
        .unwrap()
        .unwrap();
    assert!(second.goal.contains("Retained first report"));
    assert_eq!(
        f.managed
            .project_policy(&f.conversation)
            .unwrap()
            .unwrap()
            .admitted_tasks,
        2
    );
}

async fn next_child(f: &Fixture, parent: &ManagedTask, report: &str) -> (ManagedTask, ManagedTask) {
    let status = f.managed.program_status(&parent.id).unwrap().unwrap();
    let child = f.managed.task(&status.child.unwrap()).unwrap().unwrap();
    settle_child(f, &child, report).await;
    f.managed.tick_programs(&f.store, true).await.unwrap();
    let parent = f.managed.task(&parent.id).unwrap().unwrap();
    let (running, slice) = run_slice(f, &parent).await;
    let parent = f
        .managed
        .finish_program_slice(&running.id, running.revision, &Ok(slice))
        .await
        .unwrap();
    let id = f
        .managed
        .program_status(&parent.id)
        .unwrap()
        .unwrap()
        .child
        .unwrap();
    (parent, f.managed.task(&id).unwrap().unwrap())
}

fn history_query(view: &str) -> Value {
    json!({"op":"history","history":{"contract":"xcb.program-history.v1","view":view}})
}

#[tokio::test]
async fn program_history_keeps_call_identity_state_and_declared_report_scope() {
    let f = fixture().await;
    let (parent, _first) = waiting(&f, 4).await;
    let (parent, _second) = next_child(&f, &parent, "worker0 report SEALED-0 🐚").await;
    let (parent, _third) = next_child(&f, &parent, "worker1 report SEALED-1").await;
    let (_parent, fourth) = next_child(&f, &parent, "worker2 report SEALED-2").await;
    let fourth = context_worker(&f, &fourth).await;

    // The running fourth child (worker3) declares only worker2's report.
    // Earlier calls keep identity and state but expose no report body.
    let overview = f
        .managed
        .program_context_query(
            &fourth,
            &json!({"op":"history","history":{"contract":"xcb.program-history.v1","view":"overview","recentLeaves":0}}),
        )
        .unwrap();
    assert_eq!(overview["schema"], "algal.context-history-view.v1");
    assert_eq!(overview["status"], "incomplete");
    assert_eq!(overview["reason"], "missing-summary");
    let items = overview["items"].as_array().unwrap();
    assert_eq!(items.len(), 3);
    assert_eq!(items[0]["kind"], "pending");
    assert_eq!(items[0]["start"], json!(0));
    assert_eq!(items[0]["end"], json!(2));
    for item in &items[1..] {
        assert_eq!(item["kind"], "exact");
    }
    assert!(items[1]["text"].as_str().unwrap().contains("SEALED-2"));
    assert!(
        items[1]["text"]
            .as_str()
            .unwrap()
            .contains("\"state\":\"completed\"")
    );
    assert!(!items[1]["text"].as_str().unwrap().contains("SEALED-1"));
    let own = items[2]["text"].as_str().unwrap();
    assert!(own.contains(&fourth.id.as_str().to_string()));
    assert!(own.contains("\"cellId\":\"worker3\""));
    assert!(own.contains("\"state\":\"running\""));

    // Expansion recovers the pending range's exact metadata leaves.
    let expanded = f
        .managed
        .program_context_query(
            &fourth,
            &json!({"op":"history","history":{"contract":"xcb.program-history.v1","view":"expand","node":items[0]["node"]}}),
        )
        .unwrap();
    assert_eq!(expanded["status"], "complete");
    let children = expanded["items"].as_array().unwrap();
    assert_eq!(children.len(), 2);
    for (position, child) in children.iter().enumerate() {
        assert_eq!(child["kind"], "exact");
        let text = child["text"].as_str().unwrap();
        assert!(text.contains(&format!("\"call\":{}", position + 1)));
        assert!(!text.contains("\"result\""), "hidden report leaked");
    }

    // Exact reads carry the original task and state without report bodies.
    for (index, sealed) in [(0usize, "SEALED-0"), (1, "SEALED-1")] {
        let read = f
            .managed
            .program_context_query(
                &fourth,
                &json!({"op":"history","history":{"contract":"xcb.program-history.v1","view":"read","sourceIndex":index}}),
            )
            .unwrap();
        let text = read["text"].as_str().unwrap();
        assert!(text.contains(&format!("\"call\":{}", index + 1)));
        assert!(text.contains("\"state\":\"completed\""));
        assert!(!text.contains(sealed), "hidden sibling report leaked");
    }
    let read = f
        .managed
        .program_context_query(
            &fourth,
            &json!({"op":"history","history":{"contract":"xcb.program-history.v1","view":"read","sourceIndex":2}}),
        )
        .unwrap();
    assert!(read["text"].as_str().unwrap().contains("SEALED-2"));

    // Literal search covers only what this audience may read.
    let found = f
        .managed
        .program_context_query(
            &fourth,
            &json!({"op":"history","history":{"contract":"xcb.program-history.v1","view":"search","query":"SEALED-2"}}),
        )
        .unwrap();
    assert_eq!(
        found["matches"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["sourceIndex"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        vec![2]
    );
    for sealed in ["SEALED-0", "SEALED-1"] {
        let missed = f
            .managed
            .program_context_query(
                &fourth,
                &json!({"op":"history","history":{"contract":"xcb.program-history.v1","view":"search","query":sealed}}),
            )
            .unwrap();
        assert!(
            missed["matches"].as_array().unwrap().is_empty(),
            "search reached a report outside the declared view"
        );
    }
}

#[tokio::test]
async fn program_history_accepts_valid_derivatives_and_rejects_tampering() {
    let f = fixture().await;
    let (parent, _first) = waiting(&f, 4).await;
    let (parent, _second) = next_child(&f, &parent, "worker0 report").await;
    let (parent, _third) = next_child(&f, &parent, "worker1 report").await;
    let (_parent, fourth) = next_child(&f, &parent, "worker2 report").await;
    let fourth = context_worker(&f, &fourth).await;

    let inspected = f
        .managed
        .program_context_query(&fourth, &history_query("inspect"))
        .unwrap();
    let history = inspected["history"].clone();
    let history_id = algal::canonical::digest(&history).unwrap();
    let node = algal::context_history_contract::context_history_node(&history, 0, 2).unwrap();
    let node_value = serde_json::to_value(&node).unwrap();
    let node_id = algal::canonical::digest(&node_value).unwrap();
    let prompt = algal::canonical::digest(&json!("xcb.test.summary-prompt")).unwrap();
    let policy = algal::canonical::digest(&json!("xcb.test.summary-policy")).unwrap();
    let summarizer = algal::canonical::digest(&json!("xcb.test.summarizer")).unwrap();
    let summary = json!({
        "schema": "algal.context-history-summary.v1",
        "history": history_id,
        "node": node_id,
        "sources": node.sources,
        "children": [],
        "prompt": prompt,
        "policy": policy,
        "summarizer": summarizer,
        "body": "Calls 1 and 2 settled before this audience existed.",
    });
    let summary_id = algal::canonical::digest(&summary).unwrap();
    let generation = json!({
        "schema": "algal.context-history-generation.v1",
        "history": history_id,
        "generation": 1,
        "prompt": prompt,
        "policy": policy,
        "summarizer": summarizer,
        "summaries": [{"node": node_id, "summary": summary_id}],
    });
    let derivatives = json!({
        "generation": generation,
        "nodes": [node_value],
        "summaries": [summary],
    });
    let overview = f
        .managed
        .program_context_query(
            &fourth,
            &json!({"op":"history","history":{"contract":"xcb.program-history.v1","view":"overview","recentLeaves":0,"derivatives":derivatives}}),
        )
        .unwrap();
    assert_eq!(overview["status"], "complete");
    assert_eq!(overview["items"][0]["kind"], "summary");
    assert_eq!(
        overview["items"][0]["text"],
        "Calls 1 and 2 settled before this audience existed."
    );

    // A summary bound to a different range or history cannot substitute.
    let foreign = algal::context_history_contract::context_history_node(&history, 2, 4).unwrap();
    let foreign_value = serde_json::to_value(&foreign).unwrap();
    let mut bad = summary.clone();
    bad["node"] = json!(algal::canonical::digest(&foreign_value).unwrap());
    bad["sources"] = json!(foreign.sources);
    let bad_id = algal::canonical::digest(&bad).unwrap();
    let bad_generation = json!({
        "schema": "algal.context-history-generation.v1",
        "history": history_id,
        "generation": 1,
        "prompt": prompt,
        "policy": policy,
        "summarizer": summarizer,
        "summaries": [{"node": node_id, "summary": bad_id}],
    });
    assert!(
        f.managed
            .program_context_query(
                &fourth,
                &json!({"op":"history","history":{"contract":"xcb.program-history.v1","view":"overview","derivatives":{
                    "generation": bad_generation,
                    "nodes": [node_value, foreign_value],
                    "summaries": [bad],
                }}}),
            )
            .is_err()
    );
}

#[tokio::test]
async fn program_history_rejects_unknown_versions_and_fields() {
    let f = fixture().await;
    let (_parent, first) = waiting(&f, 2).await;
    let first = context_worker(&f, &first).await;
    for input in [
        json!({"op":"history","history":{"contract":"xcb.program-history.v2","view":"overview"}}),
        json!({"op":"history","history":{"contract":"xcb.program-history.v1","view":"publish"}}),
        json!({"op":"history","history":{"contract":"xcb.program-history.v1","view":"overview","taskId":"other"}}),
        json!({"op":"history"}),
        json!({"op":"history","history":{"contract":"xcb.program-history.v1"}}),
    ] {
        assert!(
            f.managed.program_context_query(&first, &input).is_err(),
            "unexpectedly accepted {input}"
        );
    }
    // The tool is not bound to non-children, and history cannot name a task.
    assert!(
        f.managed
            .program_context_query(&first, &json!({"op":"history","history":{"contract":"xcb.program-history.v1","view":"read"}}))
            .is_err()
    );
}

#[tokio::test]
async fn an_owner_dismissed_child_settles_its_waiting_or_cancelled_program() {
    for cancel in [false, true] {
        let f = fixture().await;
        let (parent, child) = waiting(&f, 1).await;
        let mut uncertain = child.clone();
        uncertain.state = TaskState::Uncertain;
        uncertain.session = Some(Id::new("s_dismissed_child").unwrap());
        uncertain.attempts = 1;
        uncertain.revision += 1;
        uncertain.updated_at_ms = now_ms().max(child.updated_at_ms);
        let uncertain = f.managed.transition(&child, uncertain, None).await.unwrap();
        if cancel {
            let mut next = parent.clone();
            next.cancel_requested = true;
            next.revision += 1;
            next.updated_at_ms = now_ms().max(parent.updated_at_ms);
            f.managed.transition(&parent, next, None).await.unwrap();
        }
        f.managed.tick_programs(&f.store, true).await.unwrap();
        assert!(f.managed.task(&parent.id).unwrap().unwrap().program_waiting);

        let dismissed = f
            .managed
            .dismiss_uncertain(&f.store, &uncertain.id, uncertain.revision)
            .await
            .unwrap();
        assert!(dismissed.dismissed);
        f.managed.tick_programs(&f.store, true).await.unwrap();
        let settled = f.managed.task(&parent.id).unwrap().unwrap();
        assert!(!settled.program_waiting);
        assert_eq!(
            settled.state,
            if cancel {
                TaskState::Cancelled
            } else {
                TaskState::Failed
            }
        );
    }
}

#[tokio::test]
async fn a_cancelled_child_settles_its_waiting_program() {
    // The child stopped on an approval question, then the owner cancelled it.
    let f = fixture().await;
    let (parent, child) = waiting(&f, 1).await;
    let question = xcb_core::policy::TurnFacts {
        terminal: Terminal::Failed,
        joined: true,
        effects: EffectState::Settled,
        pending_attention: true,
        failure: Some(xcb_core::policy::Failure::Policy),
    };
    let asked = settle_child_turn(&f, &child, "", question, State::NeedsApproval).await;
    assert_eq!(asked.state, TaskState::NeedsInput);
    f.managed.tick_programs(&f.store, true).await.unwrap();
    assert!(f.managed.task(&parent.id).unwrap().unwrap().program_waiting);
    let cancelled = f.managed.settle_unstarted_cancel(&asked).await.unwrap();
    assert_eq!(cancelled.state, TaskState::Cancelled);
    f.managed.tick_programs(&f.store, true).await.unwrap();
    let stopped = f.managed.task(&parent.id).unwrap().unwrap();
    assert!(!stopped.program_waiting);
    assert_eq!(stopped.state, TaskState::Failed);
    assert_eq!(
        stopped.detail,
        format!(
            "program stopped because child {} settled as cancelled",
            child.id
        )
    );

    // A parent already left waiting on a cancelled child whose last turn is
    // absent settles the next time the program tick evaluates it.
    let f = fixture().await;
    let (parent, child) = waiting(&f, 1).await;
    let mut stale = child.clone();
    stale.state = TaskState::Cancelled;
    stale.session = Some(Id::new("s_cancelled_child").unwrap());
    stale.attempts = 1;
    stale.detail = "cancelled before worker dispatch".into();
    stale.revision += 1;
    stale.updated_at_ms = now_ms().max(child.updated_at_ms);
    f.managed.transition(&child, stale, None).await.unwrap();
    let mut stuck = parent.clone();
    stuck.detail = format!(
        "waiting for child {} · cancelled; no worker slot held",
        child.id
    );
    stuck.revision += 1;
    stuck.updated_at_ms = now_ms().max(parent.updated_at_ms);
    f.managed.transition(&parent, stuck, None).await.unwrap();
    f.managed.tick_programs(&f.store, true).await.unwrap();
    let stopped = f.managed.task(&parent.id).unwrap().unwrap();
    assert!(!stopped.program_waiting);
    assert_eq!(stopped.state, TaskState::Failed);
}

#[tokio::test]
async fn cancelling_a_program_waiting_on_a_cancelled_child_settles_it_as_cancelled() {
    // The child stopped on an approval question and was cancelled; the owner
    // then cancelled the queued parent. Neither holds a provider process.
    let f = fixture().await;
    let (parent, child) = waiting(&f, 1).await;
    let question = xcb_core::policy::TurnFacts {
        terminal: Terminal::Failed,
        joined: true,
        effects: EffectState::Settled,
        pending_attention: true,
        failure: Some(xcb_core::policy::Failure::Policy),
    };
    let asked = settle_child_turn(&f, &child, "", question, State::NeedsApproval).await;
    f.managed.settle_unstarted_cancel(&asked).await.unwrap();
    let parent = f.managed.task(&parent.id).unwrap().unwrap();
    let requested = f
        .managed
        .cancel_task(&parent.id, parent.revision)
        .await
        .unwrap();
    assert!(requested.cancel_requested && requested.program_waiting);
    f.managed.tick_programs(&f.store, true).await.unwrap();
    let settled = f.managed.task(&parent.id).unwrap().unwrap();
    assert_eq!(settled.state, TaskState::Cancelled);
    assert!(!settled.program_waiting && !settled.cancel_requested);
    assert_eq!(
        settled.detail,
        "program cancelled after linked child settled"
    );

    // Cancellation settles even while the service is draining.
    let f = fixture().await;
    let (parent, child) = waiting(&f, 1).await;
    let mut stale = child.clone();
    stale.state = TaskState::Cancelled;
    stale.session = Some(Id::new("s_cancelled_child").unwrap());
    stale.attempts = 1;
    stale.revision += 1;
    stale.updated_at_ms = now_ms().max(child.updated_at_ms);
    f.managed.transition(&child, stale, None).await.unwrap();
    let requested = f
        .managed
        .cancel_task(&parent.id, parent.revision)
        .await
        .unwrap();
    assert!(requested.cancel_requested);
    f.managed.tick_programs(&f.store, false).await.unwrap();
    assert_eq!(
        f.managed.task(&parent.id).unwrap().unwrap().state,
        TaskState::Cancelled
    );
}

#[tokio::test]
async fn unanswered_schedule_settles_a_waiting_program_without_retrying_its_child() {
    let f = fixture().await;
    let (parent, child) = waiting(&f, 1).await;
    let schedule = f
        .managed
        .create_schedule_at_with_timeout(
            &f.conversation,
            None,
            "inspect the previous program".into(),
            60_000,
            now_ms(),
            Some(60_000),
        )
        .await
        .unwrap();
    let facts = xcb_core::policy::TurnFacts {
        terminal: Terminal::Completed,
        joined: true,
        effects: EffectState::Settled,
        pending_attention: true,
        failure: None,
    };
    let question = settle_child_turn(
        &f,
        &child,
        "Which fix should I use?",
        facts,
        State::NeedsAnswer,
    )
    .await;
    assert_eq!(question.state, TaskState::NeedsInput);
    assert_eq!(question.attention, Some(State::NeedsAnswer));
    assert!(
        f.store
            .settled_outcome(
                question.session.as_ref().unwrap(),
                question.message_count_before
            )
            .unwrap()
            .is_some()
    );
    f.managed.tick_programs(&f.store, true).await.unwrap();
    assert!(f.managed.task(&parent.id).unwrap().unwrap().program_waiting);
    f.managed
        .tick_schedule_unanswered(&f.store, question.updated_at_ms + 60_000)
        .await
        .unwrap();
    let failed = f.managed.task(&child.id).unwrap().unwrap();
    assert_eq!(failed.state, TaskState::Failed);
    assert!(failed.dismissed);
    assert_eq!(failed.attempts, question.attempts);
    f.managed.tick_programs(&f.store, true).await.unwrap();
    let stopped = f.managed.task(&parent.id).unwrap().unwrap();
    assert_eq!(stopped.state, TaskState::Failed);
    assert!(!stopped.program_waiting);
    assert!(stopped.detail.contains("settled as failed"));
    assert!(
        f.managed
            .schedule_view(&schedule)
            .unwrap()
            .blocker
            .is_none()
    );
    f.managed
        .tick_schedules(schedule.next_due_ms)
        .await
        .unwrap();
    assert!(
        f.managed
            .schedule(&schedule.id)
            .unwrap()
            .unwrap()
            .last_task
            .is_some()
    );
}

#[tokio::test]
async fn managed_program_schedule_persists_unanswered_timeout_under_existing_grant() {
    let f = fixture().await;
    let created = f
        .managed
        .create_program_schedule_at_with_timeout(
            &f.conversation,
            None,
            "planned work".into(),
            program(1),
            60_000,
            now_ms() + 60_000,
            Some(7_200_000),
        )
        .await
        .unwrap();
    let loaded = f.managed.schedule(&created.id).unwrap().unwrap();
    assert_eq!(loaded.settle_unanswered_after_ms, Some(7_200_000));
    assert!(loaded.program.is_some());
    assert_eq!(
        serde_json::to_value(f.managed.schedule_view(&loaded).unwrap()).unwrap()["settle_unanswered_after_ms"],
        7_200_000
    );
}
