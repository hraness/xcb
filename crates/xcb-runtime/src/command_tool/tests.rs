//! Synthetic lifecycle and publication tests; these do not qualify a live VM.
use super::*;
use crate::{
    broker::snapshot::CommandSnapshot,
    command::{CommandCustody, CommandOutput},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use std::{future::Future, pin::Pin, task::Poll, time::Duration};
use tokio::sync::oneshot;
use xcb_core::{Id, MAX_TEXT_BYTES};

async fn assert_pending<F: Future>(mut future: Pin<&mut F>) {
    std::future::poll_fn(|context| {
        assert!(matches!(future.as_mut().poll(context), Poll::Pending));
        Poll::Ready(())
    })
    .await;
}
fn joined_result() -> CommandToolResult {
    CommandToolResult {
        output: Ok(json!({"cleanupComplete":true})),
        effects: EffectState::Settled,
        joined: true,
    }
}

#[tokio::test]
async fn cancelled_wait_preserves_owner_until_independent_cleanup_finishes() {
    for timeout in [false, true] {
        let (cancel, mut cancellation) = watch::channel(false);
        let (entered, started) = oneshot::channel();
        let (observed, signalled) = oneshot::channel();
        let (release, cleanup) = oneshot::channel();
        let task = tokio::spawn(async move {
            entered.send(()).unwrap();
            cancellation.changed().await.unwrap();
            assert!(*cancellation.borrow());
            observed.send(()).unwrap();
            cleanup.await.unwrap();
            joined_result()
        });
        let mut commands = CommandTools {
            active: Some(Active { cancel, task }),
            preparing: None,
            blocking: None,
        };
        started.await.unwrap();
        let mut pending = Box::pin(commands.wait());
        assert_pending(pending.as_mut()).await;
        if timeout {
            assert!(tokio::time::timeout(Duration::ZERO, pending).await.is_err());
        } else {
            drop(pending);
        }
        assert!(commands.active.is_some());
        let mut joining = Box::pin(commands.cancel_and_join());
        assert_pending(joining.as_mut()).await;
        signalled.await.unwrap();
        // Receiving cancellation does not become join evidence. Only the
        // separate cleanup latch is allowed to complete the owned task.
        assert_pending(joining.as_mut()).await;
        release.send(()).unwrap();
        let result = tokio::time::timeout(Duration::from_secs(5), joining)
            .await
            .unwrap()
            .unwrap();
        assert!(result.joined);
        assert_eq!(result.effects, EffectState::Settled);
        assert_eq!(result.output.unwrap()["cleanupComplete"], true);
        assert!(commands.active.is_none());
    }
}

#[tokio::test]
async fn dropping_command_collection_requests_cancellation_without_aborting_owner() {
    let (cancel, mut cancellation) = watch::channel(false);
    let (observed, signalled) = oneshot::channel();
    let (release, cleanup) = oneshot::channel();
    let (finished, completion) = oneshot::channel();
    let task = tokio::spawn(async move {
        cancellation.changed().await.unwrap();
        observed.send(*cancellation.borrow()).unwrap();
        cleanup.await.unwrap();
        finished.send(()).unwrap();
        joined_result()
    });
    let commands = CommandTools {
        active: Some(Active { cancel, task }),
        preparing: None,
        blocking: None,
    };
    drop(commands);
    assert!(
        tokio::time::timeout(Duration::from_secs(5), signalled)
            .await
            .unwrap()
            .unwrap()
    );
    release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), completion)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn command_owner_panic_is_uncertain_and_never_joined() {
    let (cancel, _cancellation) = watch::channel(false);
    let task: JoinHandle<CommandToolResult> =
        tokio::spawn(async { panic!("synthetic command owner panic") });
    let mut commands = CommandTools {
        active: Some(Active { cancel, task }),
        preparing: None,
        blocking: None,
    };
    let result = commands.wait().await;
    assert!(!result.joined);
    assert_eq!(result.effects, EffectState::Uncertain);
    assert!(result.output.is_err());
    assert!(commands.active.is_none());
}

struct Fixture {
    _directory: tempfile::TempDir,
    workspace: Workspace,
    snapshot: CommandSnapshot,
    original: PathBuf,
    replacement: Vec<u8>,
    outcome: CommandOutcome,
}
fn fixture() -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    let base = xcb_core::canonical(directory.path()).unwrap();
    let work = private::directory(&base.join("workspace")).unwrap();
    let coordination = private::directory(&base.join("coordination")).unwrap();
    let original = work.join("data.bin");
    std::fs::write(&original, [0, 255, 1, 254]).unwrap();
    let workspace = Workspace::open_with_coordination(&work, &coordination).unwrap();
    let snapshot = workspace.command_snapshot().unwrap();
    let replacement = vec![255, 0, 254, 1, 253, 2];
    let changes = json!({"version":1,"workspaceId":snapshot.document.workspace_id,"changes":[{
        "path":"data.bin","base64":STANDARD.encode(&replacement),"sha256":digest(&replacement),"executable":false}]});
    let bytes = serde_json::to_vec(&changes).unwrap();
    let output_directory = private::directory(&base.join("output")).unwrap();
    let changes_path = output_directory.join("changes.json");
    private::create(&changes_path, &bytes).unwrap();
    let custody = CommandCustody {
        version: 1,
        command_id: Id::new("cmd_synthetic").unwrap(),
        run_id: Id::new("run_synthetic").unwrap(),
        workspace_id: snapshot.document.workspace_id.clone(),
        snapshot_sha256: "a".repeat(64),
        request_sha256: "b".repeat(64),
        backend_sha256: "c".repeat(64),
        boot_id: "00000000-0000-0000-0000-000000000001".into(),
    };
    let outcome = CommandOutcome {
        custody,
        joined: true,
        error: None,
        output: Some(CommandOutput {
            exit_code: Some(0),
            stdout: "synthetic stdout".into(),
            stderr: String::new(),
            timed_out: false,
            cancelled: false,
            truncated: false,
            cleanup_pending: false,
            changes_path: Some(changes_path),
            changes_sha256: Some(digest(bytes)),
        }),
    };
    Fixture {
        _directory: directory,
        workspace,
        snapshot,
        original,
        replacement,
        outcome,
    }
}

#[test]
fn publication_requires_success_without_cancellation_timeout_truncation_or_error() {
    for case in 0..7 {
        let mut fixture = fixture();
        let output = fixture.outcome.output.as_mut().unwrap();
        match case {
            0 => {} // cancellation observed by the host owner
            1 => output.cancelled = true,
            2 => output.timed_out = true,
            3 => output.exit_code = Some(7),
            4 => output.truncated = true,
            5 => fixture.outcome.error = Some("synthetic guest failure".into()),
            6 => output.exit_code = None,
            _ => unreachable!(),
        }
        let (result, effects) = publish(
            &fixture.workspace,
            &fixture.snapshot,
            &fixture.outcome,
            case == 0,
        );
        let value = result.unwrap();
        assert_eq!(effects, EffectState::None);
        assert_eq!(value["published"], false);
        assert_eq!(value["stagedChangesRetained"], true);
        assert_eq!(std::fs::read(&fixture.original).unwrap(), [0, 255, 1, 254]);
    }
}

#[test]
fn successful_command_publishes_binary_changes_with_revision_cas() {
    let fixture = fixture();
    let (result, effects) = publish(
        &fixture.workspace,
        &fixture.snapshot,
        &fixture.outcome,
        false,
    );
    let value = result.unwrap();
    assert_eq!(effects, EffectState::Settled);
    assert_eq!(value["published"], true);
    assert_eq!(value["publication"]["written"], 1);
    assert_eq!(
        std::fs::read(&fixture.original).unwrap(),
        fixture.replacement
    );
    let (replay, effects) = publish(
        &fixture.workspace,
        &fixture.snapshot,
        &fixture.outcome,
        false,
    );
    assert!(replay.is_err());
    assert_eq!(effects, EffectState::None);
    assert_eq!(
        std::fs::read(&fixture.original).unwrap(),
        fixture.replacement
    );
}

#[test]
fn concurrent_workspace_edit_and_forged_change_digest_are_not_published() {
    for concurrent in [false, true] {
        let mut fixture = fixture();
        let expected = if concurrent {
            std::fs::write(&fixture.original, b"concurrent user content").unwrap();
            b"concurrent user content".to_vec()
        } else {
            fixture.outcome.output.as_mut().unwrap().changes_sha256 = Some("0".repeat(64));
            vec![0, 255, 1, 254]
        };
        let (result, effects) = publish(
            &fixture.workspace,
            &fixture.snapshot,
            &fixture.outcome,
            false,
        );
        assert!(result.is_err());
        assert_eq!(effects, EffectState::None);
        assert_eq!(std::fs::read(&fixture.original).unwrap(), expected);
    }
}

#[test]
fn tool_result_remains_bounded_after_json_escaping_and_multibyte_clipping() {
    for atoms in [["\0", "\u{0001}", "\u{0002}"], ["🦀", "é", "🦋"]] {
        let mut fixture = fixture();
        fixture.snapshot.excluded = (0..256)
            .map(|i| format!(".env.{i}.{}", "\"\\\n🦀".repeat(20)))
            .collect();
        fixture.outcome.error = Some(atoms[2].repeat(MAX_TEXT_BYTES));
        let output = fixture.outcome.output.as_mut().unwrap();
        output.stdout = atoms[0].repeat(20000);
        output.stderr = atoms[1].repeat(20000);
        output.exit_code = Some(1);
        let (result, effects) = publish(
            &fixture.workspace,
            &fixture.snapshot,
            &fixture.outcome,
            false,
        );
        let value = result.unwrap();
        assert_eq!(effects, EffectState::None);
        assert_eq!(value["published"], false);
        assert_eq!(value["truncated"], true);
        assert_eq!(value["excludedPathsTruncated"], true);
        assert!(value["stdout"].as_str().unwrap().len() <= 16384);
        assert!(value["stderr"].as_str().unwrap().len() <= 16384);
        assert!(value["error"].as_str().unwrap().len() <= 4096);
        assert!(serde_json::to_vec(&value).unwrap().len() < MAX_TEXT_BYTES);
        assert_eq!(std::fs::read(&fixture.original).unwrap(), [0, 255, 1, 254]);
    }
    let value = format!("{}🦀", "a".repeat(16383));
    let (prefix, truncated) = clipped_at(&value, 16384);
    assert!(truncated);
    assert_eq!(prefix.len(), 16383);
}

#[test]
fn unstarted_receipts_are_bounded_and_never_publish() {
    let mut fixture = fixture();
    fixture.outcome.output = None;
    fixture.outcome.error = Some("busy ".repeat(4000));
    let (result, effects) = publish(
        &fixture.workspace,
        &fixture.snapshot,
        &fixture.outcome,
        false,
    );
    let result = result.unwrap();
    assert_eq!(result["status"], "not_started");
    assert_eq!(result["published"], false);
    assert_eq!(result["truncated"], true);
    assert_eq!(effects, EffectState::None);
    assert_eq!(std::fs::read(&fixture.original).unwrap(), [0, 255, 1, 254]);
    fixture.outcome.joined = false;
    assert!(
        publish(
            &fixture.workspace,
            &fixture.snapshot,
            &fixture.outcome,
            false
        )
        .0
        .is_err()
    );
    fixture.outcome.joined = true;
    fixture.outcome.custody.workspace_id = "0".repeat(64);
    assert!(
        publish(
            &fixture.workspace,
            &fixture.snapshot,
            &fixture.outcome,
            false
        )
        .0
        .is_err()
    );
}

fn owned_input(fixture: &Fixture) -> (PathBuf, OwnedSnapshot) {
    let directory = private::directory(
        &xcb_core::canonical(fixture._directory.path())
            .unwrap()
            .join("snapshots"),
    )
    .unwrap();
    let path = directory.join("cmd_synthetic.json");
    let bytes = serde_json::to_vec(&fixture.snapshot.document).unwrap();
    private::create(&path, &bytes).unwrap();
    let owned = OwnedSnapshot::capture(&path, &digest(bytes)).unwrap();
    (path, owned)
}

fn registered_command(fixture: &mut Fixture, input: &OwnedSnapshot) -> (Store, RunRecord) {
    let store = Store::open(
        &xcb_core::canonical(fixture._directory.path())
            .unwrap()
            .join("state"),
    )
    .unwrap();
    let account = store
        .add_account(xcb_core::Provider::Claude, "test", 1, None)
        .unwrap();
    let run = store.prepare_probe(&account.id, None, 2).unwrap();
    fixture.outcome.custody.run_id = run.id.clone();
    fixture.outcome.custody.snapshot_sha256 = input.sha256.clone();
    store
        .begin_tool(&run, "call", "workspace_exec", &"a".repeat(64))
        .unwrap();
    store
        .record_command_custody(&run, &fixture.outcome.custody)
        .unwrap();
    (store, run)
}

#[test]
fn successful_publication_retires_only_its_exact_input_after_durable_settlement() {
    let mut fixture = fixture();
    let (path, input) = owned_input(&fixture);
    let historical = path.parent().unwrap().join("cmd_historical.json");
    private::create(&historical, b"retained historical input").unwrap();
    let (store, run) = registered_command(&mut fixture, &input);
    let publication = publish(
        &fixture.workspace,
        &fixture.snapshot,
        &fixture.outcome,
        false,
    );
    let result = finish_command(&store, &run, "call", &fixture.outcome, publication, input);
    assert!(result.joined);
    assert_eq!(result.effects, EffectState::Settled);
    let output = result.output.unwrap();
    assert_eq!(output["published"], true);
    assert_eq!(output["inputSnapshotCleanup"], "removed");
    assert!(!path.exists());
    assert_eq!(
        std::fs::read(historical).unwrap(),
        b"retained historical input"
    );
    assert!(
        fixture
            .outcome
            .output
            .as_ref()
            .unwrap()
            .changes_path
            .as_ref()
            .unwrap()
            .exists()
    );
    assert!(
        store
            .run(&run.id)
            .unwrap()
            .unwrap()
            .command_custody
            .is_none()
    );
    assert!(store.settle_tool(&run, "call").is_err(), "already settled");
    assert_eq!(
        std::fs::read(&fixture.original).unwrap(),
        fixture.replacement
    );
}

#[test]
fn failed_cancelled_and_unstarted_commands_retain_their_inputs() {
    for case in 0..8 {
        let mut fixture = fixture();
        let (path, input) = owned_input(&fixture);
        let (store, run) = registered_command(&mut fixture, &input);
        let host_cancelled = case == 0;
        match case {
            1 => fixture.outcome.output.as_mut().unwrap().cancelled = true,
            2 => fixture.outcome.output.as_mut().unwrap().timed_out = true,
            3 => fixture.outcome.output.as_mut().unwrap().truncated = true,
            4 => fixture.outcome.output.as_mut().unwrap().exit_code = Some(1),
            5 => fixture.outcome.error = Some("synthetic supervisor failure".into()),
            6 => fixture.outcome.output = None,
            7 => fixture.outcome.output.as_mut().unwrap().exit_code = None,
            _ => (),
        }
        let publication = publish(
            &fixture.workspace,
            &fixture.snapshot,
            &fixture.outcome,
            host_cancelled,
        );
        let result = finish_command(&store, &run, "call", &fixture.outcome, publication, input);
        assert!(result.joined);
        assert_eq!(result.effects, EffectState::None);
        assert_eq!(result.output.unwrap()["published"], false);
        assert!(path.exists(), "retained case {case}");
        assert_eq!(std::fs::read(&fixture.original).unwrap(), [0, 255, 1, 254]);
    }
}

#[test]
fn uncertain_publication_and_failed_settlement_retain_input_and_custody_facts() {
    for case in 0..4 {
        let mut fixture = fixture();
        let (path, input) = owned_input(&fixture);
        let (store, run) = registered_command(&mut fixture, &input);
        let publication = match case {
            0 => (
                Err(Error::Conflict("synthetic partial publication")),
                EffectState::Uncertain,
            ),
            3 => {
                fixture.outcome.joined = false;
                publish(
                    &fixture.workspace,
                    &fixture.snapshot,
                    &fixture.outcome,
                    false,
                )
            }
            _ => publish(
                &fixture.workspace,
                &fixture.snapshot,
                &fixture.outcome,
                false,
            ),
        };
        match case {
            1 => store
                .clear_command_custody(&run, &fixture.outcome.custody)
                .unwrap(),
            2 => store.settle_tool(&run, "call").unwrap(),
            _ => (),
        }
        let result = finish_command(&store, &run, "call", &fixture.outcome, publication, input);
        assert_eq!(result.joined, case != 3);
        assert!(result.output.is_err());
        assert_eq!(result.effects, EffectState::Uncertain);
        assert!(path.exists());
        if case == 3 {
            assert!(
                store
                    .run(&run.id)
                    .unwrap()
                    .unwrap()
                    .command_custody
                    .is_some()
            );
        }
    }
}

#[test]
fn input_cleanup_rejects_content_identity_links_mode_and_parent_changes() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    for case in 0..6 {
        let fixture = fixture();
        let (path, input) = owned_input(&fixture);
        let original = std::fs::read(&path).unwrap();
        let parent = path.parent().unwrap();
        match case {
            0 => std::fs::write(&path, b"changed input").unwrap(),
            1 => {
                std::fs::rename(&path, parent.join("original.json")).unwrap();
                private::create(&path, &original).unwrap();
            }
            2 => std::fs::hard_link(&path, parent.join("alias.json")).unwrap(),
            3 => std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap(),
            4 => {
                std::fs::rename(&path, parent.join("original.json")).unwrap();
                symlink("original.json", &path).unwrap();
            }
            5 => {
                let retained = parent.with_file_name("retained-snapshots");
                std::fs::rename(parent, &retained).unwrap();
                private::directory(parent).unwrap();
                private::create(&path, &original).unwrap();
            }
            _ => unreachable!(),
        }
        assert!(input.remove().is_err(), "case {case}");
        assert!(
            std::fs::symlink_metadata(&path).is_ok(),
            "replacement preserved case {case}"
        );
    }
    let fixture = fixture();
    let (path, input) = owned_input(&fixture);
    drop(input);
    assert!(OwnedSnapshot::capture(&path, &"0".repeat(64)).is_err());
    assert!(path.exists());
}

#[test]
fn failed_input_cleanup_is_a_bounded_notice_without_reclassifying_joined_effects() {
    let mut fixture = fixture();
    let (path, input) = owned_input(&fixture);
    let (store, run) = registered_command(&mut fixture, &input);
    let publication = publish(
        &fixture.workspace,
        &fixture.snapshot,
        &fixture.outcome,
        false,
    );
    std::fs::write(&path, b"changed input").unwrap();
    let result = finish_command(&store, &run, "call", &fixture.outcome, publication, input);
    assert!(result.joined);
    assert_eq!(result.effects, EffectState::Settled);
    let output = result.output.unwrap();
    assert_eq!(output["published"], true);
    assert_eq!(output["inputSnapshotCleanup"], "unconfirmed");
    assert!(output["notice"].as_str().unwrap().len() < 128);
    assert!(
        !output["notice"]
            .as_str()
            .unwrap()
            .contains(path.to_str().unwrap())
    );
    assert_eq!(std::fs::read(path).unwrap(), b"changed input");
    assert!(
        store
            .run(&run.id)
            .unwrap()
            .unwrap()
            .command_custody
            .is_none()
    );
    assert!(store.settle_tool(&run, "call").is_err());
}

#[test]
fn unsubmitted_input_cleanup_requires_owned_run_and_absent_durable_marker() {
    for case in 0..4 {
        let mut fixture = fixture();
        let (path, input) = owned_input(&fixture);
        let (store, run) = registered_command(&mut fixture, &input);
        if case != 1 {
            store
                .clear_command_custody(&run, &fixture.outcome.custody)
                .unwrap();
        }
        match case {
            2 => {
                let sibling = Store::open(store.root()).unwrap();
                discard_unsubmitted(&sibling, &run, input);
            }
            3 => {
                store.settle_tool(&run, "call").unwrap();
                store
                    .settle(&run, xcb_core::session::State::Failed, 3)
                    .unwrap();
                discard_unsubmitted(&store, &run, input);
            }
            _ => discard_unsubmitted(&store, &run, input),
        }
        assert_eq!(path.exists(), case != 0, "case {case}");
    }
}

#[test]
fn pending_scratch_cleanup_preserves_joined_publication() {
    let mut fixture = fixture();
    fixture.outcome.output.as_mut().unwrap().cleanup_pending = true;
    let (result, effects) = publish(
        &fixture.workspace,
        &fixture.snapshot,
        &fixture.outcome,
        false,
    );
    let output = result.unwrap();
    assert_eq!(output["joined"], true);
    assert_eq!(output["published"], true);
    assert_eq!(output["scratchCleanupPending"], true);
    assert!(
        output["scratchCleanupNotice"]
            .as_str()
            .unwrap()
            .contains("results remain valid")
    );
    assert_eq!(effects, EffectState::Settled);
    assert_eq!(
        std::fs::read(&fixture.original).unwrap(),
        fixture.replacement
    );
}

#[test]
fn recovery_discard_refuses_live_owner_stale_digest_and_recoverable_command() {
    use xcb_core::Provider;
    let temp = tempfile::tempdir().unwrap();
    let root = xcb_core::canonical(temp.path()).unwrap();
    let store = Store::open(&root.join("state")).unwrap();
    let account = store
        .add_account(Provider::Codex, "Synthetic", 1, None)
        .unwrap();
    let prepared = store.prepare_probe(&account.id, None, 2).unwrap();
    let live = store.mark_spawned(&prepared, i32::MAX as u32).unwrap();
    let custody = CommandCustody {
        version: 1,
        command_id: new_id("cmd"),
        run_id: live.id.clone(),
        workspace_id: "a".repeat(64),
        snapshot_sha256: "b".repeat(64),
        request_sha256: "c".repeat(64),
        backend_sha256: "d".repeat(64),
        boot_id: "00000000-0000-0000-0000-000000000000".into(),
    };
    store.record_command_custody(&live, &custody).unwrap();
    let command_root = private::directory(&root.join("commands")).unwrap();
    private::directory(&command_root.join("jobs")).unwrap();
    let job =
        private::directory(&command_root.join("jobs").join(custody.command_id.as_str())).unwrap();
    private::create(
        &job.join("custody.json"),
        &serde_json::to_vec(&custody).unwrap(),
    )
    .unwrap();
    let (live, live_digest) = store.recovery_candidate(&live.id).unwrap().unwrap();
    assert!(discard_unstarted_in(&command_root, &store, &live, &live_digest, None).is_err());
    assert_eq!(store.unsettled_runs().unwrap().len(), 1);
    let dead = store.orphan_run_for_test(&store.run(&live.id).unwrap().unwrap());
    let (_, digest) = store.recovery_candidate(&dead.id).unwrap().unwrap();
    dead.verify_recovery_stop().unwrap();
    assert!(discard_unstarted_in(&command_root, &store, &dead, &live_digest, None).is_err());
    private::create(&job.join("outcome.json"), b"{}").unwrap();
    let refusal = discard_unstarted_in(&command_root, &store, &dead, &digest, None)
        .unwrap_err()
        .to_string();
    assert!(
        refusal.contains("recovery file or start record exists"),
        "{refusal}"
    );
    assert_eq!(store.unsettled_runs().unwrap().len(), 1);
    assert!(store.prepare_probe(&account.id, None, 4).is_err());
}

#[test]
fn recovery_discard_of_proven_unsubmitted_command_retains_job_and_requires_fresh_run_digest() {
    use xcb_core::Provider;
    let temp = tempfile::tempdir().unwrap();
    let root = xcb_core::canonical(temp.path()).unwrap();
    let store = Store::open(&root.join("state")).unwrap();
    let account = store
        .add_account(Provider::Codex, "Synthetic", 1, None)
        .unwrap();
    let prepared = store.prepare_probe(&account.id, None, 2).unwrap();
    let live = store.mark_spawned(&prepared, i32::MAX as u32).unwrap();
    let custody = CommandCustody {
        version: 1,
        command_id: new_id("cmd"),
        run_id: live.id.clone(),
        workspace_id: "a".repeat(64),
        snapshot_sha256: "b".repeat(64),
        request_sha256: "c".repeat(64),
        backend_sha256: "d".repeat(64),
        boot_id: "00000000-0000-0000-0000-000000000000".into(),
    };
    store.record_command_custody(&live, &custody).unwrap();
    let dead = store.orphan_run_for_test(&store.run(&live.id).unwrap().unwrap());
    let (_, digest) = store.recovery_candidate(&dead.id).unwrap().unwrap();
    let command_root = private::directory(&root.join("commands")).unwrap();
    private::directory(&command_root.join("jobs")).unwrap();
    let job =
        private::directory(&command_root.join("jobs").join(custody.command_id.as_str())).unwrap();
    private::create(
        &job.join("custody.json"),
        &serde_json::to_vec(&custody).unwrap(),
    )
    .unwrap();
    assert!(discard_unstarted_in(&command_root, &store, &dead, "stale", None).is_err());
    assert!(job.join("discarded.json").exists());
    assert_eq!(store.unsettled_runs().unwrap().len(), 1);
    let cleared = discard_unstarted_in(&command_root, &store, &dead, &digest, None).unwrap();
    assert!(cleared.command_custody.is_none());
    assert!(job.join("discarded.json").exists());
    assert!(store.recover_run(&dead.id, &digest, 4).is_err());
    let (_, fresh) = store.recovery_candidate(&dead.id).unwrap().unwrap();
    store.recover_run(&dead.id, &fresh, 4).unwrap();
    assert_eq!(store.unsettled_runs().unwrap().len(), 0);
    assert!(store.prepare_probe(&account.id, None, 5).is_ok());
}

// No VM or credential is needed: these receipts model one exact started guest
// command while the run, lease and revision live in a real isolated store.
const COMMAND_LAUNCH_BOOT: &str = "abcdef00-0000-0000-0000-000000000001";
const COMMAND_NEXT_BOOT: &str = "abcdef00-0000-0000-0000-000000000002";

struct RecoveryFixture {
    _directory: tempfile::TempDir,
    store: Store,
    run: RunRecord,
    run_digest: String,
    command_root: PathBuf,
    job: PathBuf,
    original: PathBuf,
}

fn recovery_fixture(launch_boot: Option<&str>, orphan: bool) -> RecoveryFixture {
    let directory = tempfile::tempdir().unwrap();
    let base = xcb_core::canonical(directory.path()).unwrap();
    let store = Store::open(&base.join("state")).unwrap();
    let account = store
        .add_account(xcb_core::Provider::Codex, "Recovery test", 1, None)
        .unwrap();
    let prepared = store.prepare_probe(&account.id, None, 2).unwrap();
    store
        .mark_capability_starting_with_boot(&prepared, "tool-server", launch_boot)
        .unwrap();
    let running = store.mark_spawned(&prepared, i32::MAX as u32).unwrap();
    let custody = CommandCustody {
        version: 1,
        command_id: new_id("cmd"),
        run_id: running.id.clone(),
        workspace_id: "a".repeat(64),
        snapshot_sha256: "b".repeat(64),
        request_sha256: "c".repeat(64),
        backend_sha256: "d".repeat(64),
        boot_id: COMMAND_LAUNCH_BOOT.into(),
    };
    store.record_command_custody(&running, &custody).unwrap();
    let running = store.run(&running.id).unwrap().unwrap();
    if orphan {
        store.orphan_run_for_test(&running);
    }
    let (run, run_digest) = store.recovery_candidate(&running.id).unwrap().unwrap();
    let command_root = private::directory(&base.join("commands")).unwrap();
    let jobs = private::directory(&command_root.join("jobs")).unwrap();
    let job = private::directory(&jobs.join(custody.command_id.as_str())).unwrap();
    private::create(
        &job.join("custody.json"),
        &serde_json::to_vec(&custody).unwrap(),
    )
    .unwrap();
    private::create(&job.join("started.json"), b"{\"started\":true}").unwrap();
    let original = base.join("original.txt");
    std::fs::write(&original, b"original, never publish recovered edits").unwrap();
    RecoveryFixture {
        _directory: directory,
        store,
        run,
        run_digest,
        command_root,
        job,
        original,
    }
}

fn recorded_command_join(fixture: &RecoveryFixture) {
    let custody = fixture.run.command_custody.as_ref().unwrap();
    let changes = b"staged edit, never publish";
    private::create(&fixture.job.join("changes.json"), changes).unwrap();
    let raw = serde_json::to_vec(&json!({
        "version": 1, "custody": custody, "joined": true,
        "cgroup": {"path": format!("/sys/fs/cgroup/system.slice/xcb-command-{}.service/worker", custody.command_id), "dev":1, "ino":2},
        "exitCode": 0, "stdout": "ok", "stderr": "", "timedOut": false,
        "cancelled": false, "truncated": false, "error": null,
        "changesBase64": STANDARD.encode(changes), "changesSha256": digest(changes)
    })).unwrap();
    private::create(&fixture.job.join("guest-result.json"), &raw).unwrap();
    private::create(
        &fixture.job.join("guest-result-binding.json"),
        &serde_json::to_vec(&json!({
            "version": 1, "custody": custody, "resultSha256": digest(&raw)
        }))
        .unwrap(),
    )
    .unwrap();
    let outcome = CommandOutcome {
        custody: custody.clone(),
        joined: true,
        error: None,
        output: Some(CommandOutput {
            exit_code: Some(0),
            stdout: "ok".into(),
            stderr: String::new(),
            timed_out: false,
            cancelled: false,
            truncated: false,
            changes_path: Some(fixture.job.join("changes.json")),
            changes_sha256: Some(digest(changes)),
            cleanup_pending: false,
        }),
    };
    private::create(
        &fixture.job.join("recovered.json"),
        &serde_json::to_vec(&outcome).unwrap(),
    )
    .unwrap();
}

#[tokio::test]
async fn command_recovery_rechecks_changed_boot_and_exact_join_without_publishing() {
    let fixture = recovery_fixture(Some(COMMAND_LAUNCH_BOOT), true);
    recorded_command_join(&fixture);
    assert!(fixture.run.verify_recovery_stop().is_err());
    assert!(
        recover_in(
            &fixture.command_root,
            &fixture.store,
            &fixture.run,
            &fixture.run_digest,
            Some(COMMAND_LAUNCH_BOOT)
        )
        .await
        .is_err()
    );
    let result = recover_in(
        &fixture.command_root,
        &fixture.store,
        &fixture.run,
        &fixture.run_digest,
        Some(COMMAND_NEXT_BOOT),
    )
    .await
    .unwrap();
    assert!(result.command_custody.is_none());
    assert_eq!(result.phase, "running"); // Ordinary run recovery still owns the lease.
    assert_eq!(
        std::fs::read(&fixture.original).unwrap(),
        b"original, never publish recovered edits"
    );
    assert_eq!(
        std::fs::read(fixture.job.join("changes.json")).unwrap(),
        b"staged edit, never publish"
    );
    assert!(
        fixture
            .store
            .unsettled_runs()
            .unwrap()
            .iter()
            .any(|run| run.id == result.id)
    );
}

#[tokio::test]
async fn command_recovery_refuses_unusable_file_missing_guest_witness_live_worker_and_stale_revision()
 {
    for case in 0..5 {
        let fixture = recovery_fixture(
            if case == 2 {
                None
            } else {
                Some(COMMAND_LAUNCH_BOOT)
            },
            case != 3,
        );
        if case == 0 {
            private::create(&fixture.job.join("recovered.json"), b"{broken").unwrap();
        } else {
            recorded_command_join(&fixture);
        }
        if case == 1 {
            std::fs::remove_file(fixture.job.join("guest-result-binding.json")).unwrap();
        }
        let revision = if case == 4 {
            "stale"
        } else {
            &fixture.run_digest
        };
        let error = recover_in(
            &fixture.command_root,
            &fixture.store,
            &fixture.run,
            revision,
            Some(COMMAND_NEXT_BOOT),
        )
        .await
        .unwrap_err()
        .to_string();
        match case {
            0 | 1 => assert!(
                error.contains("recovery file")
                    && error.contains("custody-bound joined guest receipt"),
                "{error}"
            ),
            2 => assert!(
                error.contains("no recorded process group or run-bound boot witness"),
                "{error}"
            ),
            3 => assert!(error.contains("still running"), "{error}"),
            4 => assert!(
                error.contains("run changed since command recovery proof"),
                "{error}"
            ),
            _ => unreachable!(),
        }
        assert_eq!(
            fixture
                .store
                .run(&fixture.run.id)
                .unwrap()
                .unwrap()
                .command_custody,
            fixture.run.command_custody
        );
        assert_eq!(fixture.store.unsettled_runs().unwrap().len(), 1);
        assert_eq!(
            std::fs::read(&fixture.original).unwrap(),
            b"original, never publish recovered edits"
        );
        assert!(
            fixture
                .store
                .prepare_probe(&fixture.run.account, None, 4)
                .is_err()
        );
    }
}

#[test]
fn unsubmitted_discard_rechecks_run_bound_boot_without_relaxing_no_start_rule() {
    let fixture = recovery_fixture(Some(COMMAND_LAUNCH_BOOT), true);
    let custody = fixture.run.command_custody.as_ref().unwrap();
    // A boot witness cannot justify discarding a command that reached the guest.
    assert!(
        discard_unstarted_in(
            &fixture.command_root,
            &fixture.store,
            &fixture.run,
            &fixture.run_digest,
            Some(COMMAND_NEXT_BOOT)
        )
        .is_err()
    );
    assert!(!fixture.job.join("discarded.json").exists());
    std::fs::remove_file(fixture.job.join("started.json")).unwrap();
    assert!(
        discard_unstarted_in(
            &fixture.command_root,
            &fixture.store,
            &fixture.run,
            &fixture.run_digest,
            Some(COMMAND_LAUNCH_BOOT)
        )
        .is_err()
    );
    let settled = discard_unstarted_in(
        &fixture.command_root,
        &fixture.store,
        &fixture.run,
        &fixture.run_digest,
        Some(COMMAND_NEXT_BOOT),
    )
    .unwrap();
    assert!(settled.command_custody.is_none());
    assert!(fixture.job.join("discarded.json").exists());
    assert!(
        fixture
            .store
            .recover_run(&fixture.run.id, &fixture.run_digest, 4)
            .is_err()
    );
    assert_eq!(
        std::fs::read(&fixture.original).unwrap(),
        b"original, never publish recovered edits"
    );
    assert_eq!(custody.run_id, fixture.run.id);
}
