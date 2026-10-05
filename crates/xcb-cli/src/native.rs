use clap::{Subcommand, ValueEnum};
use serde_json::json;
use std::path::{Path, PathBuf};
use xcb_core::{Provider, session::State};
use xcb_runtime::{
    Error, Result,
    config::Config,
    native_backend::{self, NativeScope, StatusQuery},
    process::Pin,
    runner,
    store::Store,
};

#[derive(Clone, Copy, ValueEnum)]
pub(crate) enum StatusSection {
    Accounts,
    Sessions,
    Runs,
    Effects,
}

impl StatusSection {
    fn name(self) -> &'static str {
        match self {
            Self::Accounts => "accounts",
            Self::Sessions => "sessions",
            Self::Runs => "runs",
            Self::Effects => "effects",
        }
    }
    fn record_type(self) -> &'static str {
        match self {
            Self::Accounts => "account",
            Self::Sessions => "session",
            Self::Runs => "run",
            Self::Effects => "effect",
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
pub(crate) enum StatusState {
    Idle,
    Working,
    #[value(name = "needs_answer")]
    NeedsAnswer,
    #[value(name = "needs_action")]
    NeedsAction,
    #[value(name = "needs_approval")]
    NeedsApproval,
    Limited,
    Failed,
    Cancelled,
    Uncertain,
}

impl From<StatusState> for State {
    fn from(state: StatusState) -> Self {
        match state {
            StatusState::Idle => Self::Idle,
            StatusState::Working => Self::Working,
            StatusState::NeedsAnswer => Self::NeedsAnswer,
            StatusState::NeedsAction => Self::NeedsAction,
            StatusState::NeedsApproval => Self::NeedsApproval,
            StatusState::Limited => Self::Limited,
            StatusState::Failed => Self::Failed,
            StatusState::Cancelled => Self::Cancelled,
            StatusState::Uncertain => Self::Uncertain,
        }
    }
}

#[derive(Subcommand)]
pub(crate) enum Commands {
    #[command(
        about = "Read local account, session, run and tool status without starting a provider"
    )]
    Status {
        #[arg(long, help = "Only records for this provider")]
        provider: Option<Provider>,
        #[arg(long, help = "Only records for this account name or id")]
        account: Option<String>,
        #[arg(long, help = "Only records for this session id")]
        session: Option<xcb_core::Id>,
        #[arg(long, value_enum, help = "Only records for this session state")]
        session_state: Option<StatusState>,
        #[arg(long, help = "Only records holding an account slot")]
        has_lease: bool,
        #[arg(
            long = "unfinished-effects",
            alias = "unsettled-effects",
            help = "Only records with tool work still marked unfinished"
        )]
        unsettled_effects: bool,
        #[arg(
            long = "pending-command-proof",
            alias = "pending-command-custody",
            help = "Only records whose command stop or join proof is still pending"
        )]
        pending_command_custody: bool,
        #[arg(
            long,
            default_value_t = 64,
            value_parser = clap::value_parser!(u16).range(1..=256),
            help = "Maximum records in each section, from 1 to 256"
        )]
        limit: u16,
        #[arg(
            long,
            help = "Read records after the nextCursor value from a previous response"
        )]
        cursor: Option<u64>,
        #[arg(
            long,
            requires = "section",
            help = "Emit one JSON record per line; requires --section"
        )]
        jsonl: bool,
        #[arg(
            long,
            value_enum,
            requires = "jsonl",
            help = "Section to render as JSON lines"
        )]
        section: Option<StatusSection>,
    },
    #[command(about = "Inspect native provider execution and workspace grants")]
    Inspect {
        #[arg(
            long,
            help = "Inspect a session's stored run, process and tool records"
        )]
        session: Option<xcb_core::Id>,
    },
    #[command(
        about = "List the checked provider methods and their integration status without starting a provider"
    )]
    Methods {
        #[arg(long, help = "Provider whose checked task-adapter methods to list")]
        provider: Provider,
    },
    #[command(
        about = "Read provider startup methods, models and account metadata without a model prompt"
    )]
    Describe {
        #[arg(long, help = "Existing Claude or Codex account name or id to inspect")]
        account: String,
        #[arg(
            long,
            help = "Also inspect this Claude metadata connection's MCP status and summary context; not a running task"
        )]
        runtime_status: bool,
    },
    #[command(about = "Verify native command filesystem confinement and DNS/HTTPS access")]
    Qualify,
    #[command(
        about = "Verify one provider's native shell, DNS/HTTPS and Git in a disposable workspace"
    )]
    Verify {
        #[arg(
            long,
            help = "Provider to test using its supported installed build and a signed-in account"
        )]
        provider: Provider,
        #[arg(long, help = "Restrict this check to an existing account name or id")]
        account: Option<String>,
        #[arg(
            long,
            help = "Also verify host GitHub credentials using read-only authenticated requests"
        )]
        github: bool,
    },
    #[command(about = "Grant native execution to this exact workspace and selected providers")]
    Grant {
        #[arg(
            long = "provider",
            help = "Provider to grant; repeat to select several, omit for Claude and Codex"
        )]
        providers: Vec<Provider>,
        #[arg(
            long,
            help = "Make host GitHub credentials available only to native command children"
        )]
        github: bool,
        #[arg(
            long = "read-only-root",
            help = "Explicit read-only toolchain directory; repeat for each directory"
        )]
        read_only_roots: Vec<PathBuf>,
        #[arg(
            long = "git-metadata",
            help = "Explicit writable Git metadata directory outside the worktree; repeat as needed"
        )]
        git_metadata: Vec<PathBuf>,
    },
}

fn status_jsonl_records(status: &serde_json::Value, section: StatusSection) -> Result<Vec<String>> {
    let section_value = status
        .get(section.name())
        .ok_or(Error::Unavailable("status section is unavailable"))?;
    let records = section_value
        .get("records")
        .and_then(serde_json::Value::as_array)
        .ok_or(Error::Unavailable("status records are unavailable"))?;
    let meta = json!({
        "type": "meta",
        "version": status.get("version").cloned().unwrap_or_else(|| json!(1)),
        "generatedAtMs": status.get("generatedAtMs"),
        "section": section.name(),
        "matched": section_value.get("matched"),
        "cursor": section_value.get("cursor"),
        "returned": section_value.get("returned"),
        "truncated": section_value.get("truncated"),
        "nextCursor": section_value.get("nextCursor"),
        "filters": status.get("filters"),
        "inspection": status.get("inspection"),
    });
    let mut lines = vec![serde_json::to_string(&meta)?];
    for record in records {
        let mut record = record.clone();
        record
            .as_object_mut()
            .ok_or(Error::Unavailable("status record is unavailable"))?
            .insert("type".into(), json!(section.record_type()));
        lines.push(serde_json::to_string(&record)?);
    }
    Ok(lines)
}

fn print_status_summary(status: &serde_json::Value) {
    let total = |name: &str| status["totals"][name].as_u64().unwrap_or(0);
    let section = |name: &str| {
        (
            status[name]["matched"].as_u64().unwrap_or(0),
            status[name]["returned"].as_u64().unwrap_or(0),
        )
    };
    let (accounts, shown_accounts) = section("accounts");
    let (sessions, shown_sessions) = section("sessions");
    let (runs, shown_runs) = section("runs");
    let (effects, shown_effects) = section("effects");
    println!(
        "Local status: {accounts} accounts, {sessions} sessions, {runs} runs, {effects} tool records"
    );
    println!(
        "Shown: {shown_accounts} accounts, {shown_sessions} sessions, {shown_runs} runs, {shown_effects} tool records"
    );
    println!(
        "Unfinished: {} runs, {} tool records; {} held account slots, {} commands awaiting stop or join proof",
        total("unsettledRuns"),
        total("unsettledEffects"),
        total("heldAccounts"),
        total("pendingCommandCustody")
    );
    let unlinked = total("unlinkedToolEffects");
    if unlinked > 0 {
        println!("Unlinked tool records without a stored run: {unlinked}");
    }
    let service = &status["service"];
    if service["available"].as_bool().unwrap_or(false) {
        println!(
            "Service: {}",
            if service["supervisorRunning"].as_bool().unwrap_or(false) {
                "running"
            } else if service["installed"].as_bool().unwrap_or(false) {
                "installed, not running"
            } else {
                "not installed"
            }
        );
    }
    println!(
        "Native commands: {}",
        if status["native"]["commandQualified"]
            .as_bool()
            .unwrap_or(false)
        {
            "qualified"
        } else {
            "not qualified"
        }
    );
}

pub(crate) async fn execute(
    store: &std::sync::Arc<Store>,
    workspace: &Path,
    command: Commands,
    as_json: bool,
) -> Result<i32> {
    match command {
        Commands::Status {
            provider,
            account,
            session,
            session_state,
            has_lease,
            unsettled_effects,
            pending_command_custody,
            limit,
            cursor,
            jsonl,
            section,
        } => {
            let account = account
                .map(|name| store.resolve_account(&name).map(|account| account.id))
                .transpose()?;
            let status = native_backend::status_snapshot(
                store,
                StatusQuery {
                    provider,
                    account,
                    session,
                    state: session_state.map(State::from),
                    has_lease,
                    unsettled_effects,
                    pending_command_custody,
                    limit: u64::from(limit),
                    cursor: cursor.unwrap_or(0),
                },
            )?;
            if jsonl {
                let section =
                    section.ok_or(Error::Unavailable("JSON lines output requires --section"))?;
                for line in status_jsonl_records(&status, section)? {
                    println!("{line}");
                }
            } else if as_json {
                crate::print_json(status)?;
            } else {
                print_status_summary(&status);
            }
        }
        Commands::Inspect { session: Some(id) } => {
            crate::print_json(native_backend::inspect_session(store, &id)?)?;
        }
        Commands::Inspect { session: None } => {
            let (config, _) = Config::load(store.root())?;
            let command_qualified = native_backend::require_qualification(store.root()).is_ok();
            let backends: Vec<_> = Provider::SUPPORTED
                .into_iter()
                .map(|provider| {
                    let mut status = native_backend::status(provider);
                    status.qualified = native_backend::require_provider_qualification(
                        store.root(),
                        provider,
                        false,
                    )
                    .is_ok()
                        && Pin::load(store.root(), provider)
                            .is_ok_and(|pin| runner::provider_admitted(store.root(), &pin));
                    status
                })
                .collect();
            if as_json {
                crate::print_json(
                    json!({"version":1,"commandQualified":command_qualified,"backends":backends,"scopes":config.native_execution.scopes}),
                )?;
            } else {
                for backend in backends {
                    println!(
                        "{}: native commands {}",
                        backend.provider,
                        if backend.qualified {
                            "qualified"
                        } else {
                            "unavailable until qualification"
                        }
                    );
                }
                println!("{} workspace grants", config.native_execution.scopes.len());
            }
        }
        Commands::Methods { provider } => {
            crate::print_json(xcb_runtime::provider_methods::describe(provider)?)?;
        }
        Commands::Describe {
            account,
            runtime_status,
        } => {
            let account = store.resolve_account(&account)?;
            let provider = account.provider;
            if !Provider::SUPPORTED.contains(&provider)
                || (runtime_status && provider != Provider::Claude)
            {
                return Err(xcb_runtime::Error::Unavailable(
                    "runtime status requires Claude; startup inspection supports Claude and Codex",
                ));
            }
            let pin = Pin::load(store.root(), provider)?;
            let mut stop = crate::stop::Stop::install()?;
            let metadata = match provider {
                Provider::Claude => {
                    let metadata = if runtime_status {
                        stop.settle(Box::pin(runner::probe_claude_diagnostics(
                            store,
                            &pin,
                            &account.id,
                        )))
                        .await?
                    } else {
                        stop.settle(Box::pin(runner::probe_claude_metadata(
                            store,
                            &pin,
                            Some(&account.id),
                        )))
                        .await?
                    };
                    store.set_account_models(&account.id, &metadata.models)?;
                    serde_json::to_value(metadata)?
                }
                Provider::Codex => {
                    let metadata = stop
                        .settle(Box::pin(runner::probe_codex_metadata(
                            store,
                            &pin,
                            &account.id,
                        )))
                        .await?;
                    store.set_account_models(&account.id, &metadata.models)?;
                    serde_json::to_value(metadata)?
                }
                Provider::Devin => unreachable!(),
            };
            crate::print_json(
                json!({"version":1,"provider":provider,"accountId":account.id,"providerVersion":pin.version,"providerSha256":pin.sha256,"modelPromptSubmitted":false,"resetCreditsConsumed":false,"inspectionScope":"metadataProbe","nativeQualified":native_backend::require_provider_qualification(store.root(), provider, false).is_ok(),"accountNativeAcceptanceEstablished":false,"metadata":metadata,"methods":xcb_runtime::provider_methods::describe(provider)?}),
            )?;
        }
        Commands::Qualify => {
            let receipt = crate::stop::Stop::install()?
                .settle(Box::pin(xcb_runtime::command_tool::qualify_native(
                    store.root(),
                )))
                .await?;
            if as_json {
                crate::print_json(receipt)?;
            } else {
                println!("Native command confinement and DNS/HTTPS checks passed.");
            }
        }
        Commands::Verify {
            provider,
            account,
            github,
        } => {
            let account = account
                .map(|name| store.resolve_account(&name).map(|account| account.id))
                .transpose()?;
            let (cancel, cancelled) = tokio::sync::watch::channel(false);
            let mut stop = crate::stop::Stop::install()?;
            let _interrupt = crate::AbortOnDrop(tokio::spawn(async move {
                stop.recv().await;
                let _ = cancel.send(true);
            }));
            let receipt = Box::pin(xcb_runtime::native_verification::verify_for_account(
                store.clone(),
                provider,
                account,
                github,
                cancelled,
            ))
            .await?;
            if as_json {
                crate::print_json(receipt)?;
            } else {
                println!("{provider} native shell, DNS/HTTPS and Git checks passed.");
            }
        }
        Commands::Grant {
            providers,
            github,
            read_only_roots,
            git_metadata,
        } => {
            native_backend::require_qualification(store.root())?;
            let workspace = xcb_core::canonical(workspace)?;
            let providers = if providers.is_empty() {
                Provider::SUPPORTED.to_vec()
            } else {
                providers
            };
            for provider in &providers {
                native_backend::require_provider_qualification(store.root(), *provider, github)?;
                let pin = Pin::load(store.root(), *provider)?;
                if !runner::provider_admitted(store.root(), &pin) {
                    return Err(xcb_runtime::Error::Unavailable(
                        "native provider build is not admitted",
                    ));
                }
            }
            let scope = NativeScope {
                workspace,
                providers,
                github_credentials: github,
                read_only_roots: read_only_roots
                    .into_iter()
                    .map(xcb_core::canonical)
                    .collect::<std::io::Result<_>>()?,
                git_metadata: git_metadata
                    .into_iter()
                    .map(xcb_core::canonical)
                    .collect::<std::io::Result<_>>()?,
            };
            native_backend::validate_grant(&scope, store.root())?;
            let (mut config, revision) = Config::load(store.root())?;
            config
                .native_execution
                .scopes
                .retain(|existing| existing.workspace != scope.workspace);
            config.native_execution.scopes.push(scope.clone());
            config.save(store.root(), revision.as_deref())?;
            if as_json {
                crate::print_json(json!({"version":1,"granted":scope}))?;
            } else {
                println!(
                    "Native execution granted for {}.",
                    scope.workspace.display()
                );
            }
        }
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn native_status_parses_exact_filters_and_jsonl_section() {
        let cli = crate::Cli::try_parse_from([
            "xcb",
            "native",
            "status",
            "--provider",
            "codex",
            "--account",
            "fixture-account",
            "--session",
            "s_fixture",
            "--session-state",
            "needs_answer",
            "--has-lease",
            "--unfinished-effects",
            "--pending-command-proof",
            "--limit",
            "25",
            "--cursor",
            "2",
            "--jsonl",
            "--section",
            "sessions",
        ])
        .unwrap();
        assert!(
            matches!(cli.command, Some(crate::Commands::Native { command: Commands::Status { provider: Some(Provider::Codex), account: Some(account), session: Some(session), session_state: Some(StatusState::NeedsAnswer), has_lease: true, unsettled_effects: true, pending_command_custody: true, limit: 25, cursor: Some(2), jsonl: true, section: Some(StatusSection::Sessions) } }) if account == "fixture-account" && session.as_str() == "s_fixture")
        );
        let aliases = crate::Cli::try_parse_from([
            "xcb",
            "native",
            "status",
            "--unsettled-effects",
            "--pending-command-custody",
        ])
        .unwrap();
        assert!(matches!(
            aliases.command,
            Some(crate::Commands::Native {
                command: Commands::Status {
                    unsettled_effects: true,
                    pending_command_custody: true,
                    ..
                }
            })
        ));
    }

    #[test]
    fn native_status_rejects_ambiguous_or_out_of_range_output_options() {
        for args in [
            vec!["xcb", "native", "status", "--jsonl"],
            vec!["xcb", "native", "status", "--section", "sessions"],
            vec!["xcb", "native", "status", "--section", "bogus", "--jsonl"],
            vec!["xcb", "native", "status", "--limit", "0"],
            vec!["xcb", "native", "status", "--session-state", "needs-answer"],
        ] {
            assert!(crate::Cli::try_parse_from(args).is_err());
        }
    }

    #[test]
    fn native_status_jsonl_has_one_meta_record_and_typed_rows() {
        let status = json!({
            "version": 1,
            "generatedAtMs": 5,
            "filters": {"provider": "codex"},
            "inspection": {"localOnly": true},
            "sessions": {
                "matched": 2,
                "cursor": 0,
                "returned": 1,
                "truncated": true,
                "nextCursor": 1,
                "records": [{"id":"s_fixture","state":"working"}],
            },
        });
        let lines = status_jsonl_records(&status, StatusSection::Sessions).unwrap();
        assert_eq!(lines.len(), 2);
        let meta: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
        assert_eq!(meta["type"], "meta");
        assert_eq!(meta["section"], "sessions");
        assert_eq!(meta["matched"], 2);
        assert_eq!(meta["nextCursor"], 1);
        let record: serde_json::Value = serde_json::from_str(&lines[1]).unwrap();
        assert_eq!(record["type"], "session");
        assert_eq!(record["id"], "s_fixture");
        assert!(lines.iter().all(|line| !line.contains('\n')));
    }

    #[test]
    fn native_verification_retains_an_explicit_account_selector() {
        let cli = crate::Cli::try_parse_from([
            "xcb",
            "native",
            "verify",
            "--provider",
            "claude",
            "--account",
            "new-account",
            "--github",
        ])
        .unwrap();
        assert!(
            matches!(cli.command, Some(crate::Commands::Native { command: Commands::Verify { provider: Provider::Claude, account: Some(account), github: true } }) if account == "new-account")
        );
    }

    #[test]
    fn method_inventory_and_runtime_inspection_accept_no_mutation_or_prompt() {
        for provider in ["claude", "codex"] {
            let cli =
                crate::Cli::try_parse_from(["xcb", "native", "methods", "--provider", provider])
                    .unwrap();
            assert!(matches!(
                cli.command,
                Some(crate::Commands::Native {
                    command: Commands::Methods { .. }
                })
            ));
            assert!(
                crate::Cli::try_parse_from([
                    "xcb",
                    "native",
                    "methods",
                    "--provider",
                    provider,
                    "--method",
                    "turn/start"
                ])
                .is_err()
            );
        }
        let cli = crate::Cli::try_parse_from([
            "xcb",
            "native",
            "describe",
            "--account",
            "new-account",
            "--runtime-status",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(crate::Commands::Native {
                command: Commands::Describe {
                    runtime_status: true,
                    ..
                }
            })
        ));
        assert!(
            crate::Cli::try_parse_from([
                "xcb",
                "native",
                "describe",
                "--account",
                "new-account",
                "--detail",
                "full"
            ])
            .is_err()
        );
    }

    #[test]
    fn startup_description_requires_an_account_and_accepts_no_prompt() {
        assert!(crate::Cli::try_parse_from(["xcb", "native", "describe"]).is_err());
        assert!(
            crate::Cli::try_parse_from([
                "xcb",
                "native",
                "describe",
                "--account",
                "new-account",
                "--prompt",
                "do work"
            ])
            .is_err()
        );
        let cli =
            crate::Cli::try_parse_from(["xcb", "native", "describe", "--account", "new-account"])
                .unwrap();
        assert!(
            matches!(cli.command, Some(crate::Commands::Native { command: Commands::Describe { account, runtime_status: false } }) if account == "new-account")
        );
    }
}
