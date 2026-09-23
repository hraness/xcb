use super::*;

struct Fixture {
    _directory: tempfile::TempDir,
    managed: ManagedStore,
    source: ManagedTask,
    target: ManagedTask,
    alternate: ManagedTask,
}

async fn fixture() -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let state = private::directory(&root.join("state")).unwrap();
    let workspace = private::directory(&root.join("workspace")).unwrap();
    let managed = ManagedStore::open(&state).unwrap();
    let conversation = managed.create_conversation(&workspace).await.unwrap();
    let mut tasks = Vec::new();
    for name in ["source", "target", "alternate"] {
        tasks.push(
            managed
                .create_task(
                    &conversation.id,
                    Id::new(format!("m_{name}")).unwrap(),
                    format!("Implement {name}"),
                    vec![],
                    &workspace,
                )
                .await
                .unwrap(),
        );
    }
    let source = managed
        .prepare(
            &tasks[0],
            Id::new("s_source").unwrap(),
            "claude/test".into(),
            "fixture".into(),
            0,
            String::new(),
        )
        .await
        .unwrap();
    Fixture {
        _directory: directory,
        managed,
        source,
        target: tasks[1].clone(),
        alternate: tasks[2].clone(),
    }
}

fn send(
    fixture: &Fixture,
    target: &ManagedTask,
    call: &str,
    body: &str,
) -> (Result<MailboxMessage>, EffectState) {
    fixture.managed.send_mailbox(
        &fixture.source,
        target,
        fixture.source.session.as_ref().unwrap(),
        Provider::Claude,
        call,
        body.into(),
    )
}

#[tokio::test]
async fn replay_rejects_changed_arguments_but_preserves_a_confirmed_delivery() {
    let fixture = fixture().await;
    let (sent, effects) = send(&fixture, &fixture.target, "call", "Module found");
    let sent = sent.unwrap();
    assert_eq!(effects, EffectState::Settled);
    for (target, body) in [
        (&fixture.target, "Changed body"),
        (&fixture.alternate, "Module found"),
    ] {
        let (result, effects) = send(&fixture, target, "call", body);
        assert!(result.is_err());
        assert_eq!(effects, EffectState::None);
    }
    let mut completed = fixture.target.clone();
    completed.state = TaskState::Completed;
    completed.revision += 1;
    completed.updated_at_ms = now_ms();
    fixture
        .managed
        .transition(&fixture.target, completed, None)
        .await
        .unwrap();
    let (replayed, effects) = send(&fixture, &fixture.target, "call", "Module found");
    assert_eq!(replayed.unwrap().id, sent.id);
    assert_eq!(effects, EffectState::Settled);
    let (new_delivery, effects) = send(&fixture, &fixture.target, "new_call", "Too late");
    assert!(new_delivery.is_err());
    assert_eq!(effects, EffectState::None);
    assert_eq!(
        fixture
            .managed
            .mailbox(&fixture.target.id, 0, 64)
            .unwrap()
            .len(),
        1
    );
    assert!(
        fixture
            .managed
            .mailbox(&fixture.alternate.id, 0, 64)
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn stale_source_snapshot_cannot_send_after_the_next_turn_starts() {
    let fixture = fixture().await;
    fixture
        .managed
        .prepare(
            &fixture.source,
            fixture.source.session.clone().unwrap(),
            "claude/test".into(),
            "next turn".into(),
            2,
            String::new(),
        )
        .await
        .unwrap();
    let (result, effects) = send(&fixture, &fixture.target, "late_call", "Stale worker");
    assert!(result.is_err());
    assert_eq!(effects, EffectState::None);
    assert!(
        fixture
            .managed
            .mailbox(&fixture.target.id, 0, 64)
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn mailbox_listing_and_replay_reject_payload_index_mismatches() {
    let fixture = fixture().await;
    let message = send(&fixture, &fixture.target, "call", "Module found")
        .0
        .unwrap();
    let original = serde_json::to_value(&message).unwrap();
    for (field, value) in [
        ("id", json!("mb_wrong")),
        ("sourceTask", json!(fixture.alternate.id)),
        ("targetTask", json!(fixture.alternate.id)),
        ("sequence", json!(message.sequence + 1)),
        ("createdAtMs", json!(message.created_at_ms + 1)),
    ] {
        let mut corrupted = original.clone();
        corrupted[field] = value;
        fixture
            .managed
            .db()
            .unwrap()
            .execute(
                "UPDATE mailbox_messages SET payload=?1 WHERE id=?2",
                params![
                    serde_json::to_string(&corrupted).unwrap(),
                    message.id.as_str()
                ],
            )
            .unwrap();
        assert!(
            fixture.managed.mailbox(&fixture.target.id, 0, 64).is_err(),
            "{field}"
        );
        let (replayed, effects) = send(&fixture, &fixture.target, "call", "Module found");
        assert!(replayed.is_err(), "{field}");
        assert_eq!(effects, EffectState::None);
    }
}

#[tokio::test]
async fn replay_cannot_adopt_a_different_sender_identity() {
    let fixture = fixture().await;
    let message = send(&fixture, &fixture.target, "call", "Module found")
        .0
        .unwrap();
    let original = serde_json::to_value(&message).unwrap();
    for (field, value) in [
        ("sourceSession", json!("s_other")),
        ("sourceProvider", json!("codex")),
    ] {
        let mut corrupted = original.clone();
        corrupted[field] = value;
        fixture
            .managed
            .db()
            .unwrap()
            .execute(
                "UPDATE mailbox_messages SET payload=?1 WHERE id=?2",
                params![
                    serde_json::to_string(&corrupted).unwrap(),
                    message.id.as_str()
                ],
            )
            .unwrap();
        let (replayed, effects) = send(&fixture, &fixture.target, "call", "Module found");
        assert!(replayed.is_err(), "{field}");
        assert_eq!(effects, EffectState::None);
    }
}
