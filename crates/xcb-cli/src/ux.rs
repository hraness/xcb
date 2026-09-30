//! The Hraness CLI style contract for xcb, on top of `hraness-cli-kit` from
//! desktop-foundation: audience, symbols and color, `Next:` hints, the
//! login-item notice, usage errors and human or JSON error output.

use hraness_cli_kit::permissions::{self, PermissionNeed, ProductRef, presets};
pub use hraness_cli_kit::{Audience, Style, Symbol};
use xcb_runtime::Error;

/// Who reads this process's output: a person, an agent, or a quiet pipe.
pub fn audience() -> Audience {
    hraness_cli_kit::audience::detect_current()
}

/// Set while one command runs another's steps (`xcb setup`), so only the
/// outer command names the next step.
static QUIET_NEXT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Hold back `Next:` hints until the returned guard drops.
pub fn hold_next() -> impl Drop {
    struct Held(bool);
    impl Drop for Held {
        fn drop(&mut self) {
            QUIET_NEXT.store(self.0, std::sync::atomic::Ordering::Relaxed);
        }
    }
    Held(QUIET_NEXT.swap(true, std::sync::atomic::Ordering::Relaxed))
}

const FILES_AND_FOLDERS_PATH: &str = "System Settings › Privacy & Security › Files & Folders";
pub const FILES_AND_FOLDERS_URL: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_FilesAndFolders";

/// The kit's `LOGIN_ITEM` notice, with xcb's reason. Login items only
/// notify, so there is no Enter confirm.
fn login_item_need(why: &str) -> PermissionNeed {
    let mut need = presets::login_item(ProductRef::new("xcb", "xcb"));
    need.why = why.to_owned();
    need
}

/// The notice as a person reads it.
#[cfg(test)]
fn login_item_text(why: &str, style: Style) -> String {
    use hraness_cli_kit::permissions::{NoticeKind, Surface};
    let notice = permissions::render_pre_prompt(
        &login_item_need(why),
        Surface::Cli,
        &hraness_cli_kit::audience::process_env,
    );
    permissions::format_notice(&notice, NoticeKind::PrePrompt, false, style)
}

/// Say the login-item notice before xcb registers a LaunchAgent: text for a
/// person, one JSON line for an agent, nothing for a quiet reader. It never
/// waits for input.
pub fn login_item_notice(why: &str) {
    let _ = permissions::pre_prompt(&login_item_need(why), None, &mut permissions::ProcessIo);
}

/// The SPEC CLI recovery block for a protected folder macOS kept from the
/// launchd-run supervisor. `folder` is named when the log line says which.
pub fn files_and_folders_denial(folder: Option<&str>, style: Style) -> String {
    let (what, turn_on) = match folder {
        Some(folder) => (
            format!("open files in ~/{folder}"),
            format!("under {folder}"),
        ),
        None => (
            "open a project folder".to_owned(),
            "for Documents, Desktop or Downloads".to_owned(),
        ),
    };
    format!(
        "{} xcb can't {what}: macOS access is off for xcb.\n  Turn on xcb {turn_on} in {FILES_AND_FOLDERS_PATH}.\n{} open '{FILES_AND_FOLDERS_URL}'\n",
        style.symbol(Symbol::Fail),
        style.symbol(Symbol::Next)
    )
}

/// Print the one `Next:` hint for a human reader. Agents get the next step
/// from `--json` output, and a quiet reader gets nothing.
pub fn next(command: &str) {
    if audience() == Audience::Human && !QUIET_NEXT.load(std::sync::atomic::Ordering::Relaxed) {
        eprintln!("Next: {command}");
    }
}

/// Error-kind prefixes that are internal taxonomy, not something a person
/// can act on. `--json` keeps the kind as `code`.
const INTERNAL_PREFIXES: [&str; 3] = ["unavailable: ", "conflict: ", "provider protocol error: "];

/// One human sentence for an error: kind prefix dropped, first letter
/// capitalized, ending in a period. A `Next:` suffix is left to `next_step`.
pub fn sentence(error: &Error) -> String {
    let text = match error {
        Error::Guided { message, .. } => message.clone(),
        other => other.to_string(),
    };
    let mut text = text.as_str();
    for prefix in INTERNAL_PREFIXES {
        if let Some(rest) = text.strip_prefix(prefix) {
            text = rest;
        }
    }
    // The product name stays lowercase at the start of a sentence.
    hraness_cli_kit::style::sentence(text, &["xcb "])
}

/// The one next command for an error, when xcb knows it.
pub fn next_step(error: &Error) -> Option<String> {
    match error {
        Error::Guided { next, .. } => next.clone(),
        _ => None,
    }
}

/// The stable `--json` error code for an error's kind.
pub fn code(error: &Error) -> &'static str {
    match error {
        Error::Core(_) => "invalid-input",
        Error::Io(_) => "local-io",
        Error::LaunchNotStarted(_) => "provider-not-started",
        Error::CleanupUnproven => "cleanup-unproven",
        Error::Database(_) => "local-database",
        Error::Json(_) => "invalid-record",
        Error::PrivateState => "private-state",
        Error::Conflict(_) => "conflict",
        Error::Unavailable(_) | Error::Message(_) | Error::Guided { .. } => "unavailable",
        Error::Protocol(_)
        | Error::CodexRpc { .. }
        | Error::CodexNotification { .. }
        | Error::DevinModelChoices { .. }
        | Error::DevinRpc { .. } => "provider-protocol",
    }
}

/// The kit's error form: one sentence, the one next command, and the
/// stable code for `--json`.
pub fn cli_error(error: &Error) -> hraness_cli_kit::CliError {
    let mut out = hraness_cli_kit::CliError::new(code(error), sentence(error));
    if let Some(next) = next_step(error) {
        out = out.with_next(next);
    }
    out
}

/// Report a failed command and return its exit code. `--json` or an agent
/// reader gets the error object on stdout; everyone else, and every internal
/// protocol helper whose stdout belongs to its peer, gets the human lines on
/// stderr.
pub fn report_error(error: &Error, json: bool, protocol: bool) -> i32 {
    let error = cli_error(error);
    if protocol {
        return error.report(false, Audience::Quiet);
    }
    error.report(json, audience())
}

/// The command line, with help wrapped at 100 columns at every level.
pub fn command() -> clap::Command {
    hraness_cli_kit::clap::cap_help_width(<crate::Cli as clap::CommandFactory>::command(), 100)
}

/// Handle a clap parse failure: help and version on stdout with exit 0,
/// otherwise `✗ Unknown command "x". Did you mean "y"?` and the help to
/// read, exit 2, or the JSON error on stdout for `--json` and agents.
pub fn clap_failure(error: clap::Error, root: &clap::Command, args: &[String]) -> i32 {
    let options = hraness_cli_kit::clap::UsageOptions::default()
        .cli("xcb")
        .alias("login", "accounts login")
        .alias("add", "accounts add");
    hraness_cli_kit::clap::exit_on_parse_error(error, root, args, &options)
}

/// A closed pipe (`xcb accounts | head -1`) ends output quietly instead of
/// panicking. The workspace forbids `unsafe`, so SIGPIPE stays ignored and a
/// broken-pipe panic from `println!` or a writer's `expect` exits 0 without a
/// trace.
pub fn restore_sigpipe() {
    hraness_cli_kit::style::exit_quietly_on_broken_pipe();
}

/// Write `text` to stdout, ending quietly on a closed pipe (`| head -1`).
pub fn write_stdout(text: &str) {
    hraness_cli_kit::style::write_stdout(text);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_render_one_sentence_and_one_next_step() {
        let guided = Error::guided("No account matches \"zz\".", "xcb accounts");
        assert_eq!(
            cli_error(&guided).render_human(Style::PLAIN),
            "✗ No account matches \"zz\".\n→ xcb accounts\n"
        );
        assert_eq!(
            guided.to_string(),
            "No account matches \"zz\". Next: xcb accounts"
        );
        let internal = Error::Unavailable("no saved sessions");
        assert_eq!(
            cli_error(&internal).render_human(Style::PLAIN),
            "✗ No saved sessions.\n"
        );
        let product = Error::Unavailable("xcb doctor found nothing");
        assert_eq!(sentence(&product), "xcb doctor found nothing.");
        assert_eq!(
            cli_error(&guided).render_json(),
            r#"{"ok":false,"error":{"code":"unavailable","message":"No account matches \"zz\".","next":"xcb accounts"}}"#
        );
    }

    #[test]
    fn every_help_line_fits_100_columns() {
        let over = hraness_cli_kit::clap::help_lines_over(&command(), 100);
        assert!(
            over.is_empty(),
            "help lines over 100 columns:\n{}",
            over.iter()
                .map(|(path, line)| format!("{path}: {line}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
}

/// What plain `xcb` prints when it can't open the chat (no terminal): at
/// most 25 lines on stdout, exit 0.
pub fn start_text() -> String {
    format!(
        "\
Excalibur (xcb) routes coding tasks across the Claude, Codex, and Devin
subscriptions you already pay for.

Start here
  xcb setup claude       Choose or add a Claude account and sign in
  xcb                    Open your thread (needs a terminal)
  xcb run -p \"<task>\"    Run one task here and print the result
  xcb doctor             Check providers, accounts and unfinished runs

Everyday
  xcb accounts           See your accounts and how much each has left
  xcb conversations      List your thread and project views
  xcb attention          Show questions and approvals waiting on you

All commands: xcb --help · Advanced: xcb help advanced
xcb {}
",
        env!("CARGO_PKG_VERSION")
    )
}

/// `xcb --help`: commands grouped by what the reader is doing, at most 60
/// lines. Hidden internal commands are left out, and the rest that aren't
/// here are in [`ADVANCED`]; a test keeps both lists in step with the parser.
pub const ROOT_HELP: &str = "\
Excalibur (xcb) routes coding tasks across the Claude, Codex, and Devin
subscriptions you already pay for. Plain `xcb` opens your thread from any
directory; xcb picks each task's project directory and says which.

Usage: xcb [command] [options]

Start here
  setup          Choose or add an account, check the provider and sign in
  chat           Open your thread; --new starts a project view for this folder
  run            Run one task here and print the result
  doctor         Check providers, accounts and unfinished runs

Accounts and models
  accounts       List accounts; add, sign in and manage them
  models         List models; refresh catalogs and set the default
  routing        Show which models each kind of task prefers; exclude routes
  offers         Show public plan offers (not checked against your account)

Conversations and tasks
  conversations  List your thread and project views
  workspaces     List, add and hide the project folders the thread picks from
  history        Read a conversation's saved messages
  rename         Rename a conversation
  tasks          Inspect tasks and their messages
  backlog        Manage a conversation's work queue
  steer          Queue guidance for a task's next turn
  watch          Send a task's completion report to another task
  inbox          See guidance and reports and whether they arrived
  attention      Show questions and approvals waiting on you
  schedules      Manage recurring wake-ups
  sessions       List provider sessions; discover and import recent history
  resume         Reopen a direct provider session

Setup
  service        Start xcb's background supervisor at login (macOS)
  resources      Inspect memory and disk pressure; enable launch limits
  tools          Manage browser, computer and other host tool connections
  update         Check for updates and set the update policy
  upgrade        Install the latest verified release
  completions    Print shell completions

Options
  --state <dir>  State folder (default: $XCB_STATE or ~/.local/share/xcb)
  --json         Machine-readable output where a command supports it
  --cwd <dir>    Project hint for the thread; the exact folder for run,
                 chat --new and models route (default: .)
  -h, --help     Show help; xcb <command> --help shows a command's help
  -V, --version  Show the version

More commands (other machines, project agents, extensions): xcb help advanced
";

/// `xcb help advanced`: the commands root help leaves out.
pub const ADVANCED: &str = "\
Advanced xcb commands. Each one's --help says more.

Other machines
  link           Link this machine to your xcb fleet
  fleet          List your linked devices
  dispatch       Start a task on another device
  send           Send text to an agent on another device
  remote         Steer, cancel or answer work on another device

Project agents
  context        Inspect saved source chunks and replay research programs
  daemons        Manage always-on project agents (ALGAL daemons)
  projects       Set how much a project may do on its own
  memory         Save notes to a project's local Wordcell vault
  reflex         Inspect and teach how xcb picks models and sorts turns

Extensions
  panes          List, check and install terminal panes
  plugins        Turn extensions on or off
  hooks          Run your own programs on lifecycle events
  judge          Set up the optional routing judge

Maintenance
  config         Print the effective configuration as JSON
  recover        Inspect or clean up unfinished runs
  command        Inspect or archive offline command jobs
  generate       Generate text for an app (no tools, no hooks)
";

#[cfg(test)]
mod root_help_tests {
    #[test]
    fn help_screens_fit_the_contract() {
        for (name, text, max_lines) in [
            ("start", super::start_text(), 25),
            ("root help", super::ROOT_HELP.to_owned(), 60),
            ("help advanced", super::ADVANCED.to_owned(), 60),
        ] {
            assert!(text.lines().count() <= max_lines, "{name}: too many lines");
            for line in text.lines() {
                assert!(line.chars().count() <= 80, "{name}: {line}");
            }
        }
    }
}

#[cfg(test)]
mod notice_tests {
    use super::*;

    #[test]
    fn login_item_notice_follows_the_template() {
        assert_eq!(
            login_item_text(
                "It checks once a day for a verified xcb release, until you run xcb update disable.",
                Style::PLAIN
            ),
            "🔐 macOS will show a notice that xcb can open at login.\n   It checks once a day for a verified xcb release, until you run xcb update disable. Turn it off any time in System Settings › General › Login Items & Extensions.\n"
        );
        assert!(login_item_text("x", Style::ASCII).starts_with("NOTE macOS"));
    }

    #[test]
    fn denial_block_names_the_folder_the_pane_and_the_link() {
        assert_eq!(
            files_and_folders_denial(Some("Documents"), Style::PLAIN),
            "✗ xcb can't open files in ~/Documents: macOS access is off for xcb.\n  Turn on xcb under Documents in System Settings › Privacy & Security › Files & Folders.\n→ open 'x-apple.systempreferences:com.apple.preference.security?Privacy_FilesAndFolders'\n"
        );
    }
}
