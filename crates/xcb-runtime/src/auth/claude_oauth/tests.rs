use super::*;

const OLD: &str = "sk-ant-oat01-abcdefghijklmnop_old";
const NEW: &str = "sk-ant-oat01-abcdefghijklmnop_new";
const UUID: &str = "00000000-0000-4000-8000-000000000001";

fn fixture() -> (tempfile::TempDir, Store, Id) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&xcb_core::canonical(dir.path()).unwrap().join("state")).unwrap();
    let id = store
        .add_account(Provider::Claude, "Test", 1, None)
        .unwrap()
        .id;
    (dir, store, id)
}

#[test]
fn helper_home_is_explicit_while_every_provider_config_root_stays_private() {
    let (root, store, account) = fixture();
    let run = store.prepare_probe(&account, None, 2).unwrap();
    let generation = Generation::create(&store, &run).unwrap();
    let caller_home = xcb_core::canonical(root.path()).unwrap();
    let env = helper_environment_at(&generation, Some(&caller_home)).unwrap();
    assert_eq!(env["HOME"], caller_home.to_str().unwrap());
    for key in ["CLAUDE_CONFIG_DIR", "CLAUDE_SECURESTORAGE_CONFIG_DIR"] {
        assert_eq!(env[key], generation.profile.to_str().unwrap());
    }
    assert_eq!(env["USER"], generation.username);
    for key in [
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_CACHE_HOME",
        "TMPDIR",
        "ANTHROPIC_CONFIG_DIR",
    ] {
        assert!(Path::new(&env[key]).starts_with(&generation.home), "{key}");
    }
    // Missing/relative/non-directory homes must not silently select the OS
    // user's home or enter a mutating authentication phase.
    let file = root.path().join("not-a-home");
    std::fs::write(&file, b"fixture").unwrap();
    for home in [
        None,
        Some(Path::new("relative")),
        Some(file.as_path()),
        Some(root.path().join("absent").as_path()),
    ] {
        let error = helper_environment_at(&generation, home).unwrap_err();
        assert!(error.to_string().contains("caller's HOME"));
        assert!(!error.is_cleanup_unproven());
    }
    assert_eq!(
        environment(&generation.home)["HOME"],
        generation.home.to_str().unwrap()
    );
}

#[cfg(unix)]
#[tokio::test]
async fn official_auth_command_obeys_pinned_cli_argument_and_environment_contract() {
    use std::os::unix::fs::PermissionsExt;
    let (root, store, account) = fixture();
    let run = store.prepare_probe(&account, None, 2).unwrap();
    let generation = Generation::create(&store, &run).unwrap();
    let caller_home = xcb_core::canonical(root.path()).unwrap();
    let env = helper_environment_at(&generation, Some(&caller_home)).unwrap();
    let helper = root.path().join("claude-fixture");
    std::fs::write(
        &helper,
        br#"#!/bin/sh
set -eu
[ "$#" = 5 ]
[ "$1" = --project-config-root ]
[ "$2" = "$PWD" ]
[ "$3" = auth ] && [ "$4" = login ] && [ "$5" = --claudeai ]
[ "$CLAUDE_CONFIG_DIR" = "$CLAUDE_SECURESTORAGE_CONFIG_DIR" ]
[ "$HOME" != "$PWD" ]
[ "$TMPDIR" = "$PWD/tmp" ]
[ "$XDG_CONFIG_HOME" = "$PWD/.config" ]
[ "$ANTHROPIC_CONFIG_DIR" = "$PWD/.config/anthropic" ]
[ "$BROWSER" = /usr/bin/true ]
[ "$NO_COLOR" = 1 ]
case "$USER" in xcb-*) ;; *) exit 8 ;; esac
printf contract-ok
"#,
    )
    .unwrap();
    std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
    let command = official_auth_command(&helper, &generation, env);
    let (_sender, cancel) = watch::channel(false);
    let outcome =
        crate::process::capture_supervised(command, 1024, Duration::from_secs(10), cancel, |_| {
            Ok(())
        })
        .await;
    assert!(matches!(outcome, CaptureOutcome::Joined(Ok(bytes)) if &**bytes == b"contract-ok"));
}

#[cfg(unix)]
#[tokio::test]
async fn keychain_preflight_failure_releases_only_the_never_started_login() {
    for script in ["printf 'private-home-path' >&2; exit 44", "exit 0"] {
        let (_dir, store, account) = fixture();
        super::super::store_token(&store, &account, OLD.as_bytes()).unwrap();
        let other = store
            .add_account(Provider::Claude, "Other", 1, None)
            .unwrap()
            .id;
        let held = store.prepare_probe(&other, None, 2).unwrap();
        let run = store.prepare_probe(&account, None, 2).unwrap();
        let generation = Generation::create(&store, &run).unwrap();
        let mut command = Command::new("/bin/sh");
        command.args(["-c", script]);
        let (_sender, cancel) = watch::channel(false);
        let error = keychain_preflight(&store, &run, &generation, cancel, command)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("caller's HOME"));
        assert!(!error.to_string().contains("private-home-path"));
        assert!(!error.is_cleanup_unproven());
        store.require_settled_tools(&run).unwrap();
        assert!(
            store
                .run(&run.id)
                .unwrap()
                .unwrap()
                .capability_processes
                .is_empty()
        );
        drop(LoginCustody {
            store: &store,
            run: &run,
            active: true,
        });
        assert_eq!(
            store
                .unsettled_runs()
                .unwrap()
                .iter()
                .map(|run| &run.id)
                .collect::<Vec<_>>(),
            vec![&held.id]
        );
        assert_eq!(super::super::token(&store, &account).unwrap().as_str(), OLD);
        assert!(read_active(&store, &account).unwrap().is_none());
        let intent_files: Vec<_> = std::fs::read_dir(generation.root.join("effects"))
            .unwrap()
            .collect();
        assert_eq!(intent_files.len(), 1);
        let intent: EffectIntent = serde_json::from_slice(
            &private::read(&intent_files[0].as_ref().unwrap().path(), MAX_RECORD).unwrap(),
        )
        .unwrap();
        assert_eq!(intent.operation, "keychain-preflight");
    }
}

fn wire(token: &str, expiry: u64) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({"claudeAiOauth": {
        "accessToken": token, "refreshToken": "private-refresh-sentinel",
        "expiresAt": expiry, "scopes": ["user:profile", "user:inference"],
    }}))
    .unwrap()
}

fn active(account: &Id, generation: &str, token: &str, expiry: u64) -> Active {
    Active {
        version: 1,
        account: account.clone(),
        generation: generation.into(),
        account_uuid: UUID.into(),
        email: "fixture@example.test".into(),
        scopes: vec!["user:profile".into(), "user:inference".into()],
        access_token: token.into(),
        expires_at_ms: expiry,
    }
}

#[test]
fn expired_bundle_can_refresh_but_only_a_new_unexpired_token_is_accepted() {
    let source = wire(OLD, 1);
    let previous = bundle(&source).unwrap();
    assert!(bundle_from_refresh(&wire(OLD, crate::now_ms() + 600_000), &previous).is_err());
    assert!(bundle_from_refresh(&wire(NEW, 1), &previous).is_err());
    assert!(bundle_from_refresh(&wire(NEW, crate::now_ms() + 600_000), &previous).is_ok());
}

#[test]
fn invalid_secret_documents_have_static_diagnostics() {
    for bytes in [
        b"{private-refresh-sentinel: https://private.example.test}".to_vec(),
        wire("bad-secret-token", 1),
        wire(OLD, 0),
        serde_json::to_vec(&serde_json::json!({"claudeAiOauth": {
            "accessToken": OLD, "refreshToken": "private-refresh-sentinel",
            "expiresAt": 1, "scopes": ["user:inference"],
        }}))
        .unwrap(),
    ] {
        let error = bundle(&bytes).err().unwrap();
        let text = format!("{error} {error:?}");
        for secret in [
            OLD,
            "private-refresh-sentinel",
            "private.example.test",
            "bad-secret-token",
        ] {
            assert!(!text.contains(secret));
        }
    }
    for status in [200, 401, 403, 500] {
        let error = profile_identity(status, b"{private-email@example.test}").unwrap_err();
        assert!(!format!("{error} {error:?}").contains("private-email"));
    }
}

#[test]
fn namespace_is_unique_per_generation_and_uses_official_nfc_service_hash() {
    let (_dir, store, account) = fixture();
    let run = store.prepare_probe(&account, None, 2).unwrap();
    let first = Generation::create(&store, &run).unwrap();
    let second = Generation::create(&store, &run).unwrap();
    assert_ne!(first.username, second.username);
    assert_ne!(first.profile, second.profile);
    let resolved = Generation::resolve(&store, &account, &first.id).unwrap();
    assert_eq!(first.username, resolved.username);
    assert_eq!(first.service, resolved.service);
    let normalized = ComposingNormalizer::new_nfc().normalize(first.profile.to_str().unwrap());
    assert_eq!(
        first.service,
        format!(
            "Claude Code-credentials-{}",
            &crate::digest(normalized.as_bytes())[..8]
        )
    );
    assert!(Generation::resolve(&store, &account, "../other-account").is_err());
    recovery::fixtures::write_provider_fixture(
        &first.profile.join(".credentials.json"),
        b"private-token",
    );
    assert!(first.check().is_err());
    assert_eq!(
        private::read(&first.profile.join(".credentials.json"), 100).unwrap(),
        b"private-token"
    );
}

#[test]
fn cache_preserves_legacy_and_never_falls_back_after_expiry_or_corruption() {
    let (_dir, store, account) = fixture();
    super::super::store_token(&store, &account, OLD.as_bytes()).unwrap();
    assert_eq!(super::super::token(&store, &account).unwrap().as_str(), OLD);
    let path = store.account_root(&account).unwrap().join(ACTIVE);
    let bytes =
        serde_json::to_vec(&active(&account, UUID, NEW, crate::now_ms() + 600_000)).unwrap();
    private::create(&path, &bytes).unwrap();
    assert_eq!(super::super::token(&store, &account).unwrap().as_str(), NEW);
    // A legacy import must not appear successful while silently keeping the
    // full-login token, nor may it clear an unresolved full-login failure.
    assert!(super::super::store_token(&store, &account, OLD.as_bytes()).is_err());
    assert!(store.unsettled_runs().unwrap().is_empty());
    let expired = serde_json::to_vec(&active(&account, UUID, NEW, 1)).unwrap();
    private::replace(&path, &expired, &crate::digest(bytes)).unwrap();
    assert!(super::super::token(&store, &account).is_err());
    assert!(has_claude_browser_credentials(&store, &account).unwrap());
    private::replace(&path, b"{}", &crate::digest(expired)).unwrap();
    assert!(super::super::token(&store, &account).is_err());
    assert_eq!(
        private::read(
            &store
                .account_root(&account)
                .unwrap()
                .join("subscription-token"),
            2048
        )
        .unwrap(),
        OLD.as_bytes()
    );
}

#[test]
fn identity_mismatch_and_publication_conflict_preserve_previous_bytes() {
    let (_dir, store, account) = fixture();
    let run = store.prepare_probe(&account, None, 2).unwrap();
    let generation = Generation::create(&store, &run).unwrap();
    let path = store.account_root(&account).unwrap().join(ACTIVE);
    let previous = active(&account, &generation.id, OLD, crate::now_ms() + 600_000);
    let bytes = serde_json::to_vec(&previous).unwrap();
    private::create(&path, &bytes).unwrap();
    let observed = read_active(&store, &account).unwrap().unwrap();
    assert!(
        check_identity(
            &(UUID.into(), "different@example.test".into()),
            Some(&previous),
            None
        )
        .is_err()
    );
    assert!(
        check_identity(
            &(UUID.into(), "FIXTURE@example.test".into()),
            Some(&previous),
            None
        )
        .is_ok()
    );
    let changed = serde_json::to_vec(&active(
        &account,
        &generation.id,
        NEW,
        crate::now_ms() + 600_000,
    ))
    .unwrap();
    private::replace(&path, &changed, &observed.revision).unwrap();
    assert!(publish(&store, &run, &previous, Some(&observed), false).is_err());
    assert_eq!(private::read(&path, MAX_RECORD).unwrap(), changed);
    assert!(store.require_settled_tools(&run).is_err());
}

#[test]
fn dropped_login_releases_only_without_pending_effects_or_helpers() {
    for (receipt, helper) in [(false, false), (true, false), (false, true), (true, true)] {
        let (_dir, store, account) = fixture();
        let run = store.prepare_probe(&account, None, 2).unwrap();
        if receipt {
            store
                .begin_tool(&run, "test_auth", "host_auth_claude_oauth", "fixture")
                .unwrap();
        }
        if helper {
            store.mark_capability_starting(&run, AUTH_CUSTODY).unwrap();
        }
        drop(LoginCustody {
            store: &store,
            run: &run,
            active: true,
        });
        assert_eq!(
            store.unsettled_runs().unwrap().is_empty(),
            !receipt && !helper
        );
    }
}

#[tokio::test]
async fn never_started_auth_has_no_pending_effect_and_safe_failure() {
    let (_dir, store, account) = fixture();
    let run = store.prepare_probe(&account, None, 2).unwrap();
    let generation = Generation::create(&store, &run).unwrap();
    let effect = effect_intent(&store, &run, &generation, "login", None).unwrap();
    let (_sender, cancel) = watch::channel(false);
    let result = command_capture(
        &store,
        &run,
        Command::new(generation.root.join("absent")),
        AUTH_CUSTODY,
        Some(&effect),
        cancel,
        Duration::from_secs(1),
        None,
    )
    .await;
    assert!(result.is_err());
    store.require_settled_tools(&run).unwrap();
    assert!(
        store
            .run(&run.id)
            .unwrap()
            .unwrap()
            .capability_processes
            .is_empty()
    );
    store.settle(&run, State::Failed, 3).unwrap();
}
