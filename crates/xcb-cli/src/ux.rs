//! The Hraness CLI style contract for xcb: symbols, color, audience, `Next:`
//! hints and human error output.
//!
//! TODO(df-0.8): use detectAudience — replace `detect_audience`, `Style` and
//! the error renderer with the `hraness-cli-kit` crate from desktop-foundation
//! 0.8.0 once it ships. The rules below are copied from that contract.

use std::io::{IsTerminal, Write};
use xcb_runtime::Error;

/// Who reads this process's output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Audience {
    Human,
    Agent,
    Quiet,
}

/// Exact agent markers. Prefixes never count: `CODEX_HOME` and
/// `DEVIN_API_KEY` are human configuration.
const AGENT_MARKERS: [&str; 6] = [
    "AI_AGENT",
    "CLAUDECODE",
    "CODEX_SANDBOX",
    "CODEX_SANDBOX_NETWORK_DISABLED",
    "CURSOR_AGENT",
    "GEMINI_CLI",
];

pub fn detect_audience(env: &dyn Fn(&str) -> Option<String>, stderr_is_tty: bool) -> Audience {
    match env("HRANESS_AUDIENCE").as_deref() {
        Some("human") => return Audience::Human,
        Some("agent") => return Audience::Agent,
        Some("quiet" | "off") => return Audience::Quiet,
        _ => {}
    }
    if AGENT_MARKERS
        .iter()
        .any(|name| env(name).is_some_and(|value| !value.is_empty()))
    {
        return Audience::Agent;
    }
    if stderr_is_tty {
        Audience::Human
    } else {
        Audience::Quiet
    }
}

fn process_env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

pub fn audience() -> Audience {
    detect_audience(&process_env, std::io::stderr().is_terminal())
}

/// The shared CLI symbol set. Not every command uses every symbol.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Symbol {
    Ok,
    Fail,
    Warn,
    Next,
    On,
    Off,
    Skip,
}

/// How one stream renders symbols.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Style {
    pub color: bool,
    pub ascii: bool,
}

impl Style {
    pub fn detect(env: &dyn Fn(&str) -> Option<String>, is_tty: bool) -> Self {
        let set = |name: &str| env(name).is_some_and(|value| !value.is_empty());
        let dumb = env("TERM").as_deref() == Some("dumb");
        let color = if env("FORCE_COLOR").as_deref() == Some("1") {
            true
        } else {
            is_tty && !dumb && !set("NO_COLOR")
        };
        let utf8 = ["LC_ALL", "LC_CTYPE", "LANG"].iter().any(|name| {
            env(name).is_some_and(|value| {
                let value = value.to_ascii_lowercase();
                value.contains("utf-8") || value.contains("utf8")
            })
        });
        let ascii = dumb || !utf8 || env("HRANESS_ASCII").as_deref() == Some("1");
        Self { color, ascii }
    }
    pub fn stdout() -> Self {
        Self::detect(&process_env, std::io::stdout().is_terminal())
    }
    pub fn stderr() -> Self {
        Self::detect(&process_env, std::io::stderr().is_terminal())
    }
    /// The symbol, colored when this stream allows it. Only the symbol is
    /// ever colored, never the sentence.
    pub fn sym(self, symbol: Symbol) -> String {
        let (glyph, ascii, color) = match symbol {
            Symbol::Ok => ("✓", "OK", Some("32")),
            Symbol::Fail => ("✗", "FAIL", Some("31")),
            Symbol::Warn => ("⚠", "WARN", Some("33")),
            Symbol::Next => ("→", "->", Some("2")),
            Symbol::On => ("●", "*", Some("32")),
            Symbol::Off => ("○", "o", None),
            Symbol::Skip => ("–", "-", Some("2")),
        };
        let text = if self.ascii { ascii } else { glyph };
        match color {
            Some(code) if self.color => format!("\x1b[{code}m{text}\x1b[0m"),
            _ => text.to_owned(),
        }
    }
}

/// Print the one `Next:` hint for a human reader. Agents get the next step
/// from `--json` output, and a quiet reader gets nothing.
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
    let mut out = String::with_capacity(text.len() + 1);
    let mut chars = text.chars();
    // The product name stays lowercase at the start of a sentence.
    if text.starts_with("xcb ") {
        out.push_str(text);
    } else if let Some(first) = chars.next() {
        out.extend(first.to_uppercase());
        out.push_str(chars.as_str());
    }
    if !out.ends_with(['.', '?', '!']) {
        out.push('.');
    }
    out
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
        | Error::DevinModelChoices { .. }
        | Error::DevinRpc { .. } => "provider-protocol",
    }
}

/// The human rendering: `✗ sentence` and, when known, `→ next command`.
pub fn render_human(error: &Error, style: Style) -> String {
    let mut out = format!("{} {}", style.sym(Symbol::Fail), sentence(error));
    if let Some(next) = next_step(error) {
        out.push('\n');
        out.push_str(&format!("{} {next}", style.sym(Symbol::Next)));
    }
    out
}

pub fn render_json(error: &Error) -> serde_json::Value {
    serde_json::json!({
        "ok": false,
        "error": {
            "code": code(error),
            "message": sentence(error),
            "next": next_step(error),
        }
    })
}

/// Report a failed command. `--json` or an agent reader gets the error
/// object on stdout; everyone else, and every internal protocol helper whose
/// stdout belongs to its peer, gets the human lines on stderr.
pub fn report_error(error: &Error, json: bool, protocol: bool) {
    if !protocol && (json || audience() == Audience::Agent) {
        let mut stdout = std::io::stdout().lock();
        let _ = writeln!(stdout, "{}", render_json(error));
        return;
    }
    eprintln!("{}", render_human(error, Style::stderr()));
}

/// A closed pipe (`xcb accounts | head -1`) ends output quietly instead of
/// panicking. The workspace forbids `unsafe`, so SIGPIPE stays ignored and a
/// broken-pipe panic from `println!` or a writer's `expect` exits 0 without a
/// trace.
pub fn restore_sigpipe() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let message = info
            .payload()
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| info.payload().downcast_ref::<&str>().copied())
            .unwrap_or("");
        if message.contains("Broken pipe") {
            std::process::exit(0);
        }
        default(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_owned())
        }
    }

    #[test]
    fn audience_follows_the_shared_rule() {
        assert_eq!(detect_audience(&env_of(&[]), true), Audience::Human);
        assert_eq!(detect_audience(&env_of(&[]), false), Audience::Quiet);
        assert_eq!(
            detect_audience(&env_of(&[("CLAUDECODE", "1")]), true),
            Audience::Agent
        );
        assert_eq!(
            detect_audience(&env_of(&[("CLAUDECODE", "")]), true),
            Audience::Human
        );
        assert_eq!(
            detect_audience(&env_of(&[("CODEX_HOME", "/x")]), true),
            Audience::Human
        );
        assert_eq!(
            detect_audience(
                &env_of(&[("HRANESS_AUDIENCE", "human"), ("AI_AGENT", "1")]),
                false
            ),
            Audience::Human
        );
        assert_eq!(
            detect_audience(&env_of(&[("HRANESS_AUDIENCE", "off")]), true),
            Audience::Quiet
        );
    }

    #[test]
    fn style_respects_no_color_term_and_locale() {
        let utf8 = Style::detect(&env_of(&[("LANG", "en_US.UTF-8")]), true);
        assert_eq!(
            utf8,
            Style {
                color: true,
                ascii: false
            }
        );
        assert_eq!(utf8.sym(Symbol::Ok), "\x1b[32m✓\x1b[0m");
        let no_color = Style::detect(&env_of(&[("LANG", "en_US.UTF-8"), ("NO_COLOR", "1")]), true);
        assert_eq!(no_color.sym(Symbol::Fail), "✗");
        let piped = Style::detect(&env_of(&[("LANG", "en_US.UTF-8")]), false);
        assert_eq!(piped.sym(Symbol::Warn), "⚠");
        let dumb = Style::detect(&env_of(&[("LANG", "en_US.UTF-8"), ("TERM", "dumb")]), true);
        assert_eq!(dumb.sym(Symbol::Fail), "FAIL");
        assert_eq!(dumb.sym(Symbol::Next), "->");
        let c_locale = Style::detect(&env_of(&[("LANG", "C")]), false);
        assert_eq!(c_locale.sym(Symbol::On), "*");
        let forced = Style::detect(
            &env_of(&[("LANG", "en_US.UTF-8"), ("FORCE_COLOR", "1")]),
            false,
        );
        assert!(forced.color);
    }

    #[test]
    fn errors_render_one_sentence_and_one_next_step() {
        let plain = Style {
            color: false,
            ascii: false,
        };
        let guided = Error::guided("No account matches \"zz\".", "xcb accounts");
        assert_eq!(
            render_human(&guided, plain),
            "✗ No account matches \"zz\".\n→ xcb accounts"
        );
        assert_eq!(
            guided.to_string(),
            "No account matches \"zz\". Next: xcb accounts"
        );
        let internal = Error::Unavailable("no saved sessions");
        assert_eq!(render_human(&internal, plain), "✗ No saved sessions.");
        let product = Error::Unavailable("xcb doctor found nothing");
        assert_eq!(sentence(&product), "xcb doctor found nothing.");
        assert_eq!(
            render_json(&guided).to_string(),
            r#"{"ok":false,"error":{"code":"unavailable","message":"No account matches \"zz\".","next":"xcb accounts"}}"#
        );
    }
}

/// `xcb --help`: commands grouped by what the reader is doing. Hidden
/// internal and machine-only commands are left out. A unit test checks that
/// every visible command is listed.
pub const ROOT_HELP: &str = "\
xcb routes coding tasks across the Claude, Codex, and Devin subscriptions
you already pay for. Plain `xcb` opens a conversation in the terminal UI.

Usage: xcb [command] [options]

Start here
  setup          Add an account, check the provider and sign in, in one step
  chat           Open the conversation for this folder
  run            Run one task here and print the result
  doctor         Check providers, accounts and unfinished runs

Accounts and models
  accounts       List accounts; add, sign in and manage them
  models         List models; refresh catalogs and set the default
  offers         Show public plan offers (not checked against your account)
  reflex         Inspect and teach how xcb picks models and sorts turns

Conversations and tasks
  conversations  List your conversations
  history        Read a conversation's saved messages
  rename         Rename a conversation
  tasks          Inspect tasks and their messages
  backlog        Manage a conversation's work queue
  steer          Queue guidance for a task's next turn
  watch          Send a task's completion report to another task
  inbox          See guidance and reports and whether they arrived
  attention      Show questions and approvals waiting on you
  schedules      Manage recurring wake-ups
  daemons        Manage always-on project agents (ALGAL daemons)
  projects       Set how much a project may do on its own
  memory         Save notes to a project's local Wordcell vault
  sessions       List direct provider sessions
  resume         Reopen a direct provider session

Other machines
  link           Link this machine to your xcb fleet
  fleet          List your linked devices
  dispatch       Start a task on another device
  send           Send text to an agent on another device
  remote         Steer, cancel or answer work on another device

Setup and maintenance
  service        Start xcb's background supervisor at login (macOS)
  update         Check for updates and set the update policy
  upgrade        Install the latest verified release
  panes          List, check and install terminal panes
  plugins        Turn extensions on or off
  hooks          Run your own programs on lifecycle events
  judge          Set up the optional routing judge
  config         Print the effective configuration as JSON
  recover        Inspect or clean up unfinished runs
  command        Inspect or archive offline command jobs
  generate       Generate text for an app (no tools, no hooks)
  completions    Print shell completions

Options
  --state <dir>  State folder (default: $XCB_STATE or ~/.local/share/xcb)
  --json         Machine-readable output where a command supports it
  --cwd <dir>    Workspace for run, chat and models route (default: .)
  -h, --help     Show help; xcb <command> --help shows a command's help
  -V, --version  Show the version
";

#[cfg(test)]
mod root_help_tests {
    #[test]
    fn root_help_fits_the_terminal() {
        assert!(
            super::ROOT_HELP
                .lines()
                .all(|line| line.chars().count() <= 80)
        );
    }
}
