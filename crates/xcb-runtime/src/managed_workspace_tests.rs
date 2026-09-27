use super::*;
use crate::workspace_infer::{
    BindingConfidence, BindingOrigin, BindingSource, Resolution, WorkspaceBinding,
};

struct Env {
    _root: tempfile::TempDir,
    base: PathBuf,
    state: PathBuf,
    managed: ManagedStore,
}

fn env() -> Env {
    let root = tempfile::tempdir().unwrap();
    let base = root.path().canonicalize().unwrap();
    let state = base.join("state");
    let managed = ManagedStore::open(&state).unwrap();
    Env {
        _root: root,
        base,
        state,
        managed,
    }
}

fn dir(base: &Path, name: &str) -> PathBuf {
    fs::create_dir_all(base.join(name)).unwrap();
    base.join(name).canonicalize().unwrap()
}

fn text(path: &Path) -> &str {
    path.to_str().unwrap()
}

fn git_repo(base: &Path, name: &str) -> PathBuf {
    let repo = dir(base, name);
    fs::create_dir_all(repo.join(".git")).unwrap();
    repo
}

fn raw(state: &Path) -> Connection {
    let db = Connection::open(state.join("managed").join("managed.sqlite")).unwrap();
    db.busy_timeout(Duration::from_secs(15)).unwrap();
    db
}

fn user_version(db: &Connection) -> u32 {
    db.pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap()
}

fn has_column(db: &Connection, table: &str, column: &str) -> bool {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_xinfo(?1) WHERE name=?2)",
        params![table, column],
        |row| row.get(0),
    )
    .unwrap()
}

fn has_table(db: &Connection, table: &str) -> bool {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
        [table],
        |row| row.get(0),
    )
    .unwrap()
}

/// Rewrite a freshly opened store into the 0.8.x (v6) shape: no generated
/// column, no registry, and conversation-keyed project tables.
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
fn legacy(value: impl Serialize, conversation: &str) -> String {
    let mut value = serde_json::to_value(value).unwrap();
    let object = value.as_object_mut().unwrap();
    object.remove("workspace");
    object.insert("conversation".into(), json!(conversation));
    value.to_string()
}

fn insert_legacy(db: &Connection, table: &str, key: &str, revision: u64, payload: &str) {
    db.execute(
        &format!("INSERT INTO {table}(conversation,revision,payload) VALUES(?1,?2,?3)"),
        params![key, sql(revision).unwrap(), payload],
    )
    .unwrap();
}

fn policy(workspace: &Path, enabled: bool, admitted: u32, revision: u64) -> ProjectPolicy {
    ProjectPolicy {
        workspace: text(workspace).into(),
        generation: new_id("grant"),
        goal: "Maintain the project".into(),
        enabled,
        max_tasks: 5,
        admitted_tasks: admitted,
        expires_at_ms: now_ms() + 7_200_000,
        required_provider: Some(Provider::Codex),
        revision,
    }
}

fn wordcell(vault: &str) -> crate::wordcell::WordcellConfig {
    crate::wordcell::WordcellConfig {
        executable: crate::wordcell::ExecutablePin {
            path: "/usr/bin/true".into(),
            sha256: "0".repeat(64),
        },
        interpreter: None,
        vault: vault.into(),
        vault_device: 1,
        vault_inode: 2,
    }
}

fn binding(workspace: &Path, vault: &str, revision: u64) -> MemoryBinding {
    MemoryBinding {
        workspace: text(workspace).into(),
        config: wordcell(vault),
        revision,
    }
}

/// A 0.8.x store with two project views over one directory. `second` is the
/// more recently updated view; `queued` is an unstarted task in `first`.
struct Legacy {
    _root: tempfile::TempDir,
    base: PathBuf,
    state: PathBuf,
    work: PathBuf,
    first: Id,
    second: Id,
    queued: ManagedTask,
}

async fn legacy_store() -> Legacy {
    let root = tempfile::tempdir().unwrap();
    let base = root.path().canonicalize().unwrap();
    let state = base.join("state");
    let work = dir(&base, "work");
    let managed = ManagedStore::open(&state).unwrap();
    let first = managed.create_conversation(&work).await.unwrap().id;
    let second = managed.create_conversation(&work).await.unwrap().id;
    let queued = managed
        .enqueue_backlog(&first, new_id("m"), "Queued work".into(), true, 5)
        .await
        .unwrap();
    drop(managed);
    downgrade(&state);
    raw(&state)
        .execute(
            "UPDATE conversations SET updated_at=updated_at+100000 WHERE id=?1",
            [second.as_str()],
        )
        .unwrap();
    Legacy {
        _root: root,
        base,
        state,
        work,
        first,
        second,
        queued,
    }
}

fn conflicts(store: &ManagedStore) -> Vec<MigrationConflict> {
    store.migration_conflicts(false).unwrap()
}

fn disposition<'a>(conflicts: &'a [MigrationConflict], kind: &str, conversation: &Id) -> &'a str {
    conflicts
        .iter()
        .find(|c| c.kind == kind && c.conversation == conversation.as_str())
        .map(|c| c.disposition.as_str())
        .unwrap_or("missing")
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

fn explicit(path: &Path) -> IntakeCues {
    IntakeCues {
        explicit: Some(path.to_owned()),
        ..cues(Origin::Cli)
    }
}

fn accepted(intake: Intake) -> (ManagedTask, String, WorkspaceBinding) {
    match intake {
        Intake::Accepted {
            task,
            workspace,
            binding,
            ..
        } => (task, workspace, binding),
        Intake::Ask { reason, .. } => panic!("asked: {reason}"),
    }
}

fn thread() -> Id {
    Id::new(GLOBAL_THREAD_ID).unwrap()
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap())
        .canonicalize()
        .unwrap()
}

fn refused(root: &Path, path: &Path, why: &str) {
    match validate_workspace_root(root, path) {
        Err(Error::Conflict(message)) => {
            assert_eq!(
                message,
                format!("workspace is not allowed: {why}"),
                "{path:?}"
            )
        }
        other => panic!("{path:?} was not refused as {why}: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Schema v7

#[tokio::test]
async fn v7_adds_generated_task_workspace_column_and_index() {
    let e = env();
    let work = dir(&e.base, "work");
    let chat = e.managed.create_conversation(&work).await.unwrap().id;
    let task = e
        .managed
        .enqueue_backlog(&chat, new_id("m"), "Index me".into(), true, 5)
        .await
        .unwrap();
    let db = e.managed.db().unwrap();
    assert_eq!(user_version(&db), 7);
    // `hidden` 2 marks a VIRTUAL generated column in table_xinfo.
    let hidden: i64 = db
        .query_row(
            "SELECT hidden FROM pragma_table_xinfo('tasks') WHERE name='workspace'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(hidden, 2);
    let indexed: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='index' AND name='tasks_workspace_state')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(indexed);
    let workspace: String = db
        .query_row(
            "SELECT workspace FROM tasks WHERE id=?1",
            [task.id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(workspace, task.workspace);
    assert_eq!(workspace, text(&work));
}

#[tokio::test]
async fn old_column_list_insert_after_v7_has_workspace() {
    let e = env();
    let work = dir(&e.base, "work");
    let chat = e.managed.create_conversation(&work).await.unwrap().id;
    let template = e
        .managed
        .enqueue_backlog(&chat, new_id("m"), "Template".into(), true, 5)
        .await
        .unwrap();
    // A 0.8.x writer that was already running inserts with its own column
    // list; the generated column still reads the payload's workspace.
    let mut old = template.clone();
    old.id = new_id("t");
    old.operation = new_id("m");
    old.source_message = old.operation.clone();
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
    let outstanding = project::outstanding_in(&e.managed.db().unwrap(), text(&work), None).unwrap();
    assert!(outstanding.iter().any(|task| task.id == old.id));
    assert_eq!(outstanding.len(), 2);
    let excluded =
        project::outstanding_in(&e.managed.db().unwrap(), text(&work), Some(&template.id)).unwrap();
    assert_eq!(excluded.len(), 1);
    assert!(
        e.managed
            .backlog_in(text(&work), 256)
            .unwrap()
            .iter()
            .any(|task| task.id == old.id)
    );
}

#[tokio::test]
async fn corrupt_payload_row_has_null_workspace_and_stays_unreadable() {
    let e = env();
    let work = dir(&e.base, "work");
    let chat = e.managed.create_conversation(&work).await.unwrap().id;
    let bad = e
        .managed
        .enqueue_backlog(&chat, new_id("m"), "Corrupt me".into(), true, 5)
        .await
        .unwrap();
    e.managed
        .db()
        .unwrap()
        .execute(
            "UPDATE tasks SET payload='{corrupt' WHERE id=?1",
            [bad.id.as_str()],
        )
        .unwrap();
    let workspace: Option<String> = e
        .managed
        .db()
        .unwrap()
        .query_row(
            "SELECT workspace FROM tasks WHERE id=?1",
            [bad.id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(workspace, None);
    assert!(e.managed.tasks(64).unwrap().is_empty());
    assert_eq!(e.managed.unreadable_tasks(), 1);
    assert!(e.managed.backlog_in(text(&work), 256).unwrap().is_empty());
}

#[tokio::test]
async fn v7_single_grant_moves_preserving_generation_budget_revision_and_admitted_proposal_still_dispatches()
 {
    let l = legacy_store().await;
    let grant = policy(&l.work, true, 2, 3);
    insert_legacy(
        &raw(&l.state),
        "project_policies",
        l.first.as_str(),
        3,
        &legacy(&grant, l.first.as_str()),
    );
    let store = ManagedStore::open(&l.state).unwrap();
    let moved = store.project_policy_in(text(&l.work)).unwrap().unwrap();
    assert_eq!(moved.generation, grant.generation);
    assert_eq!(moved.revision, 3);
    assert_eq!(moved.admitted_tasks, 2);
    assert_eq!(moved.max_tasks, 5);
    assert_eq!(moved.expires_at_ms, grant.expires_at_ms);
    assert_eq!(moved.required_provider, Some(Provider::Codex));
    assert!(moved.enabled);
    let rows = conflicts(&store);
    assert_eq!(disposition(&rows, "grant", &l.first), "moved");
    assert!(store.migration_conflicts(true).unwrap().is_empty());
    // An admitted proposal of the old generation keeps dispatching, now from
    // either view over the directory (the I8 scope change).
    for conversation in [&l.first, &l.second] {
        let mut task = store.task(&l.queued.id).unwrap().unwrap();
        task.conversation = conversation.clone();
        task.project_proposal = Some(ProjectProposal {
            parent: task.id.clone(),
            generation: grant.generation.clone(),
            admitted: true,
            required_provider: Some(Provider::Codex),
        });
        project::check_dispatch(&store.db().unwrap(), &task, now_ms()).unwrap();
        task.project_proposal.as_mut().unwrap().generation = new_id("grant");
        assert!(project::check_dispatch(&store.db().unwrap(), &task, now_ms()).is_err());
    }
}

#[tokio::test]
async fn v7_one_active_grant_among_several_stays_enabled() {
    let l = legacy_store().await;
    let active = policy(&l.work, true, 0, 1);
    let paused = policy(&l.work, false, 0, 4);
    let db = raw(&l.state);
    insert_legacy(
        &db,
        "project_policies",
        l.first.as_str(),
        1,
        &legacy(&active, l.first.as_str()),
    );
    insert_legacy(
        &db,
        "project_policies",
        l.second.as_str(),
        4,
        &legacy(&paused, l.second.as_str()),
    );
    drop(db);
    let store = ManagedStore::open(&l.state).unwrap();
    let kept = store.project_policy_in(text(&l.work)).unwrap().unwrap();
    assert_eq!(kept.generation, active.generation);
    assert!(kept.enabled);
    assert_eq!(kept.revision, 1);
    let rows = conflicts(&store);
    assert_eq!(disposition(&rows, "grant", &l.first), "moved");
    assert_eq!(disposition(&rows, "grant", &l.second), "superseded");
}

#[tokio::test]
async fn v7_two_active_grants_pause_most_recent_and_count_stranded() {
    let l = legacy_store().await;
    let older = policy(&l.work, true, 1, 2);
    let newer = policy(&l.work, true, 0, 5);
    let db = raw(&l.state);
    insert_legacy(
        &db,
        "project_policies",
        l.first.as_str(),
        2,
        &legacy(&older, l.first.as_str()),
    );
    insert_legacy(
        &db,
        "project_policies",
        l.second.as_str(),
        5,
        &legacy(&newer, l.second.as_str()),
    );
    // One unstarted task depends on the grant that will be superseded.
    db.execute(
        "UPDATE tasks SET payload=json_set(payload,'$.project_proposal',json_object('parent',id,'generation',?1,'admitted',json('false'),'required_provider',json('null'))) WHERE id=?2",
        params![older.generation.as_str(), l.queued.id.as_str()],
    )
    .unwrap();
    drop(db);
    let store = ManagedStore::open(&l.state).unwrap();
    let winner = store.project_policy_in(text(&l.work)).unwrap().unwrap();
    // The most recently updated view wins, paused, one revision later.
    assert_eq!(winner.generation, newer.generation);
    assert!(!winner.enabled);
    assert_eq!(winner.revision, 6);
    assert_eq!(winner.max_tasks, newer.max_tasks);
    assert_eq!(winner.admitted_tasks, newer.admitted_tasks);
    let rows = conflicts(&store);
    assert_eq!(disposition(&rows, "grant", &l.second), "winner_paused");
    let superseded = rows
        .iter()
        .find(|c| c.conversation == l.first.as_str())
        .unwrap();
    assert_eq!(superseded.disposition, "superseded");
    assert_eq!(superseded.stranded_tasks, 1);
    assert_eq!(superseded.workspace.as_deref(), Some(text(&l.work)));
    let project = store.project_rows().unwrap();
    assert_eq!(project[0].status, "paused by upgrade");
    assert!(!store.migration_conflicts(true).unwrap().is_empty());
    // Resuming deliberately settles the directory's grant conflicts.
    store
        .set_project_policy_enabled_in(text(&l.work), winner.revision, true)
        .unwrap();
    assert!(
        store
            .migration_conflicts(true)
            .unwrap()
            .iter()
            .all(|c| c.kind != "grant")
    );
    assert_eq!(store.project_rows().unwrap()[0].status, "active");
}

#[tokio::test]
async fn v7_identical_bindings_merge_distinct_bindings_unbind_and_search_reports_conflict() {
    let l = legacy_store().await;
    let other = dir(&l.base, "other");
    let db = raw(&l.state);
    // Two more views over a second directory, written in the v6 shape.
    let [third, fourth] = ["c_third", "c_fourth"].map(|name| {
        let conversation = ManagedConversation {
            version: 1,
            id: Id::new(name).unwrap(),
            title: name.into(),
            workspace: Some(text(&other).into()),
            created_at_ms: 1,
            updated_at_ms: 1,
        };
        db.execute(
            "INSERT INTO conversations(id,updated_at,payload) VALUES(?1,1,?2)",
            params![name, serde_json::to_string(&conversation).unwrap()],
        )
        .unwrap();
        conversation.id
    });
    for (conversation, config, revision) in [
        (&l.first, binding(&l.work, "/vault/shared", 2), 2),
        (&l.second, binding(&l.work, "/vault/shared", 7), 7),
        (&third, binding(&other, "/vault/a", 1), 1),
        (&fourth, binding(&other, "/vault/b", 1), 1),
    ] {
        let payload = legacy(&config, conversation.as_str());
        insert_legacy(
            &db,
            "project_memory",
            conversation.as_str(),
            revision,
            &payload,
        );
    }
    drop(db);
    let store = ManagedStore::open(&l.state).unwrap();
    let merged = store.memory_binding_in(text(&l.work)).unwrap().unwrap();
    assert_eq!(merged.revision, 7);
    assert_eq!(merged.config.vault, PathBuf::from("/vault/shared"));
    assert!(store.memory_binding_in(text(&other)).unwrap().is_none());
    let rows = conflicts(&store);
    assert_eq!(disposition(&rows, "memory", &l.first), "merged");
    assert_eq!(disposition(&rows, "memory", &l.second), "merged");
    assert_eq!(disposition(&rows, "memory", &third), "unbound");
    assert_eq!(disposition(&rows, "memory", &fourth), "unbound");
    match store.search_memory_in(text(&other), "anything", 4).await {
        Err(Error::Unavailable(message)) => assert_eq!(
            message,
            "conflicting Wordcell bindings from upgrade; run xcb memory configure <dir>"
        ),
        other => panic!("unexpected search result: {other:?}"),
    }
}

#[tokio::test]
async fn v7_undecodable_and_orphan_rows_recorded_open_succeeds() {
    let l = legacy_store().await;
    let db = raw(&l.state);
    // Orphans exist only where a 0.8.x writer ran without foreign keys.
    db.execute_batch("PRAGMA foreign_keys=OFF;").unwrap();
    insert_legacy(&db, "project_policies", l.first.as_str(), 1, "{bad");
    // A row keyed on a conversation that no longer exists.
    let ghost = policy(&l.work, true, 0, 1);
    insert_legacy(
        &db,
        "project_policies",
        "c_missing",
        1,
        &legacy(&ghost, "c_missing"),
    );
    // A row whose payload names a different conversation than its key.
    let mismatched = binding(&l.work, "/vault/x", 1);
    insert_legacy(
        &db,
        "project_memory",
        l.second.as_str(),
        1,
        &legacy(&mismatched, l.first.as_str()),
    );
    drop(db);
    let store = ManagedStore::open(&l.state).unwrap();
    assert!(store.project_policy_in(text(&l.work)).unwrap().is_none());
    assert!(store.memory_binding_in(text(&l.work)).unwrap().is_none());
    let rows = conflicts(&store);
    assert_eq!(rows.len(), 3);
    assert!(rows.iter().all(|c| c.disposition == "dropped"));
    assert_eq!(disposition(&rows, "undecodable", &l.first), "dropped");
    assert_eq!(
        disposition(&rows, "orphan", &Id::new("c_missing").unwrap()),
        "dropped"
    );
    assert_eq!(disposition(&rows, "orphan", &l.second), "dropped");
    assert!(rows.iter().any(|c| c.payload == "{bad"));
}

/// Audit-only rows are settled at upgrade: an orphan is dropped, and with
/// one active grant nothing was paused, so its superseded sibling needs no
/// action either. The supervisor raises no conflict notice.
#[tokio::test]
async fn v7_audit_only_conflicts_are_settled_and_raise_no_notice() {
    let l = legacy_store().await;
    let active = policy(&l.work, true, 0, 1);
    let spent = policy(&l.work, true, 5, 3);
    let db = raw(&l.state);
    db.execute_batch("PRAGMA foreign_keys=OFF;").unwrap();
    insert_legacy(
        &db,
        "project_policies",
        l.first.as_str(),
        1,
        &legacy(&active, l.first.as_str()),
    );
    insert_legacy(
        &db,
        "project_policies",
        l.second.as_str(),
        3,
        &legacy(&spent, l.second.as_str()),
    );
    let ghost = policy(&l.work, true, 0, 1);
    insert_legacy(
        &db,
        "project_policies",
        "c_missing",
        1,
        &legacy(&ghost, "c_missing"),
    );
    drop(db);
    let store = ManagedStore::open(&l.state).unwrap();
    let rows = conflicts(&store);
    assert_eq!(disposition(&rows, "grant", &l.first), "moved");
    assert_eq!(disposition(&rows, "grant", &l.second), "superseded");
    assert_eq!(
        disposition(&rows, "orphan", &Id::new("c_missing").unwrap()),
        "dropped"
    );
    assert!(store.migration_conflicts(true).unwrap().is_empty());
    assert_eq!(store.project_rows().unwrap()[0].status, "active");
    store.tick_workspace_identity(now_ms()).unwrap();
    let fault = supervisor_fault(store.root());
    assert!(
        !fault
            .as_deref()
            .is_some_and(|fault| fault.contains("upgrade conflict")),
        "{fault:?}"
    );
    // A grant the owner pauses later is merely paused.
    let kept = store.project_policy_in(text(&l.work)).unwrap().unwrap();
    store
        .set_project_policy_enabled_in(text(&l.work), kept.revision, false)
        .unwrap();
    assert_eq!(store.project_rows().unwrap()[0].status, "paused");
}

/// A legacy row whose columns have the wrong types is recorded as a dropped
/// `undecodable` conflict instead of failing every open.
#[tokio::test]
async fn v7_rows_with_wrong_column_types_are_dropped_not_fatal() {
    let l = legacy_store().await;
    let db = raw(&l.state);
    db.execute_batch(
        "PRAGMA foreign_keys=OFF;
         INSERT INTO project_policies(conversation,revision,payload) VALUES(NULL,1,'{}');
         INSERT INTO project_policies(conversation,revision,payload) VALUES('c_x','abc',x'00');",
    )
    .unwrap();
    drop(db);
    let store = ManagedStore::open(&l.state).unwrap();
    assert_eq!(user_version(&store.db().unwrap()), 7);
    let rows = conflicts(&store);
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert!(
        rows.iter()
            .all(|c| c.kind == "undecodable" && c.disposition == "dropped")
    );
    assert!(
        rows.iter()
            .any(|c| c.conversation == "NULL" && c.payload == "{}")
    );
    assert!(
        rows.iter()
            .any(|c| c.conversation == "c_x" && c.payload == "revision=abc payload=x'00'")
    );
    assert!(store.migration_conflicts(true).unwrap().is_empty());
}

/// A failed upgrade removes the copy it made, so retries never pile up
/// copies; the attempt that succeeds keeps exactly one.
#[tokio::test]
async fn failed_upgrade_attempts_keep_no_backup() {
    let l = legacy_store().await;
    let managed_root = l.state.join("managed");
    // A stray table in the registry's name makes the v7 step fail.
    raw(&l.state)
        .execute_batch("CREATE TABLE workspaces(x TEXT);")
        .unwrap();
    let backups = || {
        fs::read_dir(&managed_root)
            .unwrap()
            .flatten()
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("managed.pre-v7.")
            })
            .count()
    };
    for _ in 0..2 {
        assert!(ManagedStore::open(&l.state).is_err());
        assert_eq!(user_version(&raw(&l.state)), 6);
        assert_eq!(backups(), 0);
    }
    raw(&l.state)
        .execute_batch("DROP TABLE workspaces;")
        .unwrap();
    let store = ManagedStore::open(&l.state).unwrap();
    assert_eq!(user_version(&store.db().unwrap()), 7);
    assert_eq!(backups(), 1);
}

#[tokio::test]
async fn v7_is_idempotent_after_habitat_version_reset() {
    let e = env();
    let work = dir(&e.base, "work");
    let chat = e.managed.create_conversation(&work).await.unwrap().id;
    let grant = e
        .managed
        .configure_project_policy(
            &chat,
            None,
            "Keep going".into(),
            3,
            now_ms() + 7_200_000,
            None,
        )
        .unwrap();
    e.managed
        .db()
        .unwrap()
        .execute_batch("DROP TABLE habitat_schedules; PRAGMA user_version=1;")
        .unwrap();
    let state = e.state.clone();
    drop(e.managed);
    for _ in 0..2 {
        let store = ManagedStore::open(&state).unwrap();
        let db = store.db().unwrap();
        assert_eq!(user_version(&db), 7);
        let columns: i64 = db
            .query_row(
                "SELECT count(*) FROM pragma_table_xinfo('tasks') WHERE name='workspace'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(columns, 1);
        assert!(has_table(&db, "habitat_schedules"));
        assert!(has_column(&db, "project_policies", "workspace"));
        drop(db);
        assert_eq!(
            store
                .project_policy_in(text(&work))
                .unwrap()
                .unwrap()
                .generation,
            grant.generation
        );
        assert!(conflicts(&store).is_empty());
    }
}

#[tokio::test]
async fn v7_rebuilds_legacy_shaped_project_tables_after_drop_and_reset() {
    let e = env();
    let work = dir(&e.base, "work");
    let chat = e.managed.create_conversation(&work).await.unwrap().id;
    let state = e.state.clone();
    drop(e.managed);
    // A v7 store whose grant table an older step recreated in the legacy
    // shape and filled: the rows are evidence, not authority.
    let stale = legacy(policy(&work, true, 0, 1), chat.as_str());
    let db = raw(&state);
    db.execute_batch(
        "DROP TABLE project_policies;
         CREATE TABLE project_policies(conversation TEXT PRIMARY KEY REFERENCES conversations(id),revision INTEGER NOT NULL,payload TEXT NOT NULL);
         PRAGMA user_version=6;",
    )
    .unwrap();
    insert_legacy(&db, "project_policies", chat.as_str(), 1, &stale);
    drop(db);
    let store = ManagedStore::open(&state).unwrap();
    let db = store.db().unwrap();
    assert_eq!(user_version(&db), 7);
    assert!(has_column(&db, "project_policies", "workspace"));
    assert!(!has_column(&db, "project_policies", "conversation"));
    assert!(has_table(&db, "project_policies_v6"));
    drop(db);
    assert!(store.project_policy_in(text(&work)).unwrap().is_none());
    let rows = conflicts(&store);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].kind, "grant");
    assert_eq!(rows[0].disposition, "dropped");
    assert_eq!(rows[0].payload, stale);
    assert_eq!(rows[0].conversation, chat.as_str());
}

#[tokio::test]
async fn v7_upgrade_blocked_by_live_supervisor_without_mutation() {
    let l = legacy_store().await;
    let managed_root = l.state.join("managed");
    let guard = managed_migration_guard(&managed_root).unwrap();
    let started = Instant::now();
    assert!(matches!(
        ManagedStore::open(&l.state),
        Err(Error::Conflict(_))
    ));
    assert!(started.elapsed() >= workspace::MIGRATION_GUARD_WAIT);
    let db = raw(&l.state);
    assert_eq!(user_version(&db), 6);
    assert!(!has_table(&db, "workspaces"));
    assert!(!has_column(&db, "tasks", "workspace"));
    assert!(has_column(&db, "project_policies", "conversation"));
    drop(db);
    let backups = |root: &Path| {
        fs::read_dir(root)
            .unwrap()
            .flatten()
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("managed.pre-v7.")
            })
            .count()
    };
    assert_eq!(backups(&managed_root), 0);
    drop(guard);
    let store = ManagedStore::open(&l.state).unwrap();
    assert_eq!(user_version(&store.db().unwrap()), 7);
    // The upgrade that ran made its one downgrade path first.
    assert_eq!(backups(&managed_root), 1);
}

#[tokio::test]
async fn upgrade_guard_loser_waits_then_opens_migrated_store() {
    let l = legacy_store().await;
    let guard = managed_migration_guard(&l.state.join("managed")).unwrap();
    let state = l.state.clone();
    let opener = std::thread::spawn(move || {
        let started = Instant::now();
        let result = ManagedStore::open(&state).map(|store| store.root().to_owned());
        (result, started.elapsed())
    });
    std::thread::sleep(Duration::from_millis(300));
    // The winner upgrades while it keeps holding the lock, as a new
    // supervisor does for its whole life.
    workspace::migrate_v7(&mut raw(&l.state), now_ms()).unwrap();
    let (result, waited) = opener.join().unwrap();
    result.unwrap();
    assert!(waited < workspace::MIGRATION_GUARD_WAIT);
    drop(guard);
    assert_eq!(user_version(&raw(&l.state)), 7);
}

#[tokio::test]
async fn store_at_version_8_is_refused() {
    let e = env();
    let state = e.state.clone();
    drop(e.managed);
    raw(&state).execute_batch("PRAGMA user_version=8;").unwrap();
    assert!(matches!(
        ManagedStore::open(&state),
        Err(Error::Unavailable(
            "managed state was written by a newer xcb"
        ))
    ));
}

#[tokio::test]
async fn registry_backfill_is_db_only_and_skips_relative_and_global_scopes() {
    let l = legacy_store().await;
    let ghost = "/nonexistent-xcb-backfill/abs";
    let db = raw(&l.state);
    let conversation = ManagedConversation {
        version: 1,
        id: Id::new("c_ghost").unwrap(),
        title: "ghost".into(),
        workspace: Some(ghost.into()),
        created_at_ms: 5,
        updated_at_ms: 5,
    };
    db.execute(
        "INSERT INTO conversations(id,updated_at,payload) VALUES('c_ghost',5,?1)",
        [serde_json::to_string(&conversation).unwrap()],
    )
    .unwrap();
    db.execute_batch(
        "INSERT INTO route_stats(scope,provider,completed,failed) VALUES('relative/x','codex',1,0),('/abs/route','codex',1,0);
         INSERT INTO preferences(id,scope,created_at,payload) VALUES('p_global','global',3,'{}'),('p_abs','/abs/pref',7,'{}');",
    )
    .unwrap();
    drop(db);
    let store = ManagedStore::open(&l.state).unwrap();
    assert!(!Path::new(ghost).exists());
    let rows: BTreeMap<String, (String, i64)> = {
        let db = store.db().unwrap();
        let mut query = db
            .prepare("SELECT path,admitted_by,task_count FROM workspaces")
            .unwrap();
        query
            .query_map([], |row| Ok((row.get(0)?, (row.get(1)?, row.get(2)?))))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    };
    for path in [text(&l.work), ghost, "/abs/route", "/abs/pref"] {
        assert_eq!(
            rows.get(path).map(|row| row.0.as_str()),
            Some("history"),
            "{path}"
        );
    }
    assert_eq!(rows[text(&l.work)].1, 1);
    assert!(!rows.contains_key("relative/x"));
    assert!(!rows.contains_key("global"));
    // Validity is checked on use: the missing directory is never offered.
    assert!(
        store
            .known_workspaces(256)
            .unwrap()
            .iter()
            .all(|entry| entry.path != ghost)
    );
    let listed = store.all_workspaces().unwrap();
    assert_eq!(
        listed.iter().find(|row| row.path == ghost).unwrap().status,
        "invalid"
    );
}

#[tokio::test]
async fn migrate_copy_leaves_source_at_6() {
    let l = legacy_store().await;
    let grant = policy(&l.work, true, 0, 1);
    insert_legacy(
        &raw(&l.state),
        "project_policies",
        l.first.as_str(),
        1,
        &legacy(&grant, l.first.as_str()),
    );
    let scratch = l.base.join("scratch");
    let report = ManagedStore::migrate_copy(&l.state, &scratch).unwrap();
    assert_eq!(report.from_version, 6);
    assert_eq!(report.to_version, 7);
    assert_eq!(report.conversations, 2);
    assert_eq!(report.tasks, 1);
    assert_eq!(report.grants, 1);
    assert_eq!(report.conflicts.len(), 1);
    assert_eq!(report.conflicts[0].disposition, "moved");
    let source = raw(&l.state);
    assert_eq!(user_version(&source), 6);
    assert!(has_column(&source, "project_policies", "conversation"));
    assert!(!has_table(&source, "workspaces"));
    assert_eq!(user_version(&raw(&scratch)), 7);
    // A second preview never overwrites the first.
    assert!(ManagedStore::migrate_copy(&l.state, &scratch).is_err());
}

#[tokio::test]
async fn pre_v7_task_receipts_verify_after_upgrade() {
    let l = legacy_store().await;
    let store = ManagedStore::open(&l.state).unwrap();
    store.verify_task(&l.queued.id).await.unwrap();
    let task = store.task(&l.queued.id).unwrap().unwrap();
    assert!(task.binding.is_none());
    assert_eq!(task.workspace, text(&l.work));
    assert_eq!(
        store.backlog_in(text(&l.work), 256).unwrap()[0].id,
        l.queued.id
    );
}

// ---------------------------------------------------------------------------
// Validation and the registry

#[test]
fn validate_rejects_root_home_home_ancestors_state_coordination_and_system_dirs() {
    let e = env();
    let root = e.managed.root().to_owned();
    refused(&root, Path::new("/"), "filesystem root");
    let home = home();
    refused(&root, &home, "home");
    refused(&root, home.parent().unwrap(), "home");
    refused(&root, &root, "xcb state");
    refused(&root, &e.state, "xcb state");
    // Containing the state root is as bad as lying inside it.
    refused(&root, &e.base, "xcb state");
    refused(&root, &dir(&e.state, "attachments"), "xcb state");
    refused(&root, &dir(&e.state, "input-recovery/x"), "xcb state");
    refused(&root, &dir(&root, "inner"), "xcb state");
    if let Ok(coordination) = crate::coordination::default_root()
        && coordination.is_dir()
    {
        assert!(validate_workspace_root(&root, &coordination).is_err());
    }
    for system in ["/usr/bin", "/etc", "/sbin"] {
        if Path::new(system).is_dir() {
            refused(&root, Path::new(system), "system directory");
        }
    }
    // These hold the test's own tempdir on some hosts, so the state check
    // may answer first; either way they are refused.
    for exact in ["/tmp", "/var"] {
        if Path::new(exact).is_dir() {
            assert!(validate_workspace_root(&root, Path::new(exact)).is_err());
        }
    }
}

#[test]
fn validate_rejects_hidden_home_dirs_and_library() {
    let e = env();
    let root = e.managed.root();
    let home = home();
    for existing in [".ssh", ".aws", ".config"] {
        if home.join(existing).is_dir() {
            refused(root, &home.join(existing), "hidden or library directory");
        }
    }
    if let Ok(hidden) = tempfile::Builder::new()
        .prefix(".xcb-validate-")
        .tempdir_in(&home)
    {
        let inside = dir(hidden.path(), "x");
        refused(root, hidden.path(), "hidden or library directory");
        refused(root, &inside, "hidden or library directory");
    }
    let library = home.join("Library");
    if library.is_dir()
        && let Ok(inside) = tempfile::Builder::new()
            .prefix("xcb-validate-")
            .tempdir_in(&library)
    {
        refused(root, &library, "hidden or library directory");
        refused(
            root,
            &dir(inside.path(), "y"),
            "hidden or library directory",
        );
    }
}

#[test]
fn validate_accepts_tempdir_children() {
    let e = env();
    let work = dir(&e.base, "work/nested");
    assert_eq!(
        validate_workspace_root(e.managed.root(), &work).unwrap(),
        text(&work)
    );
    // A non-canonical spelling validates to the canonical bytes.
    let spelled = e.base.join("work/nested/../nested");
    assert_eq!(e.managed.validate_workspace(&spelled).unwrap(), text(&work));
}

#[test]
fn snap_stops_at_nested_worktree_not_outer_admitted_repo() {
    let e = env();
    let repo = git_repo(&e.base, "repo");
    e.managed.admit_workspace(&repo, "command", None).unwrap();
    let worktree = dir(&repo, ".claude/worktrees/x");
    fs::write(
        worktree.join(".git"),
        "gitdir: /elsewhere/.git/worktrees/x\n",
    )
    .unwrap();
    let src = dir(&worktree, "src");
    assert_eq!(e.managed.snap_root(&src).unwrap(), text(&worktree));
    let sub = dir(&repo, "sub/deeper");
    assert_eq!(e.managed.snap_root(&sub).unwrap(), text(&repo));
    // An admitted non-repository directory also stops the walk.
    let plain = dir(&e.base, "plain");
    e.managed.admit_workspace(&plain, "command", None).unwrap();
    assert_eq!(
        e.managed.snap_root(&dir(&plain, "a/b")).unwrap(),
        text(&plain)
    );
}

#[test]
fn file_token_snaps_to_parent_dir() {
    let e = env();
    let repo = git_repo(&e.base, "repo");
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::write(repo.join("src/x.rs"), "fn main() {}\n").unwrap();
    let token = format!("{}/src/x.rs:12:3", text(&repo));
    assert_eq!(e.managed.snap_root(Path::new(&token)).unwrap(), text(&repo));
    let plain = dir(&e.base, "plain");
    fs::write(plain.join("a.txt"), "x").unwrap();
    let token = format!("{}/a.txt:5", text(&plain));
    assert_eq!(
        e.managed.snap_root(Path::new(&token)).unwrap(),
        text(&plain)
    );
}

#[test]
fn subdirectory_admission_never_makes_git_toplevel_a_container() {
    let e = env();
    let repo = git_repo(&e.base, "repo");
    e.managed.admit_workspace(&repo, "command", None).unwrap();
    let site = dir(&repo, "site");
    e.managed.admit_workspace(&site, "command", None).unwrap();
    let docs = dir(&e.base, "docs");
    e.managed.admit_workspace(&docs, "command", None).unwrap();
    e.managed
        .admit_workspace(&dir(&docs, "a"), "command", None)
        .unwrap();
    let known = e.managed.known_workspaces(256).unwrap();
    let container = |path: &Path| {
        known
            .iter()
            .find(|entry| entry.path == text(path))
            .unwrap()
            .container
    };
    assert!(!container(&repo));
    assert!(!container(&site));
    assert!(container(&docs));
    let listed = e.managed.all_workspaces().unwrap();
    let status = |path: &Path| {
        listed
            .iter()
            .find(|row| row.path == text(path))
            .unwrap()
            .status
    };
    assert_eq!(status(&repo), "ok");
    assert_eq!(status(&docs), "container");
}

#[test]
fn launch_from_container_admits_nothing() {
    let e = env();
    let documents = dir(&e.base, "documents");
    e.managed
        .admit_workspace(&dir(&documents, "project"), "command", None)
        .unwrap();
    assert!(matches!(
        e.managed.admit_workspace(&documents, "launch", None),
        Err(Error::Conflict(_))
    ));
    assert!(
        e.managed
            .all_workspaces()
            .unwrap()
            .iter()
            .all(|row| row.path != text(&documents))
    );
    // Only a human act admits a container, and it is still never inferred.
    e.managed
        .admit_workspace(&documents, "command", None)
        .unwrap();
    let known = e.managed.known_workspaces(256).unwrap();
    let entry = known
        .iter()
        .find(|entry| entry.path == text(&documents))
        .unwrap();
    assert!(entry.container && entry.explicit_add);
    // `prompt` is not an admission.
    assert!(
        e.managed
            .admit_workspace(&documents, "prompt", None)
            .is_err()
    );
}

/// Before the registry knows any project, a directory of repositories
/// still looks like a container to a launch; a human can add it.
#[test]
fn launch_from_parent_of_repositories_admits_nothing_on_empty_registry() {
    let e = env();
    let documents = dir(&e.base, "documents");
    git_repo(&documents, "app");
    assert!(matches!(
        e.managed.admit_workspace(&documents, "launch", None),
        Err(Error::Conflict(_))
    ));
    assert!(!e.managed.launch_admissible(text(&documents)).unwrap());
    assert!(e.managed.all_workspaces().unwrap().is_empty());
    // A plain directory and a repository are still admitted by launch.
    let plain = dir(&e.base, "plain");
    e.managed.admit_workspace(&plain, "launch", None).unwrap();
    let app = documents.join("app");
    e.managed.admit_workspace(&app, "launch", None).unwrap();
    e.managed
        .admit_workspace(&documents, "command", None)
        .unwrap();
}

#[test]
fn known_workspaces_never_persists_hide_for_invalid_entry() {
    let e = env();
    let work = dir(&e.base, "work");
    e.managed.admit_workspace(&work, "command", None).unwrap();
    let away = e.base.join("away");
    fs::rename(&work, &away).unwrap();
    assert!(
        e.managed
            .known_workspaces(256)
            .unwrap()
            .iter()
            .all(|entry| entry.path != text(&work))
    );
    let listed = e.managed.all_workspaces().unwrap();
    assert_eq!(
        listed
            .iter()
            .find(|row| row.path == text(&work))
            .unwrap()
            .status,
        "invalid"
    );
    let hidden: i64 = e
        .managed
        .db()
        .unwrap()
        .query_row(
            "SELECT hidden FROM workspaces WHERE path=?1",
            [text(&work)],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(hidden, 0);
    fs::rename(&away, &work).unwrap();
    assert!(
        e.managed
            .known_workspaces(256)
            .unwrap()
            .iter()
            .any(|entry| entry.path == text(&work))
    );
}

#[test]
fn lookup_name_excludes_hidden_and_containers() {
    let e = env();
    let alpha = dir(&e.base, "alpha");
    e.managed.admit_workspace(&alpha, "command", None).unwrap();
    assert_eq!(e.managed.lookup_name("alpha").unwrap(), vec![text(&alpha)]);
    e.managed.hide_workspace(text(&alpha)).unwrap();
    assert!(e.managed.lookup_name("alpha").unwrap().is_empty());
    e.managed.show_workspace(text(&alpha)).unwrap();
    assert_eq!(e.managed.lookup_name("alpha").unwrap(), vec![text(&alpha)]);
    e.managed
        .db()
        .unwrap()
        .execute(
            "UPDATE workspaces SET repo='owner/gamma' WHERE path=?1",
            [text(&alpha)],
        )
        .unwrap();
    assert_eq!(e.managed.lookup_name("gamma").unwrap(), vec![text(&alpha)]);
    let holder = dir(&e.base, "box");
    let inner = dir(&holder, "inner");
    e.managed.admit_workspace(&holder, "command", None).unwrap();
    e.managed.admit_workspace(&inner, "command", None).unwrap();
    assert!(e.managed.lookup_name("box").unwrap().is_empty());
    assert_eq!(e.managed.lookup_name("inner").unwrap(), vec![text(&inner)]);
    assert!(e.managed.lookup_name("ALPHA").unwrap().is_empty());
}

#[tokio::test]
async fn resolve_scope_order_dir_alias_name_relative_error() {
    let e = env();
    let work = dir(&e.base, "work");
    let chat = e.managed.create_conversation(&work).await.unwrap().id;
    // 1. A value with a slash, `.` or `..` is a directory.
    assert_eq!(
        e.managed.resolve_scope("./work", &e.base).unwrap(),
        text(&work)
    );
    assert_eq!(e.managed.resolve_scope(".", &work).unwrap(), text(&work));
    // 2. A conversation id is a legacy alias for its directory.
    assert_eq!(
        e.managed.resolve_scope(chat.as_str(), &e.base).unwrap(),
        text(&work)
    );
    assert!(matches!(
        e.managed.resolve_scope(GLOBAL_THREAD_ID, &e.base),
        Err(Error::Conflict(_))
    ));
    // 3. A unique registry name beats 4. a same-named relative directory.
    let named = dir(&e.base, "named");
    e.managed
        .admit_workspace(&named, "command", Some("shadow"))
        .unwrap();
    dir(&e.base, "shadow");
    assert_eq!(
        e.managed.resolve_scope("shadow", &e.base).unwrap(),
        text(&named)
    );
    let relative = dir(&e.base, "relative");
    assert_eq!(
        e.managed.resolve_scope("relative", &e.base).unwrap(),
        text(&relative)
    );
    // 5. Anything else names the candidates.
    match e.managed.resolve_scope("nowhere", &e.base) {
        Err(Error::Guided { message, .. }) => assert!(message.contains("shadow"), "{message}"),
        other => panic!("unexpected scope: {other:?}"),
    }
    assert!(e.managed.resolve_scope("../..", &e.base).is_err());
}

#[tokio::test]
async fn view_stamp_changes_after_workspace_add_and_hide() {
    let e = env();
    let work = dir(&e.base, "work");
    let chat = e.managed.create_conversation(&work).await.unwrap().id;
    let before = e.managed.view_stamp(&e.state, &chat).unwrap();
    let other = dir(&e.base, "other");
    e.managed.admit_workspace(&other, "command", None).unwrap();
    let added = e.managed.view_stamp(&e.state, &chat).unwrap();
    assert_ne!(before, added);
    e.managed.hide_workspace(text(&other)).unwrap();
    let hidden = e.managed.view_stamp(&e.state, &chat).unwrap();
    assert_ne!(added, hidden);
    e.managed.show_workspace(text(&other)).unwrap();
    assert_ne!(hidden, e.managed.view_stamp(&e.state, &chat).unwrap());
}

// ---------------------------------------------------------------------------
// The global thread

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn global_thread_is_singleton_under_concurrent_calls() {
    let e = env();
    let managed = std::sync::Arc::new(e.managed);
    let receipts = |managed: &ManagedStore| -> i64 {
        managed
            .db()
            .unwrap()
            .query_row(
                "SELECT count(*) FROM receipts WHERE task IS NULL",
                [],
                |row| row.get(0),
            )
            .unwrap()
    };
    let before = receipts(&managed);
    let calls: Vec<_> = (0..8)
        .map(|_| {
            let managed = managed.clone();
            tokio::spawn(async move { managed.global_thread().await.unwrap() })
        })
        .collect();
    for call in calls {
        let thread = call.await.unwrap();
        assert_eq!(thread.id.as_str(), GLOBAL_THREAD_ID);
        assert_eq!(thread.title, "Thread");
        assert!(thread.is_thread() && thread.workspace.is_none());
    }
    let rows: i64 = managed
        .db()
        .unwrap()
        .query_row(
            "SELECT count(*) FROM conversations WHERE id=?1",
            [GLOBAL_THREAD_ID],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(rows, 1);
    assert_eq!(receipts(&managed), before + 1);
}

#[tokio::test]
async fn thread_excluded_from_max_conversations_count() {
    let e = env();
    let work = dir(&e.base, "work");
    e.managed.global_thread().await.unwrap();
    {
        let mut db = e.managed.db().unwrap();
        let tx = db.transaction().unwrap();
        for index in 0..MAX_CONVERSATIONS - 1 {
            let conversation = ManagedConversation {
                version: 1,
                id: Id::new(format!("c_fill_{index}")).unwrap(),
                title: "fill".into(),
                workspace: Some(text(&work).into()),
                created_at_ms: 1,
                updated_at_ms: 1,
            };
            tx.execute(
                "INSERT INTO conversations(id,updated_at,payload) VALUES(?1,1,?2)",
                params![
                    conversation.id.as_str(),
                    serde_json::to_string(&conversation).unwrap()
                ],
            )
            .unwrap();
        }
        tx.commit().unwrap();
    }
    // 4095 views plus the thread: one more view still fits.
    e.managed.create_conversation(&work).await.unwrap();
    assert!(matches!(
        e.managed.create_conversation(&work).await,
        Err(Error::Core(xcb_core::Error::Limit(_)))
    ));
}

#[tokio::test]
async fn only_c_global_may_have_no_workspace() {
    let conversation = |id: &str, workspace: Option<&str>| ManagedConversation {
        version: 1,
        id: Id::new(id).unwrap(),
        title: "t".into(),
        workspace: workspace.map(str::to_owned),
        created_at_ms: 1,
        updated_at_ms: 1,
    };
    assert!(conversation(GLOBAL_THREAD_ID, None).validate().is_ok());
    assert!(
        conversation(GLOBAL_THREAD_ID, Some("/w"))
            .validate()
            .is_err()
    );
    assert!(conversation("c_view", None).validate().is_err());
    assert!(conversation("c_view", Some("relative")).validate().is_err());
    assert!(conversation("c_view", Some("/w")).validate().is_ok());
    let long = format!("/{}", "w".repeat(4096));
    assert!(conversation("c_view", Some(&long)).validate().is_err());
    // Conversation-keyed entry points refuse the thread until a directory
    // is named.
    let e = env();
    let thread = e.managed.global_thread().await.unwrap().id;
    for result in [
        e.managed
            .enqueue_backlog(&thread, new_id("m"), "x".into(), true, 5)
            .await
            .map(drop),
        e.managed.project_policy(&thread).map(drop),
        e.managed.memory_binding(&thread).map(drop),
    ] {
        assert!(
            matches!(result, Err(Error::Conflict(workspace::THREAD_SPANS))),
            "{result:?}"
        );
    }
}

#[tokio::test]
async fn latest_conversation_for_workspace_never_returns_thread() {
    let e = env();
    let work = dir(&e.base, "work");
    let view = e.managed.create_conversation(&work).await.unwrap();
    e.managed.global_thread().await.unwrap();
    // Even a corrupted thread row naming the directory is never the view.
    e.managed
        .db()
        .unwrap()
        .execute(
            "UPDATE conversations SET updated_at=?1,payload=json_set(payload,'$.workspace',?2) WHERE id=?3",
            params![i64::MAX, text(&work), GLOBAL_THREAD_ID],
        )
        .unwrap();
    let latest = e
        .managed
        .latest_conversation_for_workspace(&work)
        .unwrap()
        .unwrap();
    assert_eq!(latest.id, view.id);
}

#[tokio::test]
async fn legacy_conversation_payload_decodes_and_thread_payload_omits_workspace() {
    let legacy: ManagedConversation = serde_json::from_value(json!({
        "version": 1, "id": "c_old", "title": "old", "workspace": "/w",
        "created_at_ms": 1, "updated_at_ms": 2
    }))
    .unwrap();
    assert_eq!(legacy.workspace.as_deref(), Some("/w"));
    assert_eq!(legacy.workspace_path(), Some(Path::new("/w")));
    assert!(!legacy.is_thread());
    let thread = ManagedConversation {
        version: 1,
        id: Id::new(GLOBAL_THREAD_ID).unwrap(),
        title: "Thread".into(),
        workspace: None,
        created_at_ms: 1,
        updated_at_ms: 1,
    };
    let value = serde_json::to_value(&thread).unwrap();
    assert!(value.get("workspace").is_none());
    let back: ManagedConversation = serde_json::from_value(value).unwrap();
    assert!(back.is_thread());
    // A view task's payload carries no binding fields, as before 0.9.0.
    let e = env();
    let work = dir(&e.base, "work");
    let view = e.managed.create_conversation(&work).await.unwrap().id;
    let task = e
        .managed
        .enqueue_backlog(&view, new_id("m"), "Old shape".into(), true, 5)
        .await
        .unwrap();
    let mut task = serde_json::to_value(task).unwrap();
    for field in ["binding", "hold_until_ms", "moved_from"] {
        assert!(task.get(field).is_none());
    }
    task["binding"] = json!({"source":"explicit","confidence":"high","reason":"x"});
    assert!(
        serde_json::from_value::<ManagedTask>(task).is_err(),
        "origin is required"
    );
}

#[tokio::test]
async fn thread_accepts_tasks_in_two_workspaces_via_submit_to_thread_explicit() {
    let e = env();
    let a = dir(&e.base, "a");
    let b = dir(&e.base, "b");
    let (first, workspace, binding) = accepted(
        e.managed
            .submit_to_thread(new_id("m"), "Fix the parser".into(), vec![], explicit(&a))
            .await
            .unwrap(),
    );
    assert_eq!(workspace, text(&a));
    assert_eq!(first.conversation.as_str(), GLOBAL_THREAD_ID);
    assert_eq!(binding.source, BindingSource::Explicit);
    assert_eq!(binding.confidence, BindingConfidence::High);
    assert_eq!(binding.origin, BindingOrigin::Cli);
    assert_eq!(first.hold_until_ms, None);
    assert_eq!(first.binding.as_ref(), Some(&binding));
    let (second, workspace, _) = accepted(
        e.managed
            .submit_to_thread(new_id("m"), "Ship the site".into(), vec![], explicit(&b))
            .await
            .unwrap(),
    );
    assert_eq!(workspace, text(&b));
    assert_eq!(second.conversation.as_str(), GLOBAL_THREAD_ID);
    for (path, task) in [(&a, &first), (&b, &second)] {
        let outstanding =
            project::outstanding_in(&e.managed.db().unwrap(), text(path), None).unwrap();
        assert_eq!(outstanding.len(), 1);
        assert_eq!(outstanding[0].id, task.id);
    }
    e.managed.verify_task(&first.id).await.unwrap();
    // Each directory entered the registry when its task committed.
    let known = e.managed.known_workspaces(256).unwrap();
    assert!(known.iter().any(|entry| entry.path == text(&a)));
    assert!(known.iter().any(|entry| entry.path == text(&b)));
    let ack = e
        .managed
        .messages(&thread(), 16)
        .unwrap()
        .into_iter()
        .map(|message| message.text)
        .find(|text| text.starts_with("Started **Fix the parser**"))
        .unwrap();
    assert!(
        ack.contains("in `a` · named directory · /workspace to move"),
        "{ack}"
    );
}

#[tokio::test]
async fn explicit_workspace_is_validated_not_snapped() {
    let e = env();
    let repo = git_repo(&e.base, "repo");
    e.managed.admit_workspace(&repo, "command", None).unwrap();
    let sub = dir(&repo, "sub");
    let (_, workspace, _) = accepted(
        e.managed
            .submit_to_thread(new_id("m"), "Work here".into(), vec![], explicit(&sub))
            .await
            .unwrap(),
    );
    assert_eq!(workspace, text(&sub));
    // Validation still applies to an explicit directory.
    let home = home();
    assert!(
        e.managed
            .submit_to_thread(new_id("m"), "Not here".into(), vec![], explicit(&home))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn submit_to_thread_retry_replays_after_registry_change() {
    let e = env();
    let a = dir(&e.base, "a");
    let b = dir(&e.base, "b");
    e.managed.admit_workspace(&a, "command", None).unwrap();
    e.managed.admit_workspace(&b, "command", None).unwrap();
    let message = new_id("m");
    let focus = |path: &Path| IntakeCues {
        focus: Some(text(path).into()),
        ..cues(Origin::Tui)
    };
    let (task, workspace, binding) = accepted(
        e.managed
            .submit_to_thread(message.clone(), "Run the tests".into(), vec![], focus(&a))
            .await
            .unwrap(),
    );
    assert_eq!(workspace, text(&a));
    assert_eq!(binding.source, BindingSource::Focus);
    assert_eq!(binding.origin, BindingOrigin::Tui);
    e.managed.hide_workspace(text(&a)).unwrap();
    let (replayed, workspace, saved) = accepted(
        e.managed
            .submit_to_thread(message.clone(), "Run the tests".into(), vec![], focus(&b))
            .await
            .unwrap(),
    );
    assert_eq!(replayed.id, task.id);
    assert_eq!(workspace, text(&a));
    assert_eq!(saved, binding);
    match e
        .managed
        .resolve_intake(&thread(), &message, "Run the tests", &focus(&b))
        .unwrap()
    {
        Resolution::Bound { workspace, .. } => assert_eq!(workspace, text(&a)),
        other => panic!("unexpected resolution: {other:?}"),
    }
    let tasks: i64 = e
        .managed
        .db()
        .unwrap()
        .query_row("SELECT count(*) FROM tasks", [], |row| row.get(0))
        .unwrap();
    assert_eq!(tasks, 1);
}

#[tokio::test]
async fn replayed_message_id_with_different_text_conflicts() {
    let e = env();
    let a = dir(&e.base, "a");
    let message = new_id("m");
    e.managed
        .submit_to_thread(message.clone(), "First text".into(), vec![], explicit(&a))
        .await
        .unwrap();
    let conflict = Error::Conflict("message id was reused with different input");
    for result in [
        e.managed
            .submit_to_thread(message.clone(), "Second text".into(), vec![], explicit(&a))
            .await
            .map(drop),
        e.managed
            .resolve_intake(&thread(), &message, "Second text", &explicit(&a))
            .map(drop),
    ] {
        assert_eq!(
            format!("{result:?}"),
            format!("{:?}", Err::<(), _>(&conflict))
        );
    }
    // A message id already used by a view is not a thread submission.
    let work = dir(&e.base, "work");
    let view = e.managed.create_conversation(&work).await.unwrap().id;
    let used = new_id("m");
    e.managed
        .enqueue_backlog(&view, used.clone(), "View work".into(), true, 5)
        .await
        .unwrap();
    assert!(
        e.managed
            .submit_to_thread(used, "View work".into(), vec![], explicit(&a))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn view_conversation_still_rejects_foreign_workspace() {
    let e = env();
    let work = dir(&e.base, "work");
    let other = dir(&e.base, "other");
    let view = e.managed.create_conversation(&work).await.unwrap().id;
    assert!(matches!(
        e.managed
            .create_task(&view, new_id("m"), "Elsewhere".into(), vec![], &other)
            .await,
        Err(Error::Conflict("managed conversation workspace changed"))
    ));
    let task = e
        .managed
        .create_task(&view, new_id("m"), "Here".into(), vec![], &work)
        .await
        .unwrap();
    assert!(task.binding.is_none());
    assert_eq!(task.workspace, text(&work));
    // A view rooted in a refused directory cannot be created.
    assert!(e.managed.create_conversation(&home()).await.is_err());
}
