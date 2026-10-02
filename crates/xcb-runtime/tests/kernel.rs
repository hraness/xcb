use std::collections::BTreeSet;
use std::{
    fs,
    sync::{Arc, mpsc::sync_channel},
    time::{Duration, Instant},
};
use xcb_core::{
    Id, Provider,
    models::{Mode, ModelChoice},
    policy::Failure,
    session::{Attachment, State},
    ui::{AccountRow, Intent, Update, View},
    usage::Estimate,
};
use xcb_runtime::{kernel, store::Store};

fn root() -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    fs::create_dir(directory.path().join("work")).unwrap();
    directory
}
fn choice() -> ModelChoice {
    ModelChoice {
        provider: Provider::Claude,
        id: Id::new("claude-fable-5-1").unwrap(),
        label: "Fable 5.1".into(),
        mode: Mode::Fixed,
        resolved: None,
        effort: Some(Id::new("max").unwrap()),
        observed_at_ms: 1,
    }
}
fn image() -> Attachment {
    Attachment {
        digest: "a".repeat(64),
        media_type: "image/png".into(),
        bytes: 512,
        width: 16,
        height: 16,
    }
}

/// A submission the kernel rejects must hand the complete draft — text and
/// attachments — back to the composer instead of losing it to history.
#[tokio::test]
async fn a_rejected_submission_returns_the_full_draft() {
    let dir = root();
    let base = xcb_core::canonical(dir.path()).unwrap();
    // No accounts exist, so every submission is rejected before it can run.
    let store = Arc::new(Store::open(&base.join("state")).unwrap());
    let (updates, display) = sync_channel(256);
    let (commands, input) = sync_channel(32);
    let serve = tokio::spawn(kernel::serve(
        store,
        base.join("work"),
        None,
        input,
        updates,
    ));

    let submission_id = Id::new("m_task").unwrap();
    let prompt = format!(
        "{}\nPreserve the final line: λ 🇵🇷",
        "do the whole thing\n".repeat(512)
    );
    commands
        .send(Intent::Submit {
            id: submission_id.clone(),
            text: prompt.clone(),
            attachments: vec![image()],
        })
        .unwrap();
    let collected = tokio::task::spawn_blocking(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .ok_or_else(|| "timed out waiting for the rejection response".to_owned())?;
            match display.recv_timeout(remaining) {
                Ok(Update::SubmitRejected {
                    id,
                    context,
                    text,
                    attachments,
                    reason,
                }) => return Ok((id, context, text, attachments, reason)),
                Ok(Update::Submitted { .. }) => {
                    return Err("a submission without an account was accepted".to_owned());
                }
                Ok(_) => (),
                Err(error) => return Err(format!("update channel failed: {error}")),
            }
        }
    })
    .await;
    drop(commands);
    serve.await.unwrap().unwrap();

    let (id, context, text, attachments, reason) = collected
        .unwrap()
        .expect("a rejected submission returns the draft");
    assert_eq!(id, submission_id);
    assert_eq!(
        context, None,
        "no session could be created without an account"
    );
    assert_eq!(text, prompt);
    assert_eq!(attachments, vec![image()]);
    assert!(
        reason.contains("account"),
        "the rejection is explained: {reason}"
    );
}

/// A session with a live run owned by a sibling terminal is active parallel
/// work: it renders remote, never "needs recovery".
#[tokio::test]
async fn a_run_owned_by_a_sibling_terminal_is_remote_not_recovery() {
    let dir = root();
    let base = xcb_core::canonical(dir.path()).unwrap();
    let path = base.join("state");
    // Terminal one opens the state root and starts a run on the session.
    let owner = Store::open(&path).unwrap();
    let account = owner.add_account(Provider::Claude, "Max", 1, None).unwrap();
    let session = owner
        .create_session(&account.id, choice(), &base.join("work"), 2)
        .unwrap();
    let run = owner.prepare_run(&session.id, session.revision, 3).unwrap();
    assert_eq!(run.phase, "prepared");

    // Terminal two opens the same root and views the same session.
    let viewer = Arc::new(Store::open(&path).unwrap());
    let (updates, display) = sync_channel(256);
    let (commands, input) = sync_channel(32);
    let serve = tokio::spawn(kernel::serve(
        viewer,
        base.join("work"),
        Some(session.id.clone()),
        input,
        updates,
    ));

    let seen = tokio::task::spawn_blocking(move || {
        for _ in 0..64 {
            match display.recv_timeout(Duration::from_secs(10)) {
                Ok(Update::View(view)) => return Some((view.remote_active, view.state)),
                Ok(_) => (),
                Err(error) => panic!("update channel failed: {error}"),
            }
        }
        None
    })
    .await
    .unwrap();
    drop(commands);
    serve.await.unwrap().unwrap();

    let (remote_active, state) = seen.expect("a view is always published");
    assert!(remote_active, "a live foreign-owned run is remote work");
    assert_eq!(state, State::Working, "remote work is not a recovery case");
}

#[test]
fn explicit_model_selects_its_provider_instead_of_an_unrelated_default_account() {
    let dir = root();
    let base = xcb_core::canonical(dir.path()).unwrap();
    let store = Store::open(&base.join("state")).unwrap();
    let claude = store
        .add_account(Provider::Claude, "Test", 1, None)
        .unwrap();
    let codex = store.add_account(Provider::Codex, "Test", 1, None).unwrap();
    let claude_model = choice();
    let codex_model = xcb_core::models::ModelChoice {
        provider: Provider::Codex,
        id: Id::new("codex-fixture").unwrap(),
        ..choice()
    };
    store
        .set_models(Provider::Claude, std::slice::from_ref(&claude_model))
        .unwrap();
    store
        .set_models(Provider::Codex, std::slice::from_ref(&codex_model))
        .unwrap();
    let config = xcb_runtime::config::Config {
        default_account: Some(claude.id.clone()),
        ..Default::default()
    };
    let session = kernel::new_session(
        &store,
        &base.join("work"),
        &config,
        None,
        Some(&codex_model.key()),
        None,
    )
    .unwrap();
    assert_eq!(session.account, codex.id);
    assert_eq!(session.model, codex_model);
    assert!(
        kernel::new_session(
            &store,
            &base.join("work"),
            &config,
            Some(&claude.id),
            Some(&codex_model.key()),
            None
        )
        .is_err()
    );
    store.set_account_enabled(&claude.id, false).unwrap();
    let session =
        kernel::new_session(&store, &base.join("work"), &config, None, None, None).unwrap();
    assert_eq!(session.account, codex.id);
    assert!(
        kernel::new_session(
            &store,
            &base.join("work"),
            &config,
            Some(&claude.id),
            None,
            None
        )
        .is_err()
    );
}

/// When a usage limit stops a turn and nothing else can take the task, the
/// terminal is told so in public words: the limited account, why each other
/// account was passed over, and the earliest known reset.
#[test]
fn a_usage_limit_without_a_fallback_is_explained_not_silent() {
    let now = 1_700_000_000_000;
    let row = |id: &str, provider: Provider| AccountRow {
        id: Id::new(id).unwrap(),
        provider,
        name: format!("{provider}/{id}"),
        email: None,
        subscription: "Max".into(),
        remaining_percent: None,
        resets_at_ms: None,
        quota_blocked_until_ms: None,
        runway: Estimate::unknown("quota_or_burn_unmeasured"),
        busy: false,
        active_runs: 0,
        enabled: true,
        authentication_required: false,
    };
    let view = View {
        accounts: vec![
            AccountRow {
                remaining_percent: Some(0.0),
                resets_at_ms: Some(now + 90 * 60_000),
                ..row("limited", Provider::Claude)
            },
            AccountRow {
                busy: true,
                active_runs: 1,
                ..row("working", Provider::Claude)
            },
            AccountRow {
                enabled: false,
                ..row("parked", Provider::Codex)
            },
        ],
        models: vec![choice()],
        ..View::default()
    };
    let credentialed = view.accounts.iter().map(|row| row.id.clone()).collect();
    let notice = kernel::failover_unavailable_notice(&kernel::FailoverNoticeInput {
        view: &view,
        account: &Id::new("limited").unwrap(),
        model: &choice(),
        failure: Failure::AccountQuota,
        tried: &BTreeSet::new(),
        limited_accounts: &BTreeSet::new(),
        admitted: &Provider::ALL.into_iter().collect(),
        credentialed: &credentialed,
        required_provider: None,
        run_limit: 1,
        now,
    });
    assert_eq!(
        notice,
        "Usage limit on claude · claude/limited · no other account is able to take the task now · 1 at its run limit · 1 disabled · earliest known reset in ~1h 30m"
    );
    for internal in ["lease", "custody", "eligible", "admitted", "credential"] {
        assert!(!notice.contains(internal), "{notice}");
    }
}
