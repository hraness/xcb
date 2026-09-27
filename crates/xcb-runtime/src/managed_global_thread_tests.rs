//! The thread's intake: inference against a real store, moves and holds,
//! launch revalidation and overlap-aware serialization.
use super::*;
use crate::workspace_infer::{
    BindingConfidence, BindingOrigin, BindingSource, Resolution, WORKSPACE_HOLD_MS,
};
use xcb_core::ui::WorkspaceRow;

struct Env {
    _root: tempfile::TempDir,
    base: PathBuf,
    managed: Arc<ManagedStore>,
    xcb: Arc<Store>,
}

/// A store at `base/state`; project directories are its siblings.
fn env() -> Env {
    let root = tempfile::tempdir().unwrap();
    let base = root.path().canonicalize().unwrap();
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

fn dir(base: &Path, name: &str) -> PathBuf {
    fs::create_dir_all(base.join(name)).unwrap();
    base.join(name).canonicalize().unwrap()
}

fn git_repo(base: &Path, name: &str) -> PathBuf {
    let repo = dir(base, name);
    fs::create_dir_all(repo.join(".git")).unwrap();
    repo
}

fn text(path: &Path) -> &str {
    path.to_str().unwrap()
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

fn explicit(origin: Origin, path: &Path) -> IntakeCues {
    IntakeCues {
        explicit: Some(path.to_owned()),
        ..cues(origin)
    }
}

fn focused(path: &Path) -> IntakeCues {
    IntakeCues {
        focus: Some(text(path).into()),
        ..cues(Origin::Tui)
    }
}

fn infer() -> IntakeCues {
    IntakeCues {
        infer_only: true,
        ..cues(Origin::Relay)
    }
}

fn thread() -> Id {
    Id::new(GLOBAL_THREAD_ID).unwrap()
}

async fn submit(env: &Env, prompt: &str, cues: IntakeCues) -> Intake {
    env.managed
        .submit_to_thread(new_id("m"), prompt.into(), vec![], cues)
        .await
        .unwrap()
}

fn accepted(intake: Intake) -> ManagedTask {
    match intake {
        Intake::Accepted {
            task,
            workspace,
            binding,
            hold_until_ms,
        } => {
            assert_eq!(task.workspace, workspace);
            assert_eq!(task.binding.as_ref(), Some(&binding));
            assert_eq!(task.hold_until_ms, hold_until_ms);
            task
        }
        Intake::Ask { reason, .. } => panic!("asked: {reason}"),
    }
}

fn asked(intake: Intake) -> (Vec<WorkspaceRow>, String) {
    match intake {
        Intake::Ask { candidates, reason } => (candidates, reason),
        Intake::Accepted { workspace, .. } => panic!("bound {workspace}"),
    }
}

/// Row counts of every table an intake could write.
fn footprint(managed: &ManagedStore) -> [i64; 5] {
    let db = managed.db().unwrap();
    [
        "tasks",
        "messages",
        "conversations",
        "workspaces",
        "receipts",
    ]
    .map(|table| {
        db.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap()
    })
}

fn task(env: &Env, id: &Id) -> ManagedTask {
    env.managed.task(id).unwrap().unwrap()
}

/// Whether the last tick tried to launch this task. Without accounts an
/// attempted launch records the no-account detail and a backoff entry.
fn attempted(supervisor: &Supervisor, env: &Env, id: &Id) -> bool {
    let attempted = supervisor.launch_attempts.contains_key(id);
    assert_eq!(
        attempted,
        task(env, id).detail.starts_with(NO_ACCOUNT_DETAIL),
        "{}",
        task(env, id).detail
    );
    attempted
}

/// Rewrite a task payload in place, as another creator would have written it.
fn rewrite(env: &Env, id: &Id, edit: impl FnOnce(&mut Value)) {
    let db = env.managed.write_db().unwrap();
    let payload: String = db
        .query_row(
            "SELECT payload FROM tasks WHERE id=?1",
            [id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    let mut value: Value = serde_json::from_str(&payload).unwrap();
    edit(&mut value);
    db.execute(
        "UPDATE tasks SET payload=?1 WHERE id=?2",
        params![value.to_string(), id.as_str()],
    )
    .unwrap();
}

#[tokio::test]
async fn submit_to_thread_binds_two_workspaces_and_they_run_concurrently_same_workspace_serializes()
{
    let e = env();
    let a = dir(&e.base, "alpha");
    let b = dir(&e.base, "beta");
    let first = accepted(submit(&e, "Fix the parser", explicit(Origin::Cli, &a)).await);
    let second = accepted(submit(&e, "Ship the site", explicit(Origin::Cli, &b)).await);
    let third = accepted(submit(&e, "Then the lexer", explicit(Origin::Cli, &a)).await);
    for task in [&first, &second, &third] {
        assert_eq!(task.conversation, thread());
    }
    let mut supervisor = Supervisor::new(e.managed.clone(), e.xcb.clone());
    // A worker already runs in `alpha`.
    supervisor
        .active_workspaces
        .insert(new_id("t"), text(&a).into());
    supervisor.tick(false).await.unwrap();
    assert!(!attempted(&supervisor, &e, &first.id));
    assert!(attempted(&supervisor, &e, &second.id));
    assert!(!attempted(&supervisor, &e, &third.id));
}

#[tokio::test]
async fn nested_workspaces_serialize_in_tick_and_workspace_busy() {
    use xcb_core::models::{Mode, ModelChoice};
    let e = env();
    let repo = git_repo(&e.base, "r");
    let sub = dir(&repo, "sub");
    let sibling = dir(&e.base, "r2");
    let inner = accepted(submit(&e, "Inner work", explicit(Origin::Cli, &sub)).await);
    let outer = accepted(submit(&e, "Outer work", explicit(Origin::Cli, &repo)).await);
    let beside = accepted(submit(&e, "Sibling work", explicit(Origin::Cli, &sibling)).await);
    let mut supervisor = Supervisor::new(e.managed.clone(), e.xcb.clone());
    supervisor
        .active_workspaces
        .insert(new_id("t"), text(&repo).into());
    supervisor.tick(false).await.unwrap();
    assert!(!attempted(&supervisor, &e, &inner.id));
    assert!(!attempted(&supervisor, &e, &outer.id));
    assert!(attempted(&supervisor, &e, &beside.id));
    // The other way round: a worker in `r/sub` holds `r` back too.
    let mut supervisor = Supervisor::new(e.managed.clone(), e.xcb.clone());
    supervisor
        .active_workspaces
        .insert(new_id("t"), text(&sub).into());
    supervisor.tick(false).await.unwrap();
    assert!(!attempted(&supervisor, &e, &outer.id));
    // An unsettled run in `r` makes `r/sub` busy, not `r2`.
    let account = e
        .xcb
        .add_account(Provider::Claude, "Test", now_ms(), None)
        .unwrap();
    let model = ModelChoice {
        provider: Provider::Claude,
        id: Id::new("sonnet").unwrap(),
        label: "Sonnet".into(),
        mode: Mode::Fixed,
        resolved: None,
        effort: None,
        observed_at_ms: now_ms(),
    };
    let session = e
        .xcb
        .create_session(&account.id, model, &repo, now_ms())
        .unwrap();
    e.xcb
        .prepare_run(&session.id, session.revision, now_ms())
        .unwrap();
    assert!(workspace_busy(&e.xcb, text(&repo)).unwrap());
    assert!(workspace_busy(&e.xcb, text(&sub)).unwrap());
    assert!(!workspace_busy(&e.xcb, text(&sibling)).unwrap());
}

#[tokio::test]
async fn prompt_path_inside_admitted_root_snaps_to_root() {
    let e = env();
    let project = dir(&e.base, "plainproject");
    e.managed
        .admit_workspace(&project, "command", None)
        .unwrap();
    let deep = dir(&project, "src/deep");
    let task = accepted(
        submit(
            &e,
            &format!("look in {} please", text(&deep)),
            cues(Origin::Tui),
        )
        .await,
    );
    assert_eq!(task.workspace, text(&project));
    let binding = task.binding.unwrap();
    assert_eq!(binding.source, BindingSource::Mention);
    assert_eq!(binding.confidence, BindingConfidence::High);
    assert_eq!(binding.origin, BindingOrigin::Tui);
    assert_eq!(task.hold_until_ms, None);
}

#[tokio::test]
async fn prompt_path_in_nested_worktree_binds_worktree() {
    let e = env();
    let repo = git_repo(&e.base, "checkout");
    let worktree = dir(&repo, ".claude/worktrees/x");
    fs::write(worktree.join(".git"), "gitdir: ../../../.git/worktrees/x\n").unwrap();
    dir(&worktree, "src");
    e.managed.admit_workspace(&repo, "command", None).unwrap();
    e.managed
        .admit_workspace(&worktree, "command", None)
        .unwrap();
    let task = accepted(
        submit(
            &e,
            &format!("cd {}/src and run the tests", text(&worktree)),
            focused(&repo),
        )
        .await,
    );
    assert_eq!(task.workspace, text(&worktree));
    // It overrides the focused outer checkout, so it is held and says so.
    let binding = task.binding.unwrap();
    assert_eq!(binding.confidence, BindingConfidence::Medium);
    assert!(
        binding.reason.ends_with("(overrides focus checkout)"),
        "{}",
        binding.reason
    );
    assert!(task.hold_until_ms.is_some());
}

#[tokio::test]
async fn file_token_binds_its_repo() {
    let e = env();
    let repo = git_repo(&e.base, "gadget");
    dir(&repo, "src");
    fs::write(repo.join("src/x.rs"), "fn main() {}\n").unwrap();
    e.managed.admit_workspace(&repo, "command", None).unwrap();
    let prompt = format!(
        "{}/src/x.rs:12:5: error[E0308]: mismatched types",
        text(&repo)
    );
    let task = accepted(submit(&e, &prompt, cues(Origin::Cli)).await);
    assert_eq!(task.workspace, text(&repo));
    assert_eq!(task.binding.unwrap().source, BindingSource::Mention);
}

#[tokio::test]
async fn ssh_and_library_prompt_tokens_refused() {
    let e = env();
    let before = footprint(&e.managed);
    let home = PathBuf::from(std::env::var_os("HOME").unwrap());
    let mut evidence = vec![("in /etc/hosts fix it", "system directory")];
    if home.join(".ssh").is_dir() {
        evidence.push(("cd ~/.ssh and tidy it", "hidden or library directory"));
    }
    if home.join("Library").is_dir() {
        evidence.push((
            "in ~/Library/Caches clear things",
            "hidden or library directory",
        ));
    }
    for (prompt, why) in evidence {
        let (candidates, reason) = asked(submit(&e, prompt, cues(Origin::Tui)).await);
        assert!(candidates.is_empty(), "{prompt}");
        assert!(reason.contains(why), "{prompt}: {reason}");
    }
    // `~` alone is home, never a project.
    let (_, reason) = asked(submit(&e, "cd ~ and look around", cues(Origin::Tui)).await);
    assert!(
        reason.contains("workspace is not allowed: home"),
        "{reason}"
    );
    assert_eq!(footprint(&e.managed), before);
}

#[tokio::test]
async fn unregistered_prompt_root_asks_and_writes_nothing() {
    let e = env();
    let known = dir(&e.base, "known");
    e.managed.admit_workspace(&known, "command", None).unwrap();
    let fresh = git_repo(&e.base, "fresh");
    let before = footprint(&e.managed);
    let (candidates, reason) = asked(
        submit(
            &e,
            &format!("cd {} and build", text(&fresh)),
            focused(&known),
        )
        .await,
    );
    assert!(reason.contains("not a known project"), "{reason}");
    let offered: Vec<_> = candidates.iter().filter(|row| row.new).collect();
    assert_eq!(offered.len(), 1);
    assert_eq!(offered[0].path, text(&fresh));
    // The relay never offers a directory for admission.
    let (candidates, _) = asked(
        submit(
            &e,
            &format!("cd {} and build", text(&fresh)),
            IntakeCues {
                focus: Some(text(&known).into()),
                ..cues(Origin::Relay)
            },
        )
        .await,
    );
    assert!(candidates.iter().all(|row| !row.new));
    assert_eq!(footprint(&e.managed), before);
    assert!(e.managed.conversation(&thread()).unwrap().is_none());
}

#[tokio::test]
async fn retry_of_same_message_never_reinfers_after_registry_change() {
    let e = env();
    let a = dir(&e.base, "gizmo");
    let b = dir(&e.base, "widget");
    e.managed.admit_workspace(&a, "command", None).unwrap();
    e.managed.admit_workspace(&b, "command", None).unwrap();
    let message = new_id("m");
    let prompt = "Bump gizmo to the new API version and update every caller";
    let first = accepted(
        e.managed
            .submit_to_thread(message.clone(), prompt.into(), vec![], cues(Origin::Tui))
            .await
            .unwrap(),
    );
    assert_eq!(first.workspace, text(&a));
    // `gizmo` leaves the registry and `widget` becomes the focus.
    e.managed.hide_workspace(text(&a)).unwrap();
    let replay = accepted(
        e.managed
            .submit_to_thread(message.clone(), prompt.into(), vec![], focused(&b))
            .await
            .unwrap(),
    );
    assert_eq!(replay.id, first.id);
    assert_eq!(replay.workspace, text(&a));
    match e
        .managed
        .resolve_intake(&thread(), &message, prompt, &focused(&b))
        .unwrap()
    {
        Resolution::Bound { workspace, .. } => assert_eq!(workspace, text(&a)),
        other => panic!("unexpected resolution: {other:?}"),
    }
    assert!(matches!(
        e.managed
            .submit_to_thread(message, "Different text".into(), vec![], focused(&b))
            .await,
        Err(Error::Conflict(
            "message id was reused with different input"
        ))
    ));
    assert_eq!(footprint(&e.managed)[0], 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_retry_yields_one_task_and_ok() {
    let e = env();
    let a = dir(&e.base, "alpha");
    let message = new_id("m");
    let runs: Vec<_> = (0..4)
        .map(|_| {
            let managed = e.managed.clone();
            let message = message.clone();
            let cues = explicit(Origin::Relay, &a);
            tokio::spawn(async move {
                managed
                    .submit_to_thread(message, "Run the suite".into(), vec![], cues)
                    .await
            })
        })
        .collect();
    let mut ids = BTreeSet::new();
    for run in runs {
        ids.insert(accepted(run.await.unwrap().unwrap()).id);
    }
    assert_eq!(ids.len(), 1);
    assert_eq!(footprint(&e.managed)[0], 1);
}

#[tokio::test]
async fn held_task_not_dispatched_until_expiry_or_release() {
    let e = env();
    let a = dir(&e.base, "alpha");
    e.managed.admit_workspace(&a, "command", None).unwrap();
    // Nothing names a project: the recent rung guesses, so the TUI holds it.
    let held = accepted(submit(&e, "tidy things up", cues(Origin::Tui)).await);
    let binding = held.binding.clone().unwrap();
    assert_eq!(binding.source, BindingSource::Recent);
    assert_eq!(binding.confidence, BindingConfidence::Low);
    let until = held.hold_until_ms.unwrap();
    assert!(until > now_ms() && until <= now_ms() + WORKSPACE_HOLD_MS);
    let mut supervisor = Supervisor::new(e.managed.clone(), e.xcb.clone());
    supervisor.tick(false).await.unwrap();
    assert!(!attempted(&supervisor, &e, &held.id));
    assert_eq!(task(&e, &held.id).revision, held.revision);
    // Releasing needs the current revision, then dispatch proceeds.
    assert!(
        e.managed
            .release_hold(&held.id, held.revision + 1)
            .await
            .is_err()
    );
    let released = e
        .managed
        .release_hold(&held.id, held.revision)
        .await
        .unwrap();
    assert_eq!(released.hold_until_ms, None);
    assert_eq!(released.revision, held.revision + 1);
    supervisor.tick(false).await.unwrap();
    assert!(attempted(&supervisor, &e, &released.id));
    // An expired hold is cleared by the next tick, which then dispatches.
    let b = dir(&e.base, "beta");
    let other = accepted(submit(&e, "Second piece", explicit(Origin::Cli, &b)).await);
    let mut expired = other.clone();
    expired.hold_until_ms = Some(now_ms().saturating_sub(1));
    expired.revision += 1;
    expired.updated_at_ms = now_ms().max(other.updated_at_ms);
    let expired = e.managed.transition(&other, expired, None).await.unwrap();
    supervisor.tick(false).await.unwrap();
    let after = task(&e, &expired.id);
    assert_eq!(after.hold_until_ms, None);
    assert!(attempted(&supervisor, &e, &expired.id));
}

#[tokio::test]
async fn mention_overriding_focus_is_held_for_tui_not_relay() {
    let e = env();
    let a = dir(&e.base, "alphaproj");
    let b = dir(&e.base, "betaproj");
    e.managed.admit_workspace(&a, "command", None).unwrap();
    e.managed.admit_workspace(&b, "command", None).unwrap();
    let tui = accepted(submit(&e, "bump betaproj", focused(&a)).await);
    assert_eq!(tui.workspace, text(&b));
    let binding = tui.binding.clone().unwrap();
    assert_eq!(binding.confidence, BindingConfidence::Medium);
    assert_eq!(
        binding.reason,
        "named `betaproj` (overrides focus alphaproj)"
    );
    assert!(tui.hold_until_ms.is_some());
    for origin in [Origin::Relay, Origin::Cli] {
        let other = accepted(
            submit(
                &e,
                "bump betaproj",
                IntakeCues {
                    focus: Some(text(&a).into()),
                    ..cues(origin)
                },
            )
            .await,
        );
        assert_eq!(other.workspace, text(&b));
        assert_eq!(other.hold_until_ms, None, "{origin:?}");
    }
}

#[tokio::test]
async fn relay_infer_continuation_requires_relay_origin_last_task() {
    let e = env();
    let a = dir(&e.base, "alpha");
    let b = dir(&e.base, "beta");
    e.managed.admit_workspace(&a, "command", None).unwrap();
    e.managed.admit_workspace(&b, "command", None).unwrap();
    accepted(submit(&e, "Laptop work", focused(&a)).await);
    // The laptop's own task never makes a remote "continue" bind.
    asked(submit(&e, "continue", infer()).await);
    // Distinct timestamps keep "most recent" unambiguous.
    let tick = || tokio::time::sleep(Duration::from_millis(5));
    tick().await;
    let remote = accepted(submit(&e, "Remote work", explicit(Origin::Relay, &b)).await);
    tick().await;
    let laptop = accepted(submit(&e, "More laptop work", focused(&a)).await);
    assert!(laptop.updated_at_ms > remote.updated_at_ms);
    tick().await;
    let continued = accepted(submit(&e, "continue", infer()).await);
    assert_eq!(continued.workspace, text(&b));
    assert_eq!(
        continued.binding.unwrap().source,
        BindingSource::Continuation
    );
    // A remote prompt with no cue asks rather than following either.
    asked(submit(&e, "run the tests", infer()).await);
    // The TUI continues the thread's most recent task, whoever made it.
    let local = accepted(submit(&e, "continue", cues(Origin::Tui)).await);
    assert_eq!(local.workspace, text(&b));
}

#[tokio::test]
async fn move_before_dispatch_cancels_with_no_effects_and_recreates_deterministically() {
    let e = env();
    let a = dir(&e.base, "alpha");
    let b = dir(&e.base, "beta");
    e.managed.admit_workspace(&b, "command", None).unwrap();
    let original = accepted(submit(&e, "Fix the parser", focused(&a)).await);
    let moved = e
        .managed
        .move_task(&original.id, original.revision, text(&b))
        .await
        .unwrap();
    assert_eq!(moved.workspace, text(&b));
    assert_eq!(moved.conversation, thread());
    assert_eq!(moved.goal, original.goal);
    assert_eq!(moved.moved_from.as_ref(), Some(&original.id));
    assert!(moved.source_message.as_str().starts_with("m_mv_"));
    assert_eq!(moved.hold_until_ms, None);
    let binding = moved.binding.clone().unwrap();
    assert_eq!(binding.source, BindingSource::Moved);
    assert_eq!(binding.confidence, BindingConfidence::High);
    assert_eq!(binding.origin, BindingOrigin::Tui);
    let cancelled = task(&e, &original.id);
    assert_eq!(cancelled.state, TaskState::Cancelled);
    assert_eq!(cancelled.detail, "moved to beta");
    assert_eq!(cancelled.session, None);
    assert_eq!(cancelled.attempts, 0);
    // A retry replays the committed move.
    let again = e
        .managed
        .move_task(&original.id, original.revision, text(&b))
        .await
        .unwrap();
    assert_eq!(again.id, moved.id);
    assert_eq!(footprint(&e.managed)[0], 2);
    e.managed.verify_task(&original.id).await.unwrap();
    e.managed.verify_task(&moved.id).await.unwrap();
    // The moved task can move on before dispatch.
    let back = e
        .managed
        .move_task(&moved.id, moved.revision, text(&a))
        .await
        .unwrap();
    assert_eq!(back.workspace, text(&a));
    assert_eq!(back.moved_from.as_ref(), Some(&moved.id));
}

#[tokio::test]
async fn move_after_launch_is_refused() {
    let e = env();
    let a = dir(&e.base, "alpha");
    let b = dir(&e.base, "beta");
    let original = accepted(submit(&e, "Fix the parser", explicit(Origin::Cli, &a)).await);
    let refused = |result: Result<ManagedTask>| match result {
        Err(Error::Guided { message, .. }) => {
            assert!(
                message.starts_with("task already started in alpha"),
                "{message}"
            )
        }
        other => panic!("not refused: {other:?}"),
    };
    // A stale revision is refused like a started task.
    refused(
        e.managed
            .move_task(&original.id, original.revision + 1, text(&b))
            .await,
    );
    let mut started = original.clone();
    started.attempts = 1;
    started.revision += 1;
    started.updated_at_ms = now_ms().max(original.updated_at_ms);
    let started = e
        .managed
        .transition(&original, started, None)
        .await
        .unwrap();
    refused(
        e.managed
            .move_task(&started.id, started.revision, text(&b))
            .await,
    );
    assert_eq!(task(&e, &started.id).revision, started.revision);
    assert_eq!(footprint(&e.managed)[0], 1);
}

#[tokio::test]
async fn move_task_refuses_proposal_schedule_worker_program_and_daemon_tasks() {
    let e = env();
    let a = dir(&e.base, "alpha");
    let b = dir(&e.base, "beta");
    for (creator, edit) in [
        ("worker", json!({"binding": {"origin": "worker"}})),
        ("program", json!({"binding": {"origin": "program"}})),
        ("daemon", json!({"binding": {"origin": "daemon"}})),
        ("schedule", json!({"binding": {"origin": "schedule"}})),
        ("schedule", json!({"schedule": "s_standing"})),
    ] {
        let original = accepted(submit(&e, "Standing work", explicit(Origin::Cli, &a)).await);
        rewrite(&e, &original.id, |value| {
            if let Some(origin) = edit["binding"]["origin"].as_str() {
                value["binding"]["origin"] = json!(origin);
            }
            if let Some(schedule) = edit.get("schedule") {
                value["schedule"] = schedule.clone();
            }
        });
        let current = task(&e, &original.id);
        let expected = format!("this task was created by {creator}; cancel it instead");
        match e
            .managed
            .move_task(&current.id, current.revision, text(&b))
            .await
        {
            Err(Error::Conflict(message)) => assert_eq!(message, expected),
            other => panic!("{creator} task moved: {other:?}"),
        }
        assert_eq!(task(&e, &current.id).state, TaskState::Queued);
    }
    // A project view's task is not the thread's to move either.
    let view = e.managed.create_conversation(&a).await.unwrap().id;
    let viewed = e
        .managed
        .create_task(&view, new_id("m"), "View work".into(), vec![], &a)
        .await
        .unwrap();
    assert!(matches!(
        e.managed
            .move_task(&viewed.id, viewed.revision, text(&b))
            .await,
        Err(Error::Conflict(_))
    ));
}

#[tokio::test]
async fn launch_revalidation_fails_task_when_workspace_replaced_by_symlink() {
    let e = env();
    let a = dir(&e.base, "alpha");
    let elsewhere = dir(&e.base, "elsewhere");
    let original = accepted(submit(&e, "Fix the parser", explicit(Origin::Cli, &a)).await);
    fs::rename(&a, e.base.join("alpha-old")).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &a).unwrap();
    let mut supervisor = Supervisor::new(e.managed.clone(), e.xcb.clone());
    supervisor.tick(false).await.unwrap();
    let failed = task(&e, &original.id);
    assert_eq!(failed.state, TaskState::Failed);
    assert_eq!(
        failed.detail,
        "workspace moved or was replaced since it was bound"
    );
    assert_eq!(failed.session, None);
}

#[tokio::test]
async fn ask_writes_nothing() {
    let e = env();
    let before = footprint(&e.managed);
    let message = new_id("m");
    let resolution = e
        .managed
        .resolve_intake(&thread(), &message, "do something", &cues(Origin::Tui))
        .unwrap();
    assert!(matches!(resolution, Resolution::Ask { .. }));
    let (candidates, reason) = asked(
        e.managed
            .submit_to_thread(message, "do something".into(), vec![], cues(Origin::Tui))
            .await
            .unwrap(),
    );
    assert!(candidates.is_empty());
    assert_eq!(reason, "which project?");
    assert_eq!(footprint(&e.managed), before);
    assert!(e.managed.conversation(&thread()).unwrap().is_none());
}

#[tokio::test]
async fn ack_names_workspace_and_reason() {
    let e = env();
    let a = dir(&e.base, "alpha");
    let b = dir(&e.base, "beta");
    e.managed.admit_workspace(&a, "command", None).unwrap();
    e.managed.admit_workspace(&b, "command", None).unwrap();
    accepted(submit(&e, "Fix the parser", focused(&a)).await);
    accepted(submit(&e, "Now bump beta", focused(&a)).await);
    let acks: Vec<String> = e
        .managed
        .messages(&thread(), 16)
        .unwrap()
        .into_iter()
        .map(|message| message.text)
        .filter(|text| text.starts_with("Started "))
        .collect();
    assert_eq!(
        acks,
        [
            "Started **Fix the parser** in `alpha` · focus · /workspace to move",
            "Started **Now bump beta** in `beta` · named `beta` (overrides focus alpha) · starts in 8s · /workspace to move",
        ]
    );
}

#[tokio::test]
async fn submit_in_the_thread_binds_the_named_directory() {
    let e = env();
    let a = dir(&e.base, "alpha");
    e.managed.global_thread().await.unwrap();
    let message = new_id("m");
    e.managed
        .submit(&thread(), message.clone(), "Fix it".into(), vec![], &a)
        .await
        .unwrap();
    let created = e.managed.tasks(8).unwrap();
    assert_eq!(created.len(), 1);
    assert_eq!(created[0].workspace, text(&a));
    assert_eq!(
        created[0].binding.as_ref().map(|binding| binding.source),
        Some(BindingSource::Explicit)
    );
    // A non-canonical directory is refused rather than stored.
    let dotted = e.base.join("alpha/../alpha");
    assert!(matches!(
        e.managed
            .submit(&thread(), new_id("m"), "Again".into(), vec![], &dotted)
            .await,
        Err(Error::Conflict("workspace is not canonical"))
    ));
}
