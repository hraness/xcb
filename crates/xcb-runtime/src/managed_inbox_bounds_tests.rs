use super::*;

struct Fixture {
    _root: tempfile::TempDir,
    managed: ManagedStore,
    conversation: Id,
    workspace: PathBuf,
}

async fn fixture() -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let base = root.path().canonicalize().unwrap();
    let workspace = private::directory(&base.join("workspace")).unwrap();
    let managed = ManagedStore::open(&base.join("state")).unwrap();
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
        .enqueue_backlog(&f.conversation, new_id("m"), prompt.into(), deferred, 5)
        .await
        .unwrap()
}

fn inbox(f: &Fixture, task: &ManagedTask) -> Vec<InboxEvent> {
    f.managed.inbox(Some(&task.id), None, None, 256).unwrap()
}

#[tokio::test]
async fn steering_identity_replay_cannot_change_input_or_target_and_survives_closure() {
    let f = fixture().await;
    let first = enqueue(&f, "First held task", true).await;
    let other = enqueue(&f, "Other held task", true).await;
    let id = new_id("steer");
    let event = f
        .managed
        .steer_task(&first.id, id.clone(), "Preserve the API".into())
        .unwrap();
    assert_eq!(
        f.managed
            .steer_task(&first.id, id.clone(), event.text.clone())
            .unwrap(),
        event
    );
    assert!(
        f.managed
            .steer_task(&first.id, id.clone(), "Changed".into())
            .is_err()
    );
    assert!(
        f.managed
            .steer_task(&other.id, id.clone(), event.text.clone())
            .is_err()
    );
    f.managed
        .complete_backlog(&first.id, first.revision, "Already verified".into())
        .await
        .unwrap();
    let replay = f
        .managed
        .steer_task(&first.id, id, event.text.clone())
        .unwrap();
    assert_eq!(replay.id, event.id);
    assert_eq!(replay.status, "closed");
    assert!(replay.receipt.is_none());
    assert!(
        f.managed
            .steer_task(&first.id, new_id("late"), "New work".into())
            .is_err()
    );
    assert_eq!(inbox(&f, &first).len(), 1);
    assert!(inbox(&f, &other).is_empty());
}

#[tokio::test]
async fn global_cursor_pages_interleaved_targets_without_duplicates_and_filters_projects() {
    let f = fixture().await;
    let left = enqueue(&f, "Left task", true).await;
    let right = enqueue(&f, "Right task", true).await;
    let second = f.managed.create_conversation(&f.workspace).await.unwrap();
    let elsewhere = f
        .managed
        .enqueue_backlog(&second.id, new_id("m"), "Other project".into(), true, 5)
        .await
        .unwrap();
    for (index, target) in [&left, &right, &elsewhere, &left, &right]
        .iter()
        .enumerate()
    {
        f.managed
            .steer_task(&target.id, new_id("e"), format!("Guidance {index}"))
            .unwrap();
    }
    let all = f.managed.inbox(None, None, None, 256).unwrap();
    assert_eq!(all.len(), 5);
    assert!(
        all.windows(2)
            .all(|rows| rows[0].sequence > rows[1].sequence)
    );
    let first = f.managed.inbox(None, None, None, 2).unwrap();
    let mut pages = first.clone();
    pages.extend(
        f.managed
            .inbox(None, None, Some(first.last().unwrap().sequence), 256)
            .unwrap(),
    );
    assert_eq!(pages, all);
    let filtered = f
        .managed
        .inbox(None, Some(&f.conversation), None, 256)
        .unwrap();
    assert_eq!(filtered.len(), 4);
    assert!(
        filtered
            .iter()
            .all(|row| row.conversation == f.conversation)
    );
    let left_rows = inbox(&f, &left);
    assert_eq!(left_rows.len(), 2);
    assert_eq!(
        f.managed
            .inbox(Some(&left.id), None, Some(left_rows[0].sequence), 1)
            .unwrap(),
        left_rows[1..]
    );
    assert!(f.managed.inbox(None, None, None, 0).is_err());
    assert!(f.managed.inbox(None, None, None, 257).is_err());
}

#[tokio::test]
async fn steering_does_not_release_deferred_work_or_answer_approval() {
    let f = fixture().await;
    let held = enqueue(&f, "Wait for release", true).await;
    let event = f
        .managed
        .steer_task(&held.id, new_id("e"), "Additional context".into())
        .unwrap();
    assert_eq!(event.status, "held");
    assert!(event.receipt.is_none());
    let saved = f.managed.task(&held.id).unwrap().unwrap();
    assert!(saved.deferred);
    assert_eq!(saved.revision, held.revision);
    assert_eq!(saved.attempts, 0);
    assert!(saved.session.is_none());

    let ready = enqueue(&f, "Needs explicit approval", false).await;
    let mut next = ready.clone();
    next.state = TaskState::NeedsInput;
    next.attention = Some(State::NeedsApproval);
    next.revision += 1;
    next.updated_at_ms = now_ms().max(ready.updated_at_ms);
    let awaiting = f.managed.transition(&ready, next, None).await.unwrap();
    let event = f
        .managed
        .steer_task(&awaiting.id, new_id("e"), "Proceed if possible".into())
        .unwrap();
    assert_eq!(event.status, "held");
    let saved = f.managed.task(&awaiting.id).unwrap().unwrap();
    assert_eq!(saved.state, TaskState::NeedsInput);
    assert_eq!(saved.attention, Some(State::NeedsApproval));
    assert_eq!(saved.revision, awaiting.revision);
    assert_eq!(saved.attempts, awaiting.attempts);
}

#[tokio::test]
async fn watches_reject_other_projects_and_immediately_report_an_already_completed_source() {
    let f = fixture().await;
    let target = enqueue(&f, "Target", true).await;
    let source = enqueue(&f, "Source", true).await;
    let other_conversation = f.managed.create_conversation(&f.workspace).await.unwrap();
    let other = f
        .managed
        .enqueue_backlog(
            &other_conversation.id,
            new_id("m"),
            "Other project".into(),
            true,
            5,
        )
        .await
        .unwrap();
    assert!(
        f.managed
            .watch_task(&target.id, &target.id, new_id("w"))
            .is_err()
    );
    assert!(
        f.managed
            .watch_task(&target.id, &other.id, new_id("w"))
            .is_err()
    );
    f.managed
        .complete_backlog(&source.id, source.revision, "Source already checked".into())
        .await
        .unwrap();
    let id = new_id("w");
    let watch = f
        .managed
        .watch_task(&target.id, &source.id, id.clone())
        .unwrap();
    assert_eq!(
        f.managed
            .watch_task(&target.id, &source.id, id.clone())
            .unwrap(),
        watch
    );
    assert!(f.managed.watch_task(&target.id, &other.id, id).is_err());
    let rows = inbox(&f, &target);
    assert_eq!(rows.len(), 1);
    assert_eq!(Some(&rows[0].id), watch.event.as_ref());
    assert_eq!(rows[0].status, "held");
    assert!(rows[0].text.contains("Source already checked"));
    assert!(rows[0].receipt.is_none());
}

#[tokio::test]
async fn reserved_watch_report_settles_even_when_target_event_capacity_is_full() {
    let f = fixture().await;
    let target = enqueue(&f, "Capacity target", true).await;
    let source = enqueue(&f, "Capacity source", true).await;
    let watch = f
        .managed
        .watch_task(&target.id, &source.id, new_id("w"))
        .unwrap();
    let reserved = inbox(&f, &target).pop().unwrap();
    assert_eq!(reserved.status, "waiting");
    assert_eq!(Some(&reserved.id), watch.event.as_ref());
    for index in 1..MAX_TASK_EVENTS {
        f.managed
            .steer_task(&target.id, new_id("e"), format!("Guidance {index}"))
            .unwrap();
    }
    assert!(
        f.managed
            .steer_task(&target.id, new_id("e"), "Overflow".into())
            .is_err()
    );
    let completed = f
        .managed
        .complete_backlog(&source.id, source.revision, "Evidence ".repeat(900))
        .await
        .unwrap();
    assert_eq!(completed.state, TaskState::Completed);
    let rows = inbox(&f, &target);
    assert_eq!(rows.len(), MAX_TASK_EVENTS as usize);
    let report = rows.iter().find(|row| row.id == reserved.id).unwrap();
    assert_eq!(report.sequence, reserved.sequence);
    assert_eq!(report.status, "held");
    assert!(report.text.contains(&completed.last_receipt));
    assert!(report.text.contains("Evidence "));
    assert!(report.text.len() <= 9216);
    assert!(report.receipt.is_none());
}

#[tokio::test]
async fn full_mailbox_body_and_escaped_steering_roundtrip_without_truncation() {
    let f = fixture().await;
    let target = enqueue(&f, "Mailbox target", true).await;
    let source = enqueue(&f, "Mailbox source", false).await;
    let source = f
        .managed
        .prepare(
            &source,
            new_id("s"),
            "claude/test".into(),
            "fixture".into(),
            0,
            String::new(),
        )
        .await
        .unwrap();
    let body = "\\\"".repeat(4096);
    assert_eq!(body.len(), 8192);
    let (message, effect) = f.managed.send_mailbox(
        &source,
        &target,
        source.session.as_ref().unwrap(),
        Provider::Claude,
        "full-body",
        body.clone(),
    );
    let message = message.unwrap();
    assert_eq!(effect, EffectState::Settled);
    let event = inbox(&f, &target).pop().unwrap();
    assert_eq!(event.kind, "message");
    assert!(event.text.ends_with(&body));
    assert_eq!(f.managed.mailbox(&target.id, 0, 1).unwrap()[0].body, body);
    let (replay, effect) = f.managed.send_mailbox(
        &source,
        &target,
        source.session.as_ref().unwrap(),
        Provider::Claude,
        "full-body",
        body.clone(),
    );
    assert_eq!(replay.unwrap().id, message.id);
    assert_eq!(effect, EffectState::Settled);
    assert_eq!(inbox(&f, &target).len(), 1);
    let steering = f
        .managed
        .steer_task(&target.id, new_id("e"), body.clone())
        .unwrap();
    assert_eq!(inbox(&f, &target)[0], steering);
    assert_eq!(steering.text, body);
    assert!(
        f.managed
            .steer_task(&target.id, new_id("e"), "x".repeat(8193))
            .is_err()
    );
}
