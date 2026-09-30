//! Explicit live qualification; never part of the default test suite.
//! cargo run -p xcb-runtime --example qualify_computer -- <state> --inspect
//! cargo run -p xcb-runtime --example qualify_computer -- <state> <account> <model-key> <codex-home>
//! Add --metadata-only to inspect native initialization without model inference
//! or any browser/computer operations.
//! Uses the normal account lease and credential publication. Connector config
//! remains in memory. Creates and closes only one owned empty embedded tab;
//! never inventories user browsers, apps, or signed-in content.

#[cfg(target_os = "macos")]
#[tokio::main]
async fn main() -> xcb_runtime::Result<()> {
    use std::{path::PathBuf, sync::Arc};
    use xcb_core::{
        Provider,
        policy::{EffectState, Terminal},
        session::{Message, Role},
    };
    use xcb_runtime::{Error, config::Config, new_id, now_ms, runner, store::Store};

    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.as_slice() == ["native-mcp-stdio"] {
        let socket = std::env::var_os("XCB_MCP_SOCKET")
            .ok_or(Error::Unavailable("native MCP socket absent"))?;
        let token = std::env::var("XCB_MCP_TOKEN")
            .map_err(|_| Error::Unavailable("native MCP token absent"))?;
        return xcb_runtime::native_mcp::run_native_mcp_stdio(&PathBuf::from(socket), &token).await;
    }
    if args.len() == 2 && args[1] == "--inspect" {
        let store = Store::open_read_only(&PathBuf::from(&args[0]))?;
        let busy = store.unsettled_runs()?;
        for account in store
            .accounts()?
            .into_iter()
            .filter(|a| a.provider == Provider::Codex)
        {
            println!(
                "account={} enabled={} busy={} requires_auth={} credentials={}",
                account.id,
                account.enabled,
                busy.iter().any(|run| run.account == account.id),
                store.authentication_required(&account.id)?,
                xcb_runtime::auth::has_credentials(&store, &account.id)?
            );
        }
        for model in store
            .models()?
            .into_iter()
            .filter(|m| m.provider == Provider::Codex)
        {
            println!("model={}", model.key());
        }
        return Ok(());
    }
    let metadata_only = args.len() == 5 && args[4] == "--metadata-only";
    if args.len() != 4 && !metadata_only {
        return Err(Error::Unavailable(
            "expected state, account, model key, and Codex home",
        ));
    }
    let state = PathBuf::from(&args[0]);
    let store = Arc::new(Store::open(&state)?);
    let account = store.resolve_account(&args[1])?;
    if account.provider != Provider::Codex || !account.enabled {
        return Err(Error::Unavailable("choose an enabled Codex account"));
    }
    store.require_authenticated_account(&account.id)?;
    if store
        .unsettled_runs()?
        .iter()
        .any(|run| run.account == account.id)
    {
        return Err(Error::Conflict("selected account has an unsettled run"));
    }
    let model = store
        .models()?
        .into_iter()
        .find(|model| model.key() == args[2])
        .ok_or(Error::Unavailable(
            "select an observed explicit Codex model key",
        ))?;
    let workspace = tempfile::tempdir()?;
    let workspace_path = xcb_core::canonical(workspace.path())?;
    let mut config = Config::load(&state)?.0;
    config.auto_failover = false;
    config.turn_timeout_ms = 120_000;
    config.capabilities.servers = vec![xcb_runtime::cua_connector::registration(
        &PathBuf::from(&args[3]),
        &state.join("tool-bundles"),
        &workspace_path,
    )?];
    let connector = &mut config.capabilities.servers[0];
    connector
        .environment
        .insert("BROWSER_USE_AVAILABLE_BACKENDS".into(), "iab".into());
    connector
        .environment
        .insert("CUA_REPL_ENABLED_SURFACES".into(), "browser".into());
    connector.features = vec![xcb_runtime::capabilities::CapabilityFeature::Browser];
    config.validate()?;
    if metadata_only {
        let inspected = runner::inspect_computer_connector(
            store,
            &account.id,
            &model,
            config.capabilities.servers.remove(0),
            workspace_path,
        )
        .await?;
        println!(
            "native_startup={:?} relay_failed={} ready={}",
            inspected.diagnostic.startup,
            inspected.diagnostic.failed,
            inspected.initialization.is_ok()
        );
        if let Some(fingerprint) = inspected.unhandled_notice_sha256 {
            println!("unhandled_notice_sha256={fingerprint}");
        }
        return inspected.initialization;
    }
    let session = store.create_session(&account.id, model, &workspace_path, now_ms())?;
    let message = Message {
        id: new_id("message"), role: Role::User, at_ms: now_ms(), attachments: vec![], provenance: None,
        text: "Qualify only xcb's native connection to a NEW EMPTY in-app browser tab. No user browsing or account content is authorized. First call cua_repl js with exactly `let xcbBlankTab = await cua.createBrowserTab(\"iab\", \"about:blank\", {visible:false});`. This must create a new owned empty embedded tab and return only that blank tab's initial state. Read the returned documentation. If creation succeeds, the second and final call must close ONLY that returned binding: `try {} finally { await xcbBlankTab.close(); }`. Do not call getState, listBrowsers, listTabs, listApps, getBrowser, getTab, getApp, or any other tool/API. Never inspect an existing tab, app, URL, profile, cookie, credentials, or files. Do not use Chrome, Edge, or any fallback. If IAB is unavailable, automatic review declines, or any step fails, stop without retries and reply only XCB_COMPUTER_BLOCKED. Only if both creation and close succeed, reply only XCB_COMPUTER_QUALIFIED. Preserve all existing user state; do not include any account or browser details in your final reply.".into(),
    };
    let session = store.append_message(&session.id, session.revision, &message)?;
    let session_id = session.id.clone();
    let (_cancel, cancellation) = tokio::sync::watch::channel(false);
    let result = runner::run(
        store.clone(),
        runner::RunInput {
            session,
            message,
            config,
            pane_generation: false,
        },
        cancellation,
        Arc::new(|_| {}),
    )
    .await;
    // An uncertain run keeps its workspace for normal recovery, even when
    // the provider returned an error before producing an Outcome.
    let retain_workspace = match store.unsettled_runs() {
        Ok(runs) => runs
            .iter()
            .any(|run| run.session.as_ref() == Some(&session_id)),
        Err(_) => true,
    };
    if retain_workspace {
        let _ = workspace.keep();
    }
    let outcome = match result {
        Ok(outcome) => outcome,
        Err(error) => {
            eprintln!("qualification session={session_id} failed: {error}");
            return Err(error);
        }
    };
    let native_replies = store
        .messages(&session_id, 128)?
        .into_iter()
        .filter(|message| message.role == Role::Tool && message.text.starts_with("cua_repl/js:"))
        .count();
    let confirmed = outcome.text.trim() == "XCB_COMPUTER_QUALIFIED";
    println!(
        "session={session_id} state={:?} terminal={:?} joined={} effects={:?} attention={} native_replies={} confirmed={confirmed}",
        outcome.state,
        outcome.facts.terminal,
        outcome.facts.joined,
        outcome.facts.effects,
        outcome.facts.pending_attention,
        native_replies
    );
    if let Some(diagnostic) = outcome.diagnostic {
        println!("diagnostic={}", diagnostic.as_str());
    }
    if confirmed
        && native_replies == 2
        && outcome.facts.joined
        && outcome.facts.terminal == Terminal::Completed
        && outcome.facts.effects == EffectState::Settled
        && !outcome.facts.pending_attention
    {
        Ok(())
    } else {
        Err(Error::Unavailable(
            "owned empty-tab qualification did not pass; connector configuration was not activated",
        ))
    }
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("native computer qualification requires macOS");
    std::process::exit(1);
}
