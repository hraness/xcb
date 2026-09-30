use clap::Subcommand;
use serde_json::json;
use std::{io::Read, path::PathBuf};
use xcb_runtime::{
    Error, Result,
    capabilities::{CapabilityServer, CapabilityTransport},
    config::Config,
    store::Store,
};

#[derive(Subcommand)]
pub enum Commands {
    /// Show tools configured for every provider and installed computer-use support.
    List,
    /// Connect the installed desktop browser and computer tools with automatic review.
    SetupComputer,
    /// Connect Claude's Chrome extension to Codex, Claude, and Devin.
    SetupBrowser {
        /// Claude account to use for the browser connection. Selected automatically when only one is signed in.
        #[arg(long)]
        account: Option<String>,
    },
    /// Register a trusted MCP server from a JSON launch definition.
    Add {
        /// Path to the trusted server's JSON launch definition.
        definition: PathBuf,
    },
    /// Remove a registered host tool server.
    Remove {
        /// Registered server name from xcb tools list.
        name: String,
    },
}

pub async fn execute(store: &Store, command: Commands, machine: bool) -> Result<i32> {
    let (mut config, revision) = Config::load(store.root())?;
    match command {
        Commands::List => {
            let servers = config
                .capabilities
                .servers
                .iter()
                .map(|server| {
                    json!({
                        "name": server.name, "features": server.features,
                        "tools": server.tools, "providers": providers(server),
                    })
                })
                .collect::<Vec<_>>();
            let home = std::env::var_os("CODEX_HOME")
                .map(PathBuf::from)
                .or_else(|| {
                    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex"))
                });
            let installed = home
                .as_deref()
                .map(xcb_runtime::cua_connector::inspect_installed)
                .transpose();
            let (computer, diagnostic) = match installed {
                Ok(value) => (serde_json::to_value(value.flatten())?, None),
                Err(error) => (serde_json::Value::Null, Some(error.to_string())),
            };
            if machine {
                println!(
                    "{}",
                    json!({"servers":servers,"installedComputerUse":computer,"diagnostic":diagnostic})
                );
            } else {
                if servers.is_empty() {
                    println!("No host tool servers configured.");
                }
                for server in &config.capabilities.servers {
                    println!(
                        "{} — configured for {}",
                        server.name,
                        providers(server).join(", ")
                    );
                }
                if let Some(detail) = computer.get("detail").and_then(|detail| detail.as_str()) {
                    println!("{detail}");
                }
                if let Some(detail) = diagnostic {
                    println!("Computer-use check: {detail}");
                }
                println!("Add a trusted server with xcb tools add <definition.json>.");
            }
        }
        Commands::SetupComputer => {
            let home = std::env::var_os("CODEX_HOME")
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex")))
                .ok_or(Error::Unavailable(
                    "cannot locate the desktop computer-use installation",
                ))?;
            let workspace = xcb_core::canonical(&std::env::current_dir()?)?;
            let server = xcb_runtime::cua_connector::registration(
                &home,
                &store.root().join("tool-bundles"),
                &workspace,
            )?;
            config
                .capabilities
                .servers
                .retain(|entry| entry.name != server.name);
            config.capabilities.servers.push(server);
            config.save(store.root(), revision.as_deref())?;
            if machine {
                println!(
                    "{}",
                    json!({"configured":"cua_repl", "providers":["codex"], "reviewer":"auto_review"})
                );
            } else {
                println!(
                    "Configured desktop browser and computer tools for Codex with automatic approval review."
                );
                println!(
                    "Tasks that need a signed-in browser stay with Codex, including work handed off by Claude or Devin."
                );
            }
        }
        Commands::SetupBrowser { account } => {
            let account = match account {
                Some(selector) => store.resolve_account(&selector)?,
                None => {
                    let mut accounts = Vec::new();
                    for account in store.accounts()? {
                        if account.provider == xcb_core::Provider::Claude
                            && account.enabled
                            && !store.authentication_required(&account.id)?
                            && xcb_runtime::auth::has_credentials(store, &account.id)?
                        {
                            accounts.push(account);
                        }
                    }
                    if accounts.len() != 1 {
                        return Err(Error::Unavailable(if accounts.is_empty() {
                            "sign in to a Claude account with xcb setup claude before connecting its browser extension"
                        } else {
                            "choose the Claude account for your browser: xcb tools setup-browser --account <name>"
                        }));
                    }
                    accounts.into_iter().next().ok_or(Error::Unavailable(
                        "sign in to a Claude account with xcb setup claude before connecting its browser extension",
                    ))?
                }
            };
            if account.provider != xcb_core::Provider::Claude || !account.enabled {
                return Err(Error::Unavailable(
                    "the browser connection needs an enabled Claude account",
                ));
            }
            store.require_authenticated_account(&account.id)?;
            if !xcb_runtime::auth::has_credentials(store, &account.id)? {
                return Err(Error::Unavailable(
                    "sign in to the selected Claude account first",
                ));
            }
            let pin = xcb_runtime::process::Pin::load(store.root(), xcb_core::Provider::Claude)?;
            let workspace = xcb_core::canonical(&std::env::current_dir()?)?;
            let mut server = xcb_runtime::chrome_connector::registration(&pin, &workspace)?;
            server.credential_account = Some(account.id);
            let (cancel, cancelled) = tokio::sync::watch::channel(false);
            let mut stop = crate::stop::Stop::install()?;
            let _interrupt = crate::AbortOnDrop(tokio::spawn(async move {
                stop.recv().await;
                let _ = cancel.send(true);
            }));
            xcb_runtime::chrome_connector::setup(
                store.root(),
                server.clone(),
                &workspace,
                cancelled,
            )
            .await?;
            config
                .capabilities
                .servers
                .retain(|entry| entry.name != server.name);
            config.capabilities.servers.push(server);
            config.save(store.root(), revision.as_deref())?;
            if machine {
                println!(
                    "{}",
                    json!({"configured":"claude_browser", "providers":["codex","claude","devin"]})
                );
            } else {
                println!("Configured Claude's Chrome extension for Codex, Claude, and Devin.");
                println!(
                    "The extension's browser group stays open between tasks; xcb closes only tabs created by each task."
                );
                println!(
                    "Keep the extension signed in to the selected Claude account. Browser permission prompts still apply."
                );
            }
        }
        Commands::Add { definition } => {
            let mut options = std::fs::OpenOptions::new();
            options.read(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.custom_flags(
                    (rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32,
                );
            }
            if std::fs::symlink_metadata(&definition)?
                .file_type()
                .is_symlink()
            {
                return Err(Error::Unavailable("tool definition must be a regular file"));
            }
            let file = options.open(&definition)?;
            if !file.metadata()?.is_file() {
                return Err(Error::Unavailable("tool definition must be a regular file"));
            }
            let mut bytes = Vec::new();
            file.take(64 * 1024 + 1).read_to_end(&mut bytes)?;
            if bytes.len() > 64 * 1024 {
                return Err(Error::Unavailable("tool definition exceeds 64 KiB"));
            }
            let server: CapabilityServer = serde_json::from_slice(&bytes)?;
            server.validate()?;
            if config
                .capabilities
                .servers
                .iter()
                .any(|entry| entry.name == server.name)
            {
                return Err(Error::Unavailable(
                    "tool server is already registered; remove it before replacing its launch definition",
                ));
            }
            let name = server.name.clone();
            let available = providers(&server).join(", ");
            config.capabilities.servers.push(server);
            config.save(store.root(), revision.as_deref())?;
            if machine {
                println!("{}", json!({"added":name}));
            } else {
                println!(
                    "Added {name} for {available}. xcb checks its executable before each launch."
                );
            }
        }
        Commands::Remove { name } => {
            let count = config.capabilities.servers.len();
            config
                .capabilities
                .servers
                .retain(|entry| entry.name != name);
            if count == config.capabilities.servers.len() {
                return Err(Error::Unavailable("tool server is not registered"));
            }
            config.save(store.root(), revision.as_deref())?;
            if machine {
                println!("{}", json!({"removed":name}));
            } else {
                println!("Removed {name}. Existing runs keep their current tools until they stop.");
            }
        }
    }
    Ok(0)
}

fn providers(server: &CapabilityServer) -> Vec<&'static str> {
    match server.transport {
        CapabilityTransport::Shared => vec!["codex", "claude", "devin"],
        CapabilityTransport::CodexNative => vec!["codex"],
    }
}
