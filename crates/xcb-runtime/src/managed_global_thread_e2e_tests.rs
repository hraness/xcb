//! Cross-cutting checks of the global thread: prompts bound by inference,
//! the relay and the terminal sharing one thread, a 0.8.x (v6) store
//! upgraded and then used, and rows from a 0.8.x writer that was already
//! running when the store upgraded. Each drives several lanes' code together.
use super::*;
use crate::workspace_infer::{BindingOrigin, BindingSource};

struct Env {
    _root: tempfile::TempDir,
    base: PathBuf,
    managed: Arc<ManagedStore>,
    xcb: Arc<Store>,
}

/// A store at `base/state`; project directories are its siblings.
fn env() -> Env {
    let root = tempfile::tempdir().unwrap();
    let base = xcb_core::canonical(root.path()).unwrap();
    let state = private::directory(&base.join("state")).unwrap();
    let managed = Arc::new(ManagedStore::open(&state).unwrap());
    let xcb = Arc::new(Store::open(&state).unwrap());
    Env {
        _root: root,
        base,
        managed,
        xcb,
    }
}

/// A real `git init` checkout, so snapping and container rules see a
/// repository the way they do on a laptop.
fn repo(base: &Path, name: &str) -> PathBuf {
    let path = base.join(name);
    fs::create_dir_all(&path).unwrap();
    let status = std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&path)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("HOME", base)
        .status()
        .unwrap();
    assert!(status.success());
    assert!(path.join(".git").is_dir());
    xcb_core::canonical(&path).unwrap()
}

fn text(path: &Path) -> &str {
    path.to_str().unwrap()
}

fn thread() -> Id {
    Id::new(GLOBAL_THREAD_ID).unwrap()
}

fn cues(origin: Origin) -> IntakeCues {
    IntakeCues {
        origin,
        explicit: None,
        target: None,
        focus: None,
        launch_hint: None,
        infer_only: false,
    }
}

async fn submit(
    managed: &ManagedStore,
    message: Id,
    prompt: &str,
    cues: IntakeCues,
) -> ManagedTask {
    match managed
        .submit_to_thread(message, prompt.into(), vec![], cues)
        .await
        .unwrap()
    {
        Intake::Accepted {
            task, workspace, ..
        } => {
            assert_eq!(task.workspace, workspace);
            assert_eq!(task.conversation, thread());
            task
        }
        Intake::Ask { reason, .. } => panic!("thread asked: {reason}"),
    }
}

fn fresh(managed: &ManagedStore, task: &ManagedTask) -> ManagedTask {
    managed.task(&task.id).unwrap().unwrap()
}

/// Move a task to `state` from its latest stored revision.
async fn set_state(managed: &ManagedStore, task: &ManagedTask, state: TaskState) -> ManagedTask {
    let current = fresh(managed, task);
    let mut next = current.clone();
    next.state = state;
    next.revision += 1;
    next.updated_at_ms = now_ms().max(current.updated_at_ms);
    if state == TaskState::Running {
        next.session = Some(new_id("s"));
    }
    managed.transition(&current, next, None).await.unwrap()
}

/// A worker in `parent` proposes a follow-up through `xcb_backlog_add`.
async fn propose(managed: &ManagedStore, parent: &ManagedTask, call: &str) -> ManagedTask {
    let (result, _) = managed
        .habitat_worker_call(
            parent,
            parent.session.as_ref().unwrap(),
            call,
            "xcb_backlog_add",
            &json!({"prompt": format!("Follow up on {call}"), "priority": 5}),
        )
        .await;
    let id = Id::new(result.unwrap()["id"].as_str().unwrap()).unwrap();
    managed.task(&id).unwrap().unwrap()
}

fn grant(managed: &ManagedStore, workspace: &Path) -> ProjectPolicy {
    managed
        .configure_project_policy_in(
            workspace,
            None,
            "Maintain this repository".into(),
            4,
            now_ms() + 7_200_000,
            None,
        )
        .unwrap()
}

/// Whether the last tick tried to launch this task. Without accounts an
/// attempted launch records the no-account detail and a backoff entry.
fn attempted(supervisor: &Supervisor, managed: &ManagedStore, task: &ManagedTask) -> bool {
    let attempted = supervisor.launch_attempts.contains_key(&task.id);
    assert_eq!(
        attempted,
        fresh(managed, task).detail.starts_with(NO_ACCOUNT_DETAIL)
    );
    attempted
}

#[tokio::test]
async fn thread_prompt_to_two_repos_binds_serializes_and_grants_do_not_cross() {
    let e = env();
    let alpha = repo(&e.base, "alpha");
    let bravo = repo(&e.base, "bravo");
    e.managed.admit_workspace(&alpha, "command", None).unwrap();
    e.managed.admit_workspace(&bravo, "command", None).unwrap();
    assert!(e.managed.global_thread().await.unwrap().workspace.is_none());

    // A name mention binds the named repository, and a bare continuation
    // stays with it; both are explained and neither waits.
    let fix = submit(
        &e.managed,
        new_id("m"),
        "Fix the flaky parser test in alpha",
        cues(Origin::Tui),
    )
    .await;
    assert_eq!(fix.workspace, text(&alpha));
    let binding = fix.binding.as_ref().unwrap();
    assert_eq!(binding.source, BindingSource::Mention);
    assert_eq!(binding.origin, BindingOrigin::Tui);
    assert_eq!(binding.reason, "named `alpha`");
    assert_eq!(fix.hold_until_ms, None);
    let more = submit(&e.managed, new_id("m"), "continue", cues(Origin::Tui)).await;
    assert_eq!(more.workspace, text(&alpha));
    assert_eq!(
        more.binding.as_ref().unwrap().source,
        BindingSource::Continuation
    );
    assert_eq!(more.hold_until_ms, None);
    let release = submit(
        &e.managed,
        new_id("m"),
        "Draft the release notes for bravo",
        cues(Origin::Tui),
    )
    .await;
    assert_eq!(release.workspace, text(&bravo));

    // One thread over two repositories: a worker in alpha holds alpha's
    // tasks back and leaves bravo free to start.
    let mut supervisor = Supervisor::new(e.managed.clone(), e.xcb.clone());
    supervisor
        .active_workspaces
        .insert(new_id("t"), text(&alpha).into());
    supervisor.tick(false).await.unwrap();
    assert!(!attempted(&supervisor, &e.managed, &fix));
    assert!(!attempted(&supervisor, &e.managed, &more));
    assert!(attempted(&supervisor, &e.managed, &release));

    // Only alpha holds a grant. A worker in bravo proposes a follow-up in
    // the same thread: it carries no grant and is never released.
    let policy = grant(&e.managed, &alpha);
    let parent_b = set_state(&e.managed, &release, TaskState::Running).await;
    let child_b = propose(&e.managed, &parent_b, "bravo").await;
    assert_eq!(child_b.workspace, text(&bravo));
    assert!(child_b.project_proposal.is_none());
    assert!(child_b.deferred);
    let parent_a = set_state(&e.managed, &fix, TaskState::Running).await;
    let child_a = propose(&e.managed, &parent_a, "alpha").await;
    assert_eq!(child_a.workspace, text(&alpha));
    assert_eq!(
        child_a.project_proposal.as_ref().unwrap().generation,
        policy.generation
    );
    for task in [&parent_a, &parent_b] {
        set_state(&e.managed, task, TaskState::Completed).await;
    }
    let continued = set_state(&e.managed, &more, TaskState::Running).await;
    set_state(&e.managed, &continued, TaskState::Completed).await;
    e.managed.tick_projects(now_ms()).await.unwrap();
    assert!(!fresh(&e.managed, &child_a).deferred);
    assert!(fresh(&e.managed, &child_b).deferred);
    assert_eq!(
        e.managed
            .project_policy_in(text(&alpha))
            .unwrap()
            .unwrap()
            .admitted_tasks,
        1
    );
    assert!(e.managed.project_policy_in(text(&bravo)).unwrap().is_none());

    // Working memory is per directory even though every task shares the
    // thread: bravo's workers never see alpha's summaries.
    let bravo_memory: Vec<Id> = e
        .managed
        .working_memory_in(text(&bravo), 32)
        .unwrap()
        .into_iter()
        .map(|row| row.task)
        .collect();
    assert_eq!(bravo_memory, vec![release.id.clone()]);
    let alpha_memory: Vec<Id> = e
        .managed
        .working_memory_in(text(&alpha), 32)
        .unwrap()
        .into_iter()
        .map(|row| row.task)
        .collect();
    assert!(alpha_memory.contains(&fix.id) && alpha_memory.contains(&more.id));
    assert!(!alpha_memory.contains(&release.id));
}

#[tokio::test]
async fn relay_dispatch_and_tui_submit_share_one_thread() {
    let e = env();
    let alpha = repo(&e.base, "alpha");
    let bravo = repo(&e.base, "bravo");
    e.managed.admit_workspace(&alpha, "command", None).unwrap();

    // What `managed_relay::dispatch` does for an absolute wire path: admit it
    // as a dispatch and submit with the relay operation as the message id.
    let operation = new_id("o");
    let admitted = e.managed.admit_workspace(&bravo, "dispatch", None).unwrap();
    assert_eq!(admitted, text(&bravo));
    let relay_cues = || IntakeCues {
        explicit: Some(bravo.clone()),
        ..cues(Origin::Relay)
    };
    let remote = submit(
        &e.managed,
        operation.clone(),
        "Run the release checks",
        relay_cues(),
    )
    .await;
    let binding = remote.binding.as_ref().unwrap();
    assert_eq!(
        (binding.source, binding.origin),
        (BindingSource::Explicit, BindingOrigin::Relay)
    );
    assert_eq!(remote.hold_until_ms, None);

    // The laptop user types into the same thread with alpha focused.
    let local = submit(
        &e.managed,
        new_id("m"),
        "Tidy the parser module",
        IntakeCues {
            focus: Some(text(&alpha).into()),
            ..cues(Origin::Tui)
        },
    )
    .await;
    assert_eq!(local.workspace, text(&alpha));
    assert_eq!(local.binding.as_ref().unwrap().source, BindingSource::Focus);

    // One conversation holds both, and no per-directory view appeared.
    let conversations = e.managed.conversations(16).unwrap();
    assert_eq!(conversations.len(), 1);
    assert_eq!(conversations[0].id, thread());
    assert!(conversations[0].workspace.is_none());
    let tasks: Vec<Id> = e
        .managed
        .tasks(16)
        .unwrap()
        .into_iter()
        .filter(|task| task.conversation == thread())
        .map(|task| task.id)
        .collect();
    assert_eq!(tasks.len(), 2);
    let attributed: i64 = e
        .managed
        .db()
        .unwrap()
        .query_row(
            "SELECT count(DISTINCT task) FROM messages WHERE conversation=?1 AND task IS NOT NULL",
            [GLOBAL_THREAD_ID],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(attributed, 2);

    // A remote `continue` follows the last relay task, never the laptop's
    // more recent focus.
    let remote_more = submit(
        &e.managed,
        new_id("o"),
        "continue",
        IntakeCues {
            infer_only: true,
            ..cues(Origin::Relay)
        },
    )
    .await;
    assert_eq!(remote_more.workspace, text(&bravo));
    assert_eq!(
        remote_more.binding.as_ref().unwrap().source,
        BindingSource::Continuation
    );

    // A retried operation replays its task; reusing it for other text fails.
    let replay = submit(
        &e.managed,
        operation.clone(),
        "Run the release checks",
        relay_cues(),
    )
    .await;
    assert_eq!(replay.id, remote.id);
    assert!(
        e.managed
            .submit_to_thread(operation, "Something else".into(), vec![], relay_cues())
            .await
            .is_err()
    );
    assert_eq!(e.managed.tasks(16).unwrap().len(), 3);
}

fn raw(state: &Path) -> Connection {
    let db = Connection::open(state.join("managed").join("managed.sqlite")).unwrap();
    db.busy_timeout(Duration::from_secs(15)).unwrap();
    db
}

/// Rewrite a closed store into the 0.8.x (v6) shape with raw SQL: no
/// generated column, no registry, and conversation-keyed project tables.
fn downgrade(state: &Path) {
    raw(state)
        .execute_batch(
            "DROP INDEX tasks_workspace_state;
             ALTER TABLE tasks DROP COLUMN workspace;
             DROP TABLE workspaces;
             DROP TABLE project_migration_conflicts;
             DROP TABLE project_policies;
             DROP TABLE project_memory;
             ALTER TABLE project_policies_v6 RENAME TO project_policies;
             ALTER TABLE project_memory_v6 RENAME TO project_memory;
             PRAGMA user_version=6;",
        )
        .unwrap();
}

/// A v7 value in its 0.8.x shape: keyed on a conversation, not a directory.
fn legacy(value: impl Serialize, conversation: &Id) -> String {
    let mut value = serde_json::to_value(value).unwrap();
    let object = value.as_object_mut().unwrap();
    object.remove("workspace");
    object.insert("conversation".into(), json!(conversation.as_str()));
    value.to_string()
}

fn insert_legacy(db: &Connection, table: &str, key: &Id, revision: u64, payload: &str) {
    db.execute(
        &format!("INSERT INTO {table}(conversation,revision,payload) VALUES(?1,?2,?3)"),
        params![key.as_str(), sql(revision).unwrap(), payload],
    )
    .unwrap();
}

fn policy(workspace: &Path, revision: u64) -> ProjectPolicy {
    ProjectPolicy {
        workspace: text(workspace).into(),
        generation: new_id("grant"),
        goal: "Maintain the project".into(),
        enabled: true,
        max_tasks: 5,
        admitted_tasks: 0,
        expires_at_ms: now_ms() + 7_200_000,
        required_provider: None,
        revision,
    }
}

fn binding(workspace: &Path, vault: &str) -> MemoryBinding {
    MemoryBinding {
        workspace: text(workspace).into(),
        config: crate::wordcell::WordcellConfig {
            executable: crate::wordcell::ExecutablePin {
                path: "/usr/bin/true".into(),
                sha256: "0".repeat(64),
            },
            interpreter: None,
            vault: vault.into(),
            vault_device: 1,
            vault_inode: 2,
        },
        revision: 1,
    }
}

fn disposition(conflicts: &[MigrationConflict], kind: &str, conversation: &Id) -> String {
    conflicts
        .iter()
        .find(|row| row.kind == kind && row.conversation == conversation.as_str())
        .map_or_else(|| "missing".into(), |row| row.disposition.clone())
}

#[tokio::test]
async fn v6_fixture_upgrade_then_thread_use() {
    let root = tempfile::tempdir().unwrap();
    let base = xcb_core::canonical(root.path()).unwrap();
    let state = base.join("state");
    let work = repo(&base, "work");
    let other = repo(&base, "other");
    // Two 0.8.x project views over `work` and one over `other`.
    let (first, second, third) = {
        let managed = ManagedStore::open(&state).unwrap();
        (
            managed.create_conversation(&work).await.unwrap().id,
            managed.create_conversation(&work).await.unwrap().id,
            managed.create_conversation(&other).await.unwrap().id,
        )
    };
    downgrade(&state);
    let (kept, dropped, single) = (policy(&work, 2), policy(&work, 1), policy(&other, 3));
    {
        let db = raw(&state);
        db.execute(
            "UPDATE conversations SET updated_at=updated_at+100000 WHERE id=?1",
            [second.as_str()],
        )
        .unwrap();
        insert_legacy(
            &db,
            "project_policies",
            &first,
            1,
            &legacy(&dropped, &first),
        );
        insert_legacy(&db, "project_policies", &second, 2, &legacy(&kept, &second));
        insert_legacy(&db, "project_policies", &third, 3, &legacy(&single, &third));
        insert_legacy(
            &db,
            "project_memory",
            &first,
            1,
            &legacy(binding(&work, "/vault/one"), &first),
        );
        insert_legacy(
            &db,
            "project_memory",
            &second,
            1,
            &legacy(binding(&work, "/vault/two"), &second),
        );
        let version: u32 = db
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, 6);
    }

    // The first 0.9 open upgrades: both grants over `work` were active, so
    // the most recent view's grant is kept paused; `other`'s only grant moves
    // unchanged; distinct Wordcell bindings bind nothing.
    let managed = Arc::new(ManagedStore::open(&state).unwrap());
    let conflicts = managed.migration_conflicts(false).unwrap();
    assert_eq!(disposition(&conflicts, "grant", &second), "winner_paused");
    assert_eq!(disposition(&conflicts, "grant", &first), "superseded");
    assert_eq!(disposition(&conflicts, "grant", &third), "moved");
    assert_eq!(disposition(&conflicts, "memory", &first), "unbound");
    assert_eq!(disposition(&conflicts, "memory", &second), "unbound");
    let paused = managed.project_policy_in(text(&work)).unwrap().unwrap();
    assert_eq!(paused.generation, kept.generation);
    assert!(!paused.enabled);
    let moved = managed.project_policy_in(text(&other)).unwrap().unwrap();
    assert_eq!(moved.generation, single.generation);
    assert!(moved.enabled);
    assert!(managed.memory_binding_in(text(&work)).unwrap().is_none());
    let search = managed
        .search_memory_in(text(&work), "parser", 4)
        .await
        .unwrap_err()
        .to_string();
    assert!(search.contains("conflicting Wordcell bindings"), "{search}");
    // The upgrade registered every directory the 0.8.x store had used.
    let known: Vec<String> = managed
        .known_workspaces(16)
        .unwrap()
        .into_iter()
        .map(|entry| entry.path)
        .collect();
    assert!(known.contains(&text(&work).to_owned()));
    assert!(known.contains(&text(&other).to_owned()));

    // Dispatch into the thread: the single grant, once bound to `third`,
    // now covers a thread task in its directory (the I8 scope change).
    let dispatched = submit(
        &managed,
        new_id("o"),
        "Check the release branch",
        IntakeCues {
            explicit: Some(other.clone()),
            ..cues(Origin::Relay)
        },
    )
    .await;
    assert_eq!(dispatched.workspace, text(&other));
    project::check_dispatch(&managed.db().unwrap(), &dispatched, now_ms()).unwrap();
    let parent = set_state(&managed, &dispatched, TaskState::Running).await;
    let proposal = propose(&managed, &parent, "upgraded").await;
    assert_eq!(
        proposal.project_proposal.as_ref().unwrap().generation,
        single.generation
    );
    set_state(&managed, &parent, TaskState::Completed).await;
    managed.tick_projects(now_ms()).await.unwrap();
    assert!(!fresh(&managed, &proposal).deferred);

    // Resuming the paused grant closes the directory's grant conflicts; the
    // memory conflict stays open until a binding is configured.
    managed
        .set_project_policy_enabled_in(text(&work), paused.revision, true)
        .unwrap();
    let open = managed.migration_conflicts(true).unwrap();
    assert!(
        !open
            .iter()
            .any(|row| row.kind == "grant" && row.workspace.as_deref() == Some(text(&work)))
    );
    assert!(open.iter().any(|row| row.kind == "memory"));
    let resumed = submit(
        &managed,
        new_id("m"),
        "Maintain the parser",
        IntakeCues {
            explicit: Some(work.clone()),
            ..cues(Origin::Cli)
        },
    )
    .await;
    assert_eq!(resumed.workspace, text(&work));
}

#[tokio::test]
async fn generated_workspace_column_survives_old_writer_then_thread_use() {
    let e = env();
    let work = repo(&e.base, "work");
    let view = e.managed.create_conversation(&work).await.unwrap().id;
    let template = e
        .managed
        .enqueue_backlog(&view, new_id("m"), "Held idea".into(), true, 5)
        .await
        .unwrap();
    // A 0.8.x process that was running when the store upgraded inserts a
    // released task with its own column list and no binding.
    let mut old = template.clone();
    old.id = new_id("t");
    old.operation = new_id("m");
    old.source_message = old.operation.clone();
    old.deferred = false;
    old.title = "Old writer work".into();
    e.managed
        .db()
        .unwrap()
        .execute(
            "INSERT INTO tasks(id,operation,source_message,conversation,state,revision,updated_at,payload) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                old.id.as_str(),
                old.operation.as_str(),
                old.source_message.as_str(),
                old.conversation.as_str(),
                old.state.as_str(),
                sql(old.revision).unwrap(),
                sql(old.updated_at_ms).unwrap(),
                serde_json::to_string(&old).unwrap()
            ],
        )
        .unwrap();
    let column: Option<String> = e
        .managed
        .db()
        .unwrap()
        .query_row(
            "SELECT workspace FROM tasks WHERE id=?1",
            [old.id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(column.as_deref(), Some(text(&work)));

    // The thread's work in that directory sees the old row: it serializes
    // behind it and shares its backlog.
    let thread_task = submit(
        &e.managed,
        new_id("m"),
        "Refresh the lockfile",
        IntakeCues {
            explicit: Some(work.clone()),
            ..cues(Origin::Cli)
        },
    )
    .await;
    let backlog: Vec<Id> = e
        .managed
        .backlog_in(text(&work), 64)
        .unwrap()
        .into_iter()
        .map(|task| task.id)
        .collect();
    assert!(backlog.contains(&old.id) && backlog.contains(&thread_task.id));
    let mut supervisor = Supervisor::new(e.managed.clone(), e.xcb.clone());
    supervisor
        .active_workspaces
        .insert(old.id.clone(), text(&work).into());
    supervisor.tick(false).await.unwrap();
    assert!(!attempted(&supervisor, &e.managed, &thread_task));

    // The directory's grant waits on the old row's outstanding work before
    // it releases a thread worker's follow-up, then admits it.
    let policy = grant(&e.managed, &work);
    let parent = set_state(&e.managed, &thread_task, TaskState::Running).await;
    let proposal = propose(&e.managed, &parent, "lockfile").await;
    assert_eq!(
        proposal.project_proposal.as_ref().unwrap().generation,
        policy.generation
    );
    set_state(&e.managed, &parent, TaskState::Completed).await;
    e.managed.tick_projects(now_ms()).await.unwrap();
    assert!(fresh(&e.managed, &proposal).deferred);
    let running = set_state(&e.managed, &old, TaskState::Running).await;
    set_state(&e.managed, &running, TaskState::Completed).await;
    e.managed.tick_projects(now_ms()).await.unwrap();
    assert!(!fresh(&e.managed, &proposal).deferred);
    assert!(
        e.managed
            .working_memory_in(text(&work), 32)
            .unwrap()
            .iter()
            .any(|row| row.task == old.id)
    );
}
