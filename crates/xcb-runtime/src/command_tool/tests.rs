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
    let base = directory.path().canonicalize().unwrap();
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
        &fixture
            ._directory
            .path()
            .canonicalize()
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
        &fixture
            ._directory
            .path()
            .canonicalize()
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
