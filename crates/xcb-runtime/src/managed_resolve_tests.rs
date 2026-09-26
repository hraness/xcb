use super::*;

struct Fixture {
    _directory: tempfile::TempDir,
    managed: ManagedStore,
    conversation: ManagedConversation,
    workspace: PathBuf,
}

async fn fixture() -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let state = private::directory(&root.join("state")).unwrap();
    let workspace = private::directory(&root.join("workspace")).unwrap();
    let managed = ManagedStore::open(&state).unwrap();
    let conversation = managed.create_conversation(&workspace).await.unwrap();
    Fixture {
        _directory: directory,
        managed,
        conversation,
        workspace,
    }
}

async fn task(fixture: &Fixture, id: &str, title: &str) -> ManagedTask {
    fixture
        .managed
        .create_task(
            &fixture.conversation.id,
            Id::new(id).unwrap(),
            title.to_string(),
            vec![],
            &fixture.workspace,
        )
        .await
        .unwrap()
}

fn prefix(id: &Id, chars: usize) -> Id {
    Id::new(&id.as_str()[..chars]).unwrap()
}

#[tokio::test]
async fn resolve_task_returns_the_exact_id_unchanged() {
    let fixture = fixture().await;
    let task = task(&fixture, "m_exact", "Exact").await;
    assert_eq!(fixture.managed.resolve_task(&task.id).unwrap(), task.id,);
}

#[tokio::test]
async fn resolve_task_expands_a_unique_prefix() {
    let fixture = fixture().await;
    let task = task(&fixture, "m_unique", "Unique").await;
    assert_eq!(
        fixture.managed.resolve_task(&prefix(&task.id, 8)).unwrap(),
        task.id,
    );
}

#[tokio::test]
async fn resolve_task_rejects_an_ambiguous_prefix_and_an_unknown_id() {
    let fixture = fixture().await;
    task(&fixture, "m_first", "First").await;
    task(&fixture, "m_second", "Second").await;
    // `t_` prefixes every derived task id, so two tasks make it ambiguous.
    let ambiguous = fixture
        .managed
        .resolve_task(&Id::new("t_").unwrap())
        .unwrap_err();
    assert!(ambiguous.to_string().contains("ambiguous"), "{ambiguous}");
    let missing = fixture
        .managed
        .resolve_task(&Id::new("t_absent").unwrap())
        .unwrap_err();
    assert!(missing.to_string().contains("not found"), "{missing}");
}

#[tokio::test]
async fn resolve_task_prefix_respects_id_wildcards() {
    let fixture = fixture().await;
    let task = task(&fixture, "m_scored", "Scored").await;
    // `_` is legal in ids; an escaped prefix must not act as a wildcard.
    let error = fixture
        .managed
        .resolve_task(&Id::new(format!("{}_", &task.id.as_str()[..8])).unwrap())
        .unwrap_err();
    assert!(error.to_string().contains("not found"), "{error}");
}

#[tokio::test]
async fn resolve_conversation_expands_a_unique_prefix() {
    let fixture = fixture().await;
    assert_eq!(
        fixture
            .managed
            .resolve_conversation(&prefix(&fixture.conversation.id, 8))
            .unwrap(),
        fixture.conversation.id,
    );
}

#[tokio::test]
async fn resolve_schedule_reports_missing_and_resolves_unique_prefixes() {
    let fixture = fixture().await;
    let schedule = fixture
        .managed
        .create_schedule(
            &fixture.conversation.id,
            "nightly".to_string(),
            60_000,
            now_ms() + 60_000,
        )
        .await
        .unwrap();
    assert_eq!(
        fixture
            .managed
            .resolve_schedule(&prefix(&schedule.id, 6))
            .unwrap(),
        schedule.id,
    );
    let error = fixture
        .managed
        .resolve_schedule(&Id::new("s_absent").unwrap())
        .unwrap_err();
    assert!(error.to_string().contains("not found"), "{error}");
}
