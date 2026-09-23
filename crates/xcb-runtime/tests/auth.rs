use xcb_core::Provider;
use xcb_runtime::{auth, private, store::Store};

#[test]
fn importing_one_explicit_legacy_token_preserves_the_source() {
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let source = private::directory(&base.join("legacy")).unwrap();
    let fixture = b"sk-ant-oat01-synthetic_fixture_not_a_real_token";
    private::create(&source.join("claude-oauth-token"), fixture).unwrap();
    let store = Store::open(&base.join("state")).unwrap();
    let account = auth::import_agentmixer_token(&store, &source).unwrap();
    assert!(auth::has_token(&store, &account).unwrap());
    assert_eq!(
        private::read(&source.join("claude-oauth-token"), 2048).unwrap(),
        fixture
    );
    let other = store.add_account(Provider::Claude, "Max", 1, None).unwrap();
    assert!(!auth::has_token(&store, &other.id).unwrap());
    auth::store_token(&store, &account, fixture).unwrap();
}

#[test]
fn tokens_rotate_atomically_and_invalid_input_preserves_the_current_credential() {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let store = Store::open(&base.join("state")).unwrap();
    let account = store
        .add_account(Provider::Claude, "Test", 1, None)
        .unwrap();
    let previous = b"sk-ant-oat01-previous_synthetic_fixture_not_real";
    let rotated = b"sk-ant-oat01-rotated_synthetic_fixture_not_real";
    auth::store_token(&store, &account.id, previous).unwrap();
    auth::store_token(&store, &account.id, rotated).unwrap();
    let path = store
        .account_root(&account.id)
        .unwrap()
        .join("subscription-token");
    assert_eq!(private::read(&path, 2048).unwrap(), rotated);
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let db = rusqlite::Connection::open(store.root().join("xcb.sqlite")).unwrap();
    let count = |table: &str| -> i64 {
        db.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap()
    };
    let before = (count("runs"), count("tool_effects"));
    for invalid in [b"invalid".to_vec(), vec![0xff], vec![b' '; 2049]] {
        assert!(auth::store_token(&store, &account.id, &invalid).is_err());
        assert_eq!(private::read(&path, 2048).unwrap(), rotated);
        assert_eq!((count("runs"), count("tool_effects")), before);
    }
    assert!(store.unsettled_runs().unwrap().is_empty());
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM tool_effects WHERE call='xcb_claude_auth_store' AND operation='host_auth_import' AND settled=1", [], |row| row.get::<_, i64>(0)).unwrap(),
        2,
    );
}

#[test]
fn credential_rotation_rejects_symlink_targets() {
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let store = Store::open(&base.join("state")).unwrap();
    let account = store
        .add_account(Provider::Claude, "Test", 1, None)
        .unwrap();
    let sibling = private::directory(&base.join("sibling")).unwrap();
    let target = sibling.join("another-account-token");
    let previous = b"sk-ant-oat01-previous_synthetic_fixture_not_real";
    private::create(&target, previous).unwrap();
    let path = store
        .account_root(&account.id)
        .unwrap()
        .join("subscription-token");
    std::os::unix::fs::symlink(&target, &path).unwrap();
    assert!(
        auth::store_token(
            &store,
            &account.id,
            b"sk-ant-oat01-rotated_synthetic_fixture_not_real"
        )
        .is_err()
    );
    assert_eq!(private::read(&target, 2048).unwrap(), previous);
    assert!(store.unsettled_runs().unwrap().is_empty());
}

#[test]
fn claude_token_rotation_respects_busy_and_disabled_accounts() {
    for busy in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let base = directory.path().canonicalize().unwrap();
        let store = Store::open(&base.join("state")).unwrap();
        let account = store.add_account(Provider::Claude, "Max", 1, None).unwrap();
        let original = b"sk-ant-oat01-original_synthetic_fixture_not_real";
        auth::store_token(&store, &account.id, original).unwrap();
        let held = if busy {
            let workspace = private::directory(&base.join("workspace")).unwrap();
            let model = xcb_core::models::ModelChoice {
                provider: Provider::Claude,
                id: xcb_core::Id::new("claude-synthetic").unwrap(),
                label: "Synthetic".into(),
                mode: xcb_core::models::Mode::Fixed,
                resolved: None,
                effort: None,
                observed_at_ms: 1,
            };
            let session = store
                .create_session(&account.id, model, &workspace, 2)
                .unwrap();
            Some(store.prepare_run(&session.id, session.revision, 3).unwrap())
        } else {
            store.set_account_enabled(&account.id, false).unwrap();
            None
        };
        let db = rusqlite::Connection::open(store.root().join("xcb.sqlite")).unwrap();
        let before: i64 = db
            .query_row("SELECT COUNT(*) FROM runs", [], |row| row.get(0))
            .unwrap();
        assert!(
            auth::store_token(
                &store,
                &account.id,
                b"sk-ant-oat01-replacement_synthetic_fixture_not_real"
            )
            .is_err()
        );
        let target = store
            .account_root(&account.id)
            .unwrap()
            .join("subscription-token");
        assert_eq!(private::read(&target, 2048).unwrap(), original);
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM runs", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            before
        );
        let unsettled = store.unsettled_runs().unwrap();
        assert_eq!(unsettled.len(), usize::from(busy));
        if let Some(run) = held {
            assert_eq!(unsettled[0].id, run.id);
        }
    }
}

#[test]
fn claude_token_receipt_failures_release_only_before_publication() {
    for after_publication in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let base = directory.path().canonicalize().unwrap();
        let store = Store::open(&base.join("state")).unwrap();
        let account = store.add_account(Provider::Claude, "Max", 1, None).unwrap();
        let original = b"sk-ant-oat01-original_synthetic_fixture_not_real";
        let replacement = b"sk-ant-oat01-replacement_synthetic_fixture_not_real";
        auth::store_token(&store, &account.id, original).unwrap();
        let db = rusqlite::Connection::open(store.root().join("xcb.sqlite")).unwrap();
        db.execute_batch(if after_publication {
            "CREATE TRIGGER fail_claude_receipt BEFORE UPDATE OF settled ON tool_effects WHEN OLD.call='xcb_claude_auth_store' BEGIN SELECT RAISE(ABORT, 'synthetic receipt failure'); END;"
        } else {
            "CREATE TRIGGER fail_claude_receipt BEFORE INSERT ON tool_effects WHEN NEW.call='xcb_claude_auth_store' BEGIN SELECT RAISE(ABORT, 'synthetic receipt failure'); END;"
        }).unwrap();
        assert!(auth::store_token(&store, &account.id, replacement).is_err());
        let target = store
            .account_root(&account.id)
            .unwrap()
            .join("subscription-token");
        assert_eq!(
            private::read(&target, 2048).unwrap(),
            if after_publication {
                replacement.as_slice()
            } else {
                original.as_slice()
            }
        );
        let unsettled = store.unsettled_runs().unwrap();
        assert_eq!(unsettled.len(), usize::from(after_publication));
        let pending: i64 = db.query_row("SELECT COUNT(*) FROM tool_effects WHERE call='xcb_claude_auth_store' AND settled=0", [], |row| row.get(0)).unwrap();
        assert_eq!(pending, i64::from(after_publication));
        if after_publication {
            assert!(auth::store_token(&store, &account.id, original).is_err());
            assert_eq!(private::read(&target, 2048).unwrap(), replacement);
        }
    }
}

fn codex_auth_fixture(account: &str, user: &str, refresh: &str) -> Vec<u8> {
    use base64::Engine;
    let claims = serde_json::json!({"sub":user,"https://api.openai.com/auth":{"chatgpt_account_id":account,"chatgpt_user_id":user}});
    let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(serde_json::to_vec(&claims).unwrap());
    serde_json::to_vec(&serde_json::json!({
        "auth_mode":"chatgpt", "OPENAI_API_KEY":null,
        "tokens":{"id_token":format!("e30.{encoded}.c3ludGhldGlj"),"access_token":"synthetic-access-only","refresh_token":refresh,"account_id":account},
        "last_refresh":"2026-09-19T00:00:00Z"
    })).unwrap()
}

fn codex_run(
    store: &Store,
    account: &xcb_core::Id,
    workspace: &std::path::Path,
) -> xcb_runtime::store::RunRecord {
    std::fs::create_dir_all(workspace).unwrap();
    let model = xcb_core::models::ModelChoice {
        provider: Provider::Codex,
        id: xcb_core::Id::new("gpt-6-astra").unwrap(),
        label: "Codex fixture".into(),
        mode: xcb_core::models::Mode::Fixed,
        resolved: None,
        effort: None,
        observed_at_ms: 1,
    };
    let session = store.create_session(account, model, workspace, 2).unwrap();
    store.prepare_run(&session.id, session.revision, 3).unwrap()
}

#[test]
fn codex_auth_import_is_explicit_private_provider_scoped_and_preserves_its_source() {
    use std::os::unix::fs::MetadataExt;
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let source = private::directory(&base.join("source"))
        .unwrap()
        .join("auth.json");
    let bytes = codex_auth_fixture("account-one", "user-one", "synthetic-refresh-one");
    private::create(&source, &bytes).unwrap();
    let store = Store::open(&base.join("state")).unwrap();
    let account = store
        .add_account(Provider::Codex, "ChatGPT", 1, None)
        .unwrap();
    let other = store
        .add_account(Provider::Codex, "ChatGPT", 1, None)
        .unwrap();
    let claude = store.add_account(Provider::Claude, "Max", 1, None).unwrap();
    let devin = store.add_account(Provider::Devin, "Core", 1, None).unwrap();
    assert!(!auth::has_credentials(&store, &account.id).unwrap());
    auth::import_codex_auth(&store, &account.id, &source).unwrap();
    assert!(auth::has_credentials(&store, &account.id).unwrap());
    assert!(!auth::has_token(&store, &account.id).unwrap());
    for id in [&other.id, &claude.id, &devin.id] {
        assert!(!auth::has_credentials(&store, id).unwrap());
    }
    assert!(auth::import_codex_auth(&store, &claude.id, &source).is_err());
    assert!(auth::import_codex_auth(&store, &devin.id, &source).is_err());
    let target = store
        .account_root(&account.id)
        .unwrap()
        .join("profile/auth.json");
    assert_eq!(private::read(&source, 65536).unwrap(), bytes);
    assert_eq!(private::read(&target, 65536).unwrap(), bytes);
    let metadata = std::fs::metadata(target).unwrap();
    assert_eq!(metadata.mode() & 0o777, 0o600);
    assert_eq!(metadata.nlink(), 1);
    assert!(store.unsettled_runs().unwrap().is_empty());
}

#[test]
fn codex_auth_import_rejects_links_public_files_invalid_json_and_api_keys() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let source = private::directory(&base.join("source"))
        .unwrap()
        .join("auth.json");
    let good = codex_auth_fixture("account-one", "user-one", "synthetic-refresh-one");
    private::create(&source, &good).unwrap();
    let store = Store::open(&base.join("state")).unwrap();
    let account = store
        .add_account(Provider::Codex, "ChatGPT", 1, None)
        .unwrap();
    std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(auth::import_codex_auth(&store, &account.id, &source).is_err());
    std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::hard_link(&source, source.with_file_name("hard")).unwrap();
    assert!(auth::import_codex_auth(&store, &account.id, &source).is_err());
    std::fs::remove_file(source.with_file_name("hard")).unwrap();
    let links = private::directory(&base.join("links")).unwrap();
    symlink(&source, links.join("auth.json")).unwrap();
    assert!(auth::import_codex_auth(&store, &account.id, &links.join("auth.json")).is_err());
    let mut api: serde_json::Value = serde_json::from_slice(&good).unwrap();
    api["OPENAI_API_KEY"] = serde_json::json!("synthetic-api-key-must-not-appear");
    let mut unknown: serde_json::Value = serde_json::from_slice(&good).unwrap();
    unknown["unrecognized-secret-key"] = serde_json::json!("synthetic-secret-value");
    for bytes in [
        b"{invalid-synthetic-secret".to_vec(),
        serde_json::to_vec(&api).unwrap(),
        serde_json::to_vec(&unknown).unwrap(),
        vec![b'x'; 65537],
    ] {
        let current = private::read(&source, 65536).unwrap();
        private::replace(&source, &bytes, &xcb_runtime::digest(current)).unwrap();
        let error = auth::import_codex_auth(&store, &account.id, &source)
            .unwrap_err()
            .to_string();
        assert!(!error.contains("synthetic-secret") && !error.contains("synthetic-api-key"));
        // Restore using the actual bytes, including the intentional oversize fixture.
        private::replace(&source, &good, &xcb_runtime::digest(&bytes)).unwrap();
    }
    assert!(!auth::has_credentials(&store, &account.id).unwrap());
    assert!(store.unsettled_runs().unwrap().is_empty());
}

#[test]
fn codex_refresh_requires_joined_exclusive_account_custody_and_preserves_rotation() {
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let source = private::directory(&base.join("source"))
        .unwrap()
        .join("auth.json");
    let original = codex_auth_fixture("account-one", "user-one", "synthetic-refresh-one");
    let refreshed = codex_auth_fixture("account-one", "user-one", "synthetic-refresh-two");
    private::create(&source, &original).unwrap();
    let store = Store::open(&base.join("state")).unwrap();
    let account = store
        .add_account(Provider::Codex, "ChatGPT", 1, None)
        .unwrap();
    auth::import_codex_auth(&store, &account.id, &source).unwrap();
    let run = codex_run(&store, &account.id, &base.join("work"));
    let profile = store.root().join("runs/synthetic/profile");
    let snapshot = auth::snapshot_codex_auth(&store, &run, &profile).unwrap();
    assert_eq!(snapshot.profile(), profile);
    let target = store
        .account_root(&account.id)
        .unwrap()
        .join("profile/auth.json");
    assert!(auth::import_codex_auth(&store, &account.id, &source).is_err());
    private::replace(
        &profile.join("auth.json"),
        &refreshed,
        &xcb_runtime::digest(&original),
    )
    .unwrap();
    assert!(auth::persist_codex_auth(&store, &run, &snapshot, false).is_err());
    assert_eq!(private::read(&target, 65536).unwrap(), original);
    auth::persist_codex_auth(&store, &run, &snapshot, true).unwrap();
    assert_eq!(private::read(&target, 65536).unwrap(), refreshed);
    assert_eq!(private::read(&source, 65536).unwrap(), original);
    assert_eq!(
        store.unsettled_runs().unwrap().len(),
        1,
        "auth helper must not release the run lease"
    );
}

#[test]
fn codex_refresh_rejects_account_switch_and_stale_persistent_revision() {
    for scenario in ["account", "user", "revision", "directory"] {
        let directory = tempfile::tempdir().unwrap();
        let base = directory.path().canonicalize().unwrap();
        let source = private::directory(&base.join("source"))
            .unwrap()
            .join("auth.json");
        let original = codex_auth_fixture("account-one", "user-one", "synthetic-refresh-one");
        private::create(&source, &original).unwrap();
        let store = Store::open(&base.join("state")).unwrap();
        let account = store
            .add_account(Provider::Codex, "ChatGPT", 1, None)
            .unwrap();
        auth::import_codex_auth(&store, &account.id, &source).unwrap();
        let run = codex_run(&store, &account.id, &base.join("work"));
        let profile = store.root().join("runs/synthetic/profile");
        let snapshot = auth::snapshot_codex_auth(&store, &run, &profile).unwrap();
        let target = store
            .account_root(&account.id)
            .unwrap()
            .join("profile/auth.json");
        let mut expected = original.clone();
        match scenario {
            "account" | "user" => {
                let changed = codex_auth_fixture(
                    if scenario == "account" {
                        "account-two"
                    } else {
                        "account-one"
                    },
                    if scenario == "user" {
                        "user-two"
                    } else {
                        "user-one"
                    },
                    "synthetic-refresh-two",
                );
                private::replace(
                    &profile.join("auth.json"),
                    &changed,
                    &xcb_runtime::digest(&original),
                )
                .unwrap();
            }
            "revision" => {
                expected = codex_auth_fixture("account-one", "user-one", "synthetic-new-owner");
                private::replace(&target, &expected, &xcb_runtime::digest(&original)).unwrap();
            }
            "directory" => {
                std::fs::rename(&profile, profile.with_file_name("moved")).unwrap();
                private::directory(&profile).unwrap();
                private::create(&profile.join("auth.json"), &original).unwrap();
            }
            _ => unreachable!(),
        }
        assert!(
            auth::persist_codex_auth(&store, &run, &snapshot, true).is_err(),
            "{scenario}"
        );
        assert_eq!(
            private::read(&target, 65536).unwrap(),
            expected,
            "{scenario}"
        );
        assert_eq!(store.unsettled_runs().unwrap().len(), 1);
    }
}

#[test]
fn codex_unstarted_discard_is_idempotent_and_snapshot_cannot_escape_state() {
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let store = Store::open(&base.join("state")).unwrap();
    let account = store
        .add_account(Provider::Codex, "ChatGPT", 1, None)
        .unwrap();
    let run = codex_run(&store, &account.id, &base.join("work"));
    assert!(auth::discard_unstarted_codex_auth(&store, &run, false).is_err());
    auth::discard_unstarted_codex_auth(&store, &run, true).unwrap();
    auth::discard_unstarted_codex_auth(&store, &run, true).unwrap();
    assert!(auth::snapshot_codex_auth(&store, &run, &base.join("work/profile")).is_err());
    assert!(!base.join("work/profile").exists());
    let source = private::directory(&base.join("source"))
        .unwrap()
        .join("auth.json");
    let fixture = codex_auth_fixture("account-one", "user-one", "synthetic-refresh-one");
    private::create(&source, &fixture).unwrap();
    let target = store
        .account_root(&account.id)
        .unwrap()
        .join("profile/auth.json");
    private::create(&target, &fixture).unwrap();
    let snapshot =
        auth::snapshot_codex_auth(&store, &run, &store.root().join("runs/fixture/profile"))
            .unwrap();
    auth::discard_unstarted_codex_auth(&store, &run, true).unwrap();
    auth::discard_unstarted_codex_auth(&store, &run, true).unwrap();
    assert!(
        snapshot.profile().join("auth.json").exists(),
        "cleanup belongs to the caller"
    );
    assert_eq!(store.unsettled_runs().unwrap().len(), 1);
}

#[test]
fn codex_device_login_plan_is_isolated_and_can_persist_a_fixture_without_spawning() {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let store = Store::open(&base.join("state")).unwrap();
    let account = store
        .add_account(Provider::Codex, "ChatGPT", 1, None)
        .unwrap();
    let run = codex_run(&store, &account.id, &base.join("work"));
    let executable = base.join("synthetic-codex");
    std::fs::write(&executable, b"#!/bin/sh\nexit 99\n").unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let pin = xcb_runtime::process::Pin {
        provider: Provider::Codex,
        sha256: xcb_runtime::process::executable_digest(&executable).unwrap(),
        executable,
        version: "synthetic".into(),
        host_sha256: xcb_runtime::process::executable_digest(
            &std::env::current_exe().unwrap().canonicalize().unwrap(),
        )
        .unwrap(),
        observed_at_ms: 1,
    };
    let profile = store.root().join("runs/login/profile");
    let plan = auth::prepare_codex_login(&store, &run, &pin, &profile).unwrap();
    let command = plan.command.as_std();
    let args: Vec<_> = command
        .get_args()
        .map(|arg| arg.to_str().unwrap())
        .collect();
    assert_eq!(&args[args.len() - 2..], ["login", "--device-auth"]);
    assert!(args.contains(&"cli_auth_credentials_store=\"file\""));
    assert!(args.contains(&"forced_login_method=\"chatgpt\""));
    let env: std::collections::BTreeMap<_, _> = command
        .get_envs()
        .map(|(key, value)| {
            (
                key.to_string_lossy().into_owned(),
                value.unwrap().to_string_lossy().into_owned(),
            )
        })
        .collect();
    assert_eq!(env["CODEX_HOME"], profile.to_str().unwrap());
    assert_eq!(env["HOME"], profile.join("login-home").to_str().unwrap());
    assert!(
        !env.contains_key("OPENAI_API_KEY")
            && !env.contains_key("CLAUDE_CODE_OAUTH_TOKEN")
            && !env.contains_key("SSH_AUTH_SOCK")
    );
    let bytes = codex_auth_fixture("account-one", "user-one", "synthetic-refresh-one");
    private::create(&profile.join("auth.json"), &bytes).unwrap();
    auth::persist_codex_auth(&store, &run, &plan.credentials, true).unwrap();
    assert!(auth::has_credentials(&store, &account.id).unwrap());
}

#[test]
fn codex_account_import_validates_before_creating_account_and_copies_once() {
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let source = private::directory(&base.join("source"))
        .unwrap()
        .join("auth.json");
    let store = Store::open(&base.join("state")).unwrap();
    private::create(&source, b"invalid-secret-fixture").unwrap();
    assert!(auth::import_codex_account(&store, &source).is_err());
    assert!(store.accounts().unwrap().is_empty());
    assert!(store.unsettled_runs().unwrap().is_empty());
    let good = codex_auth_fixture("account-one", "user-one", "synthetic-refresh-one");
    private::replace(
        &source,
        &good,
        &xcb_runtime::digest(b"invalid-secret-fixture"),
    )
    .unwrap();
    let id = auth::import_codex_account(&store, &source).unwrap();
    assert_eq!(store.accounts().unwrap().len(), 1);
    assert!(auth::has_credentials(&store, &id).unwrap());
    assert_eq!(private::read(&source, 65536).unwrap(), good);
    assert!(store.unsettled_runs().unwrap().is_empty());
}

fn recovery_fixture_run(
    store: &Store,
    run: &xcb_runtime::store::RunRecord,
    running: bool,
) -> xcb_runtime::store::RunRecord {
    let mut record = run.clone();
    record.owner.as_mut().unwrap().pid = i32::MAX as u32;
    if running {
        record.phase = "running".into();
        record.pid = Some(i32::MAX as u32);
    }
    let db = rusqlite::Connection::open(store.root().join("xcb.sqlite")).unwrap();
    db.execute(
        "UPDATE runs SET phase=?1,payload=?2 WHERE id=?3",
        rusqlite::params![
            record.phase,
            serde_json::to_string(&record).unwrap(),
            record.id.as_str()
        ],
    )
    .unwrap();
    record
}

#[test]
fn codex_recovery_preserves_refresh_and_retries_after_publication_before_receipt_commit() {
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let store = Store::open(&base.join("state")).unwrap();
    let account = store
        .add_account(Provider::Codex, "ChatGPT", 1, None)
        .unwrap();
    let target = store
        .account_root(&account.id)
        .unwrap()
        .join("profile/auth.json");
    let original = codex_auth_fixture("account-one", "user-one", "synthetic-original");
    let refreshed = codex_auth_fixture("account-one", "user-one", "synthetic-refreshed");
    private::create(&target, &original).unwrap();
    let run = recovery_fixture_run(
        &store,
        &codex_run(&store, &account.id, &base.join("workspace")),
        false,
    );
    let snapshot =
        auth::snapshot_codex_auth(&store, &run, &store.root().join("runs/recovery/profile"))
            .unwrap();
    private::replace(
        &snapshot.profile().join("auth.json"),
        &refreshed,
        &xcb_runtime::digest(&original),
    )
    .unwrap();
    let run = recovery_fixture_run(&store, &run, true);
    let (_, digest) = store.recovery_candidate(&run.id).unwrap().unwrap();
    let db = rusqlite::Connection::open(store.root().join("xcb.sqlite")).unwrap();
    db.execute_batch("CREATE TRIGGER fail_auth_recovery BEFORE UPDATE OF settled ON tool_effects WHEN NEW.settled=1 BEGIN SELECT RAISE(ABORT,'synthetic failure'); END;").unwrap();
    assert!(store.recover_run(&run.id, &digest, 4).is_err());
    assert_eq!(private::read(&target, 65536).unwrap(), refreshed);
    assert_eq!(store.unsettled_runs().unwrap().len(), 1);
    assert_eq!(
        db.query_row(
            "SELECT settled FROM tool_effects WHERE run=?1 AND call='xcb_auth_snapshot'",
            [run.id.as_str()],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    db.execute_batch("DROP TRIGGER fail_auth_recovery;")
        .unwrap();
    let recovered = store.recover_run(&run.id, &digest, 5).unwrap();
    assert_eq!(recovered.phase, "settled");
    assert_eq!(private::read(&target, 65536).unwrap(), refreshed);
    assert!(store.unsettled_runs().unwrap().is_empty());
    assert_eq!(
        db.query_row(
            "SELECT settled FROM tool_effects WHERE run=?1 AND call='xcb_auth_snapshot'",
            [run.id.as_str()],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    assert_eq!(
        store
            .session(run.session.as_ref().unwrap())
            .unwrap()
            .unwrap()
            .state,
        xcb_core::session::State::Uncertain
    );
}

#[test]
fn codex_recovery_rejects_missing_or_changed_evidence_and_never_releases_custody() {
    for scenario in [
        "missing-metadata",
        "changed-metadata",
        "profile",
        "persistent-profile",
        "account",
        "cas",
        "owner",
        "live-owner",
    ] {
        let directory = tempfile::tempdir().unwrap();
        let base = directory.path().canonicalize().unwrap();
        let store = Store::open(&base.join("state")).unwrap();
        let account = store
            .add_account(Provider::Codex, "ChatGPT", 1, None)
            .unwrap();
        let target = store
            .account_root(&account.id)
            .unwrap()
            .join("profile/auth.json");
        let original = codex_auth_fixture("account-one", "user-one", "synthetic-original");
        let refreshed = codex_auth_fixture("account-one", "user-one", "synthetic-refreshed");
        private::create(&target, &original).unwrap();
        let mut run = recovery_fixture_run(
            &store,
            &codex_run(&store, &account.id, &base.join("workspace")),
            false,
        );
        let profile = store.root().join("runs/recovery/profile");
        auth::snapshot_codex_auth(&store, &run, &profile).unwrap();
        private::replace(
            &profile.join("auth.json"),
            &refreshed,
            &xcb_runtime::digest(&original),
        )
        .unwrap();
        run = recovery_fixture_run(&store, &run, true);
        let metadata = store
            .root()
            .join("runs")
            .join(format!("{}.codex-auth-recovery.json", run.id));
        match scenario {
            "missing-metadata" => std::fs::remove_file(&metadata).unwrap(),
            "changed-metadata" => {
                let previous = private::read(&metadata, 32768).unwrap();
                private::replace(&metadata, b"{}", &xcb_runtime::digest(previous)).unwrap();
            }
            "profile" => {
                std::fs::rename(&profile, profile.with_file_name("old-profile")).unwrap();
                private::directory(&profile).unwrap();
                private::create(&profile.join("auth.json"), &refreshed).unwrap();
            }
            "persistent-profile" => {
                let parent = target.parent().unwrap();
                std::fs::rename(parent, parent.with_file_name("old-profile")).unwrap();
                private::directory(parent).unwrap();
                private::create(&target, &original).unwrap();
            }
            "account" => private::replace(
                &profile.join("auth.json"),
                &codex_auth_fixture("other-account", "user-one", "synthetic-refreshed"),
                &xcb_runtime::digest(&refreshed),
            )
            .unwrap(),
            "cas" => private::replace(
                &target,
                &codex_auth_fixture("account-one", "user-one", "external-rotation"),
                &xcb_runtime::digest(&original),
            )
            .unwrap(),
            "owner" | "live-owner" => {
                if scenario == "owner" {
                    run.owner.as_mut().unwrap().instance = "other-instance".into();
                } else {
                    run.owner.as_mut().unwrap().pid = std::process::id();
                }
                let db = rusqlite::Connection::open(store.root().join("xcb.sqlite")).unwrap();
                db.execute(
                    "UPDATE runs SET payload=?1 WHERE id=?2",
                    rusqlite::params![serde_json::to_string(&run).unwrap(), run.id.as_str()],
                )
                .unwrap();
            }
            _ => unreachable!(),
        }
        let expected = private::read(&target, 65536).unwrap();
        let (_, digest) = store.recovery_candidate(&run.id).unwrap().unwrap();
        assert!(
            store.recover_run(&run.id, &digest, 4).is_err(),
            "{scenario}"
        );
        assert_eq!(
            private::read(&target, 65536).unwrap(),
            expected,
            "{scenario}"
        );
        assert_eq!(store.unsettled_runs().unwrap().len(), 1, "{scenario}");
    }
}

#[test]
fn codex_recovery_finishes_new_device_login_or_empty_failed_flow_without_spawning() {
    use std::os::unix::fs::PermissionsExt;
    for completed in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let base = directory.path().canonicalize().unwrap();
        let store = Store::open(&base.join("state")).unwrap();
        let account = store
            .add_account(Provider::Codex, "ChatGPT", 1, None)
            .unwrap();
        // Preparation and generation publication belong to the live owner.
        // Simulate its death only after the login plan/receipt exists below.
        let run = codex_run(&store, &account.id, &base.join("workspace"));
        let executable = base.join("synthetic-codex");
        std::fs::write(&executable, b"#!/bin/sh\nexit 99\n").unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let pin = xcb_runtime::process::Pin {
            provider: Provider::Codex,
            sha256: xcb_runtime::process::executable_digest(&executable).unwrap(),
            executable,
            version: "synthetic".into(),
            host_sha256: xcb_runtime::process::executable_digest(
                &std::env::current_exe().unwrap().canonicalize().unwrap(),
            )
            .unwrap(),
            observed_at_ms: 1,
        };
        let plan =
            auth::prepare_codex_login(&store, &run, &pin, &store.root().join("runs/login/profile"))
                .unwrap();
        let fixture = codex_auth_fixture("account-one", "user-one", "synthetic-new-login");
        if completed {
            private::create(&plan.credentials.profile().join("auth.json"), &fixture).unwrap();
        }
        let run = recovery_fixture_run(&store, &run, true);
        // The synthetic dead-owner fixture must agree in both the run and its
        // digest-bound recovery metadata, just as a real stopped owner would.
        let metadata_path = store
            .root()
            .join("runs")
            .join(format!("{}.codex-auth-recovery.json", run.id));
        let previous = private::read(&metadata_path, 32768).unwrap();
        let mut metadata: serde_json::Value = serde_json::from_slice(&previous).unwrap();
        assert_eq!(metadata["owner_pid"], std::process::id());
        metadata["owner_pid"] = serde_json::json!(run.owner.as_ref().unwrap().pid);
        let updated = serde_json::to_vec(&metadata).unwrap();
        private::replace(&metadata_path, &updated, &xcb_runtime::digest(&previous)).unwrap();
        let db = rusqlite::Connection::open(store.root().join("xcb.sqlite")).unwrap();
        assert_eq!(
            db.execute(
                "UPDATE tool_effects SET input_digest=?1 WHERE run=?2 AND call='xcb_auth_snapshot' AND input_digest=?3 AND settled=0",
                rusqlite::params![xcb_runtime::digest(&updated), run.id.as_str(), xcb_runtime::digest(&previous)],
            ).unwrap(),
            1,
        );
        let (_, digest) = store.recovery_candidate(&run.id).unwrap().unwrap();
        store.recover_run(&run.id, &digest, 4).unwrap();
        assert!(store.unsettled_runs().unwrap().is_empty());
        let target = store
            .account_root(&account.id)
            .unwrap()
            .join("profile/auth.json");
        if completed {
            assert_eq!(private::read(&target, 65536).unwrap(), fixture);
        } else {
            assert!(!target.exists());
        }
    }
}

fn claude_login_pin(base: &std::path::Path, body: &str) -> xcb_runtime::process::Pin {
    use std::os::unix::fs::PermissionsExt;
    let executable = base.join("synthetic-claude");
    std::fs::write(&executable, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    xcb_runtime::process::Pin {
        provider: Provider::Claude,
        sha256: xcb_runtime::process::executable_digest(&executable).unwrap(),
        executable,
        version: "synthetic".into(),
        host_sha256: xcb_runtime::process::executable_digest(
            &std::env::current_exe().unwrap().canonicalize().unwrap(),
        )
        .unwrap(),
        observed_at_ms: 1,
    }
}

#[tokio::test]
async fn claude_login_captures_privately_and_settles_joined_invalid_or_failed_output() {
    for script in [
        "printf 'sk-ant-oat01-login_synthetic_fixture_not_real\\n'",
        "printf 'not a token\\n'",
        "printf 'sk-ant-oat01-login_synthetic_fixture_not_real\\n'; exit 7",
    ] {
        let directory = tempfile::tempdir().unwrap();
        let base = directory.path().canonicalize().unwrap();
        let store = Store::open(&base.join("state")).unwrap();
        let account = store.add_account(Provider::Claude, "Max", 1, None).unwrap();
        let original = b"sk-ant-oat01-original_synthetic_fixture_not_real";
        auth::store_token(&store, &account.id, original).unwrap();
        let pin = claude_login_pin(&base, script);
        let result = auth::login(&store, &account.id, &pin).await;
        let success = !script.contains("not a token") && !script.contains("exit 7");
        assert_eq!(result.is_ok(), success);
        if let Err(error) = result {
            assert!(!format!("{error:?} {error}").contains("synthetic_fixture"));
        }
        let target = store
            .account_root(&account.id)
            .unwrap()
            .join("subscription-token");
        assert_eq!(
            private::read(&target, 2048).unwrap(),
            if success {
                b"sk-ant-oat01-login_synthetic_fixture_not_real".as_slice()
            } else {
                original.as_slice()
            }
        );
        assert!(store.unsettled_runs().unwrap().is_empty());
        assert_eq!(
            std::fs::read_dir(store.root().join("runs"))
                .unwrap()
                .count(),
            0
        );
        assert_eq!(
            std::fs::read_dir(store.account_root(&account.id).unwrap().join("profile"))
                .unwrap()
                .count(),
            0
        );
    }
}

#[tokio::test]
async fn claude_login_cancellation_joins_before_releasing_and_busy_accounts_do_not_spawn() {
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let store = std::sync::Arc::new(Store::open(&base.join("state")).unwrap());
    let account = store.add_account(Provider::Claude, "Max", 1, None).unwrap();
    // Keep the blocking fixture in the owned leader. A shell waiting on a
    // forked sleep can leave an orphan whose reaping depends on the host;
    // this test requires a joined cancellation, not that uncertain outcome.
    let pin = claude_login_pin(&base, "exec sleep 30");
    let (sender, cancel) = tokio::sync::watch::channel(false);
    let owned_store = store.clone();
    let owned_id = account.id.clone();
    let owned_pin = pin.clone();
    let task = tokio::spawn(async move {
        auth::login_with_cancel(&owned_store, &owned_id, &owned_pin, cancel).await
    });
    let run = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let Some(run) = store
                .unsettled_runs()
                .unwrap()
                .into_iter()
                .find(|run| run.pid.is_some())
            {
                break run;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(auth::login(&store, &account.id, &pin).await.is_err());
    assert_eq!(store.unsettled_runs().unwrap().len(), 1);
    sender.send(true).unwrap();
    let error = tokio::time::timeout(std::time::Duration::from_secs(15), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(
        matches!(&error, xcb_runtime::Error::Unavailable("sign-in cancelled")),
        "login did not report joined cancellation"
    );
    assert!(xcb_runtime::process::prove_process_group_absent(run.pid.unwrap()).is_ok());
    assert!(store.unsettled_runs().unwrap().is_empty());
    assert_eq!(
        std::fs::read_dir(store.root().join("runs"))
            .unwrap()
            .count(),
        0
    );
}

#[tokio::test]
async fn claude_login_preserves_a_changed_prior_token_and_retains_uncertain_receipts() {
    for fault in ["revision", "receipt"] {
        let directory = tempfile::tempdir().unwrap();
        let base = directory.path().canonicalize().unwrap();
        let store = std::sync::Arc::new(Store::open(&base.join("state")).unwrap());
        let account = store.add_account(Provider::Claude, "Max", 1, None).unwrap();
        let original = b"sk-ant-oat01-original_synthetic_fixture_not_real";
        let changed = b"sk-ant-oat01-changed_synthetic_fixture_not_real";
        let generated = b"sk-ant-oat01-login_synthetic_fixture_not_real";
        auth::store_token(&store, &account.id, original).unwrap();
        let ready = base.join("ready");
        let proceed = base.join("proceed");
        let pin = claude_login_pin(
            &base,
            &format!(
                "touch '{}'; while [ ! -e '{}' ]; do sleep 0.01; done; printf 'sk-ant-oat01-login_synthetic_fixture_not_real\\n'",
                ready.display(),
                proceed.display()
            ),
        );
        let owned_store = store.clone();
        let owned_id = account.id.clone();
        let task = tokio::spawn(async move { auth::login(&owned_store, &owned_id, &pin).await });
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while !ready.exists() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let target = store
            .account_root(&account.id)
            .unwrap()
            .join("subscription-token");
        let db = rusqlite::Connection::open(store.root().join("xcb.sqlite")).unwrap();
        if fault == "revision" {
            private::replace(&target, changed, &xcb_runtime::digest(original)).unwrap();
        } else {
            db.execute_batch("CREATE TRIGGER fail_claude_login_receipt BEFORE UPDATE OF settled ON tool_effects WHEN OLD.call='xcb_claude_auth_store' BEGIN SELECT RAISE(ABORT, 'synthetic receipt failure'); END;").unwrap();
        }
        std::fs::write(&proceed, b"continue").unwrap();
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(15), task)
                .await
                .unwrap()
                .unwrap()
                .is_err()
        );
        assert_eq!(
            private::read(&target, 2048).unwrap(),
            if fault == "revision" {
                changed.as_slice()
            } else {
                generated.as_slice()
            }
        );
        let unsettled = store.unsettled_runs().unwrap();
        assert_eq!(unsettled.len(), 1);
        assert!(
            xcb_runtime::process::prove_process_group_absent(unsettled[0].pid.unwrap()).is_ok()
        );
        assert_eq!(
            std::fs::read_dir(store.root().join("runs"))
                .unwrap()
                .count(),
            1
        );
    }
}

#[tokio::test]
async fn dropped_claude_login_keeps_durable_custody_after_attempting_group_stop() {
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let store = std::sync::Arc::new(Store::open(&base.join("state")).unwrap());
    let account = store.add_account(Provider::Claude, "Max", 1, None).unwrap();
    // The host owns and reaps this leader directly, including after abort.
    let pin = claude_login_pin(&base, "exec sleep 30");
    let owned_store = store.clone();
    let id = account.id.clone();
    let task = tokio::spawn(async move { auth::login(&owned_store, &id, &pin).await });
    let run = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let Some(run) = store
                .unsettled_runs()
                .unwrap()
                .into_iter()
                .find(|run| run.pid.is_some())
            {
                break run;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while xcb_runtime::process::prove_process_group_absent(run.pid.unwrap()).is_err() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(store.unsettled_runs().unwrap()[0].id, run.id);
    assert_eq!(
        std::fs::read_dir(store.root().join("runs"))
            .unwrap()
            .count(),
        1
    );
    assert!(!auth::has_token(&store, &account.id).unwrap());
}

#[test]
fn explicit_claude_token_replacement_rotates_generation_before_publishing() {
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let store = Store::open(&base.join("state")).unwrap();
    let account = store.add_account(Provider::Claude, "Max", 1, None).unwrap();
    let original = b"sk-ant-oat01-original_synthetic_fixture_not_real";
    auth::store_token(&store, &account.id, original).unwrap();
    let root = store.account_root(&account.id).unwrap();
    let generation = root.join("application-generation.json");
    let first = private::read(&generation, 1024).unwrap();
    auth::store_token(&store, &account.id, original).unwrap();
    let second = private::read(&generation, 1024).unwrap();
    assert_ne!(
        first, second,
        "even explicit reimport of identical bytes invalidates qualification"
    );
    let db = rusqlite::Connection::open(store.root().join("xcb.sqlite")).unwrap();
    db.execute_batch("CREATE TRIGGER fail_generation_receipt BEFORE UPDATE OF settled ON tool_effects WHEN OLD.call='xcb_application_generation' BEGIN SELECT RAISE(ABORT, 'synthetic generation receipt failure'); END;").unwrap();
    assert!(
        auth::store_token(
            &store,
            &account.id,
            b"sk-ant-oat01-replacement_synthetic_fixture_not_real"
        )
        .is_err()
    );
    assert_eq!(
        private::read(&root.join("subscription-token"), 2048).unwrap(),
        original
    );
    assert_eq!(store.unsettled_runs().unwrap().len(), 1);
}
