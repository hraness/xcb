mod application;
mod claude_sign_in;
mod code_sign_in;
mod context;
mod device_sign_in;
mod doctor;
mod habitat;
mod health;
mod native;
mod resources;
mod route;
mod stop;
mod table;
mod terminal_input;
mod tools;
mod usage;
mod ux;
mod workspaces;

use clap::{CommandFactory, Parser, Subcommand};
use serde_json::json;
use std::{
    io::{self, IsTerminal, Read, Write},
    path::PathBuf,
    sync::Arc,
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
    about = "Excalibur (xcb) routes coding tasks across the Claude and Codex subscriptions you already pay for",
    override_help = ux::ROOT_HELP
)]
struct Cli {
    /// State root for accounts, sessions, and tasks (default:
    /// ~/.local/share/xcb, or $XCB_STATE).
    #[arg(long, global = true)]
    state: Option<PathBuf>,
    /// Emit machine-readable JSON where a command supports it.
    #[arg(long, global = true)]
    json: bool,
    /// Project hint for run and models route; where every relative directory
    /// or scope argument starts.
    #[arg(long, global = true, default_value = ".")]
    cwd: PathBuf,
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    #[command(about = "Inspect, qualify and grant native provider execution")]
    Native {
        #[command(subcommand)]
        command: native::Commands,
    },
    /// Configure host tools shared by Codex and Claude.
    Tools {
        #[command(subcommand)]
        command: tools::Commands,
    },
    /// Inspect large source snapshots with resumable ALGAL programs.
    Context {
        #[command(subcommand)]
        command: context::ContextCommand,
    },
    /// Add an account, check the provider, sign in and load its models, in
    /// one command.
    Setup {
        /// Provider to set up: claude or codex.
        provider: Provider,
        /// Plan label for a new account; a display label only.
        #[arg(long, default_value = "Subscription")]
        plan: String,
        /// Add and sign in to another account.
        #[arg(long, conflicts_with = "account")]
        new: bool,
        /// Set up this existing account by name or id.
        #[arg(long)]
        account: Option<String>,
    },
    /// Generate text for an app from a JSON request: one turn, no tools or
    /// hooks, nothing saved as a session.
    Generate {
        /// Print capability rows and exit without running inference.
        #[arg(long)]
        capabilities: bool,
    },
    /// Read private failure details, up to a size limit, for one application
    /// request.
    #[command(hide = true)]
    ApplicationDiagnostic {
        /// Account that ran the request.
        #[arg(long)]
        account: Id,
        /// Request identifier to inspect.
        #[arg(long)]
        request: Id,
    },
    /// Run a fixed application qualification challenge using private gate evidence.
    #[command(hide = true)]
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
        /// Require access to the user’s existing signed-in browser (Codex only).
        #[arg(long)]
        signed_in_browser: bool,
        /// Require native desktop application control (Codex only).
        #[arg(long)]
        desktop: bool,
        /// Task text; piped stdin is used when omitted.
        #[arg(short = 'p', long)]
        prompt: Option<String>,
        /// Restrict automatic routing to this account name or id.
        #[arg(long)]
        account: Option<String>,
        /// Model key (provider/model[/effort]); omitted or "auto" routes automatically.
        #[arg(long)]
        model: Option<String>,
        /// Attach an image file to the prompt; repeatable, up to 8.
        #[arg(long = "image")]
        images: Vec<PathBuf>,
    },
    /// Pick an account and model that can take the task now and run one turn.
    /// For programs: requires --json and a JSON request on stdin.
    #[command(hide = true)]
    Route,
    /// List accounts; subcommands add, sign in, remove, and manage them.
    Accounts {
        #[command(subcommand)]
        command: Option<AccountCommand>,
    },
    /// List observed models; subcommands refresh catalogs and set the default.
    Models {
        #[command(subcommand)]
        command: Option<ModelCommand>,
    },
    /// Your token use by day, agent and model, from aicharts' record on this
    /// computer. Nothing is uploaded. `xcb usage --help` lists the commands.
    #[command(disable_help_flag = true)]
    Usage {
        /// report (default), status, enable, disable or collect, then options.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Inspect and teach the learned reflexes: model routing and turn categorization.
    Reflex {
        #[command(subcommand)]
        command: Option<ReflexCommand>,
    },
    /// Inspect conditional public offers; observations do not verify account entitlement.
    Offers {
        /// Re-check the published offers before listing them.
        #[arg(long)]
        refresh: bool,
    },
    /// List provider sessions; discover and import recent Codex or Claude history.
    Sessions {
        #[command(subcommand)]
        command: Option<SessionCommand>,
    },
    /// Inspect durable managed tasks and their messages.
    Tasks {
        #[command(subcommand)]
        command: Option<TaskCommand>,
    },
    /// List saved conversations and project views.
    Conversations {
        #[arg(long, help = "Create a new project view in the current directory")]
        new: bool,
    },
    /// List, add, hide and explain project directories used by tasks.
    Workspaces {
        #[command(subcommand)]
        command: Option<workspaces::WorkspaceCommand>,
    },
    /// Read a page of saved conversation history, oldest message first.
    History {
        /// Conversation id, or session id when --direct is set.
        id: Id,
        /// Read a direct provider session instead of a managed conversation.
        #[arg(long)]
        direct: bool,
        /// Read messages older than the previous page's first_sequence.
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..=i64::MAX as u64))]
        before: Option<u64>,
        /// Maximum messages in this page, from 1 to 512.
        #[arg(long, default_value_t = 128, value_parser = clap::value_parser!(u16).range(1..=512))]
        limit: u16,
    },
    /// Rename a conversation only if its title still matches the inspected value.
    Rename {
        /// Conversation id, or session id when --direct is set.
        id: Id,
        /// New display title.
        title: String,
        /// Exact current title; a concurrent rename is rejected.
        #[arg(long)]
        expected_title: String,
        /// Rename a direct provider session instead of a managed conversation.
        #[arg(long)]
        direct: bool,
    },
    /// Manage a conversation's durable work queue and completed work.
    Backlog {
        #[command(subcommand)]
        command: Option<habitat::BacklogCommand>,
        /// Filter by persistent conversation; otherwise show all conversations.
        #[arg(long)]
        conversation: Option<Id>,
        /// Filter by exact project directory, across every conversation over it.
        #[arg(long, conflicts_with = "conversation")]
        workspace: Option<PathBuf>,
    },
    /// Queue guidance for a task's next safe turn without interrupting its worker.
    Steer {
        /// Existing nonclosed task from `xcb backlog`; attention gates still apply.
        task: Id,
        /// Guidance to include within the task's existing authority and budget.
        text: String,
        /// Stable event identity for an idempotent retry; generated when omitted.
        #[arg(long)]
        id: Option<Id>,
    },
    /// Request a task's completion report in another task's durable inbox.
    Watch {
        /// Task that will receive the report at an authorized turn boundary.
        target: Id,
        /// Task to observe in the same workspace.
        source: Id,
        /// Stable subscription identity for an idempotent retry.
        #[arg(long)]
        id: Option<Id>,
    },
    /// Inspect accepted guidance and reports, with their delivery evidence.
    Inbox {
        /// Filter by target task; otherwise show all targets.
        #[arg(long, conflicts_with = "conversation")]
        task: Option<Id>,
        /// Filter by conversation; cannot be combined with --task.
        #[arg(long)]
        conversation: Option<Id>,
        /// Read older events before this sequence from the previous page.
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..=i64::MAX as u64))]
        before: Option<u64>,
        /// Maximum events, newest first, from 1 to 256.
        #[arg(long, default_value_t = 64, value_parser = clap::value_parser!(u16).range(1..=256))]
        limit: u16,
    },
    /// Manage local recurring wake-ups for persistent conversations.
    Schedules {
        #[command(subcommand)]
        command: Option<habitat::ScheduleCommand>,
        /// Filter schedules by persistent conversation; otherwise show all.
        #[arg(long)]
        conversation: Option<Id>,
        /// Only schedules running in this project directory.
        #[arg(long)]
        workspace: Option<PathBuf>,
        /// Only enabled schedules.
        #[arg(long, conflicts_with = "disabled")]
        enabled: bool,
        /// Only paused schedules.
        #[arg(long)]
        disabled: bool,
        /// Only schedules whose next wake-up is now or overdue.
        #[arg(long)]
        due: bool,
    },
    /// Manage named durable ALGAL daemons in project conversations.
    Daemons {
        #[command(subcommand)]
        command: Option<habitat::DaemonCommand>,
    },
    /// Show questions, approvals and actions requiring attention across conversations.
    Attention,
    /// Set how much a project may do on its own, and see what each grant has
    /// left.
    Projects {
        #[command(subcommand)]
        command: Option<habitat::ProjectCommand>,
    },
    /// Start xcb's background supervisor at macOS login, for this state
    /// folder only.
    Service {
        #[command(subcommand)]
        command: Option<ServiceCommand>,
    },
    /// Show memory pressure and disk headroom, or enable managed launch limits.
    Resources {
        /// Also inspect the volume containing this project directory.
        #[arg(long)]
        workspace: Option<PathBuf>,
        #[command(subcommand)]
        command: Option<resources::ResourceCommand>,
    },
    /// Bind, search and explicitly promote notes to a project's local Wordcell vault.
    Memory {
        #[command(subcommand)]
        command: habitat::MemoryCommand,
    },
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
    /// Configure the optional Cloudflare Clef judge (environment token, policy, status).
    Judge {
        #[command(subcommand)]
        command: Option<JudgeCommand>,
    },
    /// Show the routing preference stack, or edit the routes never used.
    Routing {
        #[command(subcommand)]
        command: Option<RoutingCommand>,
    },
    /// Check provider builds, accounts, and unfinished runs.
    ///
    /// Exits 0 when an account can take a task and nothing needs your
    /// attention, and 1 when a check failed or needs attention; the output
    /// names the one next step.
    Doctor {
        /// Check only this provider (claude or codex).
        #[arg(long)]
        provider: Option<Provider>,
        /// Provider binary to check instead of the one found on PATH;
        /// requires --provider.
        #[arg(long)]
        executable: Option<PathBuf>,
        /// Preview the 0.9 project upgrade on a private copy of the managed
        /// state; nothing is changed.
        #[arg(long, conflicts_with_all = ["provider", "executable"])]
        upgrade_plan: bool,
        /// Linux: test xcb's bwrap sandbox on this machine and save the
        /// result Claude needs before it runs here. Run it again after a
        /// bwrap update or a change to the kernel's user-namespace settings.
        #[arg(long, conflicts_with_all = ["executable", "upgrade_plan"])]
        qualify_sandbox: bool,
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
        /// Allow installing a release older than the running one.
        #[arg(long)]
        allow_downgrade: bool,
        /// Suppress progress output (used by the updater itself).
        #[arg(long, hide = true)]
        quiet: bool,
    },
    /// Inspect or clean up unfinished runs and leftover launch folders.
    Recover {
        /// Recover this run; lists unfinished runs when omitted.
        run: Option<Id>,
        /// Apply the recovery instead of only reporting what would change.
        #[arg(long)]
        yes: bool,
        /// List leftover launch folders; --yes removes only those whose runs
        /// xcb confirmed finished. The rest stay.
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
    /// Supervise one owned background process and bound its diagnostic logs.
    #[command(hide = true)]
    ServiceRun,
    /// Internal: reviewed native computer-use transport.
    #[command(name = "native-mcp-stdio", hide = true)]
    NativeMcpStdio,
    /// Internal: the confined half of the Linux sandbox test.
    #[command(name = "sandbox-probe", hide = true)]
    SandboxProbe {
        /// Probe kind and its paths.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
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
    /// Set the update policy (default auto for verified release installs); also add a daily
    /// check. --policy disable turns checks off like `xcb update disable`.
    Enable {
        /// Update policy: notify, auto, or disable.
        #[arg(long, default_value = "auto", value_parser = parse_update_policy)]
        policy: xcb_runtime::update::Policy,
    },
    /// Turn off update checks and scheduled upgrades, and remove the daily
    /// check on macOS.
    Disable,
    /// Install a verified release using the recorded global installer.
    Install {
        /// Version tag to install; the latest verified release when omitted.
        version: Option<String>,
        /// Allow installing a release older than the running one.
        #[arg(long)]
        allow_downgrade: bool,
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
    /// Add and sign in to an account. With --json or outside a terminal,
    /// create the account only.
    Add {
        /// Provider to add: claude or codex.
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
        /// Authorize Claude browser access. On macOS, stores refresh credentials
        /// in a dedicated xcb Keychain entry.
        #[arg(long)]
        browser: bool,
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
    /// Permanently remove an account and its stored credentials.
    Remove {
        /// Account name or id (listed by `xcb accounts`).
        account: String,
        /// Allow removing the account currently selected as the default.
        #[arg(long)]
        force: bool,
    },
    /// Copy an existing Codex CLI sign-in (auth.json) into xcb.
    ImportCodex {
        /// Absolute path to the Codex auth.json to copy.
        #[arg(long)]
        source: PathBuf,
        /// Update this account's sign-in; creates an account when omitted.
        /// The imported sign-in must belong to the same Codex account and user.
        #[arg(long)]
        account: Option<String>,
    },
}
#[derive(Subcommand)]
enum ModelCommand {
    /// Discover the provider's current model catalog through an account.
    Refresh {
        /// Provider whose catalog is refreshed: claude or codex.
        provider: Provider,
        /// Account whose credentials run the discovery.
        #[arg(long)]
        account: Option<String>,
        /// Legacy discovery flag; use an explicit credential import and --account.
        #[arg(long)]
        from_native: bool,
    },
    /// Set a preferred model. Automatic routing can select a stronger eligible route.
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
        /// Restrict the preview to one provider (claude or codex).
        #[arg(long)]
        provider: Option<Provider>,
    },
}
#[derive(Subcommand)]
enum ReflexCommand {
    /// Show the active generation, program digest, live metrics and open trials.
    Status {
        /// Only this reflex (route or settle).
        reflex: Option<ReflexName>,
    },
    /// Decide finished trials and fit new challengers; xcb adopts a challenger only after it beats the active generation on labels that arrived after it was fitted.
    Train {
        /// Reflex to train (route or settle).
        reflex: ReflexName,
    },
    /// Label a task's latest decision: route frontier|standard, settle unfinished|confirm|done.
    Label {
        /// Reflex the label is for (route or settle).
        reflex: ReflexName,
        /// Managed task id.
        task: Id,
        /// frontier or standard (route); unfinished, confirm or done (settle).
        label: String,
    },
    /// Reactivate an earlier generation; 0 restores the shipped prior.
    Rollback {
        /// Reflex to roll back (route or settle).
        reflex: ReflexName,
        /// Generation to activate.
        version: u32,
    },
    /// Import labeled JSONL ({id,text,label[,weight][,judge|head,tool_calls]}), oldest first.
    /// The examples are replayed as a forward trial from the shipped prior; heads that won
    /// the replay are adopted. Only derived features are stored.
    Import {
        /// Reflex the examples are for (route or settle).
        reflex: ReflexName,
        /// JSONL file to import.
        file: PathBuf,
        /// Replay and report without storing or adopting anything.
        #[arg(long)]
        dry_run: bool,
    },
    /// Check that a reflex program file is valid and print its digest.
    Check {
        /// Program file (an ALGAL organism).
        file: PathBuf,
    },
}
#[derive(Clone, Copy, clap::ValueEnum)]
enum ReflexName {
    Route,
    Settle,
}
impl From<ReflexName> for xcb_core::reflex::Reflex {
    fn from(name: ReflexName) -> Self {
        match name {
            ReflexName::Route => Self::Route,
            ReflexName::Settle => Self::Settle,
        }
    }
}
#[derive(Subcommand)]
enum SessionCommand {
    /// Find Codex and Claude conversations active within the last 24 hours.
    Discover {
        /// Look back this many hours (1–8760).
        #[arg(long, default_value_t = 24, value_parser = clap::value_parser!(u16).range(1..=8760))]
        hours: u16,
        /// Read one provider's history: codex or claude.
        #[arg(long, value_parser = ["codex", "claude"])]
        provider: Option<String>,
    },
    /// Copy recent conversation text into xcb; original sessions keep running independently.
    Import {
        /// Candidate id from `xcb sessions discover`.
        #[arg(required_unless_present = "recent", conflicts_with = "recent")]
        id: Option<Id>,
        /// Import every conversation found in the selected time window.
        #[arg(long)]
        recent: bool,
        /// Use this project for one imported conversation; required when its source started at home.
        #[arg(long, conflicts_with = "recent")]
        workspace: Option<PathBuf>,
        /// Look back this many hours (1–8760).
        #[arg(long, default_value_t = 24, value_parser = clap::value_parser!(u16).range(1..=8760))]
        hours: u16,
        /// Read one provider's history: codex or claude.
        #[arg(long, value_parser = ["codex", "claude"])]
        provider: Option<String>,
    },
    /// Write local aicharts session observations to an export file.
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
enum ServiceCommand {
    /// Start the supervisor at login and check every minute that it runs.
    Install,
    /// Show whether it starts at login and whether the supervisor runs.
    Status,
    /// Stop starting at login; refuses while work is running and never
    /// stops it.
    Uninstall,
    /// Print the LaunchAgent (macOS) or systemd user unit (Linux) without
    /// installing it.
    Plan,
}

#[derive(Subcommand)]
enum CommandJobs {
    /// Move finished, acknowledged command jobs older than --days into
    /// jobs-archive/. Nothing is deleted; jobs whose processes haven't been
    /// confirmed stopped, jobs waiting for cleanup, and recent jobs stay.
    Prune {
        /// Archive only jobs last updated more than this many days ago.
        #[arg(long, default_value_t = 30)]
        days: u32,
        /// Apply the archive; without it only the dry-run report prints.
        #[arg(long)]
        yes: bool,
    },
}
#[derive(Subcommand)]
enum TaskCommand {
    /// List managed tasks (same as bare `xcb tasks`).
    List,
    /// Replay the task's local record and check that no step is missing or changed.
    Verify {
        /// Managed task id (listed by `xcb tasks`).
        id: Id,
    },
    /// Print one managed task's durable record as JSON.
    Show {
        /// Managed task id (listed by `xcb tasks`).
        id: Id,
    },
    /// Read a page of a task's messages, oldest first; pass the last sequence
    /// as --after for the next page.
    Messages {
        /// Managed task id (listed by `xcb tasks`).
        id: Id,
        /// Only messages after this sequence number (default 0).
        #[arg(long, default_value_t = 0, value_parser = clap::value_parser!(u64).range(..=i64::MAX as u64))]
        after: u64,
        /// Maximum messages in this page, from 1 to 64.
        #[arg(long, default_value_t = 64, value_parser = clap::value_parser!(u8).range(1..=64))]
        limit: u8,
    },
    /// Request cancellation only if the task still matches the inspected revision.
    Cancel {
        /// Managed task id from `xcb tasks` or `xcb backlog`.
        id: Id,
        /// Current task revision; stale cancellation is rejected.
        #[arg(long)]
        revision: u64,
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
    /// Store an explicitly selected legacy System One key from stdin; Clef uses environment tokens.
    Token,
    /// Remove the vaulted judge key.
    Logout,
    /// Report judge configuration without revealing the key.
    Status,
    /// Allow judged routing, safe continuation advice, and Gobstopper vetoes.
    Enable,
    /// Disable judge use; routing, continuation, and compaction stay deterministic.
    Disable,
    /// Check judge configuration locally without sending an inference request.
    Test,
    #[command(about = "Select Cloudflare Clef; credentials stay in CLOUDFLARE_API_TOKEN")]
    Clef {
        /// Clef model alias: clef or clef-flash.
        #[arg(long, default_value = "clef", value_parser = ["clef", "clef-flash"])]
        model: String,
    },
}
#[derive(Subcommand)]
enum RoutingCommand {
    /// Show each tier's route patterns and the observed models each one
    /// matches, the routes never used, and the fallback providers.
    Show,
    /// Edit the routes never used: `add <pattern>` or `remove <pattern>`.
    Never {
        #[command(subcommand)]
        command: NeverCommand,
    },
}
#[derive(Subcommand)]
enum NeverCommand {
    /// Exclude every route the pattern matches (provider/model-glob[/effort]).
    Add {
        /// Route pattern such as claude/opus*/max or codex/gpt-5.6-sol/low.
        pattern: String,
    },
    /// Stop excluding a pattern.
    Remove {
        /// A pattern listed by `xcb routing show`.
        pattern: String,
    },
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

/// One table cell: control characters stripped, cut to `width` display
/// columns and padded to exactly that width. A cut cell ends with `…` so
/// truncation is visible instead of a silent drop.
fn cell(value: &str, width: usize) -> String {
    table::cell(value, width)
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

/// Whether a settle head may act under `auto`, and the evidence.
fn certificate_line(certificate: &xcb_core::reflex::Certificate) -> String {
    let evidence = match (certificate.precision, certificate.lower) {
        (Some(precision), Some(lower)) => format!(
            "precision {precision:.2} (at least {lower:.2}) over {:.0} of {} operator-labeled turns at p ≥ {:.2}; floor {:.2}",
            certificate.fired, certificate.window, certificate.threshold, certificate.floor
        ),
        _ => format!(
            "no turns at p ≥ {:.2} among {} operator-labeled; floor {:.2}",
            certificate.threshold, certificate.window, certificate.floor
        ),
    };
    format!(
        "{} · {} · {evidence}",
        if certificate.certified {
            "certified to act under auto"
        } else {
            "observing under auto"
        },
        certificate.reason
    )
}

/// One line of reflex metrics: examples, accuracy, precision and recall at
/// the head's threshold, and AUC when both classes are present.
fn metrics_line(metrics: &xcb_core::reflex::Metrics) -> String {
    let rate = |value: Option<f64>| value.map_or_else(|| "–".into(), |value| format!("{value:.2}"));
    format!(
        "{} labels · accuracy {:.3} · precision {} · recall {}{}",
        metrics.n,
        metrics.accuracy,
        rate(metrics.precision),
        rate(metrics.recall),
        metrics
            .auc
            .map(|auc| format!(" · AUC {auc:.3}"))
            .unwrap_or_default(),
    )
}

fn print_json(value: impl serde::Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

/// `xcb run --json`: the text is cut to 256 KiB like `xcb --json route`,
/// and `textTruncated` appears only when it was cut.
fn run_output(session: &Id, result: &runner::Outcome) -> serde_json::Value {
    let truncated = result.text.len() > xcb_core::MAX_TEXT_BYTES;
    let text = if truncated {
        xcb_core::display_text(&result.text, xcb_core::MAX_TEXT_BYTES)
    } else {
        result.text.clone()
    };
    let mut output = json!({"version":1,"session":session,"state":result.state,"outcome":result.facts.reported(&result.text),"text":text});
    if truncated {
        output["textTruncated"] = json!(true);
    }
    if let Some(diagnostic) = &result.diagnostic {
        output["diagnostic"] = json!(diagnostic);
    }
    output
}

/// Aborts a background task when the command returns early.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

fn run_exit_code(result: &runner::Outcome) -> i32 {
    if result.facts.terminal == Terminal::Completed
        && result.facts.joined
        && result.facts.effects != xcb_core::policy::EffectState::Uncertain
        && !result.facts.pending_attention
        && result.facts.failure.is_none()
        && result.state == xcb_core::session::State::Idle
        && !xcb_core::policy::no_reply(&result.text, &result.facts)
    {
        0
    } else {
        1
    }
}

/// `xcb run` when the provider completed its turn without a reply or file
/// changes: one sentence, and the session to reopen.
fn no_reply_error(provider: Provider, session: &Id) -> Error {
    Error::guided(
        format!(
            "{} ended the turn without a reply or file changes; reopen the session to continue, or run again with --model to use another model",
            provider_name(provider)
        ),
        format!("xcb history {session} --direct"),
    )
}

fn import_acknowledgement(id: &Id) -> serde_json::Value {
    json!({"version":1,"account":id,"sourcePreserved":true})
}

async fn native_mcp_stdio() -> Result<i32> {
    #[cfg(unix)]
    {
        let socket = std::env::var_os("XCB_MCP_SOCKET")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .ok_or(Error::Unavailable("native MCP socket is unavailable"))?;
        let token = zeroize::Zeroizing::new(
            std::env::var("XCB_MCP_TOKEN")
                .map_err(|_| Error::Unavailable("native MCP authority is unavailable"))?,
        );
        xcb_runtime::native_mcp::run_native_mcp_stdio(&socket, &token).await?;
        Ok(0)
    }
    #[cfg(not(unix))]
    Err(Error::Unavailable(
        "native computer use is unavailable on this platform",
    ))
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
    /// What `accounts add` prints: the added account on stdout, then the one
    /// next step (sign-in) as a `Next:` hint.
    fn added_message(&self) -> (String, String) {
        let added = format!(
            "Added {} ({}) · {}",
            xcb_core::display_text(&self.name, 80),
            self.provider,
            self.id
        );
        let next = format!("xcb accounts login {}", self.id);
        (added, next)
    }
}

/// systemd stops a user's services when their last session ends unless
/// lingering is on, so a supervisor on a host reached over SSH would stop
/// at logout.
fn linger_notice() -> String {
    let user = std::env::var("USER").unwrap_or_else(|_| "$USER".into());
    format!(
        "xcb: systemd stops the supervisor when you log out. To keep it running on a server or other headless host, run: loginctl enable-linger {user}"
    )
}

/// `xcb service` status: what starts at login, whether the supervisor runs,
/// where it logs, and a Files & Folders denial found in that log.
fn service_text(status: &xcb_runtime::habitat_service::Status, style: ux::Style) -> String {
    let mut out = String::new();
    let login = match (status.installed, status.registered) {
        (true, true) => format!("{} Starts at login", style.symbol(ux::Symbol::Ok)),
        (true, false) => format!(
            "{} Installed, but {} hasn't loaded it",
            style.symbol(ux::Symbol::Warn),
            if cfg!(target_os = "linux") {
                "systemd"
            } else {
                "macOS"
            }
        ),
        (false, _) => format!("{} Doesn't start at login", style.symbol(ux::Symbol::Off)),
    };
    use xcb_runtime::habitat_service::HealthState;
    let supervisor = match status.supervisor_health.state {
        HealthState::Fresh => format!("{} supervisor running", style.symbol(ux::Symbol::On)),
        HealthState::Stale => format!(
            "{} supervisor heartbeat stalled",
            style.symbol(ux::Symbol::Warn)
        ),
        HealthState::Missing => format!(
            "{} supervisor health unknown",
            style.symbol(ux::Symbol::Warn)
        ),
        HealthState::Stopped => format!("{} supervisor idle", style.symbol(ux::Symbol::Off)),
    };
    out.push_str(&format!("{login} · {supervisor}\n"));
    if status.watchdog_enabled {
        if status.supervisor_running && !status.supervisor_watched {
            out.push_str(
                "Watchdog: installed; current supervisor started separately and is not watched\n",
            );
        } else {
            out.push_str("Watchdog: enabled · diagnostic logs limited to 6 MiB\n");
        }
    }
    if let Some(service) = &status.service {
        out.push_str(&format!("File: {}\n", service.manifest.display()));
    }
    match &status.log {
        Some(log) => {
            out.push_str(&format!("Log: {}\n", log.display()));
            if let Some(denial) = xcb_runtime::habitat_service::current_denial(log) {
                out.push_str(&ux::files_and_folders_denial(denial.folder, style));
            }
        }
        None if status.installed => {
            out.push_str("Log: off (this service was installed before xcb kept a log)\n")
        }
        None => {}
    }
    out
}

/// The provider's product name, for sentences.
fn provider_name(provider: Provider) -> &'static str {
    match provider {
        Provider::Claude => "Claude Code",
        Provider::Codex => "Codex",
        Provider::Devin => "retired Devin",
    }
}

/// Load the provider pin. On a state root that has never checked this
/// provider, first do what `xcb doctor --provider <p>` does, so the first
/// sign-in after `xcb accounts add` works without a separate doctor step.
async fn ensure_pin(root: &std::path::Path, provider: Provider) -> Result<Pin> {
    if provider == Provider::Devin {
        return Err(Error::Unavailable(
            "Devin support was removed; use Claude or Codex",
        ));
    }
    if Pin::recorded(root, provider) {
        return Pin::load(root, provider);
    }
    let name = provider_name(provider);
    eprintln!(
        "{} Checking {name} first (the same check as xcb doctor --provider {provider}).",
        ux::Style::stderr().symbol(ux::Symbol::Next)
    );
    let home = private::directory(&root.join("metadata-home"))?;
    private::directory(&home.join("tmp"))?;
    let mut pin = process::inspect(provider, None, &home)
        .await
        .map_err(|error| match error {
            guided @ Error::Guided { .. } => guided,
            error => Error::guided(
                format!("xcb couldn't check {name}: {}", ux::sentence(&error)),
                format!("xcb doctor --provider {provider}"),
            ),
        })?;
    pin.save(root)?;
    Ok(pin)
}

/// Refuse a provider build xcb can't run, saying why and what fixes it.
fn require_supported(root: &std::path::Path, pin: &Pin) -> Result<()> {
    if pin.provider == Provider::Devin {
        return Err(Error::Unavailable(
            "Devin support was removed; use Claude or Codex",
        ));
    }
    if runner::provider_admitted(root, pin) {
        return Ok(());
    }
    let name = provider_name(pin.provider);
    if cfg!(target_os = "linux") {
        if pin.provider != Provider::Claude {
            return Err(Error::guided(
                format!("xcb runs {name} on macOS ARM64 only"),
                "use Claude on Linux (xcb.sh/docs/providers#claude-on-linux)",
            ));
        }
        if !xcb_runtime::sandbox::linux_sandbox(root).qualified {
            return Err(Error::guided(
                "xcb can't run Claude Code until this machine passes its Linux sandbox checks",
                "run xcb doctor --provider claude --qualify-sandbox (xcb.sh/docs/providers#claude-on-linux)",
            ));
        }
    } else if cfg!(windows) {
        return Err(Error::providers_unsupported());
    } else if !cfg!(target_os = "macos") {
        return Err(Error::Guided {
            message: "xcb runs providers on macOS and Linux only".into(),
            next: None,
        });
    }
    Err(Error::guided(
        format!(
            "xcb can't run {name} {} yet; it runs only provider builds it has checked",
            pin.version
        ),
        format!("install a supported {name} build (xcb.sh/docs/providers lists them)"),
    ))
}

fn account_needs_sign_in(store: &Store, account: &xcb_runtime::store::Account) -> Result<bool> {
    Ok(store.authentication_required(&account.id)? || !auth::has_credentials(store, &account.id)?)
}

fn require_setup_sign_in(store: &Store, account: &xcb_runtime::store::Account) -> Result<()> {
    if account_needs_sign_in(store, account)? {
        return Err(Error::guided(
            format!(
                "{} needs sign-in before setup can finish",
                xcb_core::display_text(&account.name(), 80)
            ),
            health::sign_in_step(account.provider, &account.id),
        ));
    }
    Ok(())
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
        Provider::Devin => "Devin support was removed; use a Claude or Codex account",
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
            Provider::Devin => "Devin support was removed; use Claude or Codex",
            Provider::Codex => {
                "--from-native no longer reads ambient credentials; use xcb accounts import-codex --source <absolute auth.json path>, then models refresh codex --account <account>"
            }
            Provider::Claude => {
                "--from-native is unsupported for Claude; connect an account and use models refresh claude --account <account>"
            }
        }));
    }
    let Some(name) = name else {
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
    let now = now_ms();
    if as_json {
        let view = summary::snapshot(store, None, config, now)?;
        return print_json(
            json!({"version":1,"accounts":view.accounts.iter().map(|account| json!({"id":account.id,"name":account.name,"email":account.email,"provider":account.provider,"subscription":account.subscription,"remainingPercent":account.remaining_percent,"resetsAtMs":account.resets_at_ms,"quotaBlockedUntilMs":account.quota_blocked_until_ms,"runway":account.runway,"busy":account.busy,"enabled":account.enabled,"authenticationRequired":account.authentication_required})).collect::<Vec<_>>(),"estimatedPoolSeconds":view.total_runway_seconds,"measuredPools":view.runway_coverage.0,"totalPools":view.runway_coverage.1,"localOnly":true}),
        );
    }
    let loaded = health::load(store, config, now)?;
    if loaded.accounts.is_empty() {
        println!("No accounts yet.");
        ux::next("xcb setup <provider>");
        return Ok(());
    }
    // codeql[rust/cleartext-logging]: the account name is the user's own
    // provider email rendered as the account's display identity, which is
    // the documented purpose of this local status table.
    print!(
        "{}",
        health::table(&loaded.accounts, config.default_account.as_ref(), now)
    );
    if let Some((seconds, measured, pools)) = loaded.runway {
        println!(
            "Estimated use left at the current pace: ~{:.1}h ({measured} of {pools} usage pools measured; not a billing statement)",
            seconds / 3600.0,
        );
    }
    if let Some(step) = health::first_sign_in(&loaded.accounts, config.default_account.as_ref()) {
        ux::next(&step);
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

/// `xcb update enable|disable`: record the policy, then add or remove the
/// daily check where the platform has one. Turning updates off never
/// installs anything and works on every platform.
fn set_update_policy(
    root: &std::path::Path,
    policy: xcb_runtime::update::Policy,
    as_json: bool,
) -> Result<()> {
    use xcb_runtime::update::{self, Policy};
    let state = update::set_policy(root, policy)?;
    let home = xcb_core::home_dir();
    let scheduled = if policy == Policy::Disable {
        if let Some(home) = &home {
            update::remove_scheduler(home)?;
        }
        false
    } else if update::scheduler_supported() {
        let home = home.ok_or(Error::PrivateState)?;
        if cfg!(target_os = "macos") && !update::scheduler_path(&home).exists() {
            ux::login_item_notice(
                "It checks once a day for a verified xcb release, until you run xcb update disable.",
            );
        }
        update::install_scheduler(&std::env::current_exe()?)?;
        true
    } else {
        false
    };
    if as_json {
        return print_json(json!({
            "version": 1,
            "policy": state.policy,
            "enabled": state.policy != Policy::Disable,
            "scheduled": scheduled,
        }));
    }
    if policy == Policy::Disable {
        println!("xcb updates disabled");
    } else if scheduled {
        println!("xcb updates: {} · checked once a day", state.policy);
    } else {
        println!("xcb updates: {}", state.policy);
        ux::next("run xcb update daemon once a day from a user timer (systemd or cron)");
    }
    Ok(())
}

fn parse_expected_generation(value: &str) -> std::result::Result<String, &'static str> {
    xcb_runtime::application::validate_expected_generation(value)
        .map(|()| value.to_owned())
        .map_err(|_| "expected generation must be 64 lowercase hexadecimal characters")
}

fn update_reentry_exit_code(status: std::process::ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return 128 + signal;
        }
    }
    1
}

fn dispatch_update(root: &std::path::Path, command: Commands, as_json: bool) -> Result<i32> {
    match command {
        Commands::Update { command } => {
            match command.unwrap_or(UpdateCommand::Check) {
                UpdateCommand::Check => {
                    let result =
                        xcb_runtime::update::check(root, env!("CARGO_PKG_VERSION"), as_json);
                    if as_json {
                        print_json(result?)?;
                    } else {
                        result?;
                    }
                }
                UpdateCommand::Status => {
                    let state = xcb_runtime::update::load(root)?;
                    if as_json {
                        print_json(
                            json!({"version":1,"policy":state.policy,"lastCheckMs":state.last_check_ms,"availableVersion":state.available_version,"automaticInstallSupported":xcb_runtime::update::automatic_install_supported(root)}),
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
                    set_update_policy(root, policy, as_json)?;
                }
                UpdateCommand::Disable => {
                    set_update_policy(root, xcb_runtime::update::Policy::Disable, as_json)?;
                }
                UpdateCommand::Install {
                    version,
                    allow_downgrade,
                    quiet,
                } => {
                    let result = xcb_runtime::update::upgrade(
                        root,
                        env!("CARGO_PKG_VERSION"),
                        version.as_deref(),
                        quiet || as_json,
                        allow_downgrade,
                    )?;
                    if as_json {
                        print_json(result)?;
                    }
                }
                UpdateCommand::Daemon { quiet } => {
                    let checked = xcb_runtime::update::should_check(root)?;
                    let mut upgrade = None;
                    if checked {
                        let result = xcb_runtime::update::check(
                            root,
                            env!("CARGO_PKG_VERSION"),
                            quiet || as_json,
                        )?;
                        if xcb_runtime::update::load(root)?.policy
                            == xcb_runtime::update::Policy::Auto
                            && result.release_available
                        {
                            upgrade = Some(xcb_runtime::update::upgrade(
                                root,
                                env!("CARGO_PKG_VERSION"),
                                None,
                                quiet || as_json,
                                false,
                            )?);
                        }
                    }
                    if as_json {
                        print_json(json!({"version":1,"checked":checked,"upgrade":upgrade}))?;
                    }
                }
            }
            Ok(0)
        }
        Commands::Upgrade {
            version,
            allow_downgrade,
            quiet,
        } => {
            let result = xcb_runtime::update::upgrade(
                root,
                env!("CARGO_PKG_VERSION"),
                version.as_deref(),
                quiet || as_json,
                allow_downgrade,
            )?;
            if as_json {
                print_json(result)?;
            }
            Ok(0)
        }
        _ => unreachable!("only updater commands reach dispatch_update"),
    }
}

fn dispatch(cli: Cli) -> impl std::future::Future<Output = Result<i32>> {
    // Allocate the command state once. Embedding it in each caller's async
    // frame can overflow a normal 2 MiB stack before any command runs.
    Box::pin(async move {
        let mut installation = None;
        Box::pin(dispatch_inner(cli, &mut installation)).await
    })
}

async fn dispatch_inner(
    cli: Cli,
    installation: &mut Option<private::ExclusiveLock>,
) -> Result<i32> {
    let updater_command = matches!(
        &cli.command,
        Some(Commands::Update { .. } | Commands::Upgrade { .. })
    );
    process::initialize_host()?;
    if matches!(&cli.command, Some(Commands::NativeMcpStdio)) {
        *installation = xcb_runtime::update::hold_installation(std::path::Path::new("/"))?;
        return native_mcp_stdio().await;
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
        *installation = xcb_runtime::update::hold_installation(std::path::Path::new("/"))?;
        return egress_forward(socket, *port, lo_up, env_file, *target_port, child).await;
    }
    // Usage reports belong to aicharts' local record; they need no xcb state,
    // account or update check.
    if let Some(Commands::Usage { args }) = &cli.command
        && !usage::needs_state(args)
    {
        return Ok(usage::run(args, cli.json));
    }
    // The sandbox test's confined half runs inside bwrap with no state root
    // bound, exactly like the forwarder: it reads only its arguments.
    if let Some(Commands::SandboxProbe { args }) = &cli.command {
        #[cfg(unix)]
        return Ok(xcb_runtime::qualification::probe::inside(args));
        #[cfg(not(unix))]
        {
            let _ = args;
            return Err(Error::Unavailable("the sandbox test runs on Linux only"));
        }
    }
    // Project preflight uses only local arguments and Git, even without app state.
    if matches!(
        &cli.command,
        Some(Commands::Projects {
            command: Some(habitat::ProjectCommand::Preflight { .. })
        })
    ) {
        if let Some(Commands::Projects { command }) = cli.command {
            return habitat::projects(std::path::Path::new("/"), &cli.cwd, command, cli.json).await;
        }
        unreachable!("matched project preflight above");
    }
    // Update before opening any task state. A successful replacement re-enters
    // once with the original arguments; completed product work is never replayed.
    let update_root = cli
        .state
        .clone()
        .map(Ok)
        .unwrap_or_else(private::default_root)?;
    let interactive_command = matches!(
        &cli.command,
        Some(Commands::Run { .. } | Commands::Doctor { .. })
    );
    let update_executable = std::env::current_exe()?;
    if interactive_command
        && xcb_runtime::update::automatic_allowed(
            io::stdin().is_terminal() && io::stderr().is_terminal(),
            cli.json,
        )
        && xcb_runtime::update::automatic(&update_root, env!("CARGO_PKG_VERSION"))?
            .is_some_and(|result| result.changed)
    {
        let status = std::process::Command::new(&update_executable)
            .args(std::env::args_os().skip(1))
            .env("XCB_UPDATE_REENTRY", "1")
            .status()?;
        return Ok(update_reentry_exit_code(status));
    }
    *installation = if updater_command {
        None
    } else {
        xcb_runtime::update::hold_installation(&update_root)?
    };
    let root = cli.state.unwrap_or(private::default_root()?);
    if updater_command {
        return dispatch_update(&root, cli.command.expect("an updater command"), cli.json);
    }
    if matches!(&cli.command, Some(Commands::ManagedDaemon)) {
        return xcb_runtime::managed::daemon(root).await;
    }
    if matches!(&cli.command, Some(Commands::ServiceRun)) {
        return xcb_runtime::service_watchdog::run(root).await;
    }
    if let Some(Commands::Resources { workspace, command }) = cli.command {
        return resources::dispatch(&root, workspace, command, cli.json).await;
    }
    if let Some(Commands::Generate { capabilities }) = &cli.command {
        return application::dispatch(&root, *capabilities, cli.json).await;
    }
    if matches!(&cli.command, Some(Commands::Route)) {
        return route::dispatch(&root, cli.json).await;
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
    // The upgrade preview reads the managed store only through a private
    // copy, so it opens no store here and works beside an old supervisor.
    if let Some(Commands::Doctor {
        upgrade_plan: true, ..
    }) = &cli.command
    {
        return workspaces::upgrade_plan(&root, cli.json);
    }
    // The sandbox test writes only its receipt, so it opens no store.
    if let Some(Commands::Doctor {
        qualify_sandbox: true,
        provider,
        ..
    }) = &cli.command
    {
        return doctor::qualify_sandbox(&root, *provider, cli.json).await;
    }
    let store = Arc::new(Store::open(&root)?);
    // Refresh provider build pins before agent-run commands. The local
    // protocol remains useful offline when refresh cannot complete.
    if matches!(
        &cli.command,
        Some(Commands::Run { .. })
            | Some(Commands::Doctor {
                executable: None,
                ..
            })
    ) {
        let home = root.join("metadata-home");
        let catalog_root = root.clone();
        let _ =
            tokio::task::spawn_blocking(move || xcb_runtime::catalog::refresh(&catalog_root)).await;
        for provider in Provider::SUPPORTED {
            let report = process::refresh_provider(&root, provider, None, &home).await;
            match (report.outcome, report.detail) {
                (process::RefreshOutcome::Adopted, _) => {
                    eprintln!("xcb: {provider}: adopted the updated build");
                }
                (
                    process::RefreshOutcome::PendingCatalog | process::RefreshOutcome::Rejected,
                    Some(detail),
                ) => {
                    eprintln!("xcb: {provider}: {detail}");
                }
                _ => {}
            }
        }
    }
    let (mut config, _) = Config::load(store.root())?;
    match cli.command {
        None => Err(Error::Unavailable(
            "interactive terminal removed; use xcb run --json or the SDK",
        )),
        Some(
            Commands::Generate { .. }
            | Commands::ApplicationDiagnostic { .. }
            | Commands::QualifyApplication { .. }
            | Commands::ManagedDaemon
            | Commands::ServiceRun
            | Commands::Resources { .. }
            | Commands::Route,
        ) => unreachable!("early dispatch returns above"),
        Some(Commands::Run {
            signed_in_browser,
            desktop,
            prompt,
            account,
            model,
            images,
        }) => {
            Box::pin(async move {
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
                let (cancel, cancelled) = watch::channel(false);
                // Install both handlers before routing, which can start provider
                // work, and before any provider starts. SIGTERM must use the same
                // independent join/custody path as interactive Ctrl-C.
                let mut stop = stop::Stop::install()?;
                let _interrupt = AbortOnDrop(tokio::spawn(async move {
                    stop.recv().await;
                    let _ = cancel.send(true);
                }));
                let stopped_early =
                    || Error::Unavailable("cancelled before the task started; no provider ran");
                let attachments = images
                    .iter()
                    .map(|path| xcb_runtime::attachments::from_path(store.root(), path))
                    .collect::<Result<Vec<_>>>()?;
                let mut account = account
                    .map(|name| store.resolve_account(&name).map(|account| account.id))
                    .transpose()?;
                let mut route_pins = xcb_core::session::RoutePins {
                    provider: None,
                    account: account.clone(),
                    model: model.clone().filter(|model| model != "auto"),
                };
                let workspace = xcb_core::canonical(&cli.cwd)?;
                let mut config = config.clone();
                config
                    .native_execution
                    .scopes
                    .retain(|scope| scope.workspace == workspace);
                let mut requirements = xcb_core::session::TaskRequirements {
                    signed_in_browser,
                    desktop,
                    native_execution: !config.native_execution.is_empty(),
                    ..Default::default()
                };
                let managed = xcb_runtime::managed::ManagedStore::open(store.root())?;
                let (preference, required) =
                    managed.initial_route_preferences(&workspace, &prompt)?;
                let (preferred_provider, required_provider) =
                    preview_provider_preferences(preference, required, None)?;
                route_pins.provider = required_provider;
                // Explicit models retain their alias resolution, judge bypass,
                // and provider qualification path. Automatic routes use the judge.
                let model = if route_pins.model.is_some() {
                    model
                } else {
                    let excluded_routes = std::collections::BTreeSet::new();
                    let excluded_accounts = std::collections::BTreeSet::new();
                    let decision = xcb_runtime::routing::smart_route(
                        &store,
                        &config,
                        xcb_runtime::routing::RouteRequest {
                            requirements,
                            task: &prompt,
                            required_provider,
                            preferred_provider,
                            required_model: route_pins.model.as_deref(),
                            excluded_routes: &excluded_routes,
                            excluded_accounts: &excluded_accounts,
                            account: account.as_ref(),
                        },
                    )
                    .await?;
                    requirements = requirements.merge(decision.requirements);
                    eprintln!("xcb: {}", automatic_route_notice(&decision.reason));
                    account = Some(decision.account);
                    Some(decision.model.key())
                };
                if *cancelled.borrow() {
                    return Err(stopped_early());
                }
                let session = kernel::new_session_with_policy(
                    &store,
                    &xcb_core::canonical(&cli.cwd)?,
                    &config,
                    account.as_ref(),
                    model.as_deref(),
                    None,
                    kernel::SessionRoutePolicy {
                        requirements,
                        required_provider,
                    },
                )?;
                if route_pins.model.is_some() {
                    route_pins.model = Some(session.model.key());
                }
                store.require_session_capabilities(&session.id, requirements)?;
                store.set_session_route_pins(&session.id, route_pins)?;
                if *cancelled.borrow() {
                    return Err(stopped_early());
                }
                let observer: Observer = Arc::new(|event| {
                    if let Progress::Notice(message) = event {
                        eprintln!("xcb: {message}");
                    }
                });
                let result = kernel::execute(
                    store.clone(),
                    session.id.clone(),
                    prompt,
                    attachments,
                    false,
                    cancelled,
                    observer,
                )
                .await;
                let result = result?;
                if cli.json {
                    print_json(run_output(&session.id, &result))?;
                } else if xcb_core::policy::no_reply(&result.text, &result.facts) {
                    // Failover can move the session to another provider.
                    let provider = store
                        .session(&session.id)?
                        .map_or(session.model.provider, |current| current.model.provider);
                    return Err(no_reply_error(provider, &session.id));
                } else {
                    println!("{}", result.text);
                }
                Ok(run_exit_code(&result))
            })
            .await
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
                        let (added, next) = public.added_message();
                        println!("{} {added}", ux::Style::stdout().symbol(ux::Symbol::Ok));
                        if interactive_account_add(cli.json, provider, terminal_available()) {
                            let retry = format!("xcb setup {provider} --account {}", account.id);
                            finish_account_setup(
                                &store,
                                provider,
                                Some(account),
                                stop::Stop::install()?,
                            )
                            .await
                            .map_err(|error| Error::guided(error.to_string(), retry))?;
                        } else {
                            ux::next(&next);
                        }
                    }
                }
                Some(AccountCommand::Login { account, browser }) => {
                    let mut stop = stop::Stop::install()?;
                    let account = store.resolve_account(&account)?;
                    if browser && account.provider != Provider::Claude {
                        return Err(Error::Unavailable(
                            "--browser is only supported for Claude sign-in",
                        ));
                    }
                    let browser = browser
                        || (account.provider == Provider::Claude
                            && auth::has_claude_browser_credentials(&store, &account.id)?);
                    if browser {
                        claude_sign_in::check_login_context(&account.id, true, cli.json)?;
                    }
                    let pin = stop
                        .settle(ensure_pin(store.root(), account.provider))
                        .await?;
                    let (cancel, receiver) = tokio::sync::watch::channel(false);
                    let signal_cancel = cancel.clone();
                    let _interrupt = AbortOnDrop(tokio::spawn(async move {
                        stop.recv().await;
                        let _ = signal_cancel.send(true);
                    }));
                    match account.provider {
                        Provider::Claude => {
                            claude_sign_in::login(
                                &store,
                                &account.id,
                                &pin,
                                browser,
                                cli.json,
                                cancel,
                                receiver,
                            )
                            .await?;
                        }
                        Provider::Codex => {
                            if terminal_available() && !cli.json {
                                eprintln!(
                                    "{} Codex will show a sign-in code. Press Enter when you're ready to open its sign-in page.",
                                    ux::Style::stderr().symbol(ux::Symbol::Next)
                                );
                                let (sender, prompts) = tokio::sync::mpsc::channel(1);
                                let login = runner::login_codex_with_prompt_and_cancel(
                                    &store,
                                    &account.id,
                                    &pin,
                                    sender,
                                    receiver,
                                );
                                let assistance = device_sign_in::serve(prompts);
                                tokio::pin!(login, assistance);
                                tokio::select! {
                                    result = &mut login => result?,
                                    _ = &mut assistance => login.await?,
                                }
                            } else {
                                eprintln!(
                                    "{} Codex will print a sign-in page and a code. Open the page and enter the code to connect this account to xcb.",
                                    ux::Style::stderr().symbol(ux::Symbol::Next)
                                );
                                runner::login_codex_with_cancel(
                                    &store,
                                    &account.id,
                                    &pin,
                                    receiver,
                                )
                                .await?;
                            }
                        }
                        Provider::Devin => {
                            return Err(Error::Unavailable(
                                "Devin support was removed; use a Claude or Codex account",
                            ));
                        }
                    }
                    if cli.json {
                        print_json(json!({"version":1,"account":account.id,"stored":true}))?;
                    } else {
                        let account = store.account(&account.id)?;
                        // codeql[rust/cleartext-logging]: the account name is
                        // the user's own provider email, intentionally shown as
                        // the account's display identity after sign-in.
                        println!(
                            "{} Signed in to {}.",
                            ux::Style::stdout().symbol(ux::Symbol::Ok),
                            account.name()
                        );
                        ux::next(&format!("xcb accounts refresh {}", account.id));
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
                        Provider::Devin => {
                            return Err(Error::Unavailable(
                                "Devin support was removed; use a Claude or Codex account",
                            ));
                        }
                        Provider::Codex => {
                            return Err(Error::Unavailable(
                                "Codex uses ChatGPT sign-in; use xcb accounts login <account> or xcb accounts import-codex --source <absolute auth.json path>",
                            ));
                        }
                    };
                    let bytes = zeroize::Zeroizing::new(stdin(limit)?);
                    match account.provider {
                        Provider::Claude => auth::store_token(&store, &account.id, &bytes)?,
                        Provider::Devin => unreachable!("retired Devin support was rejected above"),
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
                    let pin = ensure_pin(store.root(), account.provider).await?;
                    require_supported(store.root(), &pin)?;
                    let models = runner::probe(&store, &pin, Some(&account.id)).await?;
                    store.set_account_models(&account.id, &models)?;
                    accounts(&store, &config, cli.json)?;
                }
                Some(AccountCommand::Remove { account, force }) => {
                    let account = store.resolve_account(&account)?;
                    let (config_before, _) = Config::load(store.root())?;
                    let was_default = config_before.default_account.as_ref() == Some(&account.id);
                    if was_default && !force {
                        return Err(Error::guided(
                            "cannot remove the default account without --force",
                            format!("xcb accounts remove {} --force", account.id),
                        ));
                    }
                    let removed = store.remove_account(&account.id)?;
                    let mut default_cleared = false;
                    if was_default && force {
                        // Re-read after removal so a concurrent config update is
                        // never overwritten while clearing the removed id.
                        let (mut current, revision) = Config::load(store.root())?;
                        if current.default_account.as_ref() == Some(&removed.id) {
                            current.default_account = None;
                            current.save(store.root(), revision.as_deref())?;
                            default_cleared = true;
                        }
                    }
                    if cli.json {
                        print_json(json!({
                            "version": 1,
                            "account": &removed.id,
                            "provider": removed.provider,
                            "name": removed.name(),
                            "removed": ["account_record", "stored_credentials"],
                            "defaultCleared": default_cleared,
                        }))?;
                    } else {
                        println!(
                            "Removed account {} ({}) and its stored credentials.",
                            xcb_core::display_text(&removed.name(), 80),
                            removed.id
                        );
                        if default_cleared {
                            println!("Default account cleared.");
                        }
                    }
                }
                Some(AccountCommand::ImportCodex { source, account }) => {
                    let updating = account.is_some();
                    let id = if let Some(account) = account {
                        let id = store.resolve_account(&account)?.id;
                        auth::import_codex_auth(&store, &id, &source)?;
                        id
                    } else {
                        auth::import_codex_account(&store, &source)?
                    };
                    if cli.json {
                        print_json(import_acknowledgement(&id))?;
                    } else if updating {
                        println!("Updated the Codex sign-in for {id}.");
                    } else {
                        println!(
                            "Imported one Codex account as {id}. Original state and sessions are unchanged."
                        );
                    }
                }
            }
            Ok(0)
        }
        // Setup can recursively dispatch login, so it needs a separate frame.
        Some(Commands::Setup {
            provider,
            plan,
            new,
            account,
        }) => {
            Box::pin(async move {
                if cli.json {
                    return Err(Error::guided(
                        "xcb setup has no --json output",
                        format!("xcb --json accounts add {provider}"),
                    ));
                }
                let stop = stop::Stop::install()?;
                let accounts: Vec<_> = store
                    .accounts()?
                    .into_iter()
                    .filter(|account| account.provider == provider)
                    .collect();
                let selected = if let Some(selector) = account {
                    let selected = store.resolve_account(&selector)?;
                    if selected.provider != provider {
                        return Err(Error::guided(
                            "The account uses a different provider",
                            format!("xcb setup {} --account {}", selected.provider, selected.id),
                        ));
                    }
                    Some(selected)
                } else if new {
                    None
                } else if !accounts.is_empty() && terminal_available() {
                    match choose_setup_account(&accounts, provider).await? {
                        SetupChoice::New => None,
                        SetupChoice::Existing(index) => Some(accounts[index].clone()),
                        SetupChoice::Cancel => return Ok(0),
                    }
                } else {
                    // Outside a terminal preserve deterministic reuse of a healthy enabled account.
                    accounts
                        .iter()
                        .find(|account| {
                            account.enabled
                                && !account_needs_sign_in(&store, account).unwrap_or(true)
                        })
                        .or_else(|| accounts.iter().find(|account| account.enabled))
                        .cloned()
                };
                if let Some(account) = &selected {
                    if !account.enabled {
                        return Err(Error::guided(
                            "This account is turned off",
                            format!("xcb accounts enable {}", account.id),
                        ));
                    }
                    let identity = if terminal_available() {
                        String::new()
                    } else {
                        format!(" · {}", account.id)
                    };
                    println!(
                        "{} Using {} ({provider}){identity}",
                        ux::Style::stdout().symbol(ux::Symbol::Ok),
                        xcb_core::display_text(&account.name(), 80)
                    );
                } else if !new
                    && !terminal_available()
                    && !accounts.is_empty()
                    && accounts.iter().all(|account| !account.enabled)
                {
                    return Err(Error::guided(
                        format!(
                            "Your {} account {} is turned off",
                            provider_name(provider),
                            xcb_core::display_text(&accounts[0].name(), 80)
                        ),
                        format!("xcb accounts enable {}", accounts[0].id),
                    ));
                }
                let selected = match selected {
                    Some(account) => Some(account),
                    None => Some(add_setup_account(&store, provider, &plan)?),
                };
                let retry = selected
                    .as_ref()
                    .map(|account| format!("xcb setup {provider} --account {}", account.id));
                finish_account_setup(&store, provider, selected, stop)
                    .await
                    .map_err(|error| match retry {
                        Some(command) => Error::guided(error.to_string(), command),
                        None => error,
                    })
            })
            .await
        }
        Some(Commands::Doctor {
            provider,
            executable,
            upgrade_plan: _,
            qualify_sandbox: _,
        }) => {
            doctor::run(
                &root,
                &store,
                &config,
                provider,
                executable.as_deref(),
                cli.json,
            )
            .await
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
                    let pin = ensure_pin(store.root(), provider).await?;
                    require_supported(store.root(), &pin)?;
                    let models = runner::probe(&store, &pin, account.as_ref()).await?;
                    match &account {
                        Some(account) => store.set_account_models(account, &models)?,
                        None => store.set_models(provider, &models)?,
                    }
                }
                Some(ModelCommand::Route { task, provider }) => {
                    let workspace = xcb_core::canonical(&cli.cwd)?;
                    let managed = xcb_runtime::managed::ManagedStore::open(store.root())?;
                    let placement = route_workspace_preview(&managed, &workspace, &task)?;
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
                            requirements: Default::default(),
                            task: &task,
                            required_provider,
                            preferred_provider,
                            required_model: None,
                            excluded_routes: &excluded_routes,
                            excluded_accounts: &excluded_accounts,
                            account: None,
                        },
                    )
                    .await?;
                    if cli.json {
                        // Additive: the route record keeps every key.
                        let mut value = serde_json::to_value(&decision)?;
                        if let (Some(object), Some((path, source))) =
                            (value.as_object_mut(), placement_parts(&placement))
                        {
                            object.insert("workspace".into(), json!(path));
                            object.insert("workspaceSource".into(), json!(source));
                        }
                        print_json(value)?;
                    } else {
                        println!(
                            "{} · {}  {}",
                            decision.model.key(),
                            decision.account,
                            xcb_core::display_text(&decision.reason, 4096)
                        );
                        println!("{}", placement_line(&placement));
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
            let catalog = store.model_catalog()?;
            let mut choices = catalog.union();
            sort_choices(&mut choices, &Config::load(store.root())?.0.favorites);
            if cli.json {
                // Additive: each row keeps every model field and adds the ids
                // of the accounts that can use it.
                let rows = choices
                    .iter()
                    .map(|choice| {
                        let mut value = serde_json::to_value(choice)?;
                        if let Some(object) = value.as_object_mut() {
                            object.insert(
                                "accounts".into(),
                                json!(catalog.accounts_offering(choice)),
                            );
                        }
                        Ok(value)
                    })
                    .collect::<Result<Vec<_>>>()?;
                print_json(rows)?;
            } else {
                let accounts = store.accounts()?;
                println!("  {} LABEL · MODE", cell("MODEL", 56));
                // choose_model defaults each provider to its first row in this
                // ordering, so mark those rows.
                let mut defaulted = std::collections::BTreeSet::new();
                for choice in choices {
                    // Name the accounts only when some account of this
                    // provider cannot use the model.
                    let offering = catalog.accounts_offering(&choice);
                    let of_provider = accounts
                        .iter()
                        .filter(|account| account.provider == choice.provider)
                        .count();
                    let only = if offering.len() < of_provider {
                        format!(
                            " · only {}",
                            accounts
                                .iter()
                                .filter(|account| offering.contains(&account.id))
                                .map(|account| xcb_core::display_text(&account.name(), 40))
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    } else {
                        String::new()
                    };
                    println!(
                        "{} {} {} · {:?}{only}",
                        if defaulted.insert(choice.provider) {
                            "*"
                        } else {
                            " "
                        },
                        cell(&choice.key(), 56),
                        table::fit(&choice.label, 56),
                        choice.mode
                    );
                }
                println!(
                    "* preferred route per provider (xcb models default <key> changes it); automatic routing may select a stronger model"
                );
            }
            Ok(0)
        }
        Some(Commands::Reflex { command }) => {
            use xcb_core::reflex::Reflex;
            use xcb_runtime::reflex::{self, ReflexStore};
            let reflexes = ReflexStore::open(store.root())?;
            let settings = Config::load(store.root())?.0.extensions.reflexes;
            match command.unwrap_or(ReflexCommand::Status { reflex: None }) {
                ReflexCommand::Status { reflex: only } => {
                    let rows = Reflex::ALL
                        .into_iter()
                        .filter(|reflex| only.is_none_or(|only| Reflex::from(only) == *reflex))
                        .map(|reflex| {
                            reflexes.status(reflex, settings.mode(reflex), settings.learn)
                        })
                        .collect::<xcb_runtime::Result<Vec<_>>>()?;
                    if cli.json {
                        print_json(rows)?;
                    } else {
                        for status in rows {
                            println!(
                                "{} · {:?} · generation {}{} · {} observed · {} labeled · program {}{}",
                                status.reflex.as_str(),
                                status.mode,
                                status.version,
                                status
                                    .params
                                    .parent
                                    .map(|parent| format!(" (from {parent})"))
                                    .unwrap_or_default(),
                                status.observations,
                                status.labeled,
                                &status.program[..status.program.len().min(19)],
                                if status.custom_program {
                                    " (custom)"
                                } else {
                                    ""
                                },
                            );
                            if let Some(fault) = status.program_fault {
                                println!("  ! {fault}");
                            }
                            for (head, row) in status.heads {
                                println!(
                                    "  {head}: {} labels ({} positive) · live {}",
                                    row.labeled,
                                    row.positives,
                                    row.live
                                        .as_ref()
                                        .map(metrics_line)
                                        .unwrap_or_else(|| "none since activation".into()),
                                );
                                if let Some(trial) = row.trial {
                                    println!("    challenger {}", trial.reason);
                                }
                                if let Some(certificate) = &row.certificate {
                                    println!("    {}", certificate_line(certificate));
                                }
                            }
                        }
                    }
                }
                ReflexCommand::Train { reflex } => {
                    let options = xcb_core::reflex::FitOptions::default();
                    let mut report = reflexes.train(reflex.into(), options)?;
                    report.certificates = reflexes.certify(reflex.into(), options)?;
                    if cli.json {
                        print_json(report)?;
                    } else {
                        for (head, comparison) in &report.heads {
                            println!("{head}: {}", comparison.reason);
                        }
                        for head in &report.started {
                            println!("{head}: fitted a challenger; it trials on the next labels");
                        }
                        for (head, certificate) in &report.certificates {
                            println!("{head}: {}", certificate_line(certificate));
                        }
                        match report.promoted_version {
                            Some(version) => println!(
                                "adopted generation {version} (from {}); `xcb reflex rollback {} {}` restores it",
                                report.from_version,
                                report.reflex.as_str(),
                                report.from_version
                            ),
                            None => println!(
                                "kept generation {} ({} labels)",
                                report.from_version, report.labeled
                            ),
                        }
                    }
                }
                ReflexCommand::Label {
                    reflex: name,
                    task,
                    label,
                } => {
                    let reflex = Reflex::from(name);
                    let value = reflex::parse_label(reflex, &label).ok_or(Error::Unavailable(
                        match reflex {
                            Reflex::Route => "route labels are frontier or standard",
                            Reflex::Settle => "settle labels are unfinished, confirm or done",
                        },
                    ))?;
                    if reflexes.latest(reflex, task.as_str())?.is_none() {
                        return Err(Error::Unavailable("no decision recorded for that task"));
                    }
                    let mut changed = false;
                    for (head, value) in value {
                        changed |= reflexes.label(
                            reflex,
                            task.as_str(),
                            *head,
                            *value,
                            1.0,
                            "explicit",
                        )?;
                    }
                    if changed {
                        println!("labeled {task} {label} for {}", reflex.as_str());
                    } else {
                        println!("{task} was already labeled {label} for {}", reflex.as_str());
                    }
                }
                ReflexCommand::Rollback { reflex, version } => {
                    reflexes.rollback(reflex.into(), version)?;
                    println!(
                        "{} now uses generation {version}",
                        Reflex::from(reflex).as_str()
                    );
                }
                ReflexCommand::Import {
                    reflex: name,
                    file,
                    dry_run,
                } => {
                    let reflex = Reflex::from(name);
                    let source = std::fs::read_to_string(&file)?;
                    let rows = reflex::parse_import(reflex, &source)?;
                    let active = reflexes.active(reflex)?;
                    let replays = reflex::replay_import(&active, &rows)?;
                    let certificates = reflex::certify_import(reflex, &rows)?;
                    if cli.json && dry_run {
                        print_json(serde_json::json!({
                            "replays": replays,
                            "certificates": certificates,
                        }))?;
                        return Ok(0);
                    }
                    if !cli.json {
                        for (head, replay) in &replays {
                            println!(
                                "{head}: replayed {} · {} trial{}, {} adopted",
                                metrics_line(&replay.prequential),
                                replay.trials,
                                if replay.trials == 1 { "" } else { "s" },
                                replay.promotions,
                            );
                        }
                        for (head, certificate) in &certificates {
                            println!(
                                "{head}: this history alone {}",
                                certificate_line(certificate)
                            );
                        }
                    }
                    if dry_run {
                        println!("dry run: nothing stored");
                        return Ok(0);
                    }
                    let inserted = reflexes.import(reflex, &rows)?;
                    // Adopt only for new history: re-importing the same file
                    // must not append another generation.
                    let won = replays
                        .into_iter()
                        .filter(|(_, replay)| inserted > 0 && replay.promotions > 0)
                        .filter_map(|(head, replay)| Some((head, (replay.head, replay.evidence?))))
                        .collect();
                    let adopted = reflexes.adopt(reflex, won, inserted)?;
                    let certificates =
                        reflexes.certify(reflex, xcb_core::reflex::FitOptions::default())?;
                    if cli.json {
                        print_json(serde_json::json!({
                            "inserted": inserted,
                            "examples": rows.len(),
                            "adopted_version": adopted,
                            "certificates": certificates,
                        }))?;
                    } else {
                        println!("imported {inserted} of {} examples", rows.len());
                        match adopted {
                            Some(version) => println!(
                                "adopted the heads that won the replay as generation {version}; `xcb reflex rollback {} {}` restores the previous one",
                                reflex.as_str(),
                                active.version
                            ),
                            None => println!(
                                "no head won a replayed trial; the active generation is unchanged"
                            ),
                        }
                        for (head, certificate) in &certificates {
                            println!("{head}: {}", certificate_line(certificate));
                        }
                    }
                }
                ReflexCommand::Check { file } => {
                    let source: serde_json::Value = serde_json::from_slice(&std::fs::read(&file)?)?;
                    let (_, digest) = reflex::admit(&source)?;
                    println!("valid reflex program {digest}");
                }
            }
            Ok(0)
        }
        Some(Commands::Offers { refresh }) => {
            let mut state = if refresh {
                xcb_runtime::offers::refresh(store.root())?
            } else {
                xcb_runtime::offers::load(store.root())?
            };
            state
                .offers
                .retain(|offer| Provider::SUPPORTED.contains(&offer.provider));
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
                Some(SessionCommand::Discover { hours, provider }) => {
                    let report = discover_provider_sessions(provider.as_deref(), hours)?;
                    if cli.json {
                        print_json(&report)?;
                    } else {
                        println!(
                            "{} recent conversation(s) in the last {hours} hours.",
                            report.sessions.len()
                        );
                        for session in &report.sessions {
                            println!(
                                "{}  {}  {}  {} message(s){}\n  {}",
                                session.id,
                                session.provider,
                                human_age(now_ms(), session.last_active_at_ms),
                                session.message_count,
                                if session.truncated {
                                    " (partial history)"
                                } else {
                                    ""
                                },
                                table::fit(&session.workspace, 160)
                            );
                        }
                        if report.skipped_files > 0 || report.truncated {
                            println!(
                                "Skipped {} unreadable, unsupported, or excluded file(s).{}",
                                report.skipped_files,
                                if report.truncated {
                                    " Scan limit reached; the newest files were read first."
                                } else {
                                    ""
                                }
                            );
                        }
                        println!(
                            "Recent activity does not establish whether a provider process is running."
                        );
                        ux::next("xcb sessions import --recent");
                    }
                }
                Some(SessionCommand::Import {
                    id,
                    recent: _,
                    workspace,
                    hours,
                    provider,
                }) => {
                    let mut report = discover_provider_sessions(provider.as_deref(), hours)?;
                    if let Some(id) = id {
                        report.sessions.retain(|session| session.id == id);
                        if report.sessions.is_empty() {
                            return Err(Error::Unavailable(
                                "session candidate not found; run xcb sessions discover with the same --hours and --provider options",
                            ));
                        }
                    }
                    let managed = xcb_runtime::managed::ManagedStore::open(store.root())?;
                    let workspace = workspace.map(|path| cli.cwd.join(path));
                    let mut results = Vec::new();
                    let mut failed = Vec::new();
                    for session in &report.sessions {
                        match managed
                            .import_session_into(session, workspace.as_deref())
                            .await
                        {
                            Ok(result) => results.push(result),
                            Err(error) => {
                                failed.push(json!({"id":session.id,"reason":error.to_string()}))
                            }
                        }
                    }
                    let created = results.iter().filter(|result| result.created).count();
                    let updated = results
                        .iter()
                        .filter(|result| !result.created && result.added_messages > 0)
                        .count();
                    let unchanged = results.len() - created - updated;
                    let added_messages: usize =
                        results.iter().map(|result| result.added_messages).sum();
                    if cli.json {
                        print_json(
                            json!({"version":1,"created":created,"updated":updated,"unchanged":unchanged,"added_messages":added_messages,"skipped_files":report.skipped_files,"truncated":report.truncated,"source_preserved":true,"tasks_started":0,"results":results,"failed":failed}),
                        )?;
                    } else {
                        println!(
                            "Imported {created} new conversation(s), updated {updated}, unchanged {unchanged}; added {added_messages} message(s)."
                        );
                        println!("Original files are unchanged. No tasks were started.");
                        for result in &results {
                            println!("xcb history {}", result.conversation);
                        }
                        for failure in &failed {
                            println!(
                                "Skipped {}: {}",
                                failure["id"].as_str().unwrap_or("session"),
                                failure["reason"].as_str().unwrap_or("import failed")
                            );
                        }
                        if report.skipped_files > 0 || report.truncated {
                            println!(
                                "Discovery skipped {} file(s).{}",
                                report.skipped_files,
                                if report.truncated {
                                    " Scan limit reached."
                                } else {
                                    ""
                                }
                            );
                        }
                    }
                    if !failed.is_empty() {
                        return Ok(1);
                    }
                }
                None => {
                    let sessions = store.sessions(64)?;
                    if cli.json {
                        print_json(sessions)?;
                    } else if sessions.is_empty() {
                        println!("No provider sessions yet.");
                        ux::next("xcb");
                    } else {
                        println!(
                            "{}  {} {} {} {} TITLE",
                            cell("SESSION ID", 34),
                            cell("PROVIDER", 8),
                            cell("MODEL", 24),
                            cell("STATE", 15),
                            cell("ACTIVE", 9)
                        );
                        let now = now_ms();
                        for session in sessions {
                            println!(
                                "{}  {} {} {} {} {}",
                                cell(session.id.as_str(), 34),
                                cell(session.model.provider.as_str(), 8),
                                cell(&session.model.label, 24),
                                cell(session.state.label(), 15),
                                cell(&human_age(now, session.last_active_at_ms), 9),
                                table::fit(&session.title, 60)
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
                            "Exported local aicharts session observations to {}",
                            path.display()
                        );
                    }
                }
                Some(SessionCommand::Rm { id, yes }) => {
                    if !yes {
                        let found = store.session(&id)?.is_some();
                        if cli.json {
                            print_json(
                                json!({"version":1,"applied":false,"session":id,"found":found}),
                            )?;
                        } else if found {
                            println!(
                                "Would remove {id} and its transcript. Repeat with --yes to apply."
                            );
                        } else {
                            println!("Session {id} not found.");
                        }
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
                            "{} {count} idle session(s) older than {days} days. Active or unfinished sessions are skipped.{}",
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
        Some(Commands::Backlog {
            command,
            conversation,
            workspace,
        }) => {
            habitat::backlog(
                store.root(),
                &cli.cwd,
                command,
                conversation.as_ref(),
                workspace.as_deref(),
                cli.json,
            )
            .await
        }
        Some(Commands::Steer { task, text, id }) => {
            habitat::steer(store.root(), &task, id, text, cli.json)
        }
        Some(Commands::Watch { target, source, id }) => {
            habitat::watch(store.root(), &target, &source, id, cli.json)
        }
        Some(Commands::Inbox {
            task,
            conversation,
            before,
            limit,
        }) => habitat::inbox(
            store.root(),
            task.as_ref(),
            conversation.as_ref(),
            before,
            usize::from(limit),
            cli.json,
        ),
        Some(Commands::Schedules {
            command,
            conversation,
            workspace,
            enabled,
            disabled,
            due,
        }) => {
            habitat::schedules(
                store.root(),
                &cli.cwd,
                command,
                conversation.as_ref(),
                habitat::ScheduleFilters {
                    workspace: workspace
                        .map(|dir| {
                            xcb_core::canonical(cli.cwd.join(dir))
                                .map(|path| path.display().to_string())
                        })
                        .transpose()?,
                    enabled: if enabled {
                        Some(true)
                    } else if disabled {
                        Some(false)
                    } else {
                        None
                    },
                    due,
                },
                cli.json,
            )
            .await
        }
        Some(Commands::Context { command }) => context::run(store.root(), &cli.cwd, command).await,
        Some(Commands::Daemons { command }) => {
            habitat::daemons(store.root(), &cli.cwd, command, cli.json).await
        }
        Some(Commands::Attention) => habitat::attention(store.root(), cli.json),
        Some(Commands::Projects { command }) => {
            habitat::projects(store.root(), &cli.cwd, command, cli.json).await
        }
        Some(Commands::Memory { command }) => {
            habitat::memory(store.root(), &cli.cwd, command, cli.json).await
        }
        Some(Commands::Service { command }) => {
            let home = xcb_core::home_dir().ok_or(Error::PrivateState)?;
            let executable = std::env::current_exe()?;
            if matches!(command, Some(ServiceCommand::Plan)) {
                let plan =
                    xcb_runtime::habitat_service::Service::plan(store.root(), &executable, &home)?;
                if cli.json {
                    print_json(&plan)?;
                } else {
                    print!("{}", plan.render()?);
                }
                return Ok(0);
            }
            let status = match command {
                Some(ServiceCommand::Install) => {
                    let loaded = xcb_runtime::habitat_service::status(store.root(), &home)
                        .is_ok_and(|status| status.registered);
                    if cfg!(target_os = "macos") && !loaded {
                        ux::login_item_notice(
                            "It resumes your conversations' background work after you log in, until you run xcb service uninstall.",
                        );
                    }
                    let status =
                        xcb_runtime::habitat_service::install(store.root(), &executable, &home)?;
                    if xcb_runtime::habitat_service::stops_at_logout() == Some(true) {
                        eprintln!("{}", linger_notice());
                    }
                    status
                }
                Some(ServiceCommand::Uninstall) => {
                    xcb_runtime::habitat_service::uninstall(store.root(), &home)?
                }
                _ => xcb_runtime::habitat_service::status(store.root(), &home)?,
            };
            if cli.json {
                print_json(status)?;
            } else {
                print!("{}", service_text(&status, ux::Style::stdout()));
                if !status.installed {
                    ux::next("xcb service install");
                } else if status.log.is_none() {
                    ux::next("xcb service uninstall, then xcb service install (turns on the log)");
                } else if !status.registered {
                    ux::next("xcb service install");
                }
            }
            Ok(0)
        }
        Some(Commands::History {
            id,
            direct,
            before,
            limit,
        }) => {
            let page = if direct {
                store.transcript_page(&id, before, usize::from(limit))?
            } else {
                xcb_runtime::managed::ManagedStore::open(store.root())?.transcript_page(
                    &id,
                    before,
                    usize::from(limit),
                )?
            };
            if cli.json {
                print_json(page)?;
            } else {
                for message in page.messages {
                    println!(
                        "{:?}\n{}\n",
                        message.role,
                        xcb_core::display_text(&message.text, xcb_core::MAX_TEXT_BYTES)
                    );
                }
                if page.has_older
                    && let Some(before) = page.first_sequence
                {
                    println!(
                        "Older messages: xcb history {id} --before {before}{}",
                        if direct { " --direct" } else { "" }
                    );
                }
            }
            Ok(0)
        }
        Some(Commands::Rename {
            id,
            title,
            expected_title,
            direct,
        }) => {
            let (renamed, new_title) = if direct {
                let session = store.rename_session(&id, &expected_title, &title)?;
                let title = session.title.clone();
                if cli.json {
                    print_json(session)?;
                }
                (id, title)
            } else {
                let managed = xcb_runtime::managed::ManagedStore::open(store.root())?;
                let conversation = managed.rename_conversation(
                    &managed.resolve_conversation(&id)?,
                    &expected_title,
                    &title,
                )?;
                let renamed = (conversation.id.clone(), conversation.title.clone());
                if cli.json {
                    print_json(conversation)?;
                }
                renamed
            };
            if !cli.json {
                println!(
                    "{} Renamed {renamed} to \u{201c}{}\u{201d}",
                    ux::Style::stdout().symbol(ux::Symbol::Ok),
                    table::fit(&new_title, 160)
                );
            }
            Ok(0)
        }
        Some(Commands::Conversations { new }) => {
            let managed = xcb_runtime::managed::ManagedStore::open(store.root())?;
            let conversations = if new {
                vec![managed.create_conversation(&cli.cwd).await?]
            } else {
                listed_conversations(&managed)?
            };
            if cli.json {
                print_json(conversation_rows(&conversations)?)?;
            } else if conversations.is_empty() {
                println!("No managed conversations.");
            } else {
                let counts = managed.message_counts()?;
                for conversation in conversations {
                    let messages = counts.get(&conversation.id).copied().unwrap_or_default();
                    let scope = match &conversation.workspace {
                        None => "thread (all projects)".to_owned(),
                        Some(workspace) => format!("project view · {workspace}"),
                    };
                    println!(
                        "{}  {} · {} msgs · {scope}",
                        conversation.id, conversation.title, messages,
                    );
                }
            }
            Ok(0)
        }
        Some(Commands::Workspaces { command }) => {
            workspaces::dispatch(store.root(), &cli.cwd, command, cli.json)
        }
        Some(Commands::Tasks { command }) => {
            let managed = xcb_runtime::managed::ManagedStore::open(store.root())?;
            match command {
                None | Some(TaskCommand::List) => {
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
                    let id = managed.resolve_task(&id)?;
                    let report = managed.verify_task(&id).await.map_err(|error| {
                        // --json keeps the stable error code.
                        if cli.json {
                            error
                        } else {
                            verify_failure(&id, error)
                        }
                    })?;
                    if cli.json {
                        print_json(report)?;
                    } else {
                        let steps = report["revisions"].as_u64().unwrap_or(0);
                        println!(
                            "{} {id}: {}",
                            ux::Style::stdout().symbol(ux::Symbol::Ok),
                            if steps == 1 {
                                "its 1 recorded step replays and matches the task.".to_owned()
                            } else {
                                format!("all {steps} recorded steps replay and match the task.")
                            }
                        );
                    }
                }
                Some(TaskCommand::Show { id }) => {
                    let task =
                        xcb_runtime::managed::inspect(&managed, &managed.resolve_task(&id)?)?
                            .ok_or(Error::Unavailable("managed task not found"))?;
                    print_json(task)?;
                }
                Some(TaskCommand::Cancel { id, revision }) => {
                    let task = managed
                        .cancel_task(&managed.resolve_task(&id)?, revision)
                        .await?;
                    if cli.json {
                        print_json(&task)?;
                    } else {
                        println!(
                            "{} Cancelling {}; it stops once its provider exits (now revision {}).",
                            ux::Style::stdout().symbol(ux::Symbol::Ok),
                            task.id,
                            task.revision
                        );
                    }
                    xcb_runtime::managed::ensure_daemon(store.root(), &std::env::current_exe()?)?;
                }
                Some(TaskCommand::Messages { id, after, limit }) => {
                    let id = managed.resolve_task(&id)?;
                    managed
                        .task(&id)?
                        .ok_or(Error::Unavailable("managed task not found"))?;
                    let messages = managed.mailbox(&id, after, usize::from(limit))?;
                    if cli.json {
                        print_json(messages)?;
                    } else if messages.is_empty() {
                        println!("No messages for {id} after sequence {after}.");
                    } else {
                        let full = messages.len() == usize::from(limit);
                        let mut last = after;
                        for message in messages {
                            last = message.sequence;
                            println!(
                                "#{} {} {} → {} · {}",
                                message.sequence,
                                message.source_provider,
                                message.source_task,
                                message.target_task,
                                xcb_core::display_text(&message.body, xcb_core::MAX_TEXT_BYTES),
                            );
                        }
                        if full {
                            println!("Newer messages: xcb tasks messages {id} --after {last}");
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
                            "automatic posting awaits a supported enrolled aicharts ingress; local exports remain available",
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
        Some(Commands::Tools { command }) => tools::execute(&store, command, cli.json).await,
        Some(Commands::Native { command }) => {
            Box::pin(native::execute(&store, &cli.cwd, command, cli.json)).await
        }
        Some(Commands::Judge { command }) => {
            match command {
                Some(JudgeCommand::Token) => {
                    if config.extensions.judge.is_clef() {
                        return Err(Error::Unavailable(
                            "Clef reads CLOUDFLARE_API_TOKEN from the environment only; judge token is for explicitly configured legacy System One",
                        ));
                    }
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
                Some(JudgeCommand::Clef { model }) => {
                    let (mut fresh, revision) = Config::load(store.root())?;
                    fresh.extensions.judge.provider =
                        Some(xcb_runtime::config::JudgeProvider::Clef);
                    fresh.extensions.judge.model = Some(Id::new(model)?);
                    fresh.extensions.judge.endpoint = None;
                    fresh.save(store.root(), revision.as_deref())?;
                    println!("Cloudflare Clef selected; enable it with xcb judge enable.");
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
                    judge::resolve(store.root(), &config.extensions.judge)?.ok_or(
                        Error::Unavailable("judge not configured: set CLOUDFLARE_ACCOUNT_ID and CLOUDFLARE_API_TOKEN, then xcb judge enable"),
                    )?;
                    println!("Judge configuration valid. No inference request was sent.");
                }
                None | Some(JudgeCommand::Status) => {
                    let source = judge::configured_key(store.root(), &config.extensions.judge)?;
                    let (judge_model, judge_endpoint) =
                        judge::effective_target(&config.extensions.judge)?;
                    if cli.json {
                        print_json(json!({
                            "version": 1,
                            "enabled": config.extensions.judge.enabled,
                            "provider": if config.extensions.judge.is_clef() { "clef" } else { "system-one" },
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
                            "judge: {} · key {key} · model {judge_model} · {}",
                            if config.extensions.judge.enabled {
                                "enabled"
                            } else {
                                "disabled"
                            },
                            judge_endpoint
                                .as_deref()
                                .unwrap_or("set CLOUDFLARE_ACCOUNT_ID"),
                        );
                    }
                }
            }
            Ok(0)
        }
        Some(Commands::Update { .. } | Commands::Upgrade { .. }) => {
            unreachable!("updater dispatched before application state")
        }
        Some(Commands::Config) => {
            print_json(config)?;
            Ok(0)
        }
        Some(Commands::Routing { command }) => {
            use xcb_runtime::routing_stack::{RoutePattern, Tier, pattern_matches};
            match command {
                Some(RoutingCommand::Never { command }) => {
                    let (mut fresh, revision) = Config::load(store.root())?;
                    match command {
                        NeverCommand::Add { pattern } => {
                            RoutePattern::parse(&pattern)?;
                            if !fresh.routing.never.contains(&pattern) {
                                fresh.routing.never.push(pattern.clone());
                            }
                            fresh.validate()?;
                            fresh.save(store.root(), revision.as_deref())?;
                            println!("Never routes to {pattern}.");
                        }
                        NeverCommand::Remove { pattern } => {
                            let before = fresh.routing.never.len();
                            fresh.routing.never.retain(|entry| *entry != pattern);
                            if fresh.routing.never.len() == before {
                                return Err(Error::Unavailable(
                                    "pattern is not in routing.never; xcb routing show lists them",
                                ));
                            }
                            fresh.save(store.root(), revision.as_deref())?;
                            println!("Routes to {pattern} are allowed again.");
                        }
                    }
                    return Ok(0);
                }
                Some(RoutingCommand::Show) | None => (),
            }
            let models = store.model_catalog()?.union();
            let stack = &config.routing;
            if cli.json {
                let tiers: serde_json::Map<String, serde_json::Value> = Tier::ALL
                    .into_iter()
                    .map(|tier| {
                        Ok((
                            tier.to_string(),
                            serde_json::to_value(pattern_matches(stack.patterns(tier), &models))?,
                        ))
                    })
                    .collect::<Result<_>>()?;
                print_json(json!({
                    "tiers": tiers,
                    "never": pattern_matches(&stack.never, &models),
                    "fallbackProviders": stack.fallback_providers,
                }))?;
                return Ok(0);
            }
            let print = |patterns: &[String]| {
                if patterns.is_empty() {
                    println!("  (none)");
                }
                for entry in pattern_matches(patterns, &models) {
                    let matched = if entry.models.is_empty() {
                        "no observed model".to_owned()
                    } else {
                        entry.models.join(", ")
                    };
                    println!("  #{} {:<28} {}", entry.position, entry.pattern, matched);
                }
            };
            for tier in Tier::ALL {
                println!("{tier}");
                print(stack.patterns(tier));
            }
            println!("never");
            print(&stack.never);
            println!(
                "fallback providers: {}",
                if stack.fallback_providers.is_empty() {
                    "none".to_owned()
                } else {
                    stack
                        .fallback_providers
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            );
            println!(
                "A tier's first matching pattern decides; the newest version of a family wins within a pattern. Edit routing in config.json to change the stack."
            );
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
                        "--launch-artifacts cleans up leftover launch folders, not a run; pass one or the other",
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
                    let count = if yes {
                        sweep.reclaimed
                    } else {
                        sweep.reclaimable
                    };
                    println!(
                        "{} {count} launch {} from finished runs ({}).",
                        if yes { "Removed" } else { "Can remove" },
                        if count == 1 { "folder" } else { "folders" },
                        human_bytes(if yes {
                            sweep.reclaimed_bytes
                        } else {
                            sweep.reclaimable_bytes
                        }),
                    );
                    println!(
                        "Kept {} in use and {} whose runs xcb can't confirm finished; --yes doesn't remove those.",
                        sweep.live,
                        sweep.unprovable.len(),
                    );
                    if !yes && sweep.reclaimable > 0 {
                        println!("Repeat with --yes to remove the finished ones.");
                    }
                }
                return Ok(0);
            }
            if let Some(run_id) = run {
                let (run, mut run_digest) = store
                    .recovery_candidate(&run_id)?
                    .ok_or(Error::Unavailable("run not found"))?;
                if let Some(info) = store.inspect_claude_auth_recovery(&run_id, &run_digest)? {
                    if !yes {
                        if cli.json {
                            print_json(json!({
                                "version": 1,
                                "dryRun": true,
                                "run": run.id,
                                "recovery": "claude-auth",
                                "retainedSignIns": info.generations.len(),
                                "reauthenticationRequired": info.reauthentication_required,
                            }))?;
                        } else {
                            println!(
                                "Claude sign-in processes for run {} have stopped. Recovery will keep their saved credentials and free the account.",
                                run.id
                            );
                            if info.reauthentication_required {
                                println!(
                                    "The account will need a new full Claude sign-in before use."
                                );
                            }
                            ux::next(&format!("xcb recover {} --yes", run.id));
                        }
                        return Ok(0);
                    }
                    store.recover_claude_auth(&run_id, &run_digest, now_ms())?;
                    // Recovery revalidates this payload and preserves its IDs.
                    // Render the public metadata selected before credential work.
                    if cli.json {
                        print_json(json!({
                            "version": 1,
                            "recovered": run.id,
                            "recovery": "claude-auth",
                            "reauthenticationRequired": info.reauthentication_required,
                        }))?;
                    } else {
                        println!(
                            "Recovered Claude sign-in for run {}. Its saved credentials were retained and its account is free.",
                            run.id
                        );
                        if info.reauthentication_required {
                            ux::next(&format!("xcb accounts login {} --browser", run.account));
                        }
                    }
                    return Ok(0);
                }
                // Prepared state also covers a child spawned before its PID
                // was persisted. Neither --yes nor owner absence proves that
                // child stopped, so this path only explains why the account
                // stays held.
                if run.phase == "prepared" && run.pid.is_none() {
                    if yes {
                        return Err(Error::Conflict(
                            "run has no recorded process ID; xcb keeps its account held because it can't confirm the provider stopped",
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
                            "Run {} has no recorded process ID, so xcb keeps its account held.",
                            run.id
                        );
                        println!(
                            "  A provider may have started before its process ID was saved, and this record can't show that it stopped."
                        );
                        println!("  --yes can't change that.");
                    }
                    return Ok(0);
                }
                let pid = run.pid.ok_or(Error::Conflict(
                    "run has no recorded process ID; recovery needs a running run with one",
                ))?;
                if run.phase != "running" {
                    return Err(Error::Conflict(
                        "run isn't running; recovery needs a running run with a recorded process ID",
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
                            "Would recover run {} (phase {}, process group {}).\nRepeat with --yes to confirm the provider and any command it ran have stopped, keep the account's saved sign-in or a verified same-account refresh, and free the account. Recovery never applies staged command edits.",
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
                let recovered = store.recover_run(&run_id, &run_digest, now_ms())?;
                if cli.json {
                    print_json(
                        json!({"version":1,"recovered":recovered.id,"phase":recovered.phase,"pid":pid}),
                    )?;
                } else {
                    println!(
                        "Recovered run {}: process group {} has exited and its account is free.",
                        recovered.id, pid
                    );
                }
            } else {
                let runs = store.unsettled_runs()?;
                if cli.json {
                    print_json(
                        json!({"version":1,"runs":runs.iter().map(|run| json!({"id":run.id,"phase":run.phase,"pid":run.pid,"createdAtMs":run.created_at_ms})).collect::<Vec<_>>()}),
                    )?;
                } else if runs.is_empty() {
                    println!("No unfinished runs.");
                } else {
                    println!("Unfinished runs:");
                    for run in &runs {
                        println!(
                            "  {} · phase {} · {}",
                            run.id,
                            run.phase,
                            run.pid
                                .map(|pid| format!("process group {pid}"))
                                .unwrap_or_else(|| "no process ID recorded".to_owned())
                        );
                    }
                    println!(
                        "After the xcb that started a run and its provider have exited, xcb recover <run-id> --yes frees its account."
                    );
                    ux::next(&format!("xcb recover {}", runs[0].id));
                }
            }
            Ok(0)
        }
        Some(Commands::Command {
            command: CommandJobs::Prune { days, yes },
        }) => {
            let root = xcb_runtime::command_tool::default_root()?;
            if !root.exists() {
                // No command runner on this machine means no jobs to archive.
                if cli.json {
                    print_json(xcb_runtime::command::PruneReport::default())?;
                } else {
                    println!(
                        "No offline command jobs: the command runner isn't set up at {}.",
                        root.display()
                    );
                }
                return Ok(0);
            }
            let report = xcb_runtime::command::CommandBackend::prune_joined_jobs(
                &root,
                now_ms().saturating_sub(u64::from(days) * 86_400_000),
                yes,
            )?;
            if cli.json {
                print_json(report)?;
            } else {
                println!(
                    "{} {} finished command job(s) older than {days} days into {}. Kept {} whose processes aren't confirmed stopped, {} waiting for cleanup, and {} recent.{}",
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
        Some(Commands::NativeMcpStdio) => native_mcp_stdio().await,
        // Dispatched before any state opens.
        Some(Commands::SandboxProbe { .. }) => Ok(2),
        // Only connect and disconnect reach here; reports ran before state opened.
        Some(Commands::Usage { args }) => usage::connect(&store, &args, cli.json),
        Some(Commands::Completions { shell }) => {
            // `clap_complete` panics on a closed pipe, so render into a buffer
            // and write it through the pipe-tolerant stdout helper.
            let mut script = Vec::new();
            clap_complete::generate(shell, &mut Cli::command(), "xcb", &mut script);
            ux::write_stdout(std::str::from_utf8(&script).unwrap_or_default());
            Ok(0)
        }
    }
}

fn discover_provider_sessions(
    provider: Option<&str>,
    hours: u16,
) -> Result<xcb_runtime::managed::imports::Discovery> {
    use xcb_runtime::managed::imports::{Sources, discover};
    discover(
        &Sources::from_environment()?,
        provider.map(str::parse).transpose()?,
        hours,
        now_ms(),
    )
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

/// Resolve a conversation for local managed state (legacy test helper).
#[allow(dead_code)]
async fn chat_conversation(
    managed: &xcb_runtime::managed::ManagedStore,
    cwd: &std::path::Path,
    resume: Option<Id>,
    new: bool,
) -> Result<(xcb_runtime::managed::ManagedConversation, Option<String>)> {
    let conversation = match resume {
        Some(id) => managed
            .conversation(&managed.resolve_conversation(&id)?)?
            .ok_or(Error::Unavailable("managed conversation not found"))?,
        None if new => return Ok((managed.create_conversation(cwd).await?, None)),
        None => managed.global_thread().await?,
    };
    let hint = if conversation.workspace.is_none() {
        launch_hint(managed, cwd, true)
    } else {
        None
    };
    Ok((conversation, hint))
}

/// The launch directory's project root as a hint for the thread: snapped,
/// valid and not a container. With `admit`, a new root is admitted as
/// `launch` (refused for containers and for directories that look like one:
/// a child of `$HOME` or a parent of repositories); without it (a read-only
/// preview) a new root counts when admitting it would succeed. Anything
/// else, such as launching from `~` or `~/Documents`, silently gives no
/// hint.
fn launch_hint(
    managed: &xcb_runtime::managed::ManagedStore,
    cwd: &std::path::Path,
    admit: bool,
) -> Option<String> {
    let root = managed.snap_root(cwd).ok()?;
    let known = managed.known_workspaces(4096).ok()?;
    if let Some(entry) = known.iter().find(|entry| entry.path == root) {
        return (!entry.container).then_some(root);
    }
    if admit {
        return managed
            .admit_workspace(std::path::Path::new(&root), "launch", None)
            .ok();
    }
    managed
        .launch_admissible(&root)
        .ok()
        .filter(|admissible| *admissible)
        .map(|_| root)
}

/// Where `xcb models route` would run the prompt in the thread. Read-only:
/// a fresh message id replays nothing, and resolving never creates the
/// thread row or admits the launch directory.
fn route_workspace_preview(
    managed: &xcb_runtime::managed::ManagedStore,
    cwd: &std::path::Path,
    task: &str,
) -> Result<xcb_runtime::workspace_infer::Resolution> {
    use xcb_runtime::managed::{GLOBAL_THREAD_ID, IntakeCues, Origin};
    managed.resolve_intake(
        &Id::new(GLOBAL_THREAD_ID)?,
        &xcb_runtime::new_id("m_preview"),
        task,
        &IntakeCues {
            origin: Origin::Cli,
            explicit: None,
            target: None,
            focus: None,
            launch_hint: launch_hint(managed, cwd, false),
            infer_only: false,
        },
    )
}

fn placement_parts(
    placement: &xcb_runtime::workspace_infer::Resolution,
) -> Option<(&str, &'static str)> {
    match placement {
        xcb_runtime::workspace_infer::Resolution::Bound {
            workspace, binding, ..
        } => Some((workspace.as_str(), binding.source.as_str())),
        xcb_runtime::workspace_infer::Resolution::Ask { .. } => None,
    }
}

fn placement_line(placement: &xcb_runtime::workspace_infer::Resolution) -> String {
    match placement_parts(placement) {
        Some((path, source)) => format!(
            "Workspace: {} ({source})",
            xcb_core::display_text(path, 4096)
        ),
        None => "Workspace: xcb would ask which project".to_owned(),
    }
}

/// `xcb conversations`: the thread first when its row exists (listing never
/// creates it), then project views, most recent first.
fn listed_conversations(
    managed: &xcb_runtime::managed::ManagedStore,
) -> Result<Vec<xcb_runtime::managed::ManagedConversation>> {
    let mut views = managed.conversations(256)?;
    let thread = views
        .iter()
        .position(|conversation| conversation.workspace.is_none())
        .map(|index| views.remove(index));
    let thread = match thread {
        Some(thread) => Some(thread),
        None => managed.conversation(&Id::new(xcb_runtime::managed::GLOBAL_THREAD_ID)?)?,
    };
    Ok(thread.into_iter().chain(views).collect())
}

/// `xcb conversations --json` rows: every row gains `isThread`; the thread's
/// `workspace` is an explicit null, and view rows keep every existing key.
fn conversation_rows(
    conversations: &[xcb_runtime::managed::ManagedConversation],
) -> Result<Vec<serde_json::Value>> {
    conversations
        .iter()
        .map(|conversation| {
            let mut row = serde_json::to_value(conversation)?;
            if let Some(object) = row.as_object_mut() {
                let thread = conversation.workspace.is_none();
                object.insert("isThread".into(), json!(thread));
                object.entry("workspace").or_insert(serde_json::Value::Null);
            }
            Ok(row)
        })
        .collect()
}

fn terminal_available() -> bool {
    io::stdin().is_terminal() && io::stdout().is_terminal()
}

fn interactive_account_add(json: bool, _provider: Provider, terminal: bool) -> bool {
    !json && terminal
}

#[derive(Debug, PartialEq, Eq)]
enum SetupChoice {
    New,
    Existing(usize),
    Cancel,
}

fn parse_setup_choice(input: &str, count: usize) -> Option<SetupChoice> {
    match input.trim() {
        "" | "0" | "q" | "cancel" => Some(SetupChoice::Cancel),
        "1" => Some(SetupChoice::New),
        value => value.parse::<usize>().ok().and_then(|number| {
            (number >= 2 && number <= count + 1).then(|| SetupChoice::Existing(number - 2))
        }),
    }
}

async fn choose_setup_account(
    accounts: &[xcb_runtime::store::Account],
    provider: Provider,
) -> Result<SetupChoice> {
    eprintln!("Set up {provider}");
    eprintln!("  1. Add another account");
    for (index, account) in accounts.iter().enumerate() {
        let status = if account.enabled { "" } else { " (turned off)" };
        eprintln!(
            "  {}. Use {}{status}",
            index + 2,
            xcb_core::display_text(&account.name(), 80)
        );
    }
    eprintln!("  0. Cancel");
    loop {
        let mut reader = terminal_input::Reader::new(false, 1024)?;
        eprint!("Choose an account: ");
        io::stderr().flush()?;
        // EOF cancels before adding accounts or opening sign-in.
        let Some(input) = reader.read_line().await? else {
            return Ok(SetupChoice::Cancel);
        };
        if let Some(choice) = parse_setup_choice(&input, accounts.len()) {
            return Ok(choice);
        }
        eprintln!("Enter a number from 0 to {}.", accounts.len() + 1);
    }
}

fn add_setup_account(
    store: &Store,
    provider: Provider,
    plan: &str,
) -> Result<xcb_runtime::store::Account> {
    let account = store.add_account(provider, plan, now_ms(), None)?;
    let (mut config, revision) = Config::load(store.root())?;
    if config.default_account.is_none() {
        config.default_account = Some(account.id.clone());
        config.save(store.root(), revision.as_deref())?;
    }
    println!(
        "{} {}",
        ux::Style::stdout().symbol(ux::Symbol::Ok),
        PublicAccount::from(&account).added_message().0
    );
    Ok(account)
}

async fn finish_account_setup(
    store: &Store,
    provider: Provider,
    account: Option<xcb_runtime::store::Account>,
    mut stop: stop::Stop,
) -> Result<i32> {
    let ok = ux::Style::stdout().symbol(ux::Symbol::Ok);
    let name = provider_name(provider);
    // 2. Check the provider build, and that xcb can run it, before
    // any sign-in starts.
    let pin = stop.settle(ensure_pin(store.root(), provider)).await?;
    require_supported(store.root(), &pin)?;
    println!("{ok} {name} {} is installed", pin.version);
    // 3. A rejected credential must be replaced by sign-in; refreshing
    // model metadata does not repair authentication.
    let Some(account) = account else {
        ux::next("run xcb accounts login <account> to connect the account");
        return Ok(0);
    };
    if account_needs_sign_in(store, &account)? {
        let _held = ux::hold_next();
        stop.settle(Box::pin(dispatch(Cli {
            state: Some(store.root().to_path_buf()),
            json: false,
            cwd: PathBuf::from("."),
            command: Some(Commands::Accounts {
                command: Some(AccountCommand::Login {
                    account: account.id.to_string(),
                    browser: false,
                }),
            }),
        })))
        .await?;
    }
    // 4. Load the account's models (what `accounts refresh` does).
    require_setup_sign_in(store, &account)?;
    let models = stop
        .settle(runner::probe(store, &pin, Some(&account.id)))
        .await?;
    require_setup_sign_in(store, &account)?;
    store.set_account_models(&account.id, &models)?;
    println!("{ok} Loaded {} models", models.len());
    println!("{ok} {name} is set up.");
    ux::next("xcb");
    Ok(0)
}

#[tokio::main]
async fn main() {
    ux::restore_sigpipe();
    let args: Vec<String> = std::env::args().collect();
    if matches!(
        command_words(&args).as_slice(),
        ["help", "advanced"] | ["advanced"] | ["advanced", "--help" | "-h"]
    ) {
        hraness_cli_kit::style::write_stdout(ux::ADVANCED);
        return;
    }
    let root = ux::command();
    let parsed = root
        .clone()
        .try_get_matches_from(&args)
        .and_then(|matches| <Cli as clap::FromArgMatches>::from_arg_matches(&matches));
    let cli = match parsed {
        Ok(cli) => cli,
        Err(error) => std::process::exit(ux::clap_failure(error, &root, &args)),
    };
    let json = cli.json;
    // Plain `xcb` is a discovery hint now that the interactive terminal
    // surface is removed. Pipes and agents get the same guidance as people.
    if cli.command.is_none() && !json && !(io::stdin().is_terminal() && io::stdout().is_terminal())
    {
        hraness_cli_kit::style::write_stdout(&ux::start_text());
        return;
    }
    // Internal helpers speak a protocol on stdout; their errors stay on
    // stderr whoever runs them.
    let protocol = matches!(
        cli.command,
        Some(
            Commands::ManagedDaemon
                | Commands::ServiceRun
                | Commands::NativeMcpStdio
                | Commands::EgressForward { .. }
                | Commands::SandboxProbe { .. }
        )
    );
    let mut installation = None;
    let code = match Box::pin(dispatch_inner(cli, &mut installation)).await {
        Ok(code) => code,
        Err(error) => ux::report_error(&error, json, protocol),
    };
    std::process::exit(code);
}

/// The words after `xcb` with the global options (`--state <dir>`,
/// `--cwd <dir>`, `--json`) left out, for the `advanced` screen, which is
/// not a subcommand.
fn command_words(args: &[String]) -> Vec<&str> {
    let mut words = Vec::new();
    let mut rest = args.iter().skip(1).map(String::as_str);
    while let Some(word) = rest.next() {
        match word {
            "--state" | "--cwd" => {
                rest.next();
            }
            "--json" => {}
            word if word.starts_with("--state=") || word.starts_with("--cwd=") => {}
            word => words.push(word),
        }
    }
    words
}

/// `xcb tasks verify` failures in plain words: which part of the task's
/// local record didn't hold up. Other errors pass through unchanged.
fn verify_failure(task: &Id, error: Error) -> Error {
    let problem = match &error {
        Error::Conflict(message) | Error::Unavailable(message) => match *message {
            "managed receipt chain is missing the persisted task revision" => {
                "the latest step of its record is missing"
            }
            "managed receipt chain is missing a prior revision" => {
                "a step in the middle of its record is missing"
            }
            "managed receipt origin mismatch" => "the first step of its record is missing",
            "managed receipt replay mismatch" | "managed receipt replay failed" => {
                "a recorded step doesn't replay to the same result"
            }
            "managed receipt does not match persisted task" => {
                "the task doesn't match the last step of its record"
            }
            "managed receipt chain identity mismatch" => {
                "a step of its record belongs to another task"
            }
            "managed receipt output rejected" => "a step of its record can't be read",
            _ => return error,
        },
        Error::Core(xcb_core::Error::Limit("managed receipt chain")) => {
            "its record has more steps than xcb can replay (1024)"
        }
        _ => return error,
    };
    Error::Guided {
        message: format!("Task {task} failed verification: {problem}"),
        next: None,
    }
}

/// Stderr is often retained by callers. Keep this notice independent of
/// account-bearing route records and provider-supplied model metadata.
fn automatic_route_notice(reason: &str) -> &'static str {
    if reason.starts_with("Warning: usage limits") {
        "Usage limits rule out a higher-ranked model; using the best one available now."
    } else {
        "Picked an account and model automatically."
    }
}

#[cfg(test)]
mod codex_import_tests {
    use super::*;

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            let root = xcb_core::canonical(std::env::temp_dir())
                .unwrap()
                .join(xcb_runtime::new_id("xcb_codex_import").as_str());
            private::directory(&root.join("source")).unwrap();
            let fixture = Self(root);
            private::create(&fixture.source(), &Self::bytes("account-one", "original")).unwrap();
            fixture
        }

        fn source(&self) -> PathBuf {
            self.0.join("source/auth.json")
        }

        fn bytes(account: &str, access: &str) -> Vec<u8> {
            // Synthetic JWT payload is {"sub":"synthetic-user"}.
            serde_json::to_vec(&json!({
                "auth_mode":"chatgpt",
                "tokens":{
                    "id_token":"synthetic.eyJzdWIiOiJzeW50aGV0aWMtdXNlciJ9.signature",
                    "access_token":access,
                    "refresh_token":"synthetic-refresh",
                    "account_id":account
                }
            }))
            .unwrap()
        }

        fn replace_source(&self, bytes: &[u8]) {
            let current = private::read(&self.source(), 65536).unwrap();
            private::replace(&self.source(), bytes, &xcb_runtime::digest(current)).unwrap();
        }

        async fn import(&self, account: Option<&str>) -> Result<i32> {
            let mut args = vec![
                "xcb".to_owned(),
                "--state".to_owned(),
                self.0.join("state").to_str().unwrap().to_owned(),
                "--json".to_owned(),
                "accounts".to_owned(),
                "import-codex".to_owned(),
                "--source".to_owned(),
                self.source().to_str().unwrap().to_owned(),
            ];
            if let Some(account) = account {
                args.extend(["--account".to_owned(), account.to_owned()]);
            }
            dispatch(Cli::try_parse_from(args).unwrap()).await
        }

        fn store(&self) -> Store {
            Store::open(&self.0.join("state")).unwrap()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[tokio::test]
    async fn codex_import_updates_sign_in_without_duplicate_accounts_or_lost_limits() {
        let fixture = Fixture::new();
        assert_eq!(fixture.import(None).await.unwrap(), 0);
        let store = fixture.store();
        let account = store.accounts().unwrap().remove(0);
        store.set_account_enabled(&account.id, false).unwrap();
        let now = now_ms();
        let reset = now + 3_600_000;
        let quota = xcb_core::usage::QuotaPoint {
            pool: account.quota_pool.clone(),
            window: Id::new("codex.primary").unwrap(),
            used_percent: 100.0,
            resets_at_ms: reset,
            observed_at_ms: now,
        };
        store.record_quota(&quota).unwrap();
        let before = serde_json::to_value(store.account(&account.id).unwrap()).unwrap();
        let db = rusqlite::Connection::open(store.root().join("xcb.sqlite")).unwrap();
        db.execute(
            "INSERT INTO account_auth_failures(account,generation,run) VALUES(?1,NULL,'r_fixture')",
            [account.id.as_str()],
        )
        .unwrap();
        drop(db);
        assert!(store.authentication_required(&account.id).unwrap());

        assert!(fixture.import(Some(account.id.as_str())).await.is_err());
        assert_eq!(
            serde_json::to_value(store.account(&account.id).unwrap()).unwrap(),
            before
        );
        assert!(store.authentication_required(&account.id).unwrap());
        assert!(store.unsettled_runs().unwrap().is_empty());
        store.set_account_enabled(&account.id, true).unwrap();
        let before = serde_json::to_value(store.account(&account.id).unwrap()).unwrap();
        fixture.import(Some(account.id.as_str())).await.unwrap();
        assert!(store.authentication_required(&account.id).unwrap());
        let changed = Fixture::bytes("account-one", "refreshed");
        fixture.replace_source(&changed);
        fixture.import(Some(account.id.as_str())).await.unwrap();

        assert_eq!(store.accounts().unwrap().len(), 1);
        assert_eq!(
            serde_json::to_value(store.account(&account.id).unwrap()).unwrap(),
            before
        );
        assert!(!store.authentication_required(&account.id).unwrap());
        assert_eq!(
            store.quota_blocked_until(&account.id, now).unwrap(),
            Some(reset)
        );
        assert_eq!(store.quotas(&account.quota_pool).unwrap(), vec![quota]);
        assert_eq!(private::read(&fixture.source(), 65536).unwrap(), changed);
        assert_eq!(
            private::read(
                &store
                    .account_root(&account.id)
                    .unwrap()
                    .join("profile/auth.json"),
                65536
            )
            .unwrap(),
            changed
        );
        assert!(store.unsettled_runs().unwrap().is_empty());
    }

    #[test]
    fn codex_import_existing_rejects_identity_changes_wrong_provider_and_active_account() {
        // Linux test threads use a 2 MiB stack. Enforce that budget on macOS
        // too: an unrelated async command must not inflate every dispatch.
        std::thread::Builder::new()
            .stack_size(2 * 1024 * 1024)
            .spawn(|| {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap()
                    .block_on(rejects_identity_changes_wrong_provider_and_active_account());
            })
            .unwrap()
            .join()
            .unwrap();
    }

    async fn rejects_identity_changes_wrong_provider_and_active_account() {
        let fixture = Fixture::new();
        fixture.import(None).await.unwrap();
        let store = fixture.store();
        let account = store.accounts().unwrap().remove(0);
        let claude = store
            .add_account(Provider::Claude, "Synthetic", now_ms(), None)
            .unwrap();
        let stored = store
            .account_root(&account.id)
            .unwrap()
            .join("profile/auth.json");
        let original = private::read(&stored, 65536).unwrap();
        let generation = store
            .account_root(&account.id)
            .unwrap()
            .join("application-generation.json");
        let original_generation = private::read(&generation, 1024).unwrap();
        fixture.replace_source(&Fixture::bytes("another-account", "refreshed"));
        assert!(
            fixture
                .import(Some(account.id.as_str()))
                .await
                .unwrap_err()
                .to_string()
                .contains("identity changed")
        );
        assert!(fixture.import(Some(claude.id.as_str())).await.is_err());
        assert!(fixture.import(Some("a_missing")).await.is_err());
        assert_eq!(private::read(&stored, 65536).unwrap(), original);
        assert_eq!(
            private::read(&generation, 1024).unwrap(),
            original_generation
        );
        assert!(store.unsettled_runs().unwrap().is_empty());

        fixture.replace_source(&Fixture::bytes("account-one", "refreshed"));
        let workspace = private::directory(&fixture.0.join("work")).unwrap();
        let session = store
            .create_session(
                &account.id,
                xcb_core::models::ModelChoice {
                    provider: Provider::Codex,
                    id: Id::new("synthetic-model").unwrap(),
                    label: "Synthetic model".into(),
                    mode: xcb_core::models::Mode::Fixed,
                    resolved: None,
                    effort: None,
                    observed_at_ms: now_ms(),
                },
                &workspace,
                now_ms(),
            )
            .unwrap();
        let held = store
            .prepare_run(&session.id, session.revision, now_ms())
            .unwrap();
        assert!(fixture.import(Some(account.id.as_str())).await.is_err());
        assert_eq!(private::read(&stored, 65536).unwrap(), original);
        assert_eq!(
            private::read(&generation, 1024).unwrap(),
            original_generation
        );
        let pending = store.unsettled_runs().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, held.id);
        assert_eq!(store.accounts().unwrap().len(), 2);
    }
}

#[cfg(test)]
mod setup_tests {
    use super::*;

    #[test]
    fn setup_requires_new_sign_in_after_rejection_even_when_models_refresh() {
        let root = xcb_core::canonical(std::env::temp_dir())
            .unwrap()
            .join(format!(
                "xcb-setup-auth-{}-{}",
                std::process::id(),
                xcb_runtime::new_id("fixture")
            ));
        let store = Store::open(&root).unwrap();
        let account = store
            .add_account(Provider::Claude, "Fixture", 1, None)
            .unwrap();
        assert!(account_needs_sign_in(&store, &account).unwrap());
        private::create(
            &store
                .account_root(&account.id)
                .unwrap()
                .join("subscription-token"),
            b"sk-ant-oat01-synthetic_fixture_credential",
        )
        .unwrap();
        assert!(!account_needs_sign_in(&store, &account).unwrap());
        require_setup_sign_in(&store, &account).unwrap();

        let db = rusqlite::Connection::open(root.join("xcb.sqlite")).unwrap();
        db.execute(
            "INSERT INTO account_auth_failures(account,generation,run) VALUES(?1,NULL,'r_fixture')",
            [account.id.as_str()],
        )
        .unwrap();
        drop(db);
        // Model metadata can refresh while the provider still rejects the
        // stored sign-in. Neither setup gate may mistake that for recovery.
        store
            .set_models(
                Provider::Claude,
                &[xcb_core::models::ModelChoice {
                    provider: Provider::Claude,
                    id: Id::new("fixture-model").unwrap(),
                    label: "Fixture model".into(),
                    mode: xcb_core::models::Mode::Fixed,
                    resolved: None,
                    effort: None,
                    observed_at_ms: now_ms(),
                }],
            )
            .unwrap();
        assert!(auth::has_credentials(&store, &account.id).unwrap());
        assert!(account_needs_sign_in(&store, &account).unwrap());
        match require_setup_sign_in(&store, &account).unwrap_err() {
            Error::Guided { next, .. } => {
                assert_eq!(next, Some(format!("xcb accounts login {}", account.id)));
            }
            error => panic!("expected sign-in guidance, got {error}"),
        }
        assert!(store.authentication_required(&account.id).unwrap());
        assert!(store.unsettled_runs().unwrap().is_empty());
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod thread_entry_tests {
    use super::*;
    use std::path::Path;
    use xcb_runtime::managed::{GLOBAL_THREAD_ID, ManagedStore};

    /// A private scratch base: `state` for the store, sibling directories for
    /// workspaces, removed on drop.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let base = xcb_core::canonical(std::env::temp_dir())
                .unwrap()
                .join(format!(
                    "xcb-cli-entry-{name}-{}-{}",
                    std::process::id(),
                    now_ms()
                ));
            let _ = std::fs::remove_dir_all(&base);
            std::fs::create_dir_all(&base).unwrap();
            Self(base)
        }
        fn store(&self) -> ManagedStore {
            ManagedStore::open(&self.0.join("state")).unwrap()
        }
        fn dir(&self, name: &str) -> PathBuf {
            let path = self.0.join(name);
            std::fs::create_dir_all(&path).unwrap();
            xcb_core::canonical(&path).unwrap()
        }
        fn repo(&self, name: &str) -> PathBuf {
            let path = self.dir(name);
            std::fs::create_dir_all(path.join(".git")).unwrap();
            path
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn text(path: &Path) -> &str {
        path.to_str().unwrap()
    }

    #[tokio::test]
    async fn bare_selection_opens_the_thread_with_a_launch_hint() {
        let scratch = Scratch::new("bare");
        let managed = scratch.store();
        let repo = scratch.repo("repo");
        let nested = scratch.dir("repo/src/deep");
        let (conversation, hint) = chat_conversation(&managed, &nested, None, false)
            .await
            .unwrap();
        assert_eq!(conversation.id.as_str(), GLOBAL_THREAD_ID);
        assert!(conversation.workspace.is_none());
        assert_eq!(hint.as_deref(), Some(text(&repo)), "the hint snaps");
        let entry = managed
            .all_workspaces()
            .unwrap()
            .into_iter()
            .find(|entry| entry.path == text(&repo))
            .unwrap();
        assert_eq!(entry.admitted_by, "launch");
        // The thread row is shared, never duplicated.
        let (again, _) = chat_conversation(&managed, &repo, None, false)
            .await
            .unwrap();
        assert_eq!(again.id, conversation.id);
        assert_eq!(managed.conversations(16).unwrap().len(), 1);
    }

    #[tokio::test]
    async fn launching_from_a_container_gives_no_hint() {
        let scratch = Scratch::new("container");
        let managed = scratch.store();
        let documents = scratch.dir("documents");
        let project = scratch.dir("documents/project");
        managed.admit_workspace(&project, "command", None).unwrap();
        let (conversation, hint) = chat_conversation(&managed, &documents, None, false)
            .await
            .unwrap();
        assert_eq!(conversation.id.as_str(), GLOBAL_THREAD_ID);
        assert_eq!(hint, None);
        assert!(
            managed
                .all_workspaces()
                .unwrap()
                .iter()
                .all(|entry| entry.path != text(&documents)),
            "a container is never admitted by launch"
        );
    }

    #[tokio::test]
    async fn launching_from_a_parent_of_repositories_admits_nothing_on_an_empty_registry() {
        let scratch = Scratch::new("parent");
        let managed = scratch.store();
        let documents = scratch.dir("documents");
        scratch.repo("documents/app");
        assert_eq!(launch_hint(&managed, &documents, false), None);
        let (_, hint) = chat_conversation(&managed, &documents, None, false)
            .await
            .unwrap();
        assert_eq!(hint, None);
        assert!(
            managed.all_workspaces().unwrap().is_empty(),
            "launch never admits a directory of repositories"
        );
    }

    #[tokio::test]
    async fn new_always_creates_a_new_project_view() {
        let scratch = Scratch::new("new");
        let managed = scratch.store();
        let work = scratch.dir("work");
        let existing = managed.create_conversation(&work).await.unwrap();
        let (first, hint) = chat_conversation(&managed, &work, None, true)
            .await
            .unwrap();
        let (second, _) = chat_conversation(&managed, &work, None, true)
            .await
            .unwrap();
        assert_eq!(hint, None);
        for view in [&first, &second] {
            assert_ne!(view.id.as_str(), GLOBAL_THREAD_ID);
            assert_ne!(view.id, existing.id);
            assert_eq!(view.workspace.as_deref(), Some(text(&work)));
        }
        assert_ne!(first.id, second.id);
        assert!(
            managed
                .conversation(&Id::new(GLOBAL_THREAD_ID).unwrap())
                .unwrap()
                .is_none(),
            "--new never opens the thread"
        );
        let (resumed, _) = chat_conversation(&managed, &work, Some(existing.id.clone()), false)
            .await
            .unwrap();
        assert_eq!(resumed.id, existing.id);
    }

    #[tokio::test]
    async fn conversations_json_marks_thread_with_null_workspace() {
        let scratch = Scratch::new("listing");
        let managed = scratch.store();
        let work = scratch.dir("work");
        let view = managed.create_conversation(&work).await.unwrap();
        let before = conversation_rows(&listed_conversations(&managed).unwrap()).unwrap();
        assert_eq!(before.len(), 1);
        assert!(
            managed
                .conversation(&Id::new(GLOBAL_THREAD_ID).unwrap())
                .unwrap()
                .is_none(),
            "listing never creates the thread"
        );
        let original = serde_json::to_value(&view).unwrap();
        managed.global_thread().await.unwrap();
        let rows = conversation_rows(&listed_conversations(&managed).unwrap()).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["id"], json!(GLOBAL_THREAD_ID));
        assert_eq!(rows[0]["isThread"], json!(true));
        assert!(rows[0].as_object().unwrap().contains_key("workspace"));
        assert_eq!(rows[0]["workspace"], serde_json::Value::Null);
        assert_eq!(rows[1]["isThread"], json!(false));
        for (key, value) in original.as_object().unwrap() {
            if key != "updated_at_ms" {
                assert_eq!(&rows[1][key], value, "{key}");
            }
        }
        assert_eq!(rows[1]["workspace"], json!(text(&work)));
    }

    #[tokio::test]
    async fn models_route_preview_writes_nothing() {
        let scratch = Scratch::new("preview");
        let managed = scratch.store();
        let known = scratch.repo("known");
        managed.admit_workspace(&known, "command", None).unwrap();
        let fresh = scratch.repo("fresh");
        let registry = managed.all_workspaces().unwrap();
        let placement = route_workspace_preview(&managed, &fresh, "fix the tests").unwrap();
        assert_eq!(
            placement_line(&placement),
            format!("Workspace: {} (launch)", text(&fresh))
        );
        assert_eq!(managed.all_workspaces().unwrap(), registry);
        assert!(managed.tasks(16).unwrap().is_empty());
        assert!(managed.message_counts().unwrap().is_empty());
        assert!(
            managed
                .conversation(&Id::new(GLOBAL_THREAD_ID).unwrap())
                .unwrap()
                .is_none(),
            "the preview never creates the thread"
        );
        let home = xcb_core::home_dir().unwrap();
        let placement = route_workspace_preview(&managed, &home, "fix the tests").unwrap();
        assert_ne!(
            placement_parts(&placement).map(|(_, source)| source),
            Some("launch"),
            "home gives no launch hint"
        );
    }
}

#[cfg(test)]
mod help_tests {
    #[test]
    fn root_help_lists_every_visible_command() {
        use clap::CommandFactory as _;
        let listed = format!("{}{}", super::ux::ROOT_HELP, super::ux::ADVANCED);
        for command in super::Cli::command().get_subcommands() {
            if command.is_hide_set() || command.get_name() == "help" {
                continue;
            }
            let name = command.get_name();
            assert!(
                listed
                    .lines()
                    .any(|line| line.split_whitespace().next() == Some(name)),
                "{name} is missing from xcb --help and xcb help advanced"
            );
        }
    }
}

#[cfg(test)]
mod recent_session_cli_tests {
    use super::*;

    #[test]
    fn defaults_to_24_hours_and_requires_an_import_selection() {
        let discover = Cli::try_parse_from(["xcb", "sessions", "discover"]).unwrap();
        assert!(matches!(
            discover.command,
            Some(Commands::Sessions {
                command: Some(SessionCommand::Discover {
                    hours: 24,
                    provider: None
                })
            })
        ));
        let import = Cli::try_parse_from([
            "xcb",
            "sessions",
            "import",
            "--recent",
            "--provider",
            "codex",
        ])
        .unwrap();
        assert!(
            matches!(import.command, Some(Commands::Sessions { command: Some(SessionCommand::Import { id: None, recent: true, workspace: None, hours: 24, provider: Some(provider) }) }) if provider == "codex")
        );
        assert!(
            Cli::try_parse_from([
                "xcb",
                "sessions",
                "import",
                "c_import_example",
                "--hours",
                "48"
            ])
            .is_ok()
        );
        assert!(
            Cli::try_parse_from([
                "xcb",
                "sessions",
                "import",
                "c_import_example",
                "--workspace",
                "/work/project"
            ])
            .is_ok()
        );
        for args in [
            vec!["xcb", "sessions", "import"],
            vec!["xcb", "sessions", "import", "c_import_example", "--recent"],
            vec![
                "xcb",
                "sessions",
                "import",
                "--recent",
                "--workspace",
                "/work/project",
            ],
            vec!["xcb", "sessions", "import", "--workspace", "/work/project"],
            vec!["xcb", "sessions", "discover", "--hours", "0"],
            vec!["xcb", "sessions", "discover", "--hours", "8761"],
            vec!["xcb", "sessions", "discover", "--provider", "devin"],
        ] {
            assert!(Cli::try_parse_from(args).is_err());
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn account_setup_parser_accepts_new_and_explicit_existing() {
        let cli = Cli::try_parse_from(["xcb", "setup", "codex", "--new"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::Setup {
                new: true,
                account: None,
                ..
            })
        ));
        let cli =
            Cli::try_parse_from(["xcb", "setup", "claude", "--account", "a_existing"]).unwrap();
        assert!(
            matches!(cli.command, Some(Commands::Setup { new: false, account: Some(account), .. }) if account == "a_existing")
        );
        assert!(
            Cli::try_parse_from(["xcb", "setup", "codex", "--new", "--account", "a_existing"])
                .is_err()
        );
    }

    #[test]
    fn account_add_interactive_policy_keeps_automation_create_only() {
        for provider in Provider::SUPPORTED {
            assert!(super::interactive_account_add(false, provider, true));
            assert!(!super::interactive_account_add(true, provider, true));
            assert!(!super::interactive_account_add(false, provider, false));
        }
    }

    #[test]
    fn account_setup_choice_has_explicit_new_existing_and_cancel() {
        use super::{SetupChoice, parse_setup_choice};
        assert_eq!(parse_setup_choice("1\n", 2), Some(SetupChoice::New));
        assert_eq!(parse_setup_choice("2", 2), Some(SetupChoice::Existing(0)));
        assert_eq!(parse_setup_choice("3", 2), Some(SetupChoice::Existing(1)));
        for input in ["", "\n", "0", "q", "cancel"] {
            assert_eq!(parse_setup_choice(input, 2), Some(SetupChoice::Cancel));
        }
        for input in ["4", "-1", "abc"] {
            assert_eq!(parse_setup_choice(input, 2), None);
        }
    }

    #[test]
    fn service_status_names_the_log_and_a_denied_folder() {
        use xcb_runtime::habitat_service::{Service, Status};
        let dir = std::env::temp_dir().join(format!("xcb-service-text-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("habitat.log");
        std::fs::write(
            &log,
            "xcb: local I/O failed: Operation not permitted (os error 1)\n",
        )
        .unwrap();
        let env = |name: &str| (name == "LANG").then(|| "en_US.UTF-8".to_owned());
        let style = super::ux::Style::detect(&env, false);
        let service = Service {
            version: 1,
            label: "dev.hraness.xcb.habitat.x".into(),
            state: dir.clone(),
            executable: "/bin/xcb".into(),
            home: dir.clone(),
            manifest: "/Users/me/Library/LaunchAgents/dev.hraness.xcb.habitat.x.plist".into(),
            coordination_root: None,
        };
        let text = super::service_text(
            &Status {
                installed: true,
                registered: true,
                supervisor_running: false,
                supervisor_health: xcb_runtime::habitat_service::SupervisorHealth {
                    state: xcb_runtime::habitat_service::HealthState::Stopped,
                    heartbeat_age_seconds: None,
                },
                watchdog_enabled: false,
                supervisor_watched: false,
                service: Some(service),
                log: Some(log.clone()),
            },
            style,
        );
        assert_eq!(
            text,
            format!(
                "✓ Starts at login · ○ supervisor idle\n\
                 File: /Users/me/Library/LaunchAgents/dev.hraness.xcb.habitat.x.plist\n\
                 Log: {}\n\
                 ✗ xcb can't open a project folder: macOS access is off for xcb.\n  \
                 Turn on xcb for Documents, Desktop or Downloads in System Settings › Privacy & Security › Files & Folders.\n\
                 → open 'x-apple.systempreferences:com.apple.preference.security?Privacy_FilesAndFolders'\n",
                log.display()
            )
        );
        let legacy = super::service_text(
            &Status {
                installed: true,
                registered: false,
                supervisor_running: true,
                supervisor_health: xcb_runtime::habitat_service::SupervisorHealth {
                    state: xcb_runtime::habitat_service::HealthState::Fresh,
                    heartbeat_age_seconds: Some(1),
                },
                watchdog_enabled: false,
                supervisor_watched: false,
                service: None,
                log: None,
            },
            style,
        );
        let manager = if cfg!(target_os = "linux") {
            "systemd"
        } else {
            "macOS"
        };
        assert_eq!(
            legacy,
            format!(
                "⚠ Installed, but {manager} hasn't loaded it · ● supervisor running\n\
                 Log: off (this service was installed before xcb kept a log)\n"
            )
        );
        let absent = super::service_text(
            &Status {
                installed: false,
                registered: false,
                supervisor_running: false,
                supervisor_health: xcb_runtime::habitat_service::SupervisorHealth {
                    state: xcb_runtime::habitat_service::HealthState::Stopped,
                    heartbeat_age_seconds: None,
                },
                watchdog_enabled: false,
                supervisor_watched: false,
                service: None,
                log: None,
            },
            style,
        );
        assert_eq!(absent, "○ Doesn't start at login · ○ supervisor idle\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn project_memory_and_program_commands_require_explicit_scope() {
        use clap::Parser;
        assert!(
            super::Cli::try_parse_from([
                "xcb",
                "projects",
                "configure",
                "project_a",
                "Maintain parser",
                "--tasks",
                "10",
                "--hours",
                "24"
            ])
            .is_ok()
        );
        for invalid in ["0", "101"] {
            assert!(
                super::Cli::try_parse_from([
                    "xcb",
                    "projects",
                    "configure",
                    "project_a",
                    "Goal",
                    "--tasks",
                    invalid,
                    "--hours",
                    "24"
                ])
                .is_err()
            );
        }
        assert!(super::Cli::try_parse_from(["xcb", "projects", "resume", "project_a"]).is_err());
        assert!(super::Cli::try_parse_from(["xcb", "memory", "promote", "task_a"]).is_err());
        assert!(
            super::Cli::try_parse_from([
                "xcb",
                "schedules",
                "program",
                "project_a",
                "planner.json",
                "--every",
                "3600"
            ])
            .is_ok()
        );
        assert!(super::Cli::try_parse_from(["xcb", "service", "plan"]).is_ok());
    }

    #[test]
    fn tasks_list_matches_the_bare_tasks_listing() {
        use clap::Parser;
        assert!(super::Cli::try_parse_from(["xcb", "tasks"]).is_ok());
        assert!(super::Cli::try_parse_from(["xcb", "tasks", "list"]).is_ok());
    }

    #[test]
    fn managed_program_commands_require_explicit_bounded_call_authority() {
        use clap::Parser;
        for calls in ["1", "8"] {
            assert!(
                super::Cli::try_parse_from([
                    "xcb",
                    "backlog",
                    "program",
                    "project_a",
                    "program.json",
                    "--managed-calls",
                    calls,
                    "--inputs",
                    "inputs.json",
                    "--id",
                    "operation_a"
                ])
                .is_ok()
            );
            assert!(
                super::Cli::try_parse_from([
                    "xcb",
                    "schedules",
                    "program",
                    "project_a",
                    "program.json",
                    "--managed-calls",
                    calls,
                    "--every",
                    "3600"
                ])
                .is_ok()
            );
        }
        for calls in ["0", "9", "-1", "unlimited"] {
            assert!(
                super::Cli::try_parse_from([
                    "xcb",
                    "backlog",
                    "program",
                    "project_a",
                    "program.json",
                    "--managed-calls",
                    calls
                ])
                .is_err()
            );
        }
        assert!(super::Cli::try_parse_from(["xcb", "backlog", "program", "program.json"]).is_err());
        assert!(
            super::Cli::try_parse_from(["xcb", "backlog", "program-status", "task_a", "--json"])
                .is_ok()
        );
    }

    #[test]
    fn automatic_route_notice_does_not_echo_route_record_data() {
        assert_eq!(
            automatic_route_notice("Warning: usage limits block private-provider-metadata"),
            "Usage limits rule out a higher-ranked model; using the best one available now."
        );
        assert_eq!(
            automatic_route_notice("private-account-and-model-metadata"),
            "Picked an account and model automatically."
        );
    }

    #[test]
    fn habitat_commands_require_revision_and_bound_schedule_intervals() {
        use clap::Parser;
        assert!(super::Cli::try_parse_from(["xcb", "backlog", "release", "task_a"]).is_err());
        assert!(
            super::Cli::try_parse_from(["xcb", "backlog", "release", "task_a", "--revision", "4"])
                .is_ok()
        );
        assert!(
            super::Cli::try_parse_from([
                "xcb",
                "schedules",
                "add",
                "project_a",
                "check progress",
                "--every",
                "59"
            ])
            .is_err()
        );
        assert!(
            super::Cli::try_parse_from([
                "xcb",
                "schedules",
                "add",
                "project_a",
                "check progress",
                "--every",
                "3600"
            ])
            .is_ok()
        );
        assert!(
            super::Cli::try_parse_from([
                "xcb",
                "backlog",
                "add",
                "project_a",
                "review",
                "--priority",
                "10"
            ])
            .is_err()
        );
        assert!(
            super::Cli::try_parse_from(["xcb", "backlog", "--conversation", "project_a", "--json"])
                .is_ok()
        );
        assert!(super::Cli::try_parse_from(["xcb", "attention", "--json"]).is_ok());
    }

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

    fn joined((added, next): (String, String)) -> String {
        format!("{added}\nNext: {next}")
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
                command: Some(AccountCommand::ImportCodex { source, account: None }),
            }) if source == std::path::Path::new("/private/source/auth.json")
        ));
        let cli = Cli::try_parse_from([
            "xcb",
            "accounts",
            "import-codex",
            "--source",
            "/private/source/auth.json",
            "--account",
            "a_existing",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::Accounts {
                command: Some(AccountCommand::ImportCodex { source, account: Some(account) }),
            }) if source == std::path::Path::new("/private/source/auth.json") && account == "a_existing"
        ));
        assert!(Cli::try_parse_from(["xcb", "accounts", "import-codex"]).is_err());
        assert!(
            Cli::try_parse_from(["xcb", "accounts", "import-codex", "--account", "a_existing"])
                .is_err()
        );
        assert!(
            Cli::try_parse_from(["xcb", "accounts", "import-codex", "--label", "Work account"])
                .is_err()
        );
    }

    #[test]
    fn internal_commands_stay_hidden() {
        assert!(matches!(
            Cli::try_parse_from(["xcb", "managed-daemon"])
                .unwrap()
                .command,
            Some(Commands::ManagedDaemon)
        ));
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
            json!({"version":1,"account":"a_public_routing_id","sourcePreserved":true}),
        );
    }

    #[test]
    fn json_run_output_includes_its_resumable_session_id() {
        let mut result = runner::Outcome {
            tool_calls: Some(0),
            text_attention: false,
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
    fn a_run_without_a_reply_or_changes_fails_with_a_next_step() {
        use xcb_core::{
            policy::{EffectState, TurnFacts},
            session::State,
        };
        let mut result = runner::Outcome {
            tool_calls: Some(1),
            text_attention: false,
            diagnostic: None,
            text: " \n".into(),
            facts: TurnFacts {
                terminal: Terminal::Completed,
                joined: true,
                effects: EffectState::None,
                pending_attention: false,
                failure: None,
            },
            state: State::Idle,
        };
        let session = Id::new("s_silent").unwrap();
        assert_eq!(run_exit_code(&result), 1);
        let output = run_output(&session, &result);
        assert_eq!(output["outcome"]["terminal"], "completed");
        assert_eq!(output["outcome"]["failure"], "no_reply");
        // The recorded facts are unchanged; only the report names the gap.
        assert_eq!(result.facts.failure, None);
        let error = no_reply_error(Provider::Claude, &session);
        assert_eq!(
            ux::next_step(&error).as_deref(),
            Some("xcb history s_silent --direct")
        );
        let sentence = ux::sentence(&error);
        assert!(sentence.starts_with("Claude Code ended the turn without a reply or file changes"));
        assert!(sentence.contains("--model"));
        // Settled file changes without a reply still succeed.
        result.facts.effects = EffectState::Settled;
        assert_eq!(run_exit_code(&result), 0);
        assert_eq!(
            run_output(&session, &result)["outcome"]["failure"],
            json!(null)
        );
    }

    #[test]
    fn json_run_output_marks_text_cut_to_the_route_limit() {
        let mut result = runner::Outcome {
            tool_calls: Some(0),
            text_attention: false,
            diagnostic: None,
            text: "é".repeat(xcb_core::MAX_TEXT_BYTES),
            facts: xcb_core::policy::TurnFacts {
                terminal: Terminal::Completed,
                joined: true,
                effects: xcb_core::policy::EffectState::None,
                pending_attention: false,
                failure: None,
            },
            state: xcb_core::session::State::Idle,
        };
        let session = Id::new("s_long").unwrap();
        let output = run_output(&session, &result);
        assert_eq!(output["textTruncated"], true);
        let text = output["text"].as_str().unwrap();
        assert!(text.len() <= xcb_core::MAX_TEXT_BYTES);
        assert!(result.text.starts_with(text));
        result.text = "short".into();
        let output = run_output(&session, &result);
        assert!(output.get("textTruncated").is_none());
        assert_eq!(output["text"], "short");
    }

    #[test]
    fn global_options_do_not_hide_the_advanced_screen() {
        let words = |args: &[&str]| {
            let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
            command_words(&args)
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            words(&["xcb", "--state", "/s", "--json", "advanced"]),
            ["advanced"]
        );
        assert_eq!(
            words(&["xcb", "--cwd=/w", "help", "advanced"]),
            ["help", "advanced"]
        );
        assert_eq!(words(&["xcb", "accounts"]), ["accounts"]);
    }

    #[test]
    fn verify_failures_name_the_broken_step_in_plain_words() {
        let id = Id::new("t_fixture").unwrap();
        let error = verify_failure(
            &id,
            Error::Conflict("managed receipt chain is missing a prior revision"),
        );
        assert_eq!(
            ux::sentence(&error),
            "Task t_fixture failed verification: a step in the middle of its record is missing."
        );
        let other = verify_failure(&id, Error::Unavailable("managed task not found"));
        assert_eq!(ux::sentence(&other), "Managed task not found.");
    }

    #[test]
    fn headless_success_requires_completed_joined_settled_idle_outcome() {
        use xcb_core::{
            policy::{EffectState, Failure, TurnFacts},
            session::State,
        };
        let mut result = runner::Outcome {
            tool_calls: Some(0),
            text_attention: false,
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
        assert!(joined(account.added_message()).ends_with("xcb accounts login a_codex"));
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
        let first_output = joined(PublicAccount::from(&first).added_message());
        let second_output = joined(PublicAccount::from(&second).added_message());
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

    #[cfg(unix)]
    #[test]
    fn update_reentry_preserves_command_status_and_signal_exit_codes() {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(
            update_reentry_exit_code(std::process::ExitStatus::from_raw(0)),
            0
        );
        assert_eq!(
            update_reentry_exit_code(std::process::ExitStatus::from_raw(7 << 8)),
            7
        );
        assert_eq!(
            update_reentry_exit_code(std::process::ExitStatus::from_raw(2)),
            130
        );
        assert_eq!(
            update_reentry_exit_code(std::process::ExitStatus::from_raw(15)),
            143
        );
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
            matches!(cli.command, Some(Commands::Update { command: Some(UpdateCommand::Install { version: Some(version), quiet: false, allow_downgrade: false }) }) if version == "0.5.0")
        );
        let cli = Cli::try_parse_from(["xcb", "upgrade", "0.5.0"]).unwrap();
        assert!(
            matches!(cli.command, Some(Commands::Upgrade { version: Some(version), quiet: false, allow_downgrade: false }) if version == "0.5.0")
        );
        let cli = Cli::try_parse_from(["xcb", "upgrade", "0.5.0", "--allow-downgrade"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::Upgrade {
                allow_downgrade: true,
                ..
            })
        ));
        let cli = Cli::try_parse_from(["xcb", "update", "disable"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::Update {
                command: Some(UpdateCommand::Disable)
            })
        ));
        assert!(Cli::try_parse_from(["xcb", "update", "enable", "--policy", "project"]).is_err());
    }

    #[test]
    fn managed_task_cli_lists_and_inspects_without_exposing_daemon_controls() {
        let cli = Cli::try_parse_from(["xcb", "conversations"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::Conversations { new: false })
        ));
        let cli = Cli::try_parse_from(["xcb", "conversations", "--new", "--json"]).unwrap();
        assert!(cli.json);
        assert!(matches!(
            cli.command,
            Some(Commands::Conversations { new: true })
        ));
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
                command: Some(TaskCommand::Messages { id, after: 4, limit: 64 })
            }) if id.as_str() == "t_example"
        ));
        let cli =
            Cli::try_parse_from(["xcb", "tasks", "messages", "t_example", "--limit", "5"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::Tasks {
                command: Some(TaskCommand::Messages { limit: 5, .. })
            })
        ));
        for limit in ["0", "65"] {
            assert!(
                Cli::try_parse_from(["xcb", "tasks", "messages", "t_example", "--limit", limit])
                    .is_err()
            );
        }
        let cli = Cli::try_parse_from(["xcb", "reflex", "label", "route", "t_example", "frontier"])
            .unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::Reflex {
                command: Some(ReflexCommand::Label { reflex: ReflexName::Route, ref label, .. })
            }) if label == "frontier"
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
    fn terminal_history_and_task_controls_require_explicit_identity() {
        assert!(
            Cli::try_parse_from([
                "xcb",
                "backlog",
                "reply",
                "t_a",
                "answer",
                "--reply-id",
                "reply_a"
            ])
            .is_err()
        );
        assert!(Cli::try_parse_from(["xcb", "tasks", "cancel", "t_a"]).is_err());
        let cli =
            Cli::try_parse_from(["xcb", "tasks", "cancel", "t_a", "--revision", "9"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::Tasks {
                command: Some(TaskCommand::Cancel { revision: 9, .. })
            })
        ));
        assert!(Cli::try_parse_from(["xcb", "rename", "c_a", "New title"]).is_err());
        let cli = Cli::try_parse_from([
            "xcb",
            "rename",
            "c_a",
            "New title",
            "--expected-title",
            "Old title",
        ])
        .unwrap();
        assert!(
            matches!(cli.command, Some(Commands::Rename { direct: false, expected_title, .. }) if expected_title == "Old title")
        );
        assert!(Cli::try_parse_from(["xcb", "history", "c_a", "--limit", "513"]).is_err());
        assert!(Cli::try_parse_from(["xcb", "history", "c_a", "--before", "0"]).is_err());
        let cli =
            Cli::try_parse_from(["xcb", "history", "s_a", "--direct", "--before", "19"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::History {
                direct: true,
                before: Some(19),
                limit: 128,
                ..
            })
        ));
        let cli = Cli::try_parse_from([
            "xcb",
            "backlog",
            "reply",
            "t_a",
            "answer",
            "--revision",
            "7",
            "--reply-id",
            "reply_a",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::Backlog {
                command: Some(habitat::BacklogCommand::Reply {
                    revision: Some(7),
                    reply_id: Some(_),
                    ..
                }),
                ..
            })
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
    fn inbox_controls_preserve_explicit_target_and_retry_identity() {
        let cli = Cli::try_parse_from([
            "xcb",
            "steer",
            "task_target",
            "Keep the existing interface",
            "--id",
            "event_retry",
        ])
        .unwrap();
        assert!(
            matches!(cli.command, Some(Commands::Steer { task, text, id: Some(id) })
            if task.as_str() == "task_target" && text == "Keep the existing interface" && id.as_str() == "event_retry")
        );
        let cli = Cli::try_parse_from([
            "xcb",
            "watch",
            "task_target",
            "task_source",
            "--id",
            "watch_retry",
        ])
        .unwrap();
        assert!(
            matches!(cli.command, Some(Commands::Watch { target, source, id: Some(id) })
            if target.as_str() == "task_target" && source.as_str() == "task_source" && id.as_str() == "watch_retry")
        );
        let cli = Cli::try_parse_from([
            "xcb",
            "inbox",
            "--task",
            "task_target",
            "--before",
            "42",
            "--limit",
            "3",
            "--json",
        ])
        .unwrap();
        assert!(cli.json);
        assert!(
            matches!(cli.command, Some(Commands::Inbox { task: Some(task), conversation: None, before: Some(42), limit: 3 })
            if task.as_str() == "task_target")
        );
    }

    #[test]
    fn inbox_filters_and_pagination_fail_closed() {
        for args in [
            vec![
                "xcb",
                "inbox",
                "--task",
                "task_a",
                "--conversation",
                "conv_a",
            ],
            vec!["xcb", "inbox", "--before", "0"],
            vec!["xcb", "inbox", "--before", "9223372036854775808"],
            vec!["xcb", "inbox", "--limit", "0"],
            vec!["xcb", "inbox", "--limit", "257"],
            vec!["xcb", "steer", "task_a"],
            vec!["xcb", "watch", "task_a"],
        ] {
            assert!(Cli::try_parse_from(args).is_err());
        }
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

    #[test]
    fn workspaces_and_upgrade_plan_commands_parse() {
        use super::workspaces::WorkspaceCommand;
        let cli = Cli::try_parse_from(["xcb", "workspaces"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::Workspaces { command: None })
        ));
        let cli = Cli::try_parse_from(["xcb", "workspaces", "list", "--json"]).unwrap();
        assert!(cli.json);
        assert!(matches!(
            cli.command,
            Some(Commands::Workspaces {
                command: Some(WorkspaceCommand::List)
            })
        ));
        let cli =
            Cli::try_parse_from(["xcb", "workspaces", "add", "../repo", "--name", "xcb"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::Workspaces { command: Some(WorkspaceCommand::Add { dir, name: Some(name) }) })
                if dir == std::path::Path::new("../repo") && name == "xcb"
        ));
        for (verb, expected) in [("hide", "old"), ("show", "old")] {
            let cli = Cli::try_parse_from(["xcb", "workspaces", verb, expected]).unwrap();
            assert!(matches!(
                cli.command,
                Some(Commands::Workspaces {
                    command: Some(WorkspaceCommand::Hide { scope } | WorkspaceCommand::Show { scope })
                }) if scope == expected
            ));
        }
        let cli = Cli::try_parse_from(["xcb", "workspaces", "why", "t_example"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::Workspaces { command: Some(WorkspaceCommand::Why { task }) })
                if task.as_str() == "t_example"
        ));
        let cli = Cli::try_parse_from([
            "xcb",
            "workspaces",
            "audit",
            "--stale-hours",
            "6",
            "--stranded-only",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::Workspaces {
                command: Some(WorkspaceCommand::Audit {
                    stale_hours: 6,
                    stranded_only: true
                })
            })
        ));
        let cli = Cli::try_parse_from(["xcb", "workspaces", "conflicts", "--json"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::Workspaces {
                command: Some(WorkspaceCommand::Conflicts)
            })
        ));
        assert!(Cli::try_parse_from(["xcb", "workspaces", "hide"]).is_err());
        assert!(Cli::try_parse_from(["xcb", "workspaces", "why"]).is_err());
        let cli = Cli::try_parse_from(["xcb", "doctor", "--upgrade-plan"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::Doctor {
                upgrade_plan: true,
                provider: None,
                executable: None,
                qualify_sandbox: false,
            })
        ));
        let cli =
            Cli::try_parse_from(["xcb", "doctor", "--provider", "claude", "--qualify-sandbox"])
                .unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::Doctor {
                qualify_sandbox: true,
                provider: Some(Provider::Claude),
                ..
            })
        ));
        assert!(
            Cli::try_parse_from(["xcb", "doctor", "--qualify-sandbox", "--upgrade-plan"]).is_err()
        );
        let cli = Cli::try_parse_from(["xcb", "sandbox-probe", "loopback"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::SandboxProbe { args }) if args == ["loopback"]
        ));
        assert!(
            Cli::try_parse_from(["xcb", "doctor", "--upgrade-plan", "--provider", "claude"])
                .is_err()
        );
        let cli = Cli::try_parse_from(["xcb", "doctor"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::Doctor {
                upgrade_plan: false,
                ..
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
        assert!(ux::ROOT_HELP.contains("interactive terminal surface"));
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
