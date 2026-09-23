//! Synthetic accounts only: no provider process or ambient credential access.
use crate::{auth, config::Config, private, runner::Outcome, store::Store};
use base64::Engine;
use serde_json::json;
use xcb_core::{
    Id, Provider,
    models::{Mode, ModelChoice},
    policy::{EffectState, Failure, Terminal, TurnFacts},
    session::{Message, Role, State},
};

pub(crate) fn model(provider: Provider) -> ModelChoice {
    ModelChoice {
        provider,
        id: Id::new(if provider == Provider::Codex {
            "gpt-5.6-sol"
        } else {
            "synthetic-model"
        })
        .unwrap(),
        label: "Synthetic model".into(),
        mode: Mode::Fixed,
        resolved: None,
        effort: None,
        observed_at_ms: crate::now_ms(),
    }
}

fn codex_bytes(access: &str) -> Vec<u8> {
    let claims = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
        br#"{"sub":"synthetic-user","https://api.openai.com/auth":{"chatgpt_account_id":"synthetic-account"}}"#,
    );
    serde_json::to_vec(&json!({"auth_mode":"chatgpt","tokens":{
        "id_token":format!("synthetic.{claims}.signature"),"access_token":access,
        "refresh_token":"synthetic-refresh","account_id":"synthetic-account"
    },"last_refresh":"2026-09-22T00:00:00Z"}))
    .unwrap()
}

pub(crate) fn account(store: &Store, provider: Provider) -> Id {
    let account = store
        .add_account(provider, "Synthetic", crate::now_ms(), None)
        .unwrap();
    match provider {
        Provider::Codex => {
            let source = private::directory(&store.root().join(format!("source-{}", account.id)))
                .unwrap()
                .join("auth.json");
            private::create(&source, &codex_bytes("synthetic-access")).unwrap();
            auth::import_codex_auth(store, &account.id, &source).unwrap();
        }
        Provider::Claude => auth::store_token(
            store,
            &account.id,
            b"sk-ant-oat01-synthetic_not_a_real_token",
        )
        .unwrap(),
        Provider::Devin => {
            crate::devin::auth::store_token(store, &account.id, b"synthetic-devin-token").unwrap()
        }
    }
    account.id
}

pub(crate) fn fail_authentication(store: &Store, account: &Id) -> Id {
    let now = crate::now_ms();
    let provider = store.account(account).unwrap().provider;
    let work = private::directory(&store.root().parent().unwrap().join("synthetic-work")).unwrap();
    let session = store
        .create_session(account, model(provider), &work, now)
        .unwrap();
    let message = Message {
        id: crate::new_id("m"),
        role: Role::User,
        text: "Synthetic authentication check".into(),
        at_ms: now,
        attachments: vec![],
        provenance: None,
    };
    let current = store
        .append_message(&session.id, session.revision, &message)
        .unwrap();
    let run = store
        .prepare_run(&session.id, current.revision, now)
        .unwrap();
    let outcome = Outcome {
        text: String::new(),
        diagnostic: None,
        state: State::NeedsAction,
        facts: TurnFacts {
            terminal: Terminal::Failed,
            joined: true,
            effects: EffectState::None,
            pending_attention: true,
            failure: Some(Failure::Authentication),
        },
    };
    store
        .settle_outcome(&run, &message.id, &outcome, now)
        .unwrap();
    assert!(store.unsettled_runs().unwrap().is_empty());
    session.id
}

#[test]
fn authentication_health_survives_restart_metadata_rotation_and_session_pruning() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().canonicalize().unwrap().join("state");
    let store = Store::open(&state).unwrap();
    let account = account(&store, Provider::Codex);
    let session = fail_authentication(&store, &account);
    drop(store);
    let store = Store::open(&state).unwrap();
    let other = Store::open(&state).unwrap();
    assert!(store.authentication_required(&account).unwrap());
    assert!(auth::has_credentials(&store, &account).unwrap());
    let current = store.session(&session).unwrap().unwrap();
    assert!(
        other
            .prepare_run(&session, current.revision, crate::now_ms())
            .unwrap_err()
            .to_string()
            .contains("reconnect")
    );
    let probe = store
        .prepare_probe(&account, None, crate::now_ms())
        .unwrap();
    crate::application_qualification::rotate_generation(&store, &probe).unwrap();
    store
        .set_models(Provider::Codex, &[model(Provider::Codex)])
        .unwrap();
    store
        .settle(&probe, State::Failed, crate::now_ms())
        .unwrap();
    assert!(other.authentication_required(&account).unwrap());
    assert!(store.clear_authentication_failure(&probe).is_err());
    let probe = store
        .prepare_probe(&account, None, crate::now_ms())
        .unwrap();
    let profile = store.root().join("runs/metadata-profile");
    let snapshot = auth::snapshot_codex_auth(&store, &probe, &profile).unwrap();
    auth::persist_codex_auth(&store, &probe, &snapshot, true).unwrap();
    store.settle(&probe, State::Idle, crate::now_ms()).unwrap();
    assert!(other.authentication_required(&account).unwrap());
    assert!(store.remove_session(&session).unwrap());
    assert!(other.authentication_required(&account).unwrap());
    assert!(store.account(&account).unwrap().enabled);
    let view = crate::summary::snapshot(&store, None, &Config::default(), crate::now_ms()).unwrap();
    assert!(view.accounts[0].authentication_required);
}

#[test]
fn authentication_health_codex_import_requires_changed_token_material() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&root.path().canonicalize().unwrap().join("state")).unwrap();
    let account = account(&store, Provider::Codex);
    let session = fail_authentication(&store, &account);
    let source = private::directory(&store.root().join("replacement"))
        .unwrap()
        .join("auth.json");
    let mut value: serde_json::Value =
        serde_json::from_slice(&codex_bytes("synthetic-access")).unwrap();
    value["last_refresh"] = json!("2026-09-23T00:00:00Z");
    private::create(&source, &serde_json::to_vec_pretty(&value).unwrap()).unwrap();
    auth::import_codex_auth(&store, &account, &source).unwrap();
    assert!(store.authentication_required(&account).unwrap());
    let previous = std::fs::read(&source).unwrap();
    private::replace(
        &source,
        &codex_bytes("synthetic-new-access"),
        &crate::digest(previous),
    )
    .unwrap();
    auth::import_codex_auth(&store, &account, &source).unwrap();
    assert!(!store.authentication_required(&account).unwrap());
    assert!(
        store
            .settled_outcome(&session, 0)
            .unwrap()
            .unwrap()
            .facts
            .failure
            == Some(Failure::Authentication)
    );
    let current = store.session(&session).unwrap().unwrap();
    let run = store
        .prepare_run(&session, current.revision, crate::now_ms())
        .unwrap();
    store.settle(&run, State::Idle, crate::now_ms()).unwrap();
}

#[test]
fn authentication_health_token_import_keeps_unchanged_claude_and_devin_blocked() {
    for provider in [Provider::Claude, Provider::Devin] {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(&root.path().canonicalize().unwrap().join("state")).unwrap();
        let account = account(&store, provider);
        fail_authentication(&store, &account);
        let (old, new): (&[u8], &[u8]) = if provider == Provider::Claude {
            (
                b"sk-ant-oat01-synthetic_not_a_real_token",
                b"sk-ant-oat01-synthetic_changed_not_a_real_token",
            )
        } else {
            (b"synthetic-devin-token", b"synthetic-changed-devin-token")
        };
        let publish = |bytes: &[u8]| {
            if provider == Provider::Claude {
                auth::store_token(&store, &account, bytes)
            } else {
                crate::devin::auth::store_token(&store, &account, bytes)
            }
        };
        publish(old).unwrap();
        assert!(store.authentication_required(&account).unwrap());
        publish(new).unwrap();
        assert!(!store.authentication_required(&account).unwrap());
    }
}

#[test]
fn authentication_health_missing_legacy_table_is_read_only_and_not_backfilled() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().canonicalize().unwrap().join("state");
    let store = Store::open(&state).unwrap();
    let account = account(&store, Provider::Codex);
    fail_authentication(&store, &account);
    drop(store);
    let db = rusqlite::Connection::open(state.join("xcb.sqlite")).unwrap();
    db.execute("DROP TABLE account_auth_failures", []).unwrap();
    drop(db);
    let store = Store::open_read_only(&state).unwrap();
    assert!(!store.authentication_required(&account).unwrap());
    drop(store);
    let store = Store::open(&state).unwrap();
    // Old outcomes have no credential-generation binding. Never guess one.
    assert!(!store.authentication_required(&account).unwrap());
    drop(store);
    let db = rusqlite::Connection::open(state.join("xcb.sqlite")).unwrap();
    db.execute("DROP TABLE account_auth_failures", []).unwrap();
    db.execute("CREATE TABLE account_auth_failures(wrong_column TEXT)", [])
        .unwrap();
    drop(db);
    assert!(
        Store::open_read_only(&state)
            .unwrap()
            .authentication_required(&account)
            .is_err()
    );
}

#[test]
fn authentication_health_codex_rejects_account_and_user_switches_before_publication() {
    for account_switch in [true, false] {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(&root.path().canonicalize().unwrap().join("state")).unwrap();
        let account = account(&store, Provider::Codex);
        let session = fail_authentication(&store, &account);
        let before_account = serde_json::to_value(store.account(&account).unwrap()).unwrap();
        let before_session = serde_json::to_value(store.session(&session).unwrap()).unwrap();
        let before_generation =
            crate::application_qualification::read_generation(store.root(), &account).unwrap();
        let stored = store
            .account_root(&account)
            .unwrap()
            .join("profile/auth.json");
        let before_credentials = std::fs::read(&stored).unwrap();
        let mut value: serde_json::Value =
            serde_json::from_slice(&codex_bytes("synthetic-new-access")).unwrap();
        if account_switch {
            value["tokens"]["account_id"] = json!("foreign-account");
        } else {
            let claims = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(br#"{"sub":"foreign-user","https://api.openai.com/auth":{"chatgpt_account_id":"synthetic-account"}}"#);
            value["tokens"]["id_token"] = json!(format!("synthetic.{claims}.signature"));
        }
        let source = private::directory(&store.root().join("wrong-identity"))
            .unwrap()
            .join("auth.json");
        private::create(&source, &serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(
            auth::import_codex_auth(&store, &account, &source)
                .unwrap_err()
                .to_string()
                .contains("identity changed")
        );
        assert_eq!(std::fs::read(&stored).unwrap(), before_credentials);
        assert_eq!(
            crate::application_qualification::read_generation(store.root(), &account).unwrap(),
            before_generation
        );
        assert_eq!(
            serde_json::to_value(store.account(&account).unwrap()).unwrap(),
            before_account
        );
        assert_eq!(
            serde_json::to_value(store.session(&session).unwrap()).unwrap(),
            before_session
        );
        assert!(store.authentication_required(&account).unwrap());
        assert!(store.unsettled_runs().unwrap().is_empty());
    }
}
