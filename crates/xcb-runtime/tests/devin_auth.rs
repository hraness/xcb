use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use xcb_core::{
    Id, Provider,
    models::{Mode, ModelChoice},
};
use xcb_runtime::{devin::auth, private, store::Store};

fn native_credentials(token: &str) -> Vec<u8> {
    format!("windsurf_api_key = \"{token}\"\napi_server_url = \"https://server.codeium.com\"\ndevin_webapp_host = \"https://app.devin.ai\"\ndevin_api_url = \"https://api.devin.ai\"\n").into_bytes()
}

#[test]
fn devin_tokens_are_provider_scoped_private_and_rotated_without_exposure() {
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let store = Store::open(&base.join("state")).unwrap();
    let account = store.add_account(Provider::Devin, "Core", 1, None).unwrap();
    let other = store.add_account(Provider::Devin, "Core", 1, None).unwrap();
    let claude = store.add_account(Provider::Claude, "Max", 1, None).unwrap();
    assert!(!auth::has_credentials(&store, &account.id).unwrap());
    auth::store_token(&store, &account.id, b"synthetic-first\n").unwrap();
    assert_eq!(
        &*auth::token(&store, &account.id).unwrap(),
        "synthetic-first"
    );
    auth::store_token(&store, &account.id, b"synthetic-rotated").unwrap();
    assert!(auth::has_credentials(&store, &account.id).unwrap());
    assert!(!auth::has_credentials(&store, &other.id).unwrap());
    assert!(!auth::has_credentials(&store, &claude.id).unwrap());
    assert!(auth::token(&store, &claude.id).is_err());
    assert!(auth::store_token(&store, &claude.id, b"synthetic-first").is_err());
    let target = store
        .account_root(&account.id)
        .unwrap()
        .join("windsurf-token");
    let metadata = std::fs::metadata(&target).unwrap();
    assert_eq!(metadata.mode() & 0o777, 0o600);
    assert_eq!(metadata.nlink(), 1);
    for invalid in [
        b"".to_vec(),
        b"two tokens".to_vec(),
        b"synthetic\nsecond-line".to_vec(),
        b"synthetic\0nul".to_vec(),
        vec![b'x'; 8193],
    ] {
        let error = auth::store_token(&store, &account.id, &invalid)
            .unwrap_err()
            .to_string();
        assert!(!error.contains("synthetic"));
        assert_eq!(private::read(&target, 8192).unwrap(), b"synthetic-rotated");
    }
    assert!(store.unsettled_runs().unwrap().is_empty());
}

#[test]
fn devin_explicit_native_import_preserves_source_and_copies_only_the_token() {
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let source = private::directory(&base.join("source"))
        .unwrap()
        .join("credentials.toml");
    let bytes = native_credentials("synthetic-import-only");
    private::create(&source, &bytes).unwrap();
    std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o644)).unwrap();
    let store = Store::open(&base.join("state")).unwrap();
    let id = auth::import_account(&store, &source).unwrap();
    assert_eq!(store.account(&id).unwrap().provider, Provider::Devin);
    assert!(auth::has_credentials(&store, &id).unwrap());
    assert_eq!(&*auth::token(&store, &id).unwrap(), "synthetic-import-only");
    assert_eq!(std::fs::read(&source).unwrap(), bytes);
    assert_eq!(std::fs::metadata(&source).unwrap().mode() & 0o777, 0o644);
    let account = store.account_root(&id).unwrap();
    assert_eq!(
        std::fs::metadata(account.join("windsurf-token"))
            .unwrap()
            .mode()
            & 0o777,
        0o600
    );
    assert!(!account.join("credentials.toml").exists());
    assert_eq!(std::fs::read_dir(account.join("home")).unwrap().count(), 0);
    assert_eq!(
        std::fs::read_dir(account.join("profile")).unwrap().count(),
        0
    );
    assert!(store.unsettled_runs().unwrap().is_empty());
}

#[test]
fn devin_native_hostname_field_imports_without_rewriting_the_source() {
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let source = private::directory(&base.join("source"))
        .unwrap()
        .join("credentials.toml");
    let bytes = String::from_utf8(native_credentials("synthetic-hostname"))
        .unwrap()
        .replace("https://app.devin.ai", "app.devin.ai");
    private::create(&source, bytes.as_bytes()).unwrap();
    let store = Store::open(&base.join("state")).unwrap();
    let id = auth::import_account(&store, &source).unwrap();
    assert_eq!(&*auth::token(&store, &id).unwrap(), "synthetic-hostname");
    assert_eq!(std::fs::read(&source).unwrap(), bytes.as_bytes());
    for alternate in [
        "app.devin.ai.other.example",
        "http://app.devin.ai",
        "app.devin.ai/path",
    ] {
        let invalid = bytes.replace("app.devin.ai", alternate);
        private::replace(
            &source,
            invalid.as_bytes(),
            &xcb_runtime::digest(bytes.as_bytes()),
        )
        .unwrap();
        assert!(auth::import_account(&store, &source).is_err());
        private::replace(
            &source,
            bytes.as_bytes(),
            &xcb_runtime::digest(invalid.as_bytes()),
        )
        .unwrap();
    }
    assert_eq!(store.accounts().unwrap().len(), 1);
    assert!(store.unsettled_runs().unwrap().is_empty());
}

#[test]
fn devin_import_rejects_unsafe_sources_before_creating_accounts() {
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let source = private::directory(&base.join("source"))
        .unwrap()
        .join("credentials.toml");
    private::create(&source, &native_credentials("synthetic-only")).unwrap();
    let store = Store::open(&base.join("state")).unwrap();
    for mode in [0o620, 0o602, 0o666] {
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(mode)).unwrap();
        assert!(auth::import_account(&store, &source).is_err());
    }
    std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o600)).unwrap();
    let linked = private::directory(&base.join("linked"))
        .unwrap()
        .join("credentials.toml");
    symlink(&source, &linked).unwrap();
    assert!(auth::import_account(&store, &linked).is_err());
    std::fs::remove_file(&linked).unwrap();
    std::fs::hard_link(&source, &linked).unwrap();
    assert!(auth::import_account(&store, &source).is_err());
    std::fs::remove_file(linked).unwrap();
    symlink(source.parent().unwrap(), base.join("linked-parent")).unwrap();
    assert!(auth::import_account(&store, &base.join("linked-parent/credentials.toml")).is_err());
    assert!(store.accounts().unwrap().is_empty());
}

#[test]
fn devin_native_parser_rejects_custom_endpoints_ambiguous_and_unbounded_inputs() {
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let source = private::directory(&base.join("source"))
        .unwrap()
        .join("credentials.toml");
    let good = String::from_utf8(native_credentials("synthetic-secret-marker")).unwrap();
    private::create(&source, good.as_bytes()).unwrap();
    let store = Store::open(&base.join("state")).unwrap();
    let cases = vec![
        good.replace("https://server.codeium.com", "https://other.example"),
        good.replace("https://app.devin.ai", "https://other.example"),
        good.replace("https://api.devin.ai", "https://api.devin.ai.other.example"),
        good.replace("https://api.devin.ai", "https://api.devin.ai/path"),
        good.replace("https://server.codeium.com", "http://server.codeium.com"),
        good.replace("synthetic-secret-marker", ""),
        good.replace("synthetic-secret-marker", r"synthetic\nsecret"),
        format!("{good}windsurf_api_key = \"duplicate\"\n"),
        format!("{good}dangerously_skip_plugin_authentication = true\n"),
        format!("[credentials]\n{good}"),
        good.lines().skip(1).collect::<Vec<_>>().join("\n"),
        "x".repeat(65537),
    ];
    for invalid in cases {
        private::replace(
            &source,
            invalid.as_bytes(),
            &xcb_runtime::digest(good.as_bytes()),
        )
        .unwrap();
        let error = auth::import_account(&store, &source)
            .unwrap_err()
            .to_string();
        assert!(!error.contains("synthetic-secret-marker"));
        private::replace(
            &source,
            good.as_bytes(),
            &xcb_runtime::digest(invalid.as_bytes()),
        )
        .unwrap();
    }
    assert!(store.accounts().unwrap().is_empty());
    assert!(store.unsettled_runs().unwrap().is_empty());
}

#[test]
fn devin_token_changes_respect_existing_account_custody() {
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let store = Store::open(&base.join("state")).unwrap();
    let account = store.add_account(Provider::Devin, "Core", 1, None).unwrap();
    auth::store_token(&store, &account.id, b"synthetic-original").unwrap();
    let workspace = base.join("work");
    std::fs::create_dir(&workspace).unwrap();
    let model = ModelChoice {
        provider: Provider::Devin,
        id: Id::new("swe-2").unwrap(),
        label: "Synthetic".into(),
        mode: Mode::Fixed,
        resolved: None,
        effort: None,
        observed_at_ms: 1,
    };
    let session = store
        .create_session(&account.id, model, &workspace, 2)
        .unwrap();
    let run = store.prepare_run(&session.id, session.revision, 3).unwrap();
    assert!(auth::store_token(&store, &account.id, b"synthetic-replacement").is_err());
    assert_eq!(
        &*auth::token(&store, &account.id).unwrap(),
        "synthetic-original"
    );
    assert_eq!(store.unsettled_runs().unwrap()[0].id, run.id);
}

#[test]
fn devin_credential_target_links_are_rejected_and_receipt_failure_retains_custody() {
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let store = Store::open(&base.join("state")).unwrap();
    let account = store.add_account(Provider::Devin, "Core", 1, None).unwrap();
    let target = store
        .account_root(&account.id)
        .unwrap()
        .join("windsurf-token");
    let outside = private::directory(&base.join("outside"))
        .unwrap()
        .join("token");
    private::create(&outside, b"synthetic-outside").unwrap();
    symlink(&outside, &target).unwrap();
    assert!(auth::store_token(&store, &account.id, b"synthetic-replacement").is_err());
    assert_eq!(private::read(&outside, 8192).unwrap(), b"synthetic-outside");
    assert!(store.unsettled_runs().unwrap().is_empty());
    std::fs::remove_file(&target).unwrap();
    let db = rusqlite::Connection::open(store.root().join("xcb.sqlite")).unwrap();
    db.execute_batch("CREATE TRIGGER fail_auth_receipt BEFORE UPDATE OF settled ON tool_effects WHEN OLD.call='xcb_devin_auth_store' BEGIN SELECT RAISE(ABORT, 'synthetic receipt failure'); END;").unwrap();
    assert!(auth::store_token(&store, &account.id, b"synthetic-published").is_err());
    assert_eq!(
        private::read(&target, 8192).unwrap(),
        b"synthetic-published"
    );
    assert_eq!(store.unsettled_runs().unwrap().len(), 1);
    assert!(auth::store_token(&store, &account.id, b"synthetic-second").is_err());
    assert_eq!(
        private::read(&target, 8192).unwrap(),
        b"synthetic-published"
    );
}

#[test]
fn devin_generation_receipt_failure_preserves_credential_and_retains_custody() {
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let store = Store::open(&base.join("state")).unwrap();
    let account = store.add_account(Provider::Devin, "Core", 1, None).unwrap();
    auth::store_token(&store, &account.id, b"synthetic-original").unwrap();
    let root = store.account_root(&account.id).unwrap();
    let generation = root.join("application-generation.json");
    let original_generation = private::read(&generation, 1024).unwrap();
    let db = rusqlite::Connection::open(store.root().join("xcb.sqlite")).unwrap();
    db.execute_batch("CREATE TRIGGER fail_generation_receipt BEFORE UPDATE OF settled ON tool_effects WHEN OLD.call='xcb_application_generation' BEGIN SELECT RAISE(ABORT, 'synthetic generation receipt failure'); END;").unwrap();
    assert!(auth::store_token(&store, &account.id, b"synthetic-replacement").is_err());
    assert_eq!(
        private::read(&root.join("windsurf-token"), 8192).unwrap(),
        b"synthetic-original"
    );
    let changed_generation = private::read(&generation, 1024).unwrap();
    assert_ne!(
        original_generation, changed_generation,
        "qualification invalidation precedes credential publication"
    );
    let held = store.unsettled_runs().unwrap();
    assert_eq!(held.len(), 1);
    let pending: i64 = db.query_row("SELECT COUNT(*) FROM tool_effects WHERE run=?1 AND settled=0 AND call IN ('xcb_application_generation','xcb_devin_auth_store')", [held[0].id.as_str()], |row| row.get(0)).unwrap();
    assert_eq!(pending, 2);
    assert!(auth::store_token(&store, &account.id, b"synthetic-second").is_err());
    assert_eq!(
        private::read(&root.join("windsurf-token"), 8192).unwrap(),
        b"synthetic-original"
    );
    assert_eq!(
        private::read(&generation, 1024).unwrap(),
        changed_generation
    );
    assert_eq!(store.unsettled_runs().unwrap()[0].id, held[0].id);
}
