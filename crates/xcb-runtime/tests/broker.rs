use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::Path,
};
use xcb_runtime::broker::Workspace;

/// The mode a plain `File::create` takes under the test process umask —
/// the same kernel-applied derivation workspace writes should use.
fn default_file_mode(dir: &Path) -> u32 {
    let probe = dir.join(".xcb-umask-probe");
    fs::File::create(&probe).unwrap();
    let mode = fs::metadata(&probe).unwrap().permissions().mode() & 0o777;
    fs::remove_file(&probe).unwrap();
    mode
}

/// The mode a plain `create_dir` takes under the test process umask.
fn default_dir_mode(dir: &Path) -> u32 {
    let probe = dir.join(".xcb-umask-probe-dir");
    fs::create_dir(&probe).unwrap();
    let mode = fs::metadata(&probe).unwrap().permissions().mode() & 0o777;
    fs::remove_dir(&probe).unwrap();
    mode
}

#[test]
fn created_workspace_entries_take_the_process_umask() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().canonicalize().unwrap();
    let root = base.join("work");
    fs::create_dir(&root).unwrap();
    let workspace = Workspace::open_with_coordination(&root, &base.join("coordination")).unwrap();
    let file_mode = default_file_mode(&base);
    let dir_mode = default_dir_mode(&base);
    workspace.write("created.txt", "new", None).unwrap();
    workspace.mkdir("made", false).unwrap();
    workspace.mkdir("a/b", true).unwrap();
    assert_eq!(
        fs::metadata(root.join("created.txt"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        file_mode
    );
    for path in ["made", "a", "a/b"] {
        assert_eq!(
            fs::metadata(root.join(path)).unwrap().permissions().mode() & 0o777,
            dir_mode,
            "{path}"
        );
    }
    // Replacing a file preserves its permission bits instead of
    // re-deriving them from the umask.
    fs::set_permissions(root.join("created.txt"), fs::Permissions::from_mode(0o640)).unwrap();
    let read = workspace.read("created.txt").unwrap();
    workspace
        .write("created.txt", "again", Some(&read.revision))
        .unwrap();
    assert_eq!(
        fs::metadata(root.join("created.txt"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o640
    );
}

#[test]
fn workspace_tools_are_descriptor_rooted_and_revision_checked() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().canonicalize().unwrap();
    let root = base.join("work");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("hello.txt"), "old").unwrap();
    let workspace = Workspace::open_with_coordination(&root, &base.join("coordination")).unwrap();
    let read = workspace.read("hello.txt").unwrap();
    assert_eq!(read.text, "old");
    workspace
        .write("hello.txt", "new", Some(&read.revision))
        .unwrap();
    assert!(
        workspace
            .write("hello.txt", "clobber", Some(&read.revision))
            .is_err()
    );
    assert!(workspace.write("hello.txt", "clobber", None).is_err());
    workspace.write("new.txt", "created", None).unwrap();
    assert_eq!(fs::read_to_string(root.join("hello.txt")).unwrap(), "new");
}

#[test]
fn workspace_symlinks_hardlinks_and_parent_paths_do_not_escape() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().canonicalize().unwrap();
    fs::create_dir(base.join("work")).unwrap();
    fs::write(base.join("private.txt"), "private").unwrap();
    symlink(base.join("private.txt"), base.join("work/link")).unwrap();
    fs::hard_link(base.join("private.txt"), base.join("work/hard")).unwrap();
    let workspace =
        Workspace::open_with_coordination(&base.join("work"), &base.join("coordination")).unwrap();
    for path in ["../private.txt", "/private.txt", "link", "hard"] {
        assert!(workspace.read(path).is_err(), "{path}");
    }
    assert!(workspace.write("../private.txt", "bad", None).is_err());
    assert_eq!(
        fs::read_to_string(base.join("private.txt")).unwrap(),
        "private"
    );
}

#[test]
fn concurrent_workspace_writers_have_one_winner_and_preserve_permissions() {
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let root = base.join("work");
    fs::create_dir(&root).unwrap();
    for existing in [false, true] {
        let name = if existing { "existing" } else { "new" };
        if existing {
            fs::write(root.join(name), "original").unwrap();
            fs::set_permissions(root.join(name), fs::Permissions::from_mode(0o755)).unwrap();
        }
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(20));
        let workers: Vec<_> = (0..20)
            .map(|index| {
                let workspace =
                    Workspace::open_with_coordination(&root, &base.join("coordination")).unwrap();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    let expected = existing.then(|| xcb_runtime::digest(b"original"));
                    barrier.wait();
                    workspace
                        .write(name, &format!("writer-{index}"), expected.as_deref())
                        .is_ok()
                })
            })
            .collect();
        let winners = workers
            .into_iter()
            .map(|worker| usize::from(worker.join().unwrap()))
            .sum::<usize>();
        assert_eq!(winners, 1);
        assert_eq!(
            fs::metadata(root.join(name)).unwrap().permissions().mode() & 0o777,
            if existing {
                0o755
            } else {
                default_file_mode(&base)
            }
        );
    }
}

#[test]
fn native_mutations_wait_for_bun_and_node_locks_and_survive_owner_exit() {
    for (runtime, operation) in ["bun", "node"].into_iter().flat_map(|runtime| {
        [
            "workspace_write",
            "workspace_mkdir",
            "workspace_remove",
            "workspace_rename",
        ]
        .into_iter()
        .map(move |operation| (runtime, operation))
    }) {
        let directory = tempfile::tempdir().unwrap();
        let base = directory.path().canonicalize().unwrap();
        let root = base.join("work");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("shared"), "original").unwrap();
        let coordination = base.join("coordination");
        let ready = base.join("ready");
        let module = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../src/cli/write-coordination.ts")
            .canonicalize()
            .unwrap();
        let script = r#"
            import { pathToFileURL } from 'node:url';
            import { writeFile } from 'node:fs/promises';
            const { withWorkspaceWriteLock } = await import(pathToFileURL(process.env.XCB_TEST_MODULE).href);
            await withWorkspaceWriteLock(process.env.XCB_TEST_WORKSPACE, process.env.XCB_TEST_COORDINATION, async () => {
                await writeFile(process.env.XCB_TEST_READY, 'ready');
                await new Promise(resolve => setTimeout(resolve, 30000));
            });
        "#;
        let mut command = std::process::Command::new(runtime);
        if runtime == "node" {
            command.args(["--experimental-strip-types", "--input-type=module"]);
        }
        let mut child = command
            .args(["-e", script])
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &base)
            .env("XCB_TEST_MODULE", module)
            .env("XCB_TEST_WORKSPACE", &root)
            .env("XCB_TEST_COORDINATION", &coordination)
            .env("XCB_TEST_READY", &ready)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !ready.exists() {
            if child.try_wait().unwrap().is_some() || std::time::Instant::now() >= deadline {
                let _ = child.kill();
                let output = child.wait_with_output().unwrap();
                panic!(
                    "{runtime} lock fixture failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let workspace = Workspace::open_with_coordination(&root, &coordination).unwrap();
        let expected = workspace.read("shared").unwrap().revision;
        let (sent, received) = std::sync::mpsc::channel();
        let writer = std::thread::spawn(move || {
            let arguments = match operation {
                "workspace_write" => {
                    serde_json::json!({"path":"shared", "text":"native", "expectedRevision":expected})
                }
                "workspace_mkdir" => serde_json::json!({"path":"created", "parents":false}),
                "workspace_remove" => {
                    serde_json::json!({"path":"shared", "expectedRevision":expected})
                }
                "workspace_rename" => {
                    serde_json::json!({"from":"shared", "to":"renamed", "expectedRevision":expected})
                }
                _ => unreachable!(),
            };
            sent.send(workspace.call(operation, &arguments)).unwrap();
        });
        let pending = received.recv_timeout(std::time::Duration::from_millis(150));
        child.kill().unwrap();
        child.wait().unwrap();
        let completed = if pending.is_ok() {
            None
        } else {
            Some(
                received
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .unwrap(),
            )
        };
        writer.join().unwrap();
        assert!(
            matches!(pending, Err(std::sync::mpsc::RecvTimeoutError::Timeout)),
            "{operation} bypassed the {runtime} writer"
        );
        completed.unwrap().unwrap();
        match operation {
            "workspace_write" => {
                assert_eq!(fs::read_to_string(root.join("shared")).unwrap(), "native")
            }
            "workspace_mkdir" => assert!(root.join("created").is_dir()),
            "workspace_remove" => assert!(!root.join("shared").exists()),
            "workspace_rename" => {
                assert!(!root.join("shared").exists());
                assert_eq!(
                    fs::read_to_string(root.join("renamed")).unwrap(),
                    "original"
                );
            }
            _ => unreachable!(),
        }
    }
}

#[test]
fn workspace_coordination_cannot_overlap_the_workspace() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    assert!(Workspace::open_with_coordination(&root, &root.join("locks")).is_err());
    assert!(Workspace::open_with_coordination(&root, root.parent().unwrap()).is_err());
}

#[test]
fn native_mcp_tool_calls_refuse_unknown_keys_and_tools() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = Workspace::open(&dir.path().canonicalize().unwrap()).unwrap();
    assert!(
        workspace
            .call("shell", &serde_json::json!({"command":"true"}))
            .is_err()
    );
    assert!(
        workspace
            .call(
                "workspace_read",
                &serde_json::json!({"path":"a","extra":true})
            )
            .is_err()
    );
}

#[test]
fn observed_workspace_effects_distinguish_rejection_from_publication() {
    use serde_json::json;
    use xcb_core::policy::EffectState;
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let root = base.join("work");
    fs::create_dir(&root).unwrap();
    let workspace = Workspace::open_with_coordination(&root, &base.join("coordination")).unwrap();
    let (created, effects) = workspace.call_observed(
        "workspace_write",
        &json!({"path":"file","text":"created","expectedRevision":null}),
    );
    assert!(created.is_ok());
    assert_eq!(effects, EffectState::Settled);
    for arguments in [
        json!({"path":"file","text":"clobber","expectedRevision":null}),
        json!({"path":"file","text":"clobber","expectedRevision":"stale"}),
        json!({"path":"../escape","text":"bad","expectedRevision":null}),
        json!({"path":"file","text":false,"expectedRevision":null}),
    ] {
        let (rejected, effects) = workspace.call_observed("workspace_write", &arguments);
        assert!(rejected.is_err());
        assert_eq!(effects, EffectState::None);
    }
    assert_eq!(fs::read_to_string(root.join("file")).unwrap(), "created");
    assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
}

#[test]
fn workspace_directory_and_file_operations_are_durable_revision_checked_and_no_clobber() {
    use serde_json::json;
    use std::os::unix::fs::MetadataExt;
    use xcb_core::policy::EffectState;
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let root = base.join("work");
    fs::create_dir(&root).unwrap();
    let workspace = Workspace::open_with_coordination(&root, &base.join("coordination")).unwrap();
    let (result, effects) = workspace.call_observed(
        "workspace_mkdir",
        &json!({"path":"src/nested", "parents":true}),
    );
    assert_eq!(result.unwrap()["created"], 2);
    assert_eq!(effects, EffectState::Settled);
    let (result, effects) = workspace.call_observed(
        "workspace_mkdir",
        &json!({"path":"src/nested", "parents":true}),
    );
    assert_eq!(result.unwrap()["created"], 0);
    assert_eq!(effects, EffectState::None);
    assert!(workspace.mkdir("missing/child", false).is_err());
    assert!(!root.join("missing").exists());
    assert!(workspace.mkdir("src", false).is_err());
    workspace.mkdir("destination", false).unwrap();
    fs::write(root.join("src/nested/file"), "contents").unwrap();
    fs::set_permissions(
        root.join("src/nested/file"),
        fs::Permissions::from_mode(0o751),
    )
    .unwrap();
    let before = fs::metadata(root.join("src/nested/file")).unwrap();
    let revision = workspace.read("src/nested/file").unwrap().revision;
    let (result, effects) = workspace.call_observed(
        "workspace_rename",
        &json!({"from":"src/nested/file", "to":"destination/file", "expectedRevision":revision}),
    );
    assert_eq!(result.unwrap()["revision"], revision);
    assert_eq!(effects, EffectState::Settled);
    assert!(!root.join("src/nested/file").exists());
    let after = fs::metadata(root.join("destination/file")).unwrap();
    assert_eq!(before.ino(), after.ino());
    assert_eq!(after.mode() & 0o777, 0o751);
    let (result, effects) = workspace.call_observed(
        "workspace_remove",
        &json!({"path":"destination/file", "expectedRevision":revision}),
    );
    assert_eq!(result.unwrap()["removed"], true);
    assert_eq!(effects, EffectState::Settled);
    assert!(!root.join("destination/file").exists());
    assert!(root.join("destination").is_dir());
}

#[test]
fn workspace_mutations_reject_escape_links_special_files_and_stale_revisions_without_effects() {
    use serde_json::json;
    use xcb_core::policy::EffectState;
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let root = base.join("work");
    fs::create_dir(&root).unwrap();
    fs::create_dir(base.join("outside")).unwrap();
    fs::write(base.join("outside/private"), "secret").unwrap();
    symlink(base.join("outside"), root.join("linked-directory")).unwrap();
    symlink(base.join("outside/private"), root.join("link")).unwrap();
    fs::hard_link(base.join("outside/private"), root.join("hard")).unwrap();
    fs::create_dir(root.join("directory")).unwrap();
    assert!(
        std::process::Command::new("/usr/bin/mkfifo")
            .arg(root.join("fifo"))
            .env_clear()
            .status()
            .unwrap()
            .success()
    );
    fs::write(root.join("source"), "original").unwrap();
    let workspace = Workspace::open_with_coordination(&root, &base.join("coordination")).unwrap();
    let revision = workspace.read("source").unwrap().revision;
    for path in [
        "../outside/private",
        "/outside/private",
        "linked-directory/private",
        "link",
        "hard",
        "directory",
        "fifo",
        ".",
    ] {
        for (name, input) in [
            (
                "workspace_remove",
                json!({"path":path,"expectedRevision":revision}),
            ),
            (
                "workspace_rename",
                json!({"from":path,"to":"new","expectedRevision":revision}),
            ),
        ] {
            let (result, effects) = workspace.call_observed(name, &input);
            assert!(result.is_err(), "{name}: {path}");
            assert_eq!(effects, EffectState::None, "{name}: {path}");
        }
    }
    for path in [
        "../escaped",
        "/escaped",
        "linked-directory/escaped",
        "link/escaped",
        "hard",
        "fifo",
        ".",
    ] {
        let (result, effects) =
            workspace.call_observed("workspace_mkdir", &json!({"path":path,"parents":true}));
        assert!(result.is_err(), "{path}");
        assert_eq!(effects, EffectState::None, "{path}");
    }
    for name in ["workspace_remove", "workspace_rename"] {
        for expected in ["stale".to_owned(), "0".repeat(64)] {
            let input = if name == "workspace_remove" {
                json!({"path":"source","expectedRevision":expected})
            } else {
                json!({"from":"source","to":"new","expectedRevision":expected})
            };
            let (result, effects) = workspace.call_observed(name, &input);
            assert!(result.is_err());
            assert_eq!(effects, EffectState::None);
        }
    }
    for to in [
        "source",
        "link",
        "hard",
        "directory",
        "fifo",
        "linked-directory/new",
        "../outside/new",
    ] {
        let (result, effects) = workspace.call_observed(
            "workspace_rename",
            &json!({"from":"source","to":to,"expectedRevision":revision}),
        );
        assert!(result.is_err(), "{to}");
        assert_eq!(effects, EffectState::None, "{to}");
    }
    assert_eq!(fs::read_to_string(root.join("source")).unwrap(), "original");
    assert_eq!(
        fs::read_to_string(base.join("outside/private")).unwrap(),
        "secret"
    );
    assert_eq!(fs::read_dir(base.join("outside")).unwrap().count(), 1);
    assert!(root.join("link").is_symlink());
}

#[test]
fn directory_partial_creation_remains_a_settled_effect_when_a_later_component_is_rejected() {
    use serde_json::json;
    use xcb_core::policy::EffectState;
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let root = base.join("work");
    fs::create_dir(&root).unwrap();
    let workspace = Workspace::open_with_coordination(&root, &base.join("coordination")).unwrap();
    let (result, effects) = workspace.call_observed(
        "workspace_mkdir",
        &json!({"path":format!("created/{}", "x".repeat(256)),"parents":true}),
    );
    assert!(result.is_err());
    assert_eq!(effects, EffectState::Settled);
    assert!(root.join("created").is_dir());
    let (result, effects) = workspace.call_observed(
        "workspace_mkdir",
        &json!({"path":vec!["d";65].join("/"),"parents":true}),
    );
    assert!(result.is_err());
    assert_eq!(effects, EffectState::None);
    assert!(!root.join("d").exists());
}

#[test]
fn new_workspace_tool_schemas_are_closed_and_require_explicit_mutation_preconditions() {
    use serde_json::json;
    use xcb_core::policy::EffectState;
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let root = base.join("work");
    fs::create_dir(&root).unwrap();
    let workspace = Workspace::open_with_coordination(&root, &base.join("coordination")).unwrap();
    for (name, args) in [
        ("workspace_mkdir", json!({"path":"dir"})),
        (
            "workspace_mkdir",
            json!({"path":"dir", "parents":true, "extra":true}),
        ),
        ("workspace_remove", json!({"path":"file"})),
        (
            "workspace_remove",
            json!({"path":"file", "expectedRevision":null}),
        ),
        (
            "workspace_remove",
            json!({"path":"file", "expectedRevision":"0".repeat(64),"recursive":true}),
        ),
        ("workspace_rename", json!({"from":"file","to":"new"})),
        (
            "workspace_rename",
            json!({"from":"file","to":"new","expectedRevision":null}),
        ),
        (
            "workspace_rename",
            json!({"from":"file","to":"new","expectedRevision":"0".repeat(64),"overwrite":true}),
        ),
    ] {
        let (result, effects) = workspace.call_observed(name, &args);
        assert!(result.is_err());
        assert_eq!(effects, EffectState::None);
    }
    for descriptor in xcb_runtime::broker::descriptors() {
        assert_eq!(descriptor["inputSchema"]["additionalProperties"], false);
    }
    assert_eq!(fs::read_dir(root).unwrap().count(), 0);
}

#[test]
fn concurrent_renames_never_clobber_the_destination_or_lose_a_losing_source() {
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let root = base.join("work");
    fs::create_dir(&root).unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(12));
    let workers: Vec<_> = (0..12)
        .map(|index| {
            let source = format!("source-{index}");
            let contents = format!("contents-{index}");
            fs::write(root.join(&source), &contents).unwrap();
            let workspace =
                Workspace::open_with_coordination(&root, &base.join("coordination")).unwrap();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                (
                    source.clone(),
                    contents.clone(),
                    workspace
                        .rename(&source, "destination", &xcb_runtime::digest(&contents))
                        .is_ok(),
                )
            })
        })
        .collect();
    let mut winners = 0;
    for worker in workers {
        let (source, contents, won) = worker.join().unwrap();
        if won {
            winners += 1;
            assert!(!root.join(source).exists());
            assert_eq!(
                fs::read_to_string(root.join("destination")).unwrap(),
                contents
            );
        } else {
            assert_eq!(fs::read_to_string(root.join(source)).unwrap(), contents);
        }
    }
    assert_eq!(winners, 1);
    assert_eq!(fs::read_dir(root).unwrap().count(), 12);
}

#[test]
fn workspace_mutations_refuse_a_replaced_root_without_touching_either_tree() {
    use serde_json::json;
    use xcb_core::policy::EffectState;
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let root = base.join("work");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("file"), "original").unwrap();
    let workspace = Workspace::open_with_coordination(&root, &base.join("coordination")).unwrap();
    let revision = workspace.read("file").unwrap().revision;
    fs::rename(&root, base.join("moved")).unwrap();
    fs::create_dir(&root).unwrap();
    fs::write(root.join("file"), "replacement").unwrap();
    for (name, input) in [
        ("workspace_mkdir", json!({"path":"new","parents":true})),
        (
            "workspace_remove",
            json!({"path":"file","expectedRevision":revision}),
        ),
        (
            "workspace_rename",
            json!({"from":"file","to":"new","expectedRevision":revision}),
        ),
    ] {
        let (result, effects) = workspace.call_observed(name, &input);
        assert!(result.is_err());
        assert_eq!(effects, EffectState::None);
    }
    assert_eq!(
        fs::read_to_string(root.join("file")).unwrap(),
        "replacement"
    );
    assert_eq!(
        fs::read_to_string(base.join("moved/file")).unwrap(),
        "original"
    );
    assert_eq!(fs::read_dir(root).unwrap().count(), 1);
    assert_eq!(fs::read_dir(base.join("moved")).unwrap().count(), 1);
}

#[test]
fn workspace_list_truncates_at_the_entry_bound_instead_of_failing() {
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let root = base.join("work");
    fs::create_dir(&root).unwrap();
    for index in 0..513 {
        fs::write(root.join(format!("f-{index:04}")), "x").unwrap();
    }
    fs::create_dir(root.join("nested")).unwrap();
    let workspace = Workspace::open_with_coordination(&root, &base.join("coordination")).unwrap();
    let listing = workspace.list(".").unwrap();
    assert_eq!(listing.entries.len(), 512);
    assert!(listing.truncated);
    // The page is the sorted prefix and includes directory kinds.
    assert_eq!(listing.entries[0].name, "f-0000");
    assert_eq!(listing.entries[0].kind, "file");
    assert_eq!(listing.entries[511].name, "f-0511");
    let small = workspace.list("nested").unwrap();
    assert_eq!(small.entries.len(), 0);
    assert!(!small.truncated);
    // The tool-level call returns the same {entries, truncated} object.
    let value = workspace
        .call("workspace_list", &serde_json::json!({"path":"."}))
        .unwrap();
    assert_eq!(value["truncated"], true);
    assert_eq!(value["entries"].as_array().unwrap().len(), 512);
}

#[test]
fn workspace_search_matches_truncates_and_skips_binary_and_vendor_dirs() {
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let root = base.join("work");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("needle.txt"), "first needle\nsecond needle\n").unwrap();
    fs::write(root.join("binary.bin"), b"\xff\xfe needle \x00").unwrap();
    for skipped in [".git", "node_modules", "target"] {
        fs::create_dir(root.join(skipped)).unwrap();
        fs::write(root.join(skipped).join("hidden.txt"), "needle").unwrap();
    }
    let workspace = Workspace::open_with_coordination(&root, &base.join("coordination")).unwrap();
    let result = workspace.search(".", "needle").unwrap();
    assert_eq!(result["truncated"], false);
    let matches = result["matches"].as_array().unwrap();
    assert_eq!(matches.len(), 2);
    assert_eq!(matches[0]["path"], "needle.txt");
    assert_eq!(matches[0]["line"], 1);
    assert_eq!(matches[1]["line"], 2);
    // A tree with a >512-entry directory truncates rather than failing.
    fs::create_dir(root.join("bulk")).unwrap();
    for index in 0..513 {
        fs::write(root.join("bulk").join(format!("f-{index:04}")), "plain").unwrap();
    }
    let result = workspace.search(".", "needle").unwrap();
    assert_eq!(result["truncated"], true);
    assert_eq!(result["matches"].as_array().unwrap().len(), 2);
    // A query absent everywhere is a settled empty result.
    let result = workspace.search(".", "absent-everywhere").unwrap();
    assert_eq!(result["matches"].as_array().unwrap().len(), 0);
}

#[test]
fn workspace_read_rejects_oversized_files_with_a_guided_tool_error() {
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let root = base.join("work");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("big.txt"), "\n".repeat(256 * 1024)).unwrap();
    let workspace = Workspace::open_with_coordination(&root, &base.join("coordination")).unwrap();
    let error = workspace.read("big.txt").expect_err("oversized read");
    assert!(
        error.to_string().contains("exceeds the 128 KiB read limit"),
        "{error}"
    );
    // A file at the read bound still succeeds and reports a revision.
    fs::write(root.join("edge.txt"), "x".repeat(128 * 1024)).unwrap();
    let edge = workspace.read("edge.txt").unwrap();
    assert_eq!(edge.text.len(), 128 * 1024);
    assert_eq!(edge.revision.len(), 64);
    // The tool-level call returns the same guided error.
    let error = workspace
        .call("workspace_read", &serde_json::json!({"path":"big.txt"}))
        .expect_err("oversized call");
    assert!(error.to_string().contains("exceeds the 128 KiB read limit"));
}
