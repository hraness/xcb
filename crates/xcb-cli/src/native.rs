use clap::Subcommand;
use serde_json::json;
use std::path::{Path, PathBuf};
use xcb_core::Provider;
use xcb_runtime::{
    Result,
    config::Config,
    native_backend::{self, NativeScope},
    process::Pin,
    runner,
    store::Store,
};

#[derive(Subcommand)]
pub(crate) enum Commands {
    #[command(about = "Inspect native provider execution and workspace grants")]
    Inspect {
        #[arg(
            long,
            help = "Inspect a test session's process outcome and native tool errors"
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

pub(crate) async fn execute(
    store: &std::sync::Arc<Store>,
    workspace: &Path,
    command: Commands,
    as_json: bool,
) -> Result<i32> {
    match command {
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
