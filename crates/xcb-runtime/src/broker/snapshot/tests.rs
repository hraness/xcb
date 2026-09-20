use super::*;
use std::{fs, os::unix::fs::symlink, path::PathBuf};

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    workspace: Workspace,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let root = base.join("workspace");
        fs::create_dir(&root).unwrap();
        let workspace =
            Workspace::open_with_coordination(&root, &base.join("coordination")).unwrap();
        Self {
            _temp: temp,
            root,
            workspace,
        }
    }
    fn write(&self, path: &str, bytes: &[u8], mode: u32) {
        let path = self.root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, bytes).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }
    fn changes(&self, snapshot: &CommandSnapshot, changes: Vec<CommandChange>) -> CommandChanges {
        CommandChanges {
            version: 1,
            workspace_id: snapshot.document.workspace_id.clone(),
            changes,
        }
    }
}
fn write(path: &str, bytes: &[u8], executable: bool) -> CommandChange {
    CommandChange {
        path: path.into(),
        base64: Some(STANDARD.encode(bytes)),
        sha256: Some(digest(bytes)),
        executable,
    }
}
fn remove(path: &str) -> CommandChange {
    CommandChange {
        path: path.into(),
        base64: None,
        sha256: None,
        executable: false,
    }
}

#[test]
fn snapshot_round_trips_binary_and_excludes_controls_dependencies_and_secrets() {
    let fixture = Fixture::new();
    fixture.write("src/bytes.bin", &[0, 255, 128, b'\n'], 0o640);
    fixture.write("scripts/check", b"#!/bin/sh\n", 0o750);
    fixture.write(".env.example", b"SYNTHETIC=value\n", 0o600);
    for path in [
        ".git/config",
        "node_modules/pkg/index.js",
        "target/debug/object",
        "nested/.env.local",
        ".env",
        ".codex/auth.json",
        ".xcb-command-synthetic",
    ] {
        fixture.write(path, b"excluded fixture", 0o600);
    }
    fs::create_dir_all(fixture.root.join("empty/nested")).unwrap();
    let snapshot = fixture.workspace.command_snapshot().unwrap();
    assert_eq!(snapshot.document.version, 1);
    assert_eq!(snapshot.document.files.len(), 3);
    assert_eq!(
        snapshot.document.workspace_id,
        digest(fixture.root.to_str().unwrap())
    );
    let binary = snapshot
        .document
        .files
        .iter()
        .find(|file| file.path == "src/bytes.bin")
        .unwrap();
    assert_eq!(
        STANDARD.decode(&binary.base64).unwrap(),
        [0, 255, 128, b'\n']
    );
    assert_eq!(binary.sha256, digest([0, 255, 128, b'\n']));
    assert!(!binary.executable);
    assert!(
        snapshot
            .document
            .files
            .iter()
            .find(|file| file.path == "scripts/check")
            .unwrap()
            .executable
    );
    assert!(
        snapshot
            .document
            .directories
            .contains(&"empty/nested".into())
    );
    assert_eq!(snapshot.excluded.len(), 7);
    let saved = private::directory(
        &fixture
            ._temp
            .path()
            .canonicalize()
            .unwrap()
            .join("private-output"),
    )
    .unwrap()
    .join("snapshot.json");
    let hash = snapshot.save(&saved).unwrap();
    let bytes = private::read(&saved, 128 * 1024).unwrap();
    assert_eq!(hash, digest(&bytes));
    let parsed: SnapshotDocument = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(parsed.files.len(), 3);
    assert!(
        !bytes
            .windows(b"excluded fixture".len())
            .any(|chunk| chunk == b"excluded fixture")
    );
}

#[test]
fn snapshot_rejects_symlinks_hardlinks_and_oversized_files() {
    for kind in ["symlink", "hardlink", "oversized"] {
        let fixture = Fixture::new();
        fixture.write("input", b"unchanged", 0o600);
        match kind {
            "symlink" => symlink("input", fixture.root.join("alias")).unwrap(),
            "hardlink" => {
                fs::hard_link(fixture.root.join("input"), fixture.root.join("alias")).unwrap()
            }
            _ => File::create(fixture.root.join("large"))
                .unwrap()
                .set_len(SNAPSHOT_FILE_LIMIT as u64 + 1)
                .unwrap(),
        }
        assert!(fixture.workspace.command_snapshot().is_err(), "{kind}");
        assert_eq!(fs::read(fixture.root.join("input")).unwrap(), b"unchanged");
    }
}

#[test]
fn command_publication_updates_creates_removes_and_preserves_permission_bits() {
    let fixture = Fixture::new();
    fixture.write("existing", b"old", 0o640);
    fixture.write("script", b"old script", 0o751);
    fixture.write("remove", b"remove me", 0o600);
    let snapshot = fixture.workspace.command_snapshot().unwrap();
    let changes = fixture.changes(
        &snapshot,
        vec![
            write("existing", &[255, 0, 1], false),
            write("script", b"new script", false),
            write("new/nested/program", b"new program", true),
            remove("remove"),
        ],
    );
    let (result, effects) = fixture
        .workspace
        .publish_command_changes(&snapshot, changes);
    let published = result.unwrap();
    assert_eq!(effects, EffectState::Settled);
    assert_eq!(
        (
            published.written,
            published.removed,
            published.directories_created
        ),
        (3, 1, 2)
    );
    assert_eq!(
        fs::read(fixture.root.join("existing")).unwrap(),
        [255, 0, 1]
    );
    assert_eq!(
        fs::metadata(fixture.root.join("existing")).unwrap().mode() & 0o777,
        0o640
    );
    assert_eq!(
        fs::metadata(fixture.root.join("script")).unwrap().mode() & 0o777,
        0o640
    );
    assert_eq!(
        fs::metadata(fixture.root.join("new/nested/program"))
            .unwrap()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(fixture.root.join("new/nested"))
            .unwrap()
            .mode()
            & 0o777,
        0o700
    );
    assert!(!fixture.root.join("remove").exists());
    assert_eq!(
        fixture
            .workspace
            .command_snapshot()
            .unwrap()
            .document
            .files
            .len(),
        3
    );
}

#[test]
fn command_publication_checks_every_revision_before_any_effect() {
    for changed in ["bytes", "mode", "removed", "created"] {
        let fixture = Fixture::new();
        fixture.write("first", b"first", 0o600);
        fixture.write("second", b"second", 0o600);
        let snapshot = fixture.workspace.command_snapshot().unwrap();
        let target = if changed == "created" {
            "new"
        } else {
            "second"
        };
        match changed {
            "bytes" => fixture.write("second", b"editor change", 0o600),
            "mode" => fs::set_permissions(
                fixture.root.join("second"),
                fs::Permissions::from_mode(0o700),
            )
            .unwrap(),
            "removed" => fs::remove_file(fixture.root.join("second")).unwrap(),
            _ => fixture.write("new", b"created by editor", 0o600),
        }
        let changes = fixture.changes(
            &snapshot,
            vec![
                write("first", b"must not publish", false),
                write(target, b"staged", false),
            ],
        );
        let (result, effects) = fixture
            .workspace
            .publish_command_changes(&snapshot, changes);
        assert!(result.is_err(), "{changed}");
        assert_eq!(effects, EffectState::None, "{changed}");
        assert_eq!(fs::read(fixture.root.join("first")).unwrap(), b"first");
    }
}

#[test]
fn command_publication_rejects_malformed_paths_identity_and_payloads_without_effects() {
    let fixture = Fixture::new();
    let snapshot = fixture.workspace.command_snapshot().unwrap();
    let invalid = [
        write("../escape", b"bad", false),
        write("/absolute", b"bad", false),
        write(".git/config", b"bad", false),
        write("src/.env.production", b"bad", false),
        write(".xcb-reserved", b"bad", false),
        remove("absent"),
        CommandChange {
            path: "output".into(),
            base64: Some("%%%".into()),
            sha256: Some("a".repeat(64)),
            executable: false,
        },
        CommandChange {
            path: "output".into(),
            base64: Some(STANDARD.encode(b"bytes")),
            sha256: Some(digest(b"different")),
            executable: false,
        },
        CommandChange {
            path: "output".into(),
            base64: Some(STANDARD.encode(b"bytes")),
            sha256: None,
            executable: false,
        },
        write(
            &std::iter::repeat_n("d", DEPTH_LIMIT + 1)
                .collect::<Vec<_>>()
                .join("/"),
            b"deep",
            false,
        ),
    ];
    for change in invalid {
        let (result, effects) = fixture
            .workspace
            .publish_command_changes(&snapshot, fixture.changes(&snapshot, vec![change]));
        assert!(result.is_err());
        assert_eq!(effects, EffectState::None);
    }
    let mut malformed = fixture.changes(&snapshot, vec![write("output", b"bad", false)]);
    malformed.workspace_id = "a".repeat(64);
    let mut wrong_version = fixture.changes(&snapshot, vec![]);
    wrong_version.version = 2;
    for changes in [
        malformed,
        wrong_version,
        fixture.changes(
            &snapshot,
            vec![
                write("duplicate", b"one", false),
                write("duplicate", b"two", false),
            ],
        ),
        fixture.changes(
            &snapshot,
            (0..513)
                .map(|i| write(&format!("file-{i}"), b"", false))
                .collect(),
        ),
    ] {
        let (result, effects) = fixture
            .workspace
            .publish_command_changes(&snapshot, changes);
        assert!(result.is_err());
        assert_eq!(effects, EffectState::None);
    }
    assert!(fs::read_dir(&fixture.root).unwrap().next().is_none());
}

#[test]
fn retained_parent_identity_rejects_replacement_and_cas_reads_the_retained_directory() {
    let fixture = Fixture::new();
    fixture.write("directory/input", b"original", 0o600);
    let snapshot = fixture.workspace.command_snapshot().unwrap();
    let (retained, name) = fixture.workspace.parent("directory/input").unwrap();
    fs::rename(fixture.root.join("directory"), fixture.root.join("moved")).unwrap();
    fixture.write("directory/input", b"original", 0o600);
    fixture.write("moved/input", b"changed retained file", 0o600);
    // The replacement named parent has matching bytes; it must not be used as
    // the CAS witness for an operation through the retained parent descriptor.
    assert!(
        fixture
            .workspace
            .command_current(&snapshot, "directory/input")
            .is_ok()
    );
    assert!(command_current_at(&snapshot, "directory/input", &retained, name).is_err());
    assert!(
        fixture
            .workspace
            .command_parent_current("directory/input", &retained)
            .is_err()
    );
    assert_eq!(
        fs::read(fixture.root.join("directory/input")).unwrap(),
        b"original"
    );
    assert_eq!(
        fs::read(fixture.root.join("moved/input")).unwrap(),
        b"changed retained file"
    );
}

#[test]
fn snapshot_global_entry_budget_includes_excluded_names_across_directories() {
    let fixture = Fixture::new();
    for directory in ["one", "two"] {
        fs::create_dir(fixture.root.join(directory)).unwrap();
        for index in 0..SNAPSHOT_ENTRY_LIMIT / 2 {
            File::create(
                fixture
                    .root
                    .join(directory)
                    .join(format!(".xcb-excluded-{index}")),
            )
            .unwrap();
        }
    }
    let error = fixture
        .workspace
        .command_snapshot()
        .err()
        .expect("global visited entry limit");
    assert!(error.to_string().contains("entry limit"));
}

#[test]
fn snapshot_and_publication_share_the_same_path_depth_bound() {
    let fixture = Fixture::new();
    let allowed = std::iter::repeat_n("d", DEPTH_LIMIT - 1)
        .collect::<Vec<_>>()
        .join("/");
    fixture.write(&format!("{allowed}/input"), b"bounded", 0o600);
    let snapshot = fixture.workspace.command_snapshot().unwrap();
    assert_eq!(snapshot.document.files.len(), 1);
    let (result, effects) = fixture.workspace.publish_command_changes(
        &snapshot,
        fixture.changes(
            &snapshot,
            vec![write(&format!("{allowed}/input"), b"updated", false)],
        ),
    );
    assert!(result.is_ok());
    assert_eq!(effects, EffectState::Settled);
    fixture.write(&format!("{allowed}/deeper/input"), b"too deep", 0o600);
    let error = fixture
        .workspace
        .command_snapshot()
        .err()
        .expect("snapshot path depth limit");
    assert!(error.to_string().contains("depth limit"));
}

#[test]
fn command_publication_rejects_post_snapshot_symlink_targets_without_writing_outside() {
    for parent_alias in [false, true] {
        let fixture = Fixture::new();
        let outside = fixture._temp.path().canonicalize().unwrap().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("input"), b"outside canary").unwrap();
        let snapshot = fixture.workspace.command_snapshot().unwrap();
        let target = if parent_alias {
            symlink(&outside, fixture.root.join("alias")).unwrap();
            "alias/input"
        } else {
            symlink(outside.join("input"), fixture.root.join("alias")).unwrap();
            "alias"
        };
        let (result, effects) = fixture.workspace.publish_command_changes(
            &snapshot,
            fixture.changes(&snapshot, vec![write(target, b"must not escape", false)]),
        );
        assert!(result.is_err());
        assert_eq!(effects, EffectState::None);
        assert_eq!(fs::read(outside.join("input")).unwrap(), b"outside canary");
    }
}

#[test]
fn command_publication_enforces_per_file_and_total_decoded_byte_limits_before_effects() {
    let fixture = Fixture::new();
    let snapshot = fixture.workspace.command_snapshot().unwrap();
    let oversized = vec![0; SNAPSHOT_FILE_LIMIT + 1];
    let bounded = vec![0; SNAPSHOT_FILE_LIMIT];
    for changes in [
        vec![write("oversized", &oversized, false)],
        (0..(CHANGE_BYTE_LIMIT / SNAPSHOT_FILE_LIMIT + 1))
            .map(|index| write(&format!("file-{index}"), &bounded, false))
            .collect(),
    ] {
        let (result, effects) = fixture
            .workspace
            .publish_command_changes(&snapshot, fixture.changes(&snapshot, changes));
        assert!(result.is_err());
        assert_eq!(effects, EffectState::None);
        assert!(fs::read_dir(&fixture.root).unwrap().next().is_none());
    }
}

#[test]
fn git_absent_or_unsupported_does_not_block_offline_commands() {
    let fixture = Fixture::new();
    fixture.write("source.py", b"print(1)\n", 0o600);
    let absent = fixture.workspace.command_snapshot().unwrap();
    assert!(absent.document.git.is_none());
    assert!(absent.git_unavailable.is_none());
    fixture.write(".git", b"gitdir: /untrusted/private/git\n", 0o600);
    let refused = fixture.workspace.command_snapshot().unwrap();
    assert!(refused.document.git.is_none());
    assert!(refused.git_unavailable.unwrap().contains("No Git metadata"));
    assert_eq!(refused.document.files.len(), 1);
    let serialized = serde_json::to_string(&refused.document).unwrap();
    assert!(!serialized.contains("untrusted"));
    assert!(!serialized.contains("\"git\""));
}

#[test]
fn bounded_unborn_git_is_optional_private_snapshot_data() {
    let fixture = Fixture::new();
    fixture.write("source.py", b"print(1)\n", 0o600);
    fixture.write(".git/HEAD", b"ref: refs/heads/main\n", 0o600);
    fs::create_dir(fixture.root.join(".git/objects")).unwrap();
    let captured = fixture.workspace.command_snapshot().unwrap();
    assert!(captured.git_unavailable.is_none());
    let git = captured.document.git.as_ref().unwrap();
    assert!(git.head_object_id.is_none());
    assert_eq!(git.files.len(), 1);
    assert_eq!(git.files[0].path, "HEAD");
    assert_eq!(captured.document.files.len(), 1);
}
