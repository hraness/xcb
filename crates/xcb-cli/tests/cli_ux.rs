//! Golden output for the CLI style contract: help, version, empty states,
//! errors (human, ASCII and JSON), `Next:` hints and closed pipes.
//!
//! Every run uses a private temporary state root and HOME, and an empty
//! PATH so no real provider (and no browser sign-in) can ever start.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("xcb-cli-ux-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("home")).unwrap();
        std::fs::create_dir_all(root.join("bin")).unwrap();
        Self { root }
    }
    fn state(&self) -> PathBuf {
        self.root.join("state")
    }
    fn command(&self, args: &[&str], env: &[(&str, &str)]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_xcb"));
        command
            .env_clear()
            .env("HOME", self.root.join("home"))
            .env("PATH", self.root.join("bin"))
            .env("LANG", "en_US.UTF-8")
            .arg("--state")
            .arg(self.state())
            .args(args)
            .stdin(Stdio::null());
        for (key, value) in env {
            command.env(key, value);
        }
        command
    }
    fn run(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        self.command(args, env).output().unwrap()
    }
    fn add_claude(&self) -> String {
        let output = self.run(&["--json", "accounts", "add", "claude"], &[]);
        assert!(output.status.success(), "{output:?}");
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        value["id"].as_str().unwrap().to_owned()
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).unwrap()
}

fn plain(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_xcb"))
        .env_clear()
        .env("PATH", "/nonexistent")
        .env("HOME", std::env::temp_dir())
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn version_prints_name_and_version() {
    let output = plain(&["--version"]);
    assert!(output.status.success());
    assert_eq!(
        text(&output.stdout),
        format!("xcb {}\n", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn every_command_help_exits_zero() {
    for args in [
        &["--help"][..],
        &["accounts", "--help"],
        &["accounts", "login", "--help"],
        &["help", "accounts"],
        &["doctor", "--help"],
        &["service", "--help"],
    ] {
        let output = plain(args);
        assert!(output.status.success(), "{args:?}: {output:?}");
        assert!(!output.stdout.is_empty(), "{args:?}");
    }
    let root = text(&plain(&["--help"]).stdout);
    assert!(root.contains("xcb accounts login <account>"), "{root}");
    assert!(!root.contains("<account-id>"), "{root}");
}

#[test]
fn empty_accounts_point_at_the_first_command() {
    let sandbox = Sandbox::new("empty");
    let quiet = sandbox.run(&["accounts"], &[]);
    assert!(quiet.status.success(), "{quiet:?}");
    assert_eq!(text(&quiet.stdout), "No accounts yet.\n");
    // A pipe is a quiet reader: no hint.
    assert_eq!(text(&quiet.stderr), "");
    let human = sandbox.run(&["accounts"], &[("HRANESS_AUDIENCE", "human")]);
    assert_eq!(text(&human.stderr), "Next: xcb accounts add claude\n");
}

#[test]
fn added_account_prints_a_check_and_the_sign_in_hint() {
    let sandbox = Sandbox::new("added");
    let output = sandbox.run(
        &["accounts", "add", "claude"],
        &[("HRANESS_AUDIENCE", "human")],
    );
    assert!(output.status.success(), "{output:?}");
    let stdout = text(&output.stdout);
    assert!(stdout.starts_with("✓ Added claude/a_"), "{stdout}");
    let id = stdout.trim_end().rsplit(' ').next().unwrap();
    assert_eq!(
        text(&output.stderr),
        format!("Next: xcb accounts login {id}\n")
    );
    let ascii = sandbox.run(&["accounts", "add", "codex"], &[("TERM", "dumb")]);
    assert!(text(&ascii.stdout).starts_with("OK Added codex/a_"));
}

#[test]
fn unknown_account_is_one_sentence_and_one_next_step() {
    let sandbox = Sandbox::new("unknown");
    sandbox.add_claude();
    let output = sandbox.run(&["accounts", "login", "zz"], &[]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(text(&output.stdout), "");
    assert_eq!(
        text(&output.stderr),
        "✗ No account matches \"zz\".\n→ xcb accounts\n"
    );
    let dumb = sandbox.run(&["accounts", "login", "zz"], &[("TERM", "dumb")]);
    assert_eq!(
        text(&dumb.stderr),
        "FAIL No account matches \"zz\".\n-> xcb accounts\n"
    );
    let no_color = sandbox.run(&["accounts", "login", "zz"], &[("NO_COLOR", "1")]);
    assert!(!text(&no_color.stderr).contains('\x1b'));
    let json = sandbox.run(&["--json", "accounts", "login", "zz"], &[]);
    assert_eq!(json.status.code(), Some(1));
    let value: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"]["code"], "unavailable");
    assert_eq!(value["error"]["message"], "No account matches \"zz\".");
    assert_eq!(value["error"]["next"], "xcb accounts");
    let agent = sandbox.run(&["accounts", "login", "zz"], &[("CLAUDECODE", "1")]);
    let value: serde_json::Value = serde_json::from_slice(&agent.stdout).unwrap();
    assert_eq!(value["error"]["next"], "xcb accounts");
}

#[test]
fn a_shortened_id_from_the_table_resolves_and_login_checks_the_provider_first() {
    let sandbox = Sandbox::new("prefix");
    let id = sandbox.add_claude();
    // The accounts table cuts ids to ten characters plus `…`.
    let shown: String = id.chars().take(10).chain(['…']).collect();
    let output = sandbox.run(&["accounts", "login", &shown], &[]);
    assert_eq!(output.status.code(), Some(1));
    // It resolved (not "No account matches") and went straight to the
    // provider check instead of a raw missing-file error.
    assert_eq!(
        text(&output.stderr),
        "→ Checking Claude Code first (the same check as xcb doctor --provider claude).\n\
         ✗ xcb can't find `claude` on your PATH. Install it, or set XCB_CLAUDE to its absolute path.\n\
         → xcb doctor --provider claude\n"
    );
    assert!(!Path::new(&sandbox.state().join("providers/claude.json")).exists());
}

#[test]
fn an_ambiguous_prefix_lists_the_candidates() {
    let sandbox = Sandbox::new("ambiguous");
    let first = sandbox.add_claude();
    let second = sandbox.add_claude();
    let output = sandbox.run(&["accounts", "disable", "a_"], &[]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = text(&output.stderr);
    assert!(
        stderr.starts_with("✗ \"a_\" matches 2 accounts: "),
        "{stderr}"
    );
    assert!(
        stderr.contains(&first) && stderr.contains(&second),
        "{stderr}"
    );
    assert!(
        stderr.ends_with("Type more of the id.\n→ xcb accounts\n"),
        "{stderr}"
    );
}

#[test]
fn a_closed_pipe_exits_quietly() {
    let sandbox = Sandbox::new("pipe");
    let mut child = sandbox
        .command(&["completions", "zsh"], &[])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Close the read end before the large completion script is written.
    drop(child.stdout.take());
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(!text(&output.stderr).contains("panicked"), "{output:?}");
}
