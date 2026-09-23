mod application;

use clap::{CommandFactory, Parser, Subcommand};
use serde_json::json;
use std::{
    io::{self, IsTerminal, Read},
    path::PathBuf,
    sync::{Arc, mpsc::sync_channel},
};
use tokio::sync::watch;
use xcb_core::{
    Id, Provider,
    models::{Preference, sort_choices},
    panes::Pane,
    policy::Terminal,
};
use xcb_runtime::{
    Error, Result, auth,
    config::Config,
    exports, hooks, judge, kernel, now_ms, panes, private,
    process::{self, Pin},
    runner::{self, Observer, Progress},
    store::Store,
    summary,
};

#[derive(Parser)]
#[command(
    name = "xcb",
    version,
    about = "Excalibur — a local, composable terminal workspace for coding agents",
    after_help = "Plain `xcb` opens a persistent managed conversation in the terminal UI.\n\nFirst run:\n  xcb accounts add <provider> --plan <label>\n  xcb doctor --provider <provider>\n  xcb accounts login <account-id>\n  xcb accounts refresh <account-id>\n  xcb"
)]
struct Cli {
    /// State root for accounts, sessions, and tasks (default:
    /// ~/.local/share/xcb, or $XCB_STATE).
    #[arg(long, global = true)]
    state: Option<PathBuf>,
    /// Emit machine-readable JSON where a command supports it.
    #[arg(long, global = true)]
    json: bool,
    /// Workspace the command applies to (run, chat, models route).
    #[arg(long, global = true, default_value = ".")]
    cwd: PathBuf,
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Open a persistent control conversation; workers continue after detach.
    Chat {
        /// Reopen this control conversation instead of starting a new one.
        #[arg(long)]
        resume: Option<Id>,
    },
    /// Bounded, ephemeral application inference with no tools or hooks.
    Generate {
        /// Print capability rows and exit without running inference.
        #[arg(long)]
        capabilities: bool,
    },
    /// Read bounded private failure metadata for one exact application request.
    ApplicationDiagnostic {
        /// Account that ran the request.
        #[arg(long)]
        account: Id,
        /// Request identifier to inspect.
        #[arg(long)]
        request: Id,
    },
    /// Run a fixed application qualification challenge using private gate evidence.
    QualifyApplication {
        /// Account the qualification runs under.
        #[arg(long)]
        account: Id,
        /// Model the qualification runs under.
        #[arg(long)]
        model: String,
        /// Evidence bundle produced by the private qualification gate.
        #[arg(long, required_unless_present = "inspect", conflicts_with = "inspect")]
        evidence: Option<PathBuf>,
        /// Renew only if this existing credential generation still matches.
        #[arg(long, conflicts_with = "inspect", value_parser = parse_expected_generation)]
        expected_generation: Option<String>,
        /// Report the stored qualification state without running a challenge.
        #[arg(long)]
        inspect: bool,
    },
    /// Run one headless task in the current workspace and print the result.
    Run {
        /// Task text; piped stdin is used when omitted.
        #[arg(short = 'p', long)]
        prompt: Option<String>,
        /// Account name or id to run on; the configured default otherwise.
        #[arg(long)]
        account: Option<String>,
        /// Model key (provider/model[/effort]); "auto" asks the judge to route.
        #[arg(long)]
        model: Option<String>,
        /// Attach an image file to the prompt; repeatable, up to 8.
        #[arg(long = "image")]
        images: Vec<PathBuf>,
    },
    /// Reopen a direct provider session in the terminal UI.
    Resume {
        /// Session to reopen; the latest session when omitted.
        id: Option<Id>,
    },
    /// List accounts; subcommands add, connect, and manage them.
    Accounts {
        #[command(subcommand)]
        command: Option<AccountCommand>,
    },
    /// List observed models; subcommands refresh catalogs and set the default.
    Models {
        #[command(subcommand)]
        command: Option<ModelCommand>,
    },
    /// Inspect conditional public offers; observations do not verify account entitlement.
    Offers {
        /// Re-check the published offers before listing them.
        #[arg(long)]
        refresh: bool,
    },
    /// List direct provider sessions (use conversations for managed chat).
    Sessions {
        #[command(subcommand)]
        command: Option<SessionCommand>,
    },
    /// Inspect durable managed tasks and their messages.
    Tasks {
        #[command(subcommand)]
        command: Option<TaskCommand>,
    },
    /// List persistent managed control conversations.
    Conversations,
    /// List installed panes; subcommands inspect, validate, and install them.
    Panes {
        #[command(subcommand)]
        command: Option<PaneCommand>,
    },
    /// Toggle product extensions (auto-continue, gobstopper, usage, hooks).
    Plugins {
        #[command(subcommand)]
        command: Option<PluginCommand>,
    },
    /// Manage user hook executables bound to lifecycle events.
    Hooks {
        #[command(subcommand)]
        command: Option<HookCommand>,
    },
    /// Configure the optional routing judge (key, policy, status).
    Judge {
        #[command(subcommand)]
        command: Option<JudgeCommand>,
    },
    /// Check provider binaries, accounts, and recent unsettled runs.
    Doctor {
        /// Check only this provider (claude, codex, or devin).
        #[arg(long)]
        provider: Option<Provider>,
        /// Provider binary to qualify instead of the discovered one;
        /// requires --provider.
        #[arg(long)]
        executable: Option<PathBuf>,
    },
    /// Print the effective configuration as JSON.
    Config,
    /// Check for a verified native release, configure update policy, or run
    /// the background update check used by a user-level scheduler.
    Update {
        #[command(subcommand)]
        command: Option<UpdateCommand>,
    },
    /// Install the latest verified native release (alias: `xcb update install`).
    Upgrade {
        /// Version tag to install; the latest verified release when omitted.
        version: Option<String>,
        /// Suppress progress output (used by the updater itself).
        #[arg(long, hide = true)]
        quiet: bool,
    },
    /// Inspect or clean up unsettled runs and disposable launch artifacts.
    Recover {
        /// Recover this run; lists unsettled runs when omitted.
        run: Option<Id>,
        /// Apply the recovery instead of only reporting what would change.
        #[arg(long)]
        yes: bool,
        /// Inventory disposable launch snapshots; --yes removes only snapshots
        /// with durable settlement evidence. Unproven artifacts remain held.
        #[arg(long = "launch-artifacts")]
        launch_artifacts: bool,
    },
    /// Inspect or archive retained offline command jobs.
    Command {
        #[command(subcommand)]
        command: CommandJobs,
    },
    /// Internal: run the managed supervisor (spawned by xcb, not for users).
    #[command(name = "managed-daemon", hide = true)]
    ManagedDaemon,
    /// Internal: stdio bridge used by a provider's MCP helper.
    #[command(name = "broker-stdio", hide = true)]
    BrokerStdio,
    /// Internal: in-namespace loopback CONNECT forwarder for sandboxed children.
    #[command(name = "egress-forward", hide = true)]
    EgressForward {
        /// Host bridge socket to dial.
        socket: PathBuf,
        /// Loopback port the forwarder listens on inside the namespace.
        port: u16,
        /// Loopback address of the in-namespace HTTP listener, or "-".
        lo_up: String,
        /// File holding the environment for the supervised child, or "-".
        env_file: String,
        /// Remote port CONNECT requests target.
        #[arg(long, default_value_t = 443)]
        target_port: u16,
        /// Command to supervise, after `--`.
        #[arg(last = true, required = true)]
        child: Vec<String>,
    },
    /// Print shell completions for xcb.
    Completions {
        /// Shell to generate completions for (bash, zsh, fish, …).
        shell: clap_complete::Shell,
    },
}

#[derive(Subcommand)]
enum UpdateCommand {
    /// Query immutable GitHub release metadata without changing the binary.
    Check,
    /// Show the local update policy and the last cached result.
    Status,
    /// Set the user-level policy. The default is notify.
    Enable {
        /// Update policy: notify, auto, or disable.
        #[arg(long, default_value = "notify", value_parser = parse_update_policy)]
        policy: xcb_runtime::update::Policy,
    },
    /// Disable update checks and scheduled upgrades.
    Disable,
    /// Install a verified release using the recorded global installer.
    Install {
        /// Version tag to install; the latest verified release when omitted.
        version: Option<String>,
        /// Suppress progress output (used by the updater itself).
        #[arg(long, hide = true)]
        quiet: bool,
    },
    /// Run one scheduled check; intended for LaunchAgent/systemd user timers.
    Daemon {
        /// Suppress progress output.
        #[arg(long, hide = true)]
        quiet: bool,
    },
}

#[derive(Subcommand)]
enum AccountCommand {
    /// Add an account. Its name is fixed: the provider account email once
    /// observed, otherwise `provider/<id>` — there are no custom labels.
    Add {
        /// Provider to add: claude, codex, or devin.
        provider: Provider,
        /// Plan label shown by `xcb accounts`; a display label only, never
        /// verified against the provider's entitlement.
        #[arg(long, default_value = "Subscription")]
        plan: String,
    },
    /// Sign in to an account through the provider's own login flow.
    Login {
        /// Account name or id (listed by `xcb accounts`).
        account: String,
    },
    /// Store a provider API token piped on stdin for this account.
    Token {
        /// Account name or id (listed by `xcb accounts`).
        account: String,
    },
    /// Make an account the default for new direct sessions.
    Default {
        /// Account name or id (listed by `xcb accounts`).
        account: String,
    },
    /// Stop routing work to an account without removing it.
    Disable {
        /// Account name or id (listed by `xcb accounts`).
        account: String,
    },
    /// Re-enable a disabled account.
    Enable {
        /// Account name or id (listed by `xcb accounts`).
        account: String,
    },
    /// Refresh an account's observed identity, plan, and usage window.
    Refresh {
        /// Account name or id (listed by `xcb accounts`).
        account: String,
    },
    /// Copy agentmixer-era accounts and sessions from a legacy state root.
    ImportAgentmixer {
        /// Absolute path to the legacy .agentmixer state directory.
        #[arg(long)]
        source: PathBuf,
    },
    /// Copy an existing Codex CLI sign-in (auth.json) into a new account.
    ImportCodex {
        /// Absolute path to the Codex auth.json to copy.
        #[arg(long)]
        source: PathBuf,
    },
    /// Copy one existing Devin sign-in into a private xcb account.
    ImportDevin {
        /// Absolute path to the Devin credentials.toml to copy.
        #[arg(long)]
        source: PathBuf,
    },
}
#[derive(Subcommand)]
enum ModelCommand {
    /// Discover the provider's current model catalog through an account.
    Refresh {
        /// Provider whose catalog is refreshed: claude, codex, or devin.
        provider: Provider,
        /// Account whose credentials run the discovery (required for devin).
        #[arg(long)]
        account: Option<String>,
        /// Legacy discovery flag; use an explicit credential import and --account.
        #[arg(long)]
        from_native: bool,
    },
    /// Make an observed model the default, e.g. `xcb models default claude/sonnet/high`.
    Default {
        /// Full observed model key (provider/model[/effort]) from `xcb models`.
        key: String,
    },
    /// Inspect relative model profiles, including models without an eligible account.
    Tiers {
        /// Task description used to rank the model profiles.
        #[arg(long, default_value = "general coding task")]
        task: String,
    },
    /// Preview managed routing for --cwd without reserving an account.
    /// Uses the configured judge when enabled; selection may change before execution.
    Route {
        /// Task description the route is previewed for.
        #[arg(long)]
        task: String,
        /// Restrict the preview to one provider (claude, codex, or devin).
        #[arg(long)]
        provider: Option<Provider>,
    },
}
#[derive(Subcommand)]
enum SessionCommand {
    /// Write local aiCharts session observations to an export file.
    Export,
    /// Remove one session and its transcript.
    Rm {
        /// Session id (listed by `xcb sessions`).
        id: Id,
        /// Apply the removal; without it the command only reports the plan.
        #[arg(long)]
        yes: bool,
    },
    /// Remove idle sessions older than a number of days.
    Prune {
        /// Age threshold in days (1–3650, default 30).
        #[arg(default_value_t = 30, value_parser = clap::value_parser!(u16).range(1..=3650))]
        days: u16,
        /// Apply the prune; without it the command only reports candidates.
        #[arg(long)]
        yes: bool,
    },
}
#[derive(Subcommand)]
enum CommandJobs {
    /// Move joined, acknowledged command jobs older than --days into
    /// jobs-archive/. Records are never deleted; unjoined, cleanup-pending
    /// and recent jobs are retained.
    Prune {
        /// Archive only jobs whose newest receipt is older than this many days.
        #[arg(long, default_value_t = 30)]
        days: u32,
        /// Apply the archive; without it only the dry-run report prints.
        #[arg(long)]
        yes: bool,
    },
}
#[derive(Subcommand)]
enum TaskCommand {
    /// Replay local ALGAL transition receipts and verify their chain and task record.
    Verify {
        /// Managed task id (listed by `xcb tasks`).
        id: Id,
    },
    /// Print one managed task's durable record as JSON.
    Show {
        /// Managed task id (listed by `xcb tasks`).
        id: Id,
    },
    /// Read up to 64 messages; pass the last sequence as --after for the next page.
    Messages {
        /// Managed task id (listed by `xcb tasks`).
        id: Id,
        /// Only messages after this sequence number (default 0).
        #[arg(long, default_value_t = 0, value_parser = clap::value_parser!(u64).range(..=i64::MAX as u64))]
        after: u64,
    },
}
#[derive(Subcommand)]
enum PaneCommand {
    /// Print one pane's definition as JSON.
    Show {
        /// Pane id (default "focus").
        #[arg(default_value = "focus")]
        id: Id,
    },
    /// Validate a pane file without installing it.
    Check {
        /// Path to the pane definition file.
        path: PathBuf,
    },
    /// Install a pane definition file.
    Install {
        /// Path to the pane definition file.
        path: PathBuf,
    },
}
#[derive(Subcommand)]
enum PluginCommand {
    /// Turn an extension on (auto-continue, gobstopper, usage, hooks).
    Enable {
        /// Extension name.
        name: String,
    },
    /// Turn an extension off.
    Disable {
        /// Extension name.
        name: String,
    },
}
#[derive(Subcommand)]
enum JudgeCommand {
    /// Store a judge API key piped on stdin; never an argument or terminal echo.
    Token,
    /// Remove the vaulted judge key.
    Logout,
    /// Report judge configuration without revealing the key.
    Status,
    /// Allow judged routing, safe continuation advice, and Gobstopper vetoes.
    Enable,
    /// Disable judge use; routing, continuation, and compaction stay deterministic.
    Disable,
    /// Send one live noul question to verify the key and endpoint.
    Test,
}
#[derive(Subcommand)]
enum HookCommand {
    /// Bind an executable to a lifecycle event.
    Add {
        /// Lifecycle event: session_start, session_end, turn_start, or turn_end.
        event: String,
        /// Executable the event runs.
        executable: PathBuf,
        /// Kill the hook after this many milliseconds (default 5000).
        #[arg(long, default_value_t = 5_000)]
        timeout_ms: u64,
    },
    /// Re-enable a disabled hook.
    Enable {
        /// Hook id.
        id: Id,
    },
    /// Disable a hook without removing it.
    Disable {
        /// Hook id.
        id: Id,
    },
}

fn stdin(max: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    io::stdin().take(max as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > max {
        return Err(xcb_core::Error::Limit("stdin").into());
    }
    Ok(bytes)
}
/// Sizes here are whole provider executables, so a plain byte count reads as
/// noise. One decimal place is enough to tell 208 MB from 1.8 GB.
fn human_bytes(bytes: u64) -> String {
    const STEP: f64 = 1024.0;
    let units = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= STEP && unit + 1 < units.len() {
        value /= STEP;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", units[unit])
    }
}

/// One table cell: control characters stripped, then cut to `width` display
/// columns. A cut cell ends with `…` so truncation is visible instead of a
/// silent drop.
fn cell(value: &str, width: usize) -> String {
    let clean = xcb_core::display_text(value, usize::MAX);
    if clean.chars().count() <= width {
        return clean;
    }
    let mut text: String = clean.chars().take(width.saturating_sub(1)).collect();
    text.push('…');
    text
}

/// Relative time like "3h ago" for table output; 0 ms renders as "never".
fn human_age(now_ms: u64, then_ms: u64) -> String {
    if then_ms == 0 {
        return "never".into();
    }
    let minutes = now_ms.saturating_sub(then_ms) / 60_000;
    if minutes < 1 {
        "just now".into()
    } else if minutes < 60 {
        format!("{minutes}m ago")
    } else if minutes < 60 * 24 {
        format!("{}h ago", minutes / 60)
    } else {
        format!("{}d ago", minutes / (60 * 24))
    }
}

fn print_json(value: impl serde::Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

fn run_output(session: &Id, result: &runner::Outcome) -> serde_json::Value {
    let mut output = json!({"version":1,"session":session,"state":result.state,"outcome":result.facts,"text":result.text});
    if let Some(diagnostic) = &result.diagnostic {
        output["diagnostic"] = json!(diagnostic);
    }
    output
}

fn run_exit_code(result: &runner::Outcome) -> i32 {
    if result.facts.terminal == Terminal::Completed
        && result.facts.joined
        && result.facts.effects != xcb_core::policy::EffectState::Uncertain
        && !result.facts.pending_attention
        && result.facts.failure.is_none()
        && result.state == xcb_core::session::State::Idle
    {
        0
    } else {
        1
    }
}

fn import_acknowledgement(id: &Id) -> serde_json::Value {
    json!({"version":1,"account":id,"sourcePreserved":true,"sessionsMigrated":false})
}

async fn broker_stdio() -> Result<i32> {
    let socket = std::env::var_os("XCB_BROKER_SOCKET")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or(Error::Unavailable("broker socket is unavailable"))?;
    let token = zeroize::Zeroizing::new(
        std::env::var("XCB_BROKER_TOKEN")
            .map_err(|_| Error::Unavailable("broker authority is unavailable"))?,
    );
    xcb_runtime::devin::broker_stdio(&socket, &token).await?;
    Ok(0)
}

/// The CLI account contract deliberately excludes storage/custody fields.
/// Never serialize the runtime record itself: adding an internal field must
/// not silently extend public command output.
#[derive(serde::Serialize)]
struct PublicAccount<'a> {
    id: &'a Id,
    provider: Provider,
    name: String,
    email: Option<&'a str>,
    subscription: &'a str,
    enabled: bool,
}
impl<'a> From<&'a xcb_runtime::store::Account> for PublicAccount<'a> {
    fn from(record: &'a xcb_runtime::store::Account) -> Self {
        Self {
            id: &record.id,
            provider: record.provider,
            name: record.name(),
            email: record.email.as_deref(),
            subscription: &record.subscription,
            enabled: record.enabled,
        }
    }
}

impl PublicAccount<'_> {
    fn added_message(&self) -> String {
        let added = format!(
            "Added {} ({}) · {}",
            xcb_core::display_text(&self.name, 80),
            self.provider,
            self.id
        );
        if matches!(self.provider, Provider::Claude | Provider::Codex) {
            format!("{added}\nNext: xcb accounts login {}", self.id)
        } else {
            format!(
                "{added}\nNext: pipe a Devin token into xcb accounts token {}.\nTo copy an existing CLI sign-in into a new account: xcb accounts import-devin --source /absolute/path/credentials.toml",
                self.id
            )
        }
    }
}

fn require_account_credentials(store: &Store, account: &xcb_runtime::store::Account) -> Result<()> {
    if auth::has_credentials(store, &account.id)? {
        return Ok(());
    }
    Err(Error::Unavailable(match account.provider {
        Provider::Claude => "connect this Claude account with xcb accounts login <account>",
        Provider::Codex => {
            "connect this Codex account with xcb accounts login <account> or explicitly import auth.json with xcb accounts import-codex --source <path>"
        }
        Provider::Devin => {
            "connect Devin by explicitly importing credentials.toml with xcb accounts import-devin --source <path>, or pipe a token into xcb accounts token <account>"
        }
    }))
}

fn catalog_account(
    store: &Store,
    provider: Provider,
    name: Option<&str>,
    from_native: bool,
) -> Result<Option<Id>> {
    // Process admission must never reinterpret this flag as an unauthenticated
    // ACP probe, nor grant ambient access to the native CLI's credential home.
    if from_native {
        return Err(Error::Unavailable(match provider {
            Provider::Devin => {
                "--from-native no longer reads ambient credentials; use xcb accounts import-devin --source <absolute credentials.toml path>, then models refresh devin --account <account>"
            }
            Provider::Codex => {
                "--from-native no longer reads ambient credentials; use xcb accounts import-codex --source <absolute auth.json path>, then models refresh codex --account <account>"
            }
            Provider::Claude => {
                "--from-native is unsupported for Claude; connect an account and use models refresh claude --account <account>"
            }
        }));
    }
    let Some(name) = name else {
        if provider == Provider::Devin {
            return Err(Error::Unavailable(
                "Devin catalog refresh requires --account after an explicit credential import or token connection",
            ));
        }
        return Ok(None);
    };
    let account = store.resolve_account(name)?;
    if account.provider != provider {
        return Err(Error::Conflict("catalog account provider mismatch"));
    }
    require_account_credentials(store, &account)?;
    Ok(Some(account.id))
}

fn accounts(store: &Store, config: &Config, as_json: bool) -> Result<()> {
    let view = summary::snapshot(store, None, config, now_ms())?;
    if as_json {
        return print_json(
            json!({"version":1,"accounts":view.accounts.iter().map(|account| json!({"id":account.id,"name":account.name,"email":account.email,"provider":account.provider,"subscription":account.subscription,"remainingPercent":account.remaining_percent,"resetsAtMs":account.resets_at_ms,"quotaBlockedUntilMs":account.quota_blocked_until_ms,"runway":account.runway,"busy":account.busy,"enabled":account.enabled,"authenticationRequired":account.authentication_required})).collect::<Vec<_>>(),"estimatedPoolSeconds":view.total_runway_seconds,"measuredPools":view.runway_coverage.0,"totalPools":view.runway_coverage.1,"localOnly":true}),
        );
    }
    if view.accounts.is_empty() {
        println!(
            "No accounts yet.\n\nxcb accounts add claude --plan Max\nxcb doctor --provider claude\nxcb accounts login <account>\nxcb accounts refresh <account>"
        );
        return Ok(());
    }
    println!(
        "  ID          ACCOUNT                             PROVIDER  PLAN                REMAINING              EST. RUNWAY"
    );
    let now = now_ms();
    for account in view.accounts {
        let reset = account
            .resets_at_ms
            .filter(|at| *at > now)
            .map(|at| {
                let minutes = (at - now).div_ceil(60_000);
                if minutes >= 60 * 24 {
                    format!(" · resets in ~{}d", minutes / (60 * 24))
                } else if minutes >= 60 {
                    format!(" · resets in ~{}h{}m", minutes / 60, minutes % 60)
                } else {
                    format!(" · resets in ~{minutes}m")
                }
            })
            .unwrap_or_default();
        let remaining = account
            .remaining_percent
            .map(|percent| format!("{percent:.0}% left{reset}"))
            .unwrap_or_else(|| "unmeasured".into());
        let runway = account
            .runway
            .seconds()
            .map(|seconds| format!("~{:.1}h", seconds / 3600.0))
            .unwrap_or_else(|| "unmeasured".into());
        // codeql[rust/cleartext-logging]: the account name is the user's own
        // provider email rendered as the account's display identity, which is
        // the documented purpose of this local status table.
        println!(
            "{} {:<11} {:<35} {:<9} {:<19} {:<22} {}{}{}{}{}",
            if config.default_account.as_ref() == Some(&account.id) {
                ">"
            } else {
                " "
            },
            cell(account.id.as_str(), 11),
            cell(&account.name, 35),
            account.provider,
            cell(&account.subscription, 19),
            remaining,
            runway,
            if account.busy { " · busy" } else { "" },
            if account.enabled { "" } else { " · disabled" },
            if account.authentication_required {
                " · reconnect required"
            } else {
                ""
            },
            account
                .quota_block_label(now)
                .map(|label| format!(" · {label}"))
                .unwrap_or_default()
        );
    }
    println!(
        "\n> marks the default account · ids are shortened; xcb accounts --json prints them in full"
    );
    if let Some(seconds) = view.total_runway_seconds {
        println!(
            "Measured pool runway: ~{:.1}h ({}/{} pools; estimate, not a billing statement)",
            seconds / 3600.0,
            view.runway_coverage.0,
            view.runway_coverage.1
        );
    }
    Ok(())
}

/// `xcb egress-forward <socket> <port> <lo_up> <env_file> -- <child...>`:
/// "-" placeholders become absent paths; the forwarder supervises the child
/// with loopback CONNECT proxying through the host bridge socket.
async fn egress_forward(
    socket: &std::path::Path,
    port: u16,
    lo_up: &str,
    env_file: &str,
    target_port: u16,
    child: &[String],
) -> Result<i32> {
    let lo_up = (lo_up != "-").then(|| PathBuf::from(lo_up));
    let env_file = (env_file != "-").then(|| PathBuf::from(env_file));
    xcb_runtime::egress::run_forwarder(
        socket,
        port,
        target_port,
        lo_up.as_deref(),
        env_file.as_deref(),
        child,
    )
    .await
}

fn parse_update_policy(value: &str) -> std::result::Result<xcb_runtime::update::Policy, String> {
    match value {
        "notify" => Ok(xcb_runtime::update::Policy::Notify),
        "auto" => Ok(xcb_runtime::update::Policy::Auto),
        "disable" => Ok(xcb_runtime::update::Policy::Disable),
        _ => Err("update policy must be notify, auto, or disable".to_owned()),
    }
}

fn parse_expected_generation(value: &str) -> std::result::Result<String, &'static str> {
    xcb_runtime::application::validate_expected_generation(value)
        .map(|()| value.to_owned())
        .map_err(|_| "expected generation must be 64 lowercase hexadecimal characters")
}

async fn dispatch(cli: Cli) -> Result<i32> {
    process::initialize_host()?;
    // The provider's MCP helper must not open application state or emit any
    // ordinary CLI output on its protocol-only standard streams.
    if matches!(&cli.command, Some(Commands::BrokerStdio)) {
        return broker_stdio().await;
    }
    // The hidden in-namespace forwarder must not touch CLI state: inside the
    // bwrap plan the environment is --clearenv (no HOME/XCB_STATE) and the
    // host state root is unbound, so Store/Config init would fail before the
    // forwarder ever read its env file. It runs on its arguments alone.
    if let Some(Commands::EgressForward {
        socket,
        port,
        lo_up,
        env_file,
        target_port,
        child,
    }) = &cli.command
    {
        return egress_forward(socket, *port, lo_up, env_file, *target_port, child).await;
    }
    let root = cli.state.unwrap_or(private::default_root()?);
    if matches!(&cli.command, Some(Commands::ManagedDaemon)) {
        return xcb_runtime::managed::daemon(root).await;
    }
    if let Some(Commands::Generate { capabilities }) = &cli.command {
        return application::dispatch(&root, *capabilities, cli.json).await;
    }
    if let Some(Commands::ApplicationDiagnostic { account, request }) = &cli.command {
        return application::diagnostic_dispatch(&root, account, request, cli.json);
    }
    if let Some(Commands::QualifyApplication {
        account,
        model,
        evidence,
        expected_generation,
        inspect,
    }) = &cli.command
    {
        if *inspect {
            return application::inspect_dispatch(&root, account, model, cli.json);
        }
        let Some(evidence) = evidence else {
            unreachable!("clap requires evidence or inspection");
        };
        return application::qualify_dispatch(
            &root,
            account.clone(),
            model.clone(),
            evidence,
            expected_generation.as_deref(),
            cli.json,
        )
        .await;
    }
    let store = Arc::new(Store::open(&root)?);
    let (mut config, _) = Config::load(store.root())?;
    match cli.command {
        Some(
            Commands::Generate { .. }
            | Commands::ApplicationDiagnostic { .. }
            | Commands::QualifyApplication { .. }
            | Commands::ManagedDaemon,
        ) => {
            unreachable!("early dispatch returns above")
        }
        None => managed_chat(store, cli.cwd.canonicalize()?, None, cli.json).await,
        Some(Commands::Chat { resume }) => {
            managed_chat(store, cli.cwd.canonicalize()?, resume, cli.json).await
        }
        Some(Commands::Resume { id }) => {
            let id = id
                .or_else(|| {
                    store
                        .sessions(1)
                        .ok()?
                        .first()
                        .map(|session| session.id.clone())
                })
                .ok_or(Error::Unavailable("no saved sessions"))?;
            let session = store
                .session(&id)?
                .ok_or(Error::Unavailable("session not found"))?;
            direct_chat(store, PathBuf::from(session.workspace), Some(id), cli.json).await
        }
        Some(Commands::Run {
            prompt,
            account,
            model,
            images,
        }) => {
            let prompt = match prompt {
                Some(prompt) => prompt,
                None if !io::stdin().is_terminal() => {
                    String::from_utf8(stdin(xcb_core::MAX_TEXT_BYTES)?)
                        .map_err(|_| xcb_core::Error::Invalid("UTF-8 prompt"))?
                }
                None => {
                    return Err(Error::Unavailable(
                        "use xcb run -p <task> or pipe a task on stdin",
                    ));
                }
            };
            xcb_core::bounded_text(&prompt, xcb_core::MAX_TEXT_BYTES)?;
            if images.len() > 8 {
                return Err(xcb_core::Error::Limit("images").into());
            }
            let attachments = images
                .iter()
                .map(|path| xcb_runtime::attachments::from_path(store.root(), path))
                .collect::<Result<Vec<_>>>()?;
            let mut account = account
                .map(|name| store.resolve_account(&name).map(|account| account.id))
                .transpose()?;
            let mut model = model;
            if model.as_deref() == Some("auto") {
                let (routed_account, choice) =
                    kernel::auto_route(&store, &config, &prompt, account.as_ref()).await?;
                eprintln!("xcb: judge selected an admitted route");
                account = Some(routed_account);
                model = Some(choice.key());
            }
            let session = kernel::new_session(
                &store,
                &cli.cwd.canonicalize()?,
                &config,
                account.as_ref(),
                model.as_deref(),
                None,
            )?;
            let (cancel, cancelled) = watch::channel(false);
            // Install both handlers before starting any provider. SIGTERM must
            // use the same independent join/custody path as interactive Ctrl-C.
            let mut interrupts =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
            let mut terminates =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
            let interrupt = tokio::spawn(async move {
                tokio::select! {
                    _ = interrupts.recv() => {},
                    _ = terminates.recv() => {},
                }
                let _ = cancel.send(true);
            });
            let observer: Observer = Arc::new(|event| {
                if let Progress::Notice(message) = event {
                    eprintln!("xcb: {message}");
                }
            });
            let result = kernel::execute(
                store,
                session.id.clone(),
                prompt,
                attachments,
                false,
                cancelled,
                observer,
            )
            .await;
            interrupt.abort();
            let result = result?;
            if cli.json {
                print_json(run_output(&session.id, &result))?;
            } else {
                println!("{}", result.text);
            }
            Ok(run_exit_code(&result))
        }
        Some(Commands::Accounts { command }) => {
            match command {
                None => accounts(&store, &config, cli.json)?,
                Some(AccountCommand::Add { provider, plan }) => {
                    let account = store.add_account(provider, &plan, now_ms(), None)?;
                    let (mut config, revision) = Config::load(store.root())?;
                    if config.default_account.is_none() {
                        config.default_account = Some(account.id.clone());
                        config.save(store.root(), revision.as_deref())?;
                    }
                    let public = PublicAccount::from(&account);
                    if cli.json {
                        print_json(public)?;
                    } else {
                        println!("{}", public.added_message());
                    }
                }
                Some(AccountCommand::Login { account }) => {
                    let account = store.resolve_account(&account)?;
                    if account.provider == Provider::Devin {
                        return Err(Error::Unavailable(
                            "sign in with devin auth login, then use xcb accounts import-devin --source <absolute credentials.toml path>; to connect this account directly, pipe a token into xcb accounts token <account>",
                        ));
                    }
                    let pin = Pin::load(store.root(), account.provider)?;
                    match account.provider {
                        Provider::Claude => {
                            eprintln!(
                                "Complete the provider's browser sign-in. Credential output is captured, not printed."
                            );
                            let (cancel, receiver) = tokio::sync::watch::channel(false);
                            let mut interrupt = tokio::signal::unix::signal(
                                tokio::signal::unix::SignalKind::interrupt(),
                            )?;
                            let mut terminate = tokio::signal::unix::signal(
                                tokio::signal::unix::SignalKind::terminate(),
                            )?;
                            let login =
                                auth::login_with_cancel(&store, &account.id, &pin, receiver);
                            tokio::pin!(login);
                            tokio::select! {
                                result = &mut login => result?,
                                _ = interrupt.recv() => { let _ = cancel.send(true); login.await?; },
                                _ = terminate.recv() => { let _ = cancel.send(true); login.await?; },
                            }
                        }
                        Provider::Codex => runner::login_codex(&store, &account.id, &pin).await?,
                        Provider::Devin => unreachable!("Devin sign-in is gated above"),
                    }
                    if cli.json {
                        print_json(json!({"version":1,"account":account.id,"stored":true}))?;
                    } else {
                        // codeql[rust/cleartext-logging]: the account name is
                        // the user's own provider email, intentionally shown as
                        // the account's display identity after sign-in.
                        println!(
                            "Sign-in completed for {}. Run xcb accounts refresh {} to refresh available account metadata.",
                            account.name(),
                            account.id
                        );
                    }
                }
                Some(AccountCommand::Token { account }) => {
                    if io::stdin().is_terminal() {
                        return Err(Error::Unavailable(
                            "token input is accepted only through a pipe, never an argument or terminal echo",
                        ));
                    }
                    let account = store.resolve_account(&account)?;
                    let limit = match account.provider {
                        Provider::Claude => 2048,
                        Provider::Devin => 8194,
                        Provider::Codex => {
                            return Err(Error::Unavailable(
                                "Codex uses ChatGPT sign-in; use xcb accounts login <account> or xcb accounts import-codex --source <absolute auth.json path>",
                            ));
                        }
                    };
                    let bytes = zeroize::Zeroizing::new(stdin(limit)?);
                    match account.provider {
                        Provider::Claude => auth::store_token(&store, &account.id, &bytes)?,
                        Provider::Devin => {
                            xcb_runtime::devin::auth::store_token(&store, &account.id, &bytes)?
                        }
                        Provider::Codex => unreachable!("Codex token input is rejected above"),
                    }
                    if cli.json {
                        print_json(json!({"version":1,"account":account.id,"stored":true}))?;
                    } else {
                        println!(
                            "Credential stored for {}",
                            xcb_core::display_text(&account.name(), 80)
                        );
                    }
                }
                Some(AccountCommand::Default { account }) => {
                    let account = store.resolve_account(&account)?;
                    let (mut config, revision) = Config::load(store.root())?;
                    config.default_account = Some(account.id);
                    config.save(store.root(), revision.as_deref())?;
                }
                Some(AccountCommand::Disable { account }) => {
                    store.set_account_enabled(&store.resolve_account(&account)?.id, false)?
                }
                Some(AccountCommand::Enable { account }) => {
                    store.set_account_enabled(&store.resolve_account(&account)?.id, true)?
                }
                Some(AccountCommand::Refresh { account }) => {
                    let account = store.resolve_account(&account)?;
                    require_account_credentials(&store, &account)?;
                    let pin = Pin::load(store.root(), account.provider)?;
                    if !runner::provider_admitted(&pin) {
                        return Err(Error::Unavailable(
                            "native account metadata querying for this runtime is not yet qualified",
                        ));
                    }
                    let models = runner::probe(&store, &pin, Some(&account.id)).await?;
                    store.set_models(account.provider, &models)?;
                    accounts(&store, &config, cli.json)?;
                }
                Some(AccountCommand::ImportAgentmixer { source }) => {
                    // This is a generated public routing ID, never a credential
                    // or an internal account record.
                    let id: Id = auth::import_agentmixer_token(&store, &source)?;
                    if cli.json {
                        print_json(import_acknowledgement(&id))?;
                    } else {
                        println!(
                            "Imported one Claude account as {id}. Original state and sessions are unchanged."
                        );
                    }
                }
                Some(AccountCommand::ImportCodex { source }) => {
                    let id = auth::import_codex_account(&store, &source)?;
                    if cli.json {
                        print_json(import_acknowledgement(&id))?;
                    } else {
                        println!(
                            "Imported one Codex account as {id}. Original state and sessions are unchanged."
                        );
                    }
                }
                Some(AccountCommand::ImportDevin { source }) => {
                    let id = xcb_runtime::devin::auth::import_account(&store, &source)?;
                    if cli.json {
                        print_json(import_acknowledgement(&id))?;
                    } else {
                        println!(
                            "Imported one Devin account as {id}. Next: xcb accounts refresh {id}"
                        );
                    }
                }
            }
            Ok(0)
        }
        Some(Commands::Doctor {
            provider,
            executable,
        }) => {
            if executable.is_some() && provider.is_none() {
                return Err(Error::Unavailable("--executable requires --provider"));
            }
            let home = private::directory(&root.join("metadata-home"))?;
            private::directory(&home.join("tmp"))?;
            let mut found = 0;
            let mut reports = vec![];
            for provider in
                provider.map_or_else(|| Provider::ALL.to_vec(), |provider| vec![provider])
            {
                match process::inspect(provider, executable.as_deref(), &home).await {
                    Ok(pin) => {
                        pin.save(&root)?;
                        let native = runner::provider_admitted(&pin);
                        let detail = if native && provider == Provider::Devin {
                            "pinned · accounts refresh <account> loads the catalog after credential import"
                        } else if native {
                            "pinned · per-run boundary verification required"
                        } else {
                            "metadata pin only · native execution unavailable"
                        };
                        reports.push(json!({"provider":provider,"version":pin.version,"sha256":pin.sha256,"nativeCandidate":native,"detail":detail}));
                        if !cli.json {
                            println!("{provider}: {} · {detail}", pin.version);
                        }
                        found += 1;
                        // Devin catalog discovery requires explicit account-owned
                        // credentials; doctor only pins its executable.
                        if native && provider != Provider::Devin {
                            match runner::probe(&store, &pin, None).await {
                                Ok(models) => store.set_models(provider, &models)?,
                                Err(error) => eprintln!("xcb: metadata probe: {error}"),
                            }
                        }
                    }
                    Err(error) => match process::Pin::load(&root, provider) {
                        Ok(pin) => {
                            let native = runner::provider_admitted(&pin);
                            let detail = if native {
                                "pinned · per-run boundary verification required"
                            } else {
                                "metadata pin only · native execution unavailable"
                            };
                            reports.push(json!({"provider":provider,"version":pin.version,"sha256":pin.sha256,"nativeCandidate":native,"storedPin":true,"detail":detail}));
                            if !cli.json {
                                println!("{provider}: {} · {detail}", pin.version);
                            }
                            found += 1;
                        }
                        Err(_) => {
                            reports.push(json!({"provider":provider,"error":error.to_string()}));
                            if !cli.json {
                                println!("{provider}: {error}");
                            }
                        }
                    },
                }
            }
            let judge_key = judge::judge_token(store.root())?.map(|(_, source)| source);
            if let Some(source) = judge_key {
                judge::check_key_target(source, &config.extensions.judge)?;
            }
            let judge_key_name = match judge_key {
                Some(judge::JudgeKeySource::Env) => "env",
                Some(judge::JudgeKeySource::Vault) => "vault",
                None => "none",
            };
            // Reclaim only snapshots already marked disposable after safe
            // settlement; parent exit alone cannot release provider custody.
            let sweep = runner::reclaim_launch_artifacts(&root, true)?;
            let (judge_model, judge_endpoint) =
                xcb_runtime::jev::effective_target(&config.extensions.judge)?;
            let judge_status = json!({
                "enabled": config.extensions.judge.enabled,
                "key": judge_key_name,
                "model": judge_model,
                "endpoint": judge_endpoint,
            });
            if cli.json {
                let mut report = json!({"version":1,"providers":reports,"unsettledRuns":store.unsettled_runs()?});
                report["judge"] = judge_status;
                report["launchArtifacts"] = json!({
                    "reclaimed": sweep.reclaimed,
                    "reclaimedBytes": sweep.reclaimed_bytes,
                    "liveHeld": sweep.live,
                    "unreclaimable": sweep.unprovable.len(),
                    "unreclaimableBytes": sweep.unprovable_bytes,
                    "remedy": if sweep.unprovable.is_empty() {
                        serde_json::Value::Null
                    } else {
                        json!("retained because independent provider-join and settled-effect evidence is missing; --yes does not override custody")
                    },
                });
                if cfg!(target_os = "linux") {
                    let status = xcb_runtime::sandbox::linux_sandbox(&root);
                    report["sandbox"] = json!({"backend":"bwrap","candidate":status.candidate,"admitted":status.admitted,"unprivilegedUsernsClone":status.unprivileged_userns_clone,"maxUserNamespaces":status.max_user_namespaces,"qualified":status.qualified});
                }
                print_json(report)?;
            } else {
                if cfg!(target_os = "linux") {
                    let status = xcb_runtime::sandbox::linux_sandbox(&root);
                    let detail = match &status.candidate {
                        Some(path) if status.admitted => {
                            format!("bwrap candidate {} admitted", path.display())
                        }
                        Some(path) => {
                            format!("bwrap candidate {} fails admission", path.display())
                        }
                        None => "bwrap unavailable".to_owned(),
                    };
                    let userns =
                        match (status.unprivileged_userns_clone, status.max_user_namespaces) {
                            (Some(false), _) | (_, Some(0)) => " · user namespaces restricted",
                            _ => "",
                        };
                    let qual = if status.qualified {
                        "qualified"
                    } else {
                        "unqualified · place a current qualification receipt"
                    };
                    println!("sandbox: {detail}{userns} · {qual}");
                }
                println!(
                    "judge: {} · key {judge_key_name} · {judge_endpoint}",
                    if config.extensions.judge.enabled {
                        "enabled"
                    } else {
                        "disabled"
                    },
                );
                if sweep.reclaimed > 0 {
                    println!(
                        "launch artifacts: reclaimed {} settled {} ({})",
                        sweep.reclaimed,
                        if sweep.reclaimed == 1 {
                            "directory"
                        } else {
                            "directories"
                        },
                        human_bytes(sweep.reclaimed_bytes),
                    );
                }
                if !sweep.unprovable.is_empty() {
                    println!(
                        "launch artifacts: {} {} ({}) retained without independent settlement evidence.",
                        sweep.unprovable.len(),
                        if sweep.unprovable.len() == 1 {
                            "directory"
                        } else {
                            "directories"
                        },
                        human_bytes(sweep.unprovable_bytes),
                    );
                    println!(
                        "  Parent exit or --yes cannot release custody; inspect the recorded run before recovery."
                    );
                }
                for run in store.unsettled_runs()? {
                    println!("Unsettled run {} · custody retained", run.id);
                }
            }
            Ok(if found > 0 { 0 } else { 1 })
        }
        Some(Commands::Models { command }) => {
            match command {
                Some(ModelCommand::Refresh {
                    provider,
                    account,
                    from_native,
                }) => {
                    let account =
                        catalog_account(&store, provider, account.as_deref(), from_native)?;
                    let pin = Pin::load(store.root(), provider)?;
                    if !runner::provider_admitted(&pin) {
                        return Err(Error::Unavailable(
                            "native catalog discovery for this runtime is not yet qualified",
                        ));
                    }
                    let models = runner::probe(&store, &pin, account.as_ref()).await?;
                    store.set_models(provider, &models)?;
                }
                Some(ModelCommand::Route { task, provider }) => {
                    let workspace = cli.cwd.canonicalize()?;
                    let managed = xcb_runtime::managed::ManagedStore::open(store.root())?;
                    let (preference, required) =
                        managed.initial_route_preferences(&workspace, &task)?;
                    let (preferred_provider, required_provider) =
                        preview_provider_preferences(preference, required, provider)?;
                    let excluded_routes = std::collections::BTreeSet::new();
                    let excluded_accounts = std::collections::BTreeSet::new();
                    let decision = xcb_runtime::routing::smart_route(
                        &store,
                        &Config::load(store.root())?.0,
                        xcb_runtime::routing::RouteRequest {
                            task: &task,
                            required_provider,
                            preferred_provider,
                            excluded_routes: &excluded_routes,
                            excluded_accounts: &excluded_accounts,
                            account: None,
                        },
                    )
                    .await?;
                    if cli.json {
                        print_json(decision)?;
                    } else {
                        println!(
                            "{} · {}  {}",
                            decision.model.key(),
                            decision.account,
                            xcb_core::display_text(&decision.reason, 4096)
                        );
                    }
                    return Ok(0);
                }
                Some(ModelCommand::Tiers { task }) => {
                    let offers = xcb_runtime::offers::load(store.root())?;
                    let rows = xcb_runtime::routing::profile_models(
                        &store.models()?,
                        &offers,
                        xcb_runtime::now_ms(),
                        &task,
                    );
                    if cli.json {
                        print_json(rows)?;
                    } else {
                        for row in rows {
                            let offer = row
                                .profile
                                .free_offer
                                .as_ref()
                                .map(|_| " · conditional offer (entitlement unverified)")
                                .unwrap_or("");
                            println!(
                                "P{}  q{:>3} c{:>3} l{:>3}  {:<56} {}{}",
                                row.profile.pareto_layer,
                                row.profile.quality,
                                row.profile.relative_cost,
                                row.profile.relative_latency,
                                row.key,
                                row.label,
                                offer,
                            );
                        }
                    }
                    return Ok(0);
                }
                Some(ModelCommand::Default { key }) => {
                    let choice = store
                        .models()?
                        .into_iter()
                        .find(|model| model.key() == key)
                        .ok_or(Error::Unavailable("use the full observed model key"))?;
                    let (mut config, revision) = Config::load(store.root())?;
                    config.favorites.retain(|favorite| {
                        favorite.provider != choice.provider
                            || favorite.model != choice.id
                            || favorite.effort != choice.effort
                    });
                    config.favorites.insert(
                        0,
                        Preference {
                            provider: choice.provider,
                            model: choice.id,
                            effort: choice.effort,
                        },
                    );
                    config.save(store.root(), revision.as_deref())?;
                }
                None => (),
            }
            let mut choices = store.models()?;
            sort_choices(&mut choices, &Config::load(store.root())?.0.favorites);
            if cli.json {
                print_json(choices)?;
            } else {
                println!("  MODEL                                                 LABEL · MODE");
                // choose_model defaults each provider to its first row in this
                // ordering, so mark those rows.
                let mut defaulted = std::collections::BTreeSet::new();
                for choice in choices {
                    println!(
                        "{} {:<56} {} · {:?}",
                        if defaulted.insert(choice.provider) {
                            "*"
                        } else {
                            " "
                        },
                        choice.key(),
                        cell(&choice.label, 56),
                        choice.mode
                    );
                }
                println!("* default route per provider (xcb models default <key> repins it)");
            }
            Ok(0)
        }
        Some(Commands::Offers { refresh }) => {
            let state = if refresh {
                xcb_runtime::offers::refresh(store.root())?
            } else {
                xcb_runtime::offers::load(store.root())?
            };
            if cli.json {
                print_json(state)?;
            } else {
                println!(
                    "offers checked {} · {} · {} observation{}",
                    state.checked_at_ms,
                    if state.fresh(xcb_runtime::now_ms()) {
                        "fresh"
                    } else {
                        "stale"
                    },
                    state.offers.len(),
                    if state.offers.len() == 1 { "" } else { "s" }
                );
                for offer in state.offers {
                    println!(
                        "{} {}* · {:?} · through {} · {}",
                        offer.provider,
                        offer.model_prefix,
                        offer.kind,
                        offer.valid_until_ms,
                        offer.terms,
                    );
                }
            }
            Ok(0)
        }
        Some(Commands::Sessions { command }) => {
            match command {
                None => {
                    let sessions = store.sessions(64)?;
                    if cli.json {
                        print_json(sessions)?;
                    } else {
                        println!(
                            "{:<34}  {:<8} {:<24} {:<15} {:<9} TITLE",
                            "SESSION ID", "PROVIDER", "MODEL", "STATE", "ACTIVE"
                        );
                        let now = now_ms();
                        for session in sessions {
                            println!(
                                "{:<34}  {:<8} {:<24} {:<15} {:<9} {}",
                                session.id.as_str(),
                                session.model.provider,
                                cell(&session.model.label, 24),
                                session.state.label(),
                                human_age(now, session.last_active_at_ms),
                                cell(&session.title, 60)
                            );
                        }
                    }
                }
                Some(SessionCommand::Export) => {
                    let path = exports::write(&store)?;
                    if cli.json {
                        print_json(
                            json!({"version":1,"profile":"session-observations-v1","path":path}),
                        )?;
                    } else {
                        println!(
                            "Exported local aiCharts session observations to {}",
                            path.display()
                        );
                    }
                }
                Some(SessionCommand::Rm { id, yes }) => {
                    if !yes {
                        println!(
                            "Would remove {id} and its transcript. Repeat with --yes to apply."
                        );
                    } else {
                        let removed = store.remove_session(&id)?;
                        if cli.json {
                            print_json(json!({"removed":removed}))?;
                        } else {
                            println!(
                                "{}",
                                if removed {
                                    "Session removed"
                                } else {
                                    "Session not found"
                                }
                            );
                        }
                    }
                }
                Some(SessionCommand::Prune { days, yes }) => {
                    let candidates = store.prune_candidates(
                        now_ms().saturating_sub(u64::from(days) * 86_400_000),
                        1000,
                    )?;
                    let count = candidates.len();
                    if yes {
                        for id in &candidates {
                            store.remove_session(id)?;
                        }
                    }
                    if cli.json {
                        print_json(
                            json!({"version":1,"applied":yes,"count":count,"sessions":candidates}),
                        )?;
                    } else {
                        println!(
                            "{} {count} idle session(s) older than {days} days. Active or unsettled sessions are excluded.{}",
                            if yes { "Pruned" } else { "Would prune" },
                            if yes {
                                ""
                            } else {
                                " Repeat with --yes to apply."
                            }
                        );
                    }
                }
            }
            Ok(0)
        }
        Some(Commands::Conversations) => {
            let managed = xcb_runtime::managed::ManagedStore::open(store.root())?;
            let conversations = managed.conversations(256)?;
            if cli.json {
                print_json(conversations)?;
            } else if conversations.is_empty() {
                println!("No managed conversations.");
            } else {
                for conversation in conversations {
                    println!(
                        "{}  {} · {}",
                        conversation.id, conversation.title, conversation.workspace
                    );
                }
            }
            Ok(0)
        }
        Some(Commands::Tasks { command }) => {
            let managed = xcb_runtime::managed::ManagedStore::open(store.root())?;
            match command {
                None => {
                    let tasks = xcb_runtime::managed::list(&managed)?;
                    if cli.json {
                        print_json(tasks)?;
                    } else if tasks.is_empty() {
                        println!("No managed tasks.");
                    } else {
                        for task in tasks {
                            println!(
                                "{}  {} · {} · {}{}",
                                task.id,
                                task.state.label(),
                                task.title,
                                task.detail,
                                task.route
                                    .as_deref()
                                    .map(|route| format!(" · {route}"))
                                    .unwrap_or_default()
                            );
                        }
                    }
                }
                Some(TaskCommand::Verify { id }) => {
                    print_json(managed.verify_task(&id).await?)?;
                }
                Some(TaskCommand::Show { id }) => {
                    let task = xcb_runtime::managed::inspect(&managed, &id)?
                        .ok_or(Error::Unavailable("managed task not found"))?;
                    print_json(task)?;
                }
                Some(TaskCommand::Messages { id, after }) => {
                    managed
                        .task(&id)?
                        .ok_or(Error::Unavailable("managed task not found"))?;
                    let messages = managed.mailbox(&id, after, 64)?;
                    if cli.json {
                        print_json(messages)?;
                    } else if messages.is_empty() {
                        println!("No XCB messages for {id} after sequence {after}.");
                    } else {
                        for message in messages {
                            println!(
                                "#{} {} {} → {} · {}",
                                message.sequence,
                                message.source_provider,
                                message.source_task,
                                message.target_task,
                                xcb_core::display_text(&message.body, xcb_core::MAX_TEXT_BYTES),
                            );
                        }
                    }
                }
            }
            Ok(0)
        }
        Some(Commands::Panes { command }) => {
            match command {
                None => {
                    let entries = panes::list(store.root())?;
                    if cli.json {
                        print_json(entries)?;
                    } else {
                        for pane in entries {
                            println!("{}  {}", pane.id, pane.title);
                        }
                    }
                }
                Some(PaneCommand::Show { id }) => print_json(panes::load(store.root(), &id)?.0)?,
                Some(PaneCommand::Check { path }) => {
                    let pane = Pane::parse(&std::fs::read(path)?)?;
                    print_json(json!({"valid":true,"id":pane.id}))?;
                }
                Some(PaneCommand::Install { path }) => {
                    let pane = Pane::parse(&std::fs::read(path)?)?;
                    panes::save(store.root(), &pane, None)?;
                    print_json(json!({"installed":pane.id,"executable":false}))?;
                }
            }
            Ok(0)
        }
        Some(Commands::Plugins { command }) => {
            if let Some(command) = command {
                let (name, enabled) = match command {
                    PluginCommand::Enable { name } => (name, true),
                    PluginCommand::Disable { name } => (name, false),
                };
                let (mut fresh, revision) = Config::load(store.root())?;
                match name.as_str() {
                    "auto-continue" => fresh.extensions.auto_continue.enabled = enabled,
                    "gobstopper" => fresh.extensions.gobstopper.enabled = enabled,
                    "usage" => fresh.extensions.usage = enabled,
                    "hooks" => fresh.extensions.hooks = enabled,
                    "aicharts-export" => fresh.extensions.aicharts_export = enabled,
                    "aicharts" | "aicharts-upload" => {
                        return Err(Error::Unavailable(
                            "automatic posting awaits a supported enrolled aiCharts ingress; local exports remain available",
                        ));
                    }
                    _ => return Err(Error::Unavailable("unknown or not-yet-available extension")),
                }
                fresh.save(store.root(), revision.as_deref())?;
                config = fresh;
            }
            print_json(&config.extensions)?;
            Ok(0)
        }
        Some(Commands::Hooks { command }) => {
            match command {
                None => print_json(hooks::list(store.root())?)?,
                Some(HookCommand::Add {
                    event,
                    executable,
                    timeout_ms,
                }) => {
                    let hook = hooks::add(store.root(), event.parse()?, &executable, timeout_ms)?;
                    print_json(
                        json!({"hook":hook,"enabled":false,"next":format!("xcb hooks enable {}", hook.id)}),
                    )?;
                }
                Some(HookCommand::Enable { id }) => {
                    print_json(hooks::set_enabled(store.root(), &id, true)?)?
                }
                Some(HookCommand::Disable { id }) => {
                    print_json(hooks::set_enabled(store.root(), &id, false)?)?
                }
            }
            Ok(0)
        }
        Some(Commands::Judge { command }) => {
            match command {
                Some(JudgeCommand::Token) => {
                    if io::stdin().is_terminal() {
                        return Err(Error::Unavailable(
                            "key input is accepted only through a pipe, never an argument or terminal echo",
                        ));
                    }
                    judge::store_judge_token(store.root(), &stdin(2048)?)?;
                    println!("Judge key stored.");
                }
                Some(JudgeCommand::Logout) => {
                    if judge::remove_judge_token(store.root())? {
                        println!("Judge key removed.");
                    } else {
                        println!("No vaulted judge key.");
                    }
                }
                Some(JudgeCommand::Enable) => {
                    let (mut fresh, revision) = Config::load(store.root())?;
                    fresh.extensions.judge.enabled = true;
                    fresh.save(store.root(), revision.as_deref())?;
                    println!("Judge enabled.");
                }
                Some(JudgeCommand::Disable) => {
                    let (mut fresh, revision) = Config::load(store.root())?;
                    fresh.extensions.judge.enabled = false;
                    fresh.save(store.root(), revision.as_deref())?;
                    println!("Judge disabled.");
                }
                Some(JudgeCommand::Test) => {
                    let backend = judge::resolve(store.root(), &config.extensions.judge)?.ok_or(
                        Error::Unavailable(
                            "judge not configured: store a key with xcb judge token and xcb judge enable",
                        ),
                    )?;
                    let mut questions = judge::JudgeQuestions::new();
                    questions.insert(
                        "ping".to_owned(),
                        judge::JudgeQuestion::Noul {
                            instructions: "Is the sky blue on a clear day?".to_owned(),
                            criteria: None,
                        },
                    );
                    questions.insert(
                        "pick".to_owned(),
                        judge::JudgeQuestion::Choice {
                            instructions: "Which option names a color?".to_owned(),
                            criteria: std::collections::BTreeMap::from([
                                ("red".to_owned(), Some("a color".to_owned())),
                                ("spoon".to_owned(), Some("not a color".to_owned())),
                            ]),
                        },
                    );
                    questions.insert(
                        "rate".to_owned(),
                        judge::JudgeQuestion::Score {
                            instructions: "How true is the claim that water is wet? Rate on the ordered criteria scale.".to_owned(),
                            criteria: vec![
                                "false".to_owned(),
                                "partly true".to_owned(),
                                "true".to_owned(),
                            ],
                        },
                    );
                    let answers = backend
                        .ask(
                            &serde_json::json!({"context": "xcb judge connectivity test"}),
                            &questions,
                        )
                        .await?;
                    let noul = answers
                        .answers
                        .get("ping")
                        .and_then(|a| a.noul())
                        .ok_or(Error::Unavailable("judge response missing noul answer"))?;
                    let (pick, pick_confidence) = answers
                        .answers
                        .get("pick")
                        .and_then(|a| a.choice())
                        .ok_or(Error::Unavailable("judge response missing choice answer"))?;
                    let (score, score_confidence) = answers
                        .answers
                        .get("rate")
                        .and_then(|a| a.score())
                        .ok_or(Error::Unavailable("judge response missing score answer"))?;
                    println!(
                        "Judge reachable · model {} · noul {noul:.3} · choice {pick}@{pick_confidence:.3} · score {score:.3}@{score_confidence:.3}",
                        answers.model.as_deref().unwrap_or("unknown")
                    );
                }
                None | Some(JudgeCommand::Status) => {
                    let source = judge::judge_token(store.root())?.map(|(_, source)| source);
                    if let Some(source) = source {
                        judge::check_key_target(source, &config.extensions.judge)?;
                    }
                    let (judge_model, judge_endpoint) =
                        xcb_runtime::jev::effective_target(&config.extensions.judge)?;
                    if cli.json {
                        print_json(json!({
                            "version": 1,
                            "enabled": config.extensions.judge.enabled,
                            "key": match source {
                                Some(judge::JudgeKeySource::Env) => "env",
                                Some(judge::JudgeKeySource::Vault) => "vault",
                                None => "none",
                            },
                            "model": judge_model,
                            "endpoint": judge_endpoint,
                        }))?;
                    } else {
                        let key = match source {
                            Some(judge::JudgeKeySource::Env) => "env",
                            Some(judge::JudgeKeySource::Vault) => "vault",
                            None => "none",
                        };
                        println!(
                            "judge: {} · key {key} · model {judge_model} · {judge_endpoint}",
                            if config.extensions.judge.enabled {
                                "enabled"
                            } else {
                                "disabled"
                            },
                        );
                    }
                }
            }
            Ok(0)
        }
        Some(Commands::Update { command }) => {
            match command.unwrap_or(UpdateCommand::Check) {
                UpdateCommand::Check => {
                    let result = xcb_runtime::update::check(
                        store.root(),
                        env!("CARGO_PKG_VERSION"),
                        cli.json,
                    );
                    if cli.json {
                        print_json(result?)?;
                    } else {
                        result?;
                    }
                }
                UpdateCommand::Status => {
                    let state = xcb_runtime::update::load(store.root())?;
                    if cli.json {
                        print_json(
                            json!({"version":1,"policy":state.policy,"lastCheckMs":state.last_check_ms,"availableVersion":state.available_version}),
                        )?;
                    } else {
                        let available = match state.available_version.as_deref() {
                            Some(version) => format!("available {version}"),
                            None if state.last_check_ms == 0 => "not checked yet".into(),
                            None => "no newer release recorded".into(),
                        };
                        println!(
                            "update policy: {} · last check {} · {}",
                            state.policy,
                            human_age(now_ms(), state.last_check_ms),
                            available
                        );
                    }
                }
                UpdateCommand::Enable { policy } => {
                    xcb_runtime::update::configure_scheduler(&std::env::current_exe()?, true)?;
                    let state = xcb_runtime::update::set_policy(store.root(), policy)?;
                    if cli.json {
                        print_json(
                            json!({"version":1,"policy":state.policy,"enabled":state.policy != xcb_runtime::update::Policy::Disable}),
                        )?;
                    } else {
                        println!("xcb updates: {}", state.policy);
                    }
                }
                UpdateCommand::Disable => {
                    xcb_runtime::update::configure_scheduler(&std::env::current_exe()?, false)?;
                    let state = xcb_runtime::update::set_policy(
                        store.root(),
                        xcb_runtime::update::Policy::Disable,
                    )?;
                    if cli.json {
                        print_json(json!({"version":1,"policy":state.policy,"enabled":false}))?;
                    } else {
                        println!("xcb updates disabled");
                    }
                }
                UpdateCommand::Install { version, quiet } => {
                    xcb_runtime::update::upgrade(
                        store.root(),
                        env!("CARGO_PKG_VERSION"),
                        version.as_deref(),
                        quiet || cli.json,
                    )?;
                }
                UpdateCommand::Daemon { quiet } => {
                    if xcb_runtime::update::should_check(store.root())? {
                        let result = xcb_runtime::update::check(
                            store.root(),
                            env!("CARGO_PKG_VERSION"),
                            quiet || cli.json,
                        )?;
                        if xcb_runtime::update::load(store.root())?.policy
                            == xcb_runtime::update::Policy::Auto
                            && result.release_available
                        {
                            xcb_runtime::update::upgrade(
                                store.root(),
                                env!("CARGO_PKG_VERSION"),
                                None,
                                quiet || cli.json,
                            )?;
                        }
                    }
                }
            }
            Ok(0)
        }
        Some(Commands::Upgrade { version, quiet }) => xcb_runtime::update::upgrade(
            store.root(),
            env!("CARGO_PKG_VERSION"),
            version.as_deref(),
            quiet || cli.json,
        ),
        Some(Commands::Config) => {
            print_json(config)?;
            Ok(0)
        }
        Some(Commands::Recover {
            run,
            yes,
            launch_artifacts,
        }) => {
            if launch_artifacts {
                if run.is_some() {
                    return Err(Error::Unavailable(
                        "--launch-artifacts recovers disposable files, not a run; pass one or the other",
                    ));
                }
                let sweep = runner::reclaim_launch_artifacts(&root, yes)?;
                if cli.json {
                    print_json(json!({
                        "version": 1,
                        "dryRun": !yes,
                        "reclaimable": sweep.reclaimable,
                        "reclaimableBytes": sweep.reclaimable_bytes,
                        "reclaimed": sweep.reclaimed,
                        "reclaimedBytes": sweep.reclaimed_bytes,
                        "liveHeld": sweep.live,
                        "unreclaimable": sweep.unprovable.len(),
                        "unreclaimableBytes": sweep.unprovable_bytes,
                    }))?;
                } else {
                    println!(
                        "{} {} settled launch snapshots ({}).",
                        if yes { "Reclaimed" } else { "Can reclaim" },
                        if yes {
                            sweep.reclaimed
                        } else {
                            sweep.reclaimable
                        },
                        human_bytes(if yes {
                            sweep.reclaimed_bytes
                        } else {
                            sweep.reclaimable_bytes
                        }),
                    );
                    println!(
                        "Retained {} live and {} unproven launch snapshots; --yes does not override custody.",
                        sweep.live,
                        sweep.unprovable.len(),
                    );
                    if !yes && sweep.reclaimable > 0 {
                        println!("Re-run with --yes to remove the proven disposable snapshots.");
                    }
                }
                return Ok(0);
            }
            if let Some(run_id) = run {
                let (run, mut run_digest) = store
                    .recovery_candidate(&run_id)?
                    .ok_or(Error::Unavailable("run not found"))?;
                // Prepared state also covers a child spawned before its PID
                // was persisted. Neither --yes nor owner absence proves that
                // child stopped, so this path only explains retained custody.
                if run.phase == "prepared" && run.pid.is_none() {
                    if yes {
                        return Err(Error::Conflict(
                            "run has no recorded process group; account custody retained because provider stop cannot be proven",
                        ));
                    }
                    if cli.json {
                        print_json(json!({
                            "version": 1,
                            "dryRun": true,
                            "run": run.id,
                            "phase": run.phase,
                            "pid": serde_json::Value::Null,
                            "leaseReleased": false,
                            "custodyRetained": true,
                            "recoverable": false,
                            "reason": "prepared state does not prove no provider child exists",
                        }))?;
                    } else {
                        println!(
                            "Run {} has no recorded process group; account custody is retained.",
                            run.id
                        );
                        println!(
                            "  A provider may have started before its PID was saved. Its stop cannot be proven from this record."
                        );
                        println!("  --yes cannot override missing process-stop evidence.");
                    }
                    return Ok(0);
                }
                let pid = run.pid.ok_or(Error::Conflict(
                    "run has no process group; recovery requires a running phase with a recorded pid",
                ))?;
                if run.phase != "running" {
                    return Err(Error::Conflict(
                        "run is not in running phase; recovery requires a recorded process group",
                    ));
                }
                run.verify_recovery_stop()?;
                if !yes {
                    if cli.json {
                        print_json(
                            json!({"version":1,"dryRun":true,"run":run.id,"phase":run.phase,"pid":pid,"guestCommandProofRequired":run.command_custody.is_some()}),
                        )?;
                    } else {
                        println!(
                            "Would recover run {} · phase {} · process group {}.\nRepeat with --yes to independently verify any guest command, reconcile retained credentials and release custody. Staged command edits are never published by recovery.",
                            run.id, run.phase, pid
                        );
                    }
                    return Ok(0);
                }
                process::prove_process_group_absent(pid)?;
                if run.command_custody.is_some() {
                    xcb_runtime::command_tool::recover(&store, &run, &run_digest).await?;
                    run_digest = store
                        .recovery_candidate(&run_id)?
                        .ok_or(Error::Unavailable("run not found"))?
                        .1;
                }
                let settled = store.recover_run(&run_id, &run_digest, now_ms())?;
                if cli.json {
                    print_json(
                        json!({"version":1,"recovered":settled.id,"phase":settled.phase,"pid":pid}),
                    )?;
                } else {
                    println!(
                        "Recovered run {} · process group {} confirmed absent",
                        settled.id, pid
                    );
                }
            } else {
                let runs = store.unsettled_runs()?;
                if cli.json {
                    print_json(
                        json!({"version":1,"runs":runs.iter().map(|run| json!({"id":run.id,"phase":run.phase,"pid":run.pid,"createdAtMs":run.created_at_ms})).collect::<Vec<_>>()}),
                    )?;
                } else if runs.is_empty() {
                    println!("No unsettled runs.");
                } else {
                    println!("Unsettled runs:");
                    for run in runs {
                        println!("  {} · phase {} · pid {:?}", run.id, run.phase, run.pid);
                    }
                    println!(
                        "Use `xcb recover <run-id> --yes` after the original host and process group have stopped; unresolved credentials must reconcile safely."
                    );
                }
            }
            Ok(0)
        }
        Some(Commands::Command {
            command: CommandJobs::Prune { days, yes },
        }) => {
            let root = xcb_runtime::command_tool::default_root()?;
            let report = xcb_runtime::command::CommandBackend::prune_joined_jobs(
                &root,
                now_ms().saturating_sub(u64::from(days) * 86_400_000),
                yes,
            )?;
            if cli.json {
                print_json(report)?;
            } else {
                println!(
                    "{} {} joined command job(s) older than {days} days into {}. {} unjoined, {} cleanup-pending and {} recent job(s) retained.{}",
                    if yes { "Archived" } else { "Would archive" },
                    report.candidates.len(),
                    root.join("jobs-archive").display(),
                    report.retained_unjoined,
                    report.retained_cleanup_pending,
                    report.retained_recent,
                    if yes {
                        ""
                    } else {
                        " Repeat with --yes to apply."
                    }
                );
            }
            Ok(0)
        }
        Some(Commands::EgressForward {
            socket,
            port,
            lo_up,
            env_file,
            target_port,
            child,
        }) => egress_forward(&socket, port, &lo_up, &env_file, target_port, &child).await,
        Some(Commands::BrokerStdio) => broker_stdio().await,
        Some(Commands::Completions { shell }) => {
            clap_complete::generate(shell, &mut Cli::command(), "xcb", &mut io::stdout());
            Ok(0)
        }
    }
}

fn preview_provider_preferences(
    preference: Option<Provider>,
    required: bool,
    provider_override: Option<Provider>,
) -> Result<(Option<Provider>, Option<Provider>)> {
    if required && provider_override.is_some() && provider_override != preference {
        return Err(Error::Conflict(
            "--provider conflicts with the task's explicit provider directive",
        ));
    }
    let preferred = provider_override.or(preference);
    let required = provider_override.or(required.then_some(preference).flatten());
    Ok((preferred, required))
}

async fn managed_chat(
    store: Arc<Store>,
    cwd: PathBuf,
    resume: Option<Id>,
    json: bool,
) -> Result<i32> {
    if json {
        return Err(Error::Unavailable(
            "interactive chat is not a JSON transport; use xcb tasks --json or xcb run --json",
        ));
    }
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(Error::Unavailable(
            "chat requires a terminal; use xcb run for headless tasks",
        ));
    }
    let managed = xcb_runtime::managed::ManagedStore::open(store.root())?;
    let conversation = match resume {
        Some(id) => managed
            .conversation(&id)?
            .ok_or(Error::Unavailable("managed conversation not found"))?,
        None => managed.create_conversation(&cwd).await?,
    };
    let executable = std::env::current_exe()?;
    let (updates, display) = sync_channel(256);
    let (commands, input) = sync_channel(32);
    let ui = tokio::task::spawn_blocking(move || xcb_tui::run(display, commands));
    let result =
        xcb_runtime::managed::serve_ui(store, conversation.id, input, updates, executable).await;
    let ui = ui
        .await
        .map_err(|_| Error::Unavailable("terminal task failed"))?;
    ui?;
    result?;
    Ok(0)
}

async fn direct_chat(
    store: Arc<Store>,
    cwd: PathBuf,
    session: Option<Id>,
    json: bool,
) -> Result<i32> {
    if json {
        return Err(Error::Unavailable(
            "interactive chat is not a JSON transport; use xcb run --json",
        ));
    }
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(Error::Unavailable(
            "chat requires a terminal; use xcb run for headless tasks",
        ));
    }
    let (updates, display) = sync_channel(256);
    let (commands, input) = sync_channel(32);
    let ui = tokio::task::spawn_blocking(move || xcb_tui::run(display, commands));
    let result = kernel::serve(store, cwd, session, input, updates).await;
    let ui = ui
        .await
        .map_err(|_| Error::Unavailable("terminal task failed"))?;
    ui?;
    result?;
    Ok(0)
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let code = match dispatch(cli).await {
        Ok(code) => code,
        Err(error) => {
            eprintln!("xcb: {error}");
            1
        }
    };
    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn application_diagnostic_requires_exact_selection_and_does_not_initialize_state() {
        let base = [
            "xcb",
            "--json",
            "application-diagnostic",
            "--account",
            "a_fixture",
        ];
        assert!(Cli::try_parse_from(base).is_err());
        assert!(matches!(
            Cli::try_parse_from(base.into_iter().chain(["--request", "application_fixture"])).unwrap().command,
            Some(Commands::ApplicationDiagnostic { account, request })
                if account.as_str() == "a_fixture" && request.as_str() == "application_fixture"
        ));
        let root = std::env::temp_dir().join(xcb_runtime::new_id("missing_diagnostic").as_str());
        assert!(!root.exists());
        assert_eq!(
            application::diagnostic_dispatch(
                &root,
                &Id::new("a_fixture").unwrap(),
                &Id::new("application_fixture").unwrap(),
                true
            )
            .unwrap(),
            1
        );
        assert!(!root.exists());
    }

    #[test]
    fn qualification_generation_flag_is_optional_canonical_and_conflicts_with_inspection() {
        let base = [
            "xcb",
            "qualify-application",
            "--account",
            "a_fixture",
            "--model",
            "claude/fixture",
        ];
        let parse =
            |tail: &[&str]| Cli::try_parse_from(base.into_iter().chain(tail.iter().copied()));
        assert!(matches!(
            parse(&["--evidence", "/private/fixture"]).unwrap().command,
            Some(Commands::QualifyApplication {
                expected_generation: None,
                ..
            })
        ));
        let expected = "0123456789abcdef".repeat(4);
        assert!(matches!(
            parse(&["--evidence", "/private/fixture", "--expected-generation", &expected]).unwrap().command,
            Some(Commands::QualifyApplication { expected_generation: Some(value), .. }) if value == expected
        ));
        for invalid in [
            "".into(),
            "a".repeat(63),
            "0".repeat(65),
            "A".repeat(64),
            "g".repeat(64),
            "é".repeat(32),
        ] {
            assert!(
                parse(&[
                    "--evidence",
                    "/private/fixture",
                    "--expected-generation",
                    &invalid
                ])
                .is_err()
            );
        }
        assert!(parse(&["--inspect"]).is_ok());
        assert!(parse(&["--inspect", "--expected-generation", &expected]).is_err());
    }

    #[test]
    fn devin_import_requires_an_explicit_source() {
        let cli = Cli::try_parse_from([
            "xcb",
            "accounts",
            "import-devin",
            "--source",
            "/private/source/credentials.toml",
        ])
        .unwrap();
        assert!(matches!(cli.command, Some(Commands::Accounts {
            command: Some(AccountCommand::ImportDevin { source }),
        }) if source == std::path::Path::new("/private/source/credentials.toml")));
        assert!(Cli::try_parse_from(["xcb", "accounts", "import-devin"]).is_err());
        assert!(
            Cli::try_parse_from(["xcb", "accounts", "import-devin", "--label", "Work account"])
                .is_err()
        );
        assert!(
            Cli::try_parse_from(["xcb", "accounts", "import-devin", "--token", "synthetic"])
                .is_err()
        );
    }

    #[test]
    fn devin_added_account_explains_token_and_explicit_import_paths() {
        let id = Id::new("a_devin").unwrap();
        let account = PublicAccount {
            id: &id,
            provider: Provider::Devin,
            name: "devin/a_devin".into(),
            email: None,
            subscription: "Subscription",
            enabled: true,
        };
        let message = account.added_message();
        assert!(message.contains("xcb accounts token a_devin"));
        assert!(message.contains("xcb accounts import-devin --source"));
        assert!(!message.contains("metadata only"));
        assert!(!message.contains("accounts login"));
    }

    #[test]
    fn catalog_selection_requires_explicit_devin_credentials_and_rejects_ambient_discovery() {
        let directory = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(xcb_runtime::new_id("xcb_cli").as_str());
        let store = Store::open(&directory).unwrap();
        let devin = store
            .add_account(Provider::Devin, "Subscription", now_ms(), None)
            .unwrap();
        let claude = store
            .add_account(Provider::Claude, "Subscription", now_ms(), None)
            .unwrap();
        assert!(catalog_account(&store, Provider::Devin, None, false).is_err());
        assert!(catalog_account(&store, Provider::Devin, Some(devin.id.as_str()), false).is_err());
        xcb_runtime::devin::auth::store_token(&store, &devin.id, b"synthetic-token").unwrap();
        assert_eq!(
            catalog_account(&store, Provider::Devin, Some(devin.id.as_str()), false).unwrap(),
            Some(devin.id.clone())
        );
        assert!(catalog_account(&store, Provider::Devin, Some(claude.id.as_str()), false).is_err());
        for provider in Provider::ALL {
            let error = catalog_account(&store, provider, None, true).unwrap_err();
            assert!(error.to_string().contains("--from-native"));
        }
        assert!(catalog_account(&store, Provider::Devin, Some(devin.id.as_str()), true).is_err());
        assert!(
            catalog_account(&store, Provider::Claude, None, false)
                .unwrap()
                .is_none()
        );
        assert!(
            catalog_account(&store, Provider::Codex, None, false)
                .unwrap()
                .is_none()
        );
        assert!(store.unsettled_runs().unwrap().is_empty());
        drop(store);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn codex_import_requires_an_explicit_source() {
        let cli = Cli::try_parse_from([
            "xcb",
            "accounts",
            "import-codex",
            "--source",
            "/private/source/auth.json",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::Accounts {
                command: Some(AccountCommand::ImportCodex { source }),
            }) if source == std::path::Path::new("/private/source/auth.json")
        ));
        assert!(Cli::try_parse_from(["xcb", "accounts", "import-codex"]).is_err());
        assert!(
            Cli::try_parse_from(["xcb", "accounts", "import-codex", "--label", "Work account"])
                .is_err()
        );
    }

    #[test]
    fn broker_helper_accepts_no_credentials_as_arguments_and_stays_hidden() {
        assert!(matches!(
            Cli::try_parse_from(["xcb", "broker-stdio"])
                .unwrap()
                .command,
            Some(Commands::BrokerStdio)
        ));
        assert!(matches!(
            Cli::try_parse_from(["xcb", "managed-daemon"])
                .unwrap()
                .command,
            Some(Commands::ManagedDaemon)
        ));
        assert!(Cli::try_parse_from(["xcb", "broker-stdio", "--token", "synthetic"]).is_err());
        assert!(
            !Cli::command()
                .render_long_help()
                .to_string()
                .contains("broker-stdio")
        );
        assert!(
            !Cli::command()
                .render_long_help()
                .to_string()
                .contains("managed-daemon")
        );
    }

    #[test]
    fn import_acknowledgements_only_expose_the_generated_routing_id() {
        let id = Id::new("a_public_routing_id").unwrap();
        assert_eq!(
            import_acknowledgement(&id),
            json!({"version":1,"account":"a_public_routing_id","sourcePreserved":true,"sessionsMigrated":false}),
        );
    }

    #[test]
    fn json_run_output_includes_its_resumable_session_id() {
        let mut result = runner::Outcome {
            diagnostic: None,
            text: "Completed response".into(),
            facts: xcb_core::policy::TurnFacts {
                terminal: Terminal::Completed,
                joined: true,
                effects: xcb_core::policy::EffectState::None,
                pending_attention: false,
                failure: None,
            },
            state: xcb_core::session::State::Idle,
        };
        let session = Id::new("s_resumable").unwrap();
        let output = run_output(&session, &result);
        assert_eq!(output["session"], "s_resumable");
        assert_eq!(output["text"], result.text);
        assert_eq!(output.as_object().unwrap().len(), 5);
        assert_eq!(
            output["outcome"],
            serde_json::to_value(&result.facts).unwrap()
        );
        result.diagnostic = Some(
            serde_json::from_value(json!("provider protocol error: fixture failure")).unwrap(),
        );
        assert_eq!(
            run_output(&session, &result)["diagnostic"],
            "provider protocol error: fixture failure"
        );
    }

    #[test]
    fn headless_success_requires_completed_joined_settled_idle_outcome() {
        use xcb_core::{
            policy::{EffectState, Failure, TurnFacts},
            session::State,
        };
        let mut result = runner::Outcome {
            diagnostic: None,
            text: "Provider said done".into(),
            facts: TurnFacts {
                terminal: Terminal::Completed,
                joined: true,
                effects: EffectState::None,
                pending_attention: false,
                failure: None,
            },
            state: State::Idle,
        };
        for effects in [EffectState::None, EffectState::Settled] {
            result.facts.effects = effects;
            assert_eq!(run_exit_code(&result), 0);
        }
        result.facts.effects = EffectState::Uncertain;
        assert_eq!(run_exit_code(&result), 1);
        result.facts.effects = EffectState::None;
        result.facts.joined = false;
        assert_eq!(run_exit_code(&result), 1);
        let output = run_output(&Id::new("s_unjoined").unwrap(), &result);
        assert_eq!(output["outcome"]["joined"], false);
        assert_eq!(output["outcome"]["terminal"], "completed");
        result.facts.joined = true;
        result.facts.pending_attention = true;
        assert_eq!(run_exit_code(&result), 1);
        result.facts.pending_attention = false;
        result.facts.failure = Some(Failure::Unknown);
        assert_eq!(run_exit_code(&result), 1);
        result.facts.failure = None;
        result.state = State::Uncertain;
        assert_eq!(run_exit_code(&result), 1);
        result.state = State::Idle;
        for terminal in [
            Terminal::Failed,
            Terminal::Cancelled,
            Terminal::TokenLimit,
            Terminal::TurnLimit,
        ] {
            result.facts.terminal = terminal;
            assert_eq!(run_exit_code(&result), 1);
        }
    }

    #[test]
    fn codex_added_account_has_a_copyable_login_command() {
        let account = PublicAccount {
            id: &Id::new("a_codex").unwrap(),
            provider: Provider::Codex,
            name: "codex/a_codex".into(),
            email: None,
            subscription: "Pro",
            enabled: true,
        };
        assert!(
            account
                .added_message()
                .ends_with("xcb accounts login a_codex")
        );
    }

    #[test]
    fn public_account_output_has_only_the_documented_metadata_fields() {
        let record = xcb_runtime::store::Account {
            id: Id::new("a_0123456789abcdef0123456789abcdef").unwrap(),
            provider: Provider::Claude,
            label: "claude/a_01234567".into(),
            email: Some("user@example.com".into()),
            subscription: "Max".into(),
            quota_pool: Id::new("private-custody-pool").unwrap(),
            enabled: true,
            created_at_ms: 987654321,
        };
        let output = serde_json::to_value(PublicAccount::from(&record)).unwrap();
        assert_eq!(
            output,
            json!({
                "id": "a_0123456789abcdef0123456789abcdef",
                "provider": "claude",
                "name": "user@example.com",
                "email": "user@example.com",
                "subscription": "Max",
                "enabled": true,
            })
        );
        let text = output.to_string();
        assert!(!text.contains("private-custody-pool"));
        assert!(!text.contains("987654321"));
    }

    #[test]
    fn accounts_with_shared_email_keep_distinct_public_ids_and_login_commands() {
        let first = xcb_runtime::store::Account {
            id: Id::new("a_0123456789abcdef0123456789abcdef").unwrap(),
            provider: Provider::Claude,
            label: "claude/a_01234567".into(),
            email: None,
            subscription: "Max".into(),
            quota_pool: Id::new("private-custody-pool").unwrap(),
            enabled: true,
            created_at_ms: 987654321,
        };
        let second = xcb_runtime::store::Account {
            id: Id::new("a_fedcba9876543210fedcba9876543210").unwrap(),
            ..first.clone()
        };
        let first_output = PublicAccount::from(&first).added_message();
        let second_output = PublicAccount::from(&second).added_message();
        assert!(
            first_output
                .contains("Added claude/a_01234567 (claude) · a_0123456789abcdef0123456789abcdef")
        );
        assert!(
            second_output
                .contains("Added claude/a_fedcba98 (claude) · a_fedcba9876543210fedcba9876543210")
        );
        assert!(first_output.ends_with("xcb accounts login a_0123456789abcdef0123456789abcdef"));
        assert!(second_output.ends_with("xcb accounts login a_fedcba9876543210fedcba9876543210"));
        for output in [first_output, second_output] {
            assert!(!output.contains("private-custody-pool"));
            assert!(!output.contains("987654321\n"));
        }
    }

    #[test]
    fn update_cli_shape_accepts_policy_and_upgrade_aliases() {
        let cli = Cli::try_parse_from(["xcb", "update", "enable", "--policy", "auto"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::Update {
                command: Some(UpdateCommand::Enable {
                    policy: xcb_runtime::update::Policy::Auto
                })
            })
        ));
        let cli = Cli::try_parse_from(["xcb", "update", "install", "0.5.0"]).unwrap();
        assert!(
            matches!(cli.command, Some(Commands::Update { command: Some(UpdateCommand::Install { version: Some(version), quiet: false }) }) if version == "0.5.0")
        );
        let cli = Cli::try_parse_from(["xcb", "upgrade", "0.5.0"]).unwrap();
        assert!(
            matches!(cli.command, Some(Commands::Upgrade { version: Some(version), quiet: false }) if version == "0.5.0")
        );
        assert!(Cli::try_parse_from(["xcb", "update", "enable", "--policy", "project"]).is_err());
    }

    #[test]
    fn managed_task_cli_lists_and_inspects_without_exposing_daemon_controls() {
        let cli = Cli::try_parse_from(["xcb", "chat"]).unwrap();
        assert!(matches!(cli.command, Some(Commands::Chat { resume: None })));
        let cli = Cli::try_parse_from(["xcb", "chat", "--resume", "c_example"]).unwrap();
        assert!(
            matches!(cli.command, Some(Commands::Chat { resume: Some(id) }) if id.as_str() == "c_example")
        );
        let cli = Cli::try_parse_from(["xcb", "conversations"]).unwrap();
        assert!(matches!(cli.command, Some(Commands::Conversations)));
        let cli = Cli::try_parse_from(["xcb", "tasks"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::Tasks { command: None })
        ));
        let cli = Cli::try_parse_from(["xcb", "tasks", "verify", "t_example"]).unwrap();
        assert!(
            matches!(cli.command, Some(Commands::Tasks { command: Some(TaskCommand::Verify { id }) }) if id.as_str() == "t_example")
        );
        let cli = Cli::try_parse_from(["xcb", "tasks", "show", "t_example"]).unwrap();
        assert!(
            matches!(cli.command, Some(Commands::Tasks { command: Some(TaskCommand::Show { id }) }) if id.as_str() == "t_example")
        );
        let cli =
            Cli::try_parse_from(["xcb", "tasks", "messages", "t_example", "--after", "4"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::Tasks {
                command: Some(TaskCommand::Messages { id, after: 4 })
            }) if id.as_str() == "t_example"
        ));
        let cli = Cli::try_parse_from(["xcb", "offers", "--refresh"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::Offers { refresh: true })
        ));
        let cli = Cli::try_parse_from(["xcb", "models", "tiers", "--task", "fix a race"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::Models {
                command: Some(ModelCommand::Tiers { task })
            }) if task == "fix a race"
        ));
        let cli = Cli::try_parse_from([
            "xcb",
            "models",
            "route",
            "--task",
            "fix a race",
            "--provider",
            "codex",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::Models {
                command: Some(ModelCommand::Route { task, provider: Some(Provider::Codex) })
            }) if task == "fix a race"
        ));
    }

    #[test]
    fn route_preview_honors_learned_and_explicit_provider_preferences() {
        assert_eq!(
            preview_provider_preferences(Some(Provider::Claude), false, None).unwrap(),
            (Some(Provider::Claude), None)
        );
        assert_eq!(
            preview_provider_preferences(Some(Provider::Claude), false, Some(Provider::Codex))
                .unwrap(),
            (Some(Provider::Codex), Some(Provider::Codex))
        );
        assert_eq!(
            preview_provider_preferences(Some(Provider::Claude), true, None).unwrap(),
            (Some(Provider::Claude), Some(Provider::Claude))
        );
        assert!(
            preview_provider_preferences(Some(Provider::Claude), true, Some(Provider::Codex))
                .is_err()
        );
    }

    #[test]
    fn mailbox_cursor_rejects_values_outside_the_storage_range() {
        assert!(
            Cli::try_parse_from([
                "xcb",
                "tasks",
                "messages",
                "t_example",
                "--after",
                "9223372036854775808"
            ])
            .is_err()
        );
    }

    #[test]
    fn command_prune_cli_shape_defaults_to_dry_run() {
        let cli = Cli::try_parse_from(["xcb", "command", "prune"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::Command {
                command: CommandJobs::Prune {
                    days: 30,
                    yes: false
                }
            })
        ));
        let cli = Cli::try_parse_from(["xcb", "command", "prune", "--days", "7", "--yes"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::Command {
                command: CommandJobs::Prune { days: 7, yes: true }
            })
        ));
    }

    #[test]
    fn recover_cli_shape_accepts_optional_run_and_yes() {
        let cli = Cli::try_parse_from(["xcb", "recover"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::Recover {
                run: None,
                yes: false,
                launch_artifacts: false
            })
        ));

        let cli = Cli::try_parse_from(["xcb", "recover", "r_abc123"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::Recover {
                run: Some(_),
                yes: false,
                launch_artifacts: false
            })
        ));

        let cli = Cli::try_parse_from(["xcb", "recover", "r_abc123", "--yes"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::Recover {
                run: Some(_),
                yes: true,
                launch_artifacts: false
            })
        ));

        // The disposable-file sweep is a separate subject from run recovery,
        // and asking for both at once is rejected rather than guessed at.
        let cli = Cli::try_parse_from(["xcb", "recover", "--launch-artifacts"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::Recover {
                run: None,
                yes: false,
                launch_artifacts: true
            })
        ));
        let cli = Cli::try_parse_from(["xcb", "recover", "--launch-artifacts", "--yes"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::Recover {
                run: None,
                yes: true,
                launch_artifacts: true
            })
        ));
    }

    /// `--help` is part of the product surface: every subcommand must carry
    /// an `about` line and every argument a `help` line, recursively, so no
    /// bare name ever ships undocumented. Hidden internal commands count too.
    #[test]
    fn every_command_and_argument_is_documented() {
        fn check(command: &clap::Command, path: &str) {
            for sub in command.get_subcommands() {
                let name = format!("{path} {}", sub.get_name());
                assert!(
                    sub.get_about()
                        .is_some_and(|about| !about.to_string().trim().is_empty()),
                    "{name} has no about text"
                );
                for arg in sub.get_arguments() {
                    assert!(
                        arg.get_help()
                            .is_some_and(|help| !help.to_string().trim().is_empty()),
                        "{name} argument '{}' has no help text",
                        arg.get_id()
                    );
                }
                check(sub, &name);
            }
        }
        let cli = Cli::command();
        assert!(
            cli.get_after_help()
                .is_some_and(|text| text.to_string().contains("Plain `xcb`")),
            "xcb --help must explain what plain `xcb` does"
        );
        for arg in cli.get_arguments() {
            assert!(
                arg.get_help()
                    .is_some_and(|help| !help.to_string().trim().is_empty()),
                "global argument '{}' has no help text",
                arg.get_id()
            );
        }
        check(&cli, "xcb");
    }
}
