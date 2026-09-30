//! Golden output for the CLI style contract: help, version, empty states,
//! errors (human, ASCII and JSON), `Next:` hints and closed pipes.
//!
//! Every run uses a private temporary state root and HOME, and an empty
//! PATH so no real provider (and no browser sign-in) can ever start.

// These tests drive Unix permission bits, symlinks, or /bin/sh fixtures;
// the Windows custody rules are covered by the platform tests.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    fn new(name: &str) -> Self {
        let root = xcb_core::canonical(std::env::temp_dir())
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
        self.add(&["claude"])
    }
    /// `xcb accounts add <args>`; returns the new account id.
    fn add(&self, args: &[&str]) -> String {
        let mut all = vec!["--json", "accounts", "add"];
        all.extend_from_slice(args);
        let output = self.run(&all, &[]);
        assert!(output.status.success(), "{output:?}");
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        value["id"].as_str().unwrap().to_owned()
    }
    /// Run with `input` piped on stdin.
    fn run_with_input(&self, args: &[&str], input: &[u8]) -> Output {
        use std::io::Write as _;
        let mut child = self
            .command(args, &[])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(input).unwrap();
        child.wait_with_output().unwrap()
    }
    /// A provider on the sandbox PATH that answers `--version` with
    /// `version_line`. xcb finds it, but it is not a build xcb supports, so
    /// nothing ever runs it.
    fn fake_provider(&self, name: &str, version_line: &str) {
        self.script(
            &self.root.join("bin").join(name),
            &format!("#!/bin/sh\necho '{version_line}'\n"),
        );
    }
    fn script(&self, path: &Path, body: &str) {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::write(path, body).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn linked_relay(&self) -> String {
        use xcb_runtime::cloud::{crypto, custody, wire};

        let root = self.state();
        let device = crypto::DeviceIdentity::generate().unwrap();
        custody::store_device(
            &root,
            &device,
            wire::EXECUTOR_CLASS,
            "fixture",
            &device.device,
        )
        .unwrap();
        custody::store_account_key(&root, &crypto::AccountKey::generate(), 1).unwrap();
        let claims = crypto::encode_base64url(
            &serde_json::to_vec(&serde_json::json!({
                "iss": "http://127.0.0.1:1",
                "sub": "synthetic-owner|synthetic-session",
                "aud": "convex",
                "exp": 4_000_000_000_u64,
            }))
            .unwrap(),
        );
        let mut session = custody::CloudSession::issue(
            format!("fixture.{claims}.synthetic-secret-signature"),
            "synthetic-private-refresh-token".into(),
            0,
        );
        session.deployment_url = Some("http://127.0.0.1:1".into());
        custody::store_session(&root, &session).unwrap();
        custody::store_link(
            &root,
            &custody::RelayLink {
                deployment_url: "http://127.0.0.1:1".into(),
                boot_generation: 3,
            },
        )
        .unwrap();
        device.device
    }

    fn cloud_bytes(&self) -> std::collections::BTreeMap<String, Vec<u8>> {
        std::fs::read_dir(self.state().join("cloud"))
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                (
                    entry.file_name().into_string().unwrap(),
                    std::fs::read(entry.path()).unwrap(),
                )
            })
            .collect()
    }

    fn assert_relay_secrets_absent(&self, output: &Output) {
        let rendered = format!("{}{}", text(&output.stdout), text(&output.stderr));
        for (name, fields) in [
            ("device.json", &["signingScalar", "agreementScalar"][..]),
            ("account.json", &["accountKey"][..]),
            ("session.json", &["token", "refreshToken"][..]),
        ] {
            let record: serde_json::Value = serde_json::from_slice(
                &std::fs::read(self.state().join("cloud").join(name)).unwrap(),
            )
            .unwrap();
            for field in fields {
                let secret = record[*field].as_str().unwrap();
                assert!(
                    !rendered.contains(secret),
                    "{name}/{field} appeared in CLI output"
                );
            }
        }
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
fn reauth_help_and_enrollment_option_conflicts_are_explicit() {
    let help = plain(&["link", "--help"]);
    assert!(help.status.success());
    let help = text(&help.stdout);
    assert!(help.contains("--reauth"));
    assert!(help.contains("same relay account"));
    assert!(help.contains("--code"));
    for incompatible in [
        &["--controller"][..],
        &["--invite", "synthetic-private-invite"][..],
        &["--label", "another-device"][..],
    ] {
        let sandbox = Sandbox::new(&format!("reauth-conflict-{}", incompatible[0]));
        let mut args = vec!["--json", "link", "--reauth"];
        args.extend_from_slice(incompatible);
        let output = sandbox.run(&args, &[]);
        assert_eq!(output.status.code(), Some(2), "{output:?}");
        assert!(output.stderr.is_empty());
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["error"]["code"], "usage");
        assert!(!text(&output.stdout).contains("synthetic-private-invite"));
        assert!(!sandbox.state().exists());
    }
}

#[test]
fn reauth_requires_a_linked_device_without_starting_local_stores() {
    let sandbox = Sandbox::new("reauth-unlinked");
    let output = sandbox.run(&["--json", "link", "--reauth"], &[]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("not linked")
    );
    assert!(!sandbox.state().join("managed").exists());
    assert!(!sandbox.state().join("cloud/device.json").exists());
}

#[test]
fn a_first_link_without_a_relay_refuses_instead_of_guessing_a_local_backend() {
    let sandbox = Sandbox::new("link-no-relay");
    for env in [&[][..], &[("XCB_RELAY_URL", "")][..]] {
        let output = sandbox.run(&["--json", "link", "--email", "owner@example.test"], env);
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let message = value["error"]["message"].as_str().unwrap().to_lowercase();
        assert!(message.contains("no relay configured"), "{message}");
        assert!(message.contains("--relay"), "{message}");
        assert!(!text(&output.stderr).contains("Requesting a sign-in code"));
    }
}

#[test]
fn ordinary_link_keeps_a_linked_device_and_session_byte_for_byte() {
    let sandbox = Sandbox::new("link-existing");
    let device = sandbox.linked_relay();
    let before = sandbox.cloud_bytes();
    let output = sandbox.run(
        &[
            "--json",
            "link",
            "--email",
            "other@example.test",
            "--code",
            "12345678",
        ],
        &[("XCB_RELAY_URL", "https://override.invalid")],
    );
    assert!(output.status.success(), "{output:?}");
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["linked"], true);
    assert_eq!(value["device"], device);
    assert!(value.get("reauthenticated").is_none());
    assert_eq!(sandbox.cloud_bytes(), before);
    assert!(!sandbox.state().join("managed").exists());
    sandbox.assert_relay_secrets_absent(&output);
}

#[test]
fn reauth_rejects_relay_overrides_before_resuming_saved_work() {
    let sandbox = Sandbox::new("reauth-endpoint");
    sandbox.linked_relay();
    xcb_runtime::private::create(
        &sandbox.state().join("cloud/reauth.json"),
        b"synthetic unreadable journal: must never be opened",
    )
    .unwrap();
    let before = sandbox.cloud_bytes();
    for (args, env) in [
        (
            &[
                "--json",
                "link",
                "--reauth",
                "--relay",
                "https://override.invalid",
            ][..],
            &[][..],
        ),
        (&["--json", "link", "--reauth", "--relay", ""][..], &[][..]),
        (
            &["--json", "link", "--reauth"][..],
            &[("XCB_RELAY_URL", "https://override.invalid")][..],
        ),
        (
            &[
                "--json",
                "link",
                "--reauth",
                "--relay",
                "http://127.0.0.1:1",
            ][..],
            &[("XCB_RELAY_URL", "https://override.invalid")][..],
        ),
    ] {
        let output = sandbox.run(args, env);
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(
            value["error"]["message"]
                .as_str()
                .unwrap()
                .contains("original relay"),
            "{value}"
        );
        assert!(output.stderr.is_empty());
        assert_eq!(sandbox.cloud_bytes(), before);
        assert!(!sandbox.state().join("managed").exists());
        sandbox.assert_relay_secrets_absent(&output);
    }
}

#[test]
fn reauth_rejects_bad_supplied_email_or_code_before_resuming_saved_work() {
    let sandbox = Sandbox::new("reauth-input");
    sandbox.linked_relay();
    xcb_runtime::private::create(
        &sandbox.state().join("cloud/reauth.json"),
        b"synthetic unreadable journal: must never be opened",
    )
    .unwrap();
    let before = sandbox.cloud_bytes();
    for (extra, expected) in [
        (&["--email", "invalid email@example.test"][..], "email"),
        (&["--code", "private-otp-placeholder"][..], "8 digits"),
        (&["--code", "1234567"][..], "8 digits"),
        (&["--code", "1234567a"][..], "8 digits"),
    ] {
        let mut args = vec!["--json", "link", "--reauth"];
        args.extend_from_slice(extra);
        let output = sandbox.run(&args, &[]);
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["error"]["code"], "invalid-input");
        assert!(
            value["error"]["message"]
                .as_str()
                .unwrap()
                .contains(expected),
            "{value}"
        );
        assert!(!text(&output.stdout).contains(extra[1]));
        assert!(output.stderr.is_empty());
        assert_eq!(sandbox.cloud_bytes(), before);
        sandbox.assert_relay_secrets_absent(&output);
    }
}

#[test]
fn reauth_without_email_needs_explicit_input_and_keeps_the_saved_state() {
    let sandbox = Sandbox::new("reauth-noninteractive");
    sandbox.linked_relay();
    let before = sandbox.cloud_bytes();
    for output in [
        sandbox.run(
            &[
                "--json",
                "link",
                "--reauth",
                "--relay",
                "http://127.0.0.1:1/",
            ],
            &[("XCB_RELAY_URL", "http://127.0.0.1:1//")],
        ),
        sandbox.run(&["--json", "link", "--reauth", "--code", "12345678"], &[]),
        sandbox.run_with_input(
            &["--json", "link", "--reauth"],
            b"owner@example.test\n12345678\n",
        ),
    ] {
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(
            value["error"]["message"]
                .as_str()
                .unwrap()
                .contains("--email"),
            "{value}"
        );
        assert!(output.stderr.is_empty());
        assert_eq!(sandbox.cloud_bytes(), before);
        assert!(!sandbox.state().join("managed").exists());
        sandbox.assert_relay_secrets_absent(&output);
    }
}

#[test]
fn bare_xcb_without_a_terminal_prints_where_to_start() {
    let sandbox = Sandbox::new("bare");
    let output = sandbox.run(&[], &[]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let stdout = text(&output.stdout);
    assert!(
        stdout
            .starts_with("Excalibur (xcb) routes coding tasks across the Claude, Codex, and Devin"),
        "{stdout}"
    );
    assert!(
        stdout.contains("\nStart here\n  xcb setup claude"),
        "{stdout}"
    );
    assert!(
        stdout.ends_with(&format!(
            "All commands: xcb --help · Advanced: xcb help advanced\nxcb {}\n",
            env!("CARGO_PKG_VERSION")
        )),
        "{stdout}"
    );
    assert!(stdout.lines().count() <= 25);
    assert!(output.stderr.is_empty(), "{output:?}");
    // It only prints; it never opens or creates the state folder.
    assert!(!sandbox.state().exists());
}

#[test]
fn usage_errors_name_the_input_and_the_help_to_read() {
    let sandbox = Sandbox::new("usage");
    for (args, expected) in [
        (
            &["acounts"][..],
            "✗ Unknown command \"acounts\". Did you mean \"accounts\"?\n→ xcb --help\n",
        ),
        (
            &["login"],
            "✗ Unknown command \"login\". Did you mean \"accounts login\"?\n→ xcb --help\n",
        ),
        (
            &["accounts", "list"],
            "✗ Unknown command \"list\".\n→ xcb accounts --help\n",
        ),
        (&["setup"], "✗ Missing <provider>.\n→ xcb setup --help\n"),
        (
            &["run", "--modl", "x"],
            "✗ Unknown option \"--modl\". Did you mean \"--model\"?\n→ xcb run --help\n",
        ),
    ] {
        let output = sandbox.run(args, &[("HRANESS_AUDIENCE", "human")]);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert_eq!(text(&output.stdout), "", "{args:?}");
        assert_eq!(text(&output.stderr), expected, "{args:?}");
    }
    let dumb = sandbox.run(&["acounts"], &[("TERM", "dumb")]);
    assert_eq!(
        text(&dumb.stderr),
        "FAIL Unknown command \"acounts\". Did you mean \"accounts\"?\n-> xcb --help\n"
    );
    // --json and agents get one JSON document on stdout, and nothing on
    // stderr, with the same exit code.
    for (args, env) in [
        (&["--json", "acounts"][..], &[][..]),
        (&["acounts"], &[("CLAUDECODE", "1")]),
    ] {
        let output = sandbox.run(args, env);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert_eq!(text(&output.stderr), "", "{args:?}");
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["ok"], false);
        assert_eq!(value["error"]["code"], "usage");
        assert_eq!(
            value["error"]["message"],
            "Unknown command \"acounts\". Did you mean \"accounts\"?"
        );
        assert_eq!(value["error"]["next"], "xcb --help");
    }
}

#[test]
fn help_is_short_grouped_and_fits_100_columns() {
    let root = plain(&["--help"]);
    let root = text(&root.stdout);
    assert!(root.lines().count() <= 60, "{root}");
    assert!(root.ends_with("xcb help advanced\n"), "{root}");
    let advanced = plain(&["help", "advanced"]);
    assert!(advanced.status.success());
    let advanced = text(&advanced.stdout);
    assert!(advanced.contains("\nOther machines\n  link "), "{advanced}");
    assert!(!root.contains("\n  link "), "{root}");
    for args in [
        &["accounts", "--help"][..],
        &["run", "--help"],
        &["models", "--help"],
        &["doctor", "--help"],
        &["service", "--help"],
        &["remote", "--help"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_xcb"))
            .env_clear()
            .env("COLUMNS", "200")
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{args:?}");
        let stdout = text(&output.stdout);
        let wide: Vec<&str> = stdout
            .lines()
            .filter(|line| line.chars().count() > 100)
            .collect();
        assert!(wide.is_empty(), "{args:?}: {wide:#?}");
    }
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
        &["setup", "--help"],
        &["-h"],
        &["help"],
        &["help", "advanced"],
    ] {
        let output = plain(args);
        assert!(output.status.success(), "{args:?}: {output:?}");
        assert!(!output.stdout.is_empty(), "{args:?}");
    }
    let root = text(&plain(&["--help"]).stdout);
    assert!(root.contains("\nStart here\n  setup          "), "{root}");
    for internal in [
        "managed-daemon",
        "broker-stdio",
        "egress-forward",
        "qualify-application",
        "application-diagnostic",
        "  route ",
    ] {
        assert!(!root.contains(internal), "{internal} leaked into --help");
    }
    assert!(!root.contains("<account-id>"), "{root}");
    let setup = text(&plain(&["setup", "--help"]).stdout);
    assert!(setup.contains("claude, codex, or devin"), "{setup}");
}

#[test]
fn setup_adds_one_account_then_stops_at_the_provider_check() {
    let sandbox = Sandbox::new("setup");
    let output = sandbox.run(&["setup", "claude"], &[]);
    assert_eq!(output.status.code(), Some(1));
    let stdout = text(&output.stdout);
    assert!(stdout.starts_with("✓ Added claude/a_"), "{stdout}");
    let stderr = text(&output.stderr);
    assert!(
        stderr.contains("xcb can't find `claude` on your PATH"),
        "{stderr}"
    );
    assert!(stderr.contains("xcb doctor --provider claude"), "{stderr}");
    assert!(stderr.contains("xcb setup claude --account a_"), "{stderr}");
    // Running it again reuses the account instead of adding a second one.
    let again = sandbox.run(&["setup", "claude"], &[]);
    assert!(
        text(&again.stdout).starts_with("✓ Using claude/a_"),
        "{again:?}"
    );
    let accounts = sandbox.run(&["--json", "accounts"], &[]);
    let list: serde_json::Value = serde_json::from_slice(&accounts.stdout).unwrap();
    assert_eq!(list["accounts"].as_array().map(Vec::len), Some(1), "{list}");
    let json = sandbox.run(&["--json", "setup", "claude"], &[]);
    assert_eq!(json.status.code(), Some(1));
    let error: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(error["error"]["next"], "xcb --json accounts add claude");
}

#[test]
fn setup_refuses_a_turned_off_account_and_names_the_fix() {
    let sandbox = Sandbox::new("setup-off");
    let id = sandbox.add_claude();
    assert!(
        sandbox
            .run(&["accounts", "disable", &id], &[])
            .status
            .success()
    );
    let output = sandbox.run(&["setup", "claude"], &[]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = text(&output.stderr);
    assert!(
        stderr.starts_with("✗ Your Claude Code account claude/a_"),
        "{stderr}"
    );
    assert!(
        stderr.ends_with(&format!("is turned off.\n→ xcb accounts enable {id}\n")),
        "{stderr}"
    );
}

#[test]
fn setup_prefers_a_healthy_enabled_account_over_rejected_or_missing_credentials() {
    let sandbox = Sandbox::new("setup-healthy");
    let mut ids: Vec<_> = (0..4).map(|_| sandbox.add_claude()).collect();
    ids.sort();
    let store = xcb_runtime::store::Store::open(&sandbox.state()).unwrap();
    for index in [0, 1, 3] {
        let id = xcb_core::Id::new(&ids[index]).unwrap();
        xcb_runtime::private::create(
            &store.account_root(&id).unwrap().join("subscription-token"),
            b"sk-ant-oat01-synthetic_fixture_credential",
        )
        .unwrap();
    }
    let rejected = xcb_core::Id::new(&ids[0]).unwrap();
    let db = rusqlite::Connection::open(sandbox.state().join("xcb.sqlite")).unwrap();
    db.execute(
        "INSERT INTO account_auth_failures(account,generation,run) VALUES(?1,NULL,'r_fixture')",
        [&ids[0]],
    )
    .unwrap();
    drop(db);
    store
        .set_account_enabled(&xcb_core::Id::new(&ids[1]).unwrap(), false)
        .unwrap();

    // Provider discovery fails on the empty PATH after choosing an account.
    // No provider or browser can run, and no failure marker is cleared.
    let output = sandbox.run(&["setup", "claude"], &[]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stdout = text(&output.stdout);
    assert!(stdout.contains(&format!(" · {}\n", ids[3])), "{stdout}");
    assert!(store.authentication_required(&rejected).unwrap());
    assert!(store.unsettled_runs().unwrap().is_empty());
    assert_eq!(store.accounts().unwrap().len(), 4);
}

#[test]
fn doctor_marks_each_provider_and_names_one_next_step() {
    let sandbox = Sandbox::new("doctor");
    let output = sandbox.run(&["doctor"], &[("HRANESS_AUDIENCE", "human")]);
    assert_eq!(output.status.code(), Some(1));
    let stdout = text(&output.stdout);
    assert!(
        stdout.contains("✗ claude: xcb can't find `claude` on your PATH."),
        "{stdout}"
    );
    assert!(
        stdout.contains("\n○ remote: not linked (xcb link connects this machine)\n"),
        "{stdout}"
    );
    assert!(!stdout.contains("custody"), "{stdout}");
    // The checks end with a count line. On Linux a missing sandbox is one
    // more warning: xcb starts no provider there without it.
    let summary = stdout.rsplit_once("\n\n").map(|(_, last)| last);
    if cfg!(target_os = "linux") {
        assert!(
            matches!(summary, Some("3 problems.\n" | "3 problems, 1 warning.\n")),
            "{stdout}"
        );
    } else {
        assert_eq!(summary, Some("3 problems.\n"), "{stdout}");
    }
    assert!(
        text(&output.stderr).ends_with("Next: install Claude Code, or run xcb doctor --provider claude --executable <absolute path>\n"),
        "{output:?}"
    );
}

/// The live report behind this: doctor said "All 3 checks passed" while
/// accounts needed attention. Accounts show under their provider, a provider
/// that isn't installed and has no accounts is optional, and doctor exits 1
/// until an account can take a task.
#[test]
fn doctor_reports_accounts_under_their_provider_and_exits_nonzero() {
    let sandbox = Sandbox::new("doctor-accounts");
    sandbox.fake_provider("codex", "codex-cli 0.0.1");
    sandbox.add(&["codex"]);
    let off = sandbox.add(&["codex"]);
    assert!(
        sandbox
            .run(&["accounts", "disable", &off], &[])
            .status
            .success()
    );
    let output = sandbox.run(&["doctor"], &[("HRANESS_AUDIENCE", "human")]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stdout = text(&output.stdout);
    assert!(
        stdout.contains(
            "⚠ codex 0.0.1: found, but xcb can't run this build yet\n  ○ 2 accounts can't take tasks until xcb can run Codex\n"
        ),
        "{stdout}"
    );
    // Claude and Devin have no accounts and another provider was found.
    assert!(
        stdout.starts_with("○ claude: xcb can't find `claude` on your PATH."),
        "{stdout}"
    );
    assert!(stdout.contains("\n○ devin: "), "{stdout}");
    assert!(!stdout.contains("passed"), "{stdout}");
    assert!(
        text(&output.stderr).ends_with(
            "Next: install a supported Codex build (xcb.sh/docs/providers lists them), then run xcb doctor\n"
        ),
        "{output:?}"
    );
    let json = sandbox.run(&["--json", "doctor"], &[]);
    assert_eq!(json.status.code(), Some(1), "{json:?}");
    let report: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    let codex = report["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["provider"] == "codex")
        .unwrap();
    assert_eq!(codex["needsSignIn"], 1, "{report}");
    assert_eq!(codex["off"], 1, "{report}");
    assert_eq!(report["checks"]["accountsReady"], 0, "{report}");
    assert_eq!(report["checks"]["problems"], 0, "{report}");
    assert!(
        report["next"].as_str().unwrap().contains("Codex"),
        "{report}"
    );
    // Nothing installed and nothing set up: every provider is a problem.
    let empty = Sandbox::new("doctor-empty-json");
    let report: serde_json::Value =
        serde_json::from_slice(&empty.run(&["--json", "doctor"], &[]).stdout).unwrap();
    assert_eq!(report["checks"]["problems"], 3, "{report}");
}

#[test]
fn accounts_table_lines_up_and_names_the_account_to_sign_in() {
    let sandbox = Sandbox::new("accounts-table");
    let claude = sandbox.add(&["claude", "--plan", "Max"]);
    let codex = sandbox.add(&["codex", "--plan", "ChatGPT subscription"]);
    let devin = sandbox.add(&["devin", "--plan", "Imported subscription"]);
    assert!(
        sandbox
            .run(&["accounts", "disable", &codex], &[])
            .status
            .success()
    );
    let stored = sandbox.run_with_input(&["accounts", "token", &devin], b"synthetic-devin-token");
    assert!(stored.status.success(), "{stored:?}");
    let output = sandbox.run(&["accounts"], &[("HRANESS_AUDIENCE", "human")]);
    assert!(output.status.success(), "{output:?}");
    let stdout = text(&output.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    let status_at = lines[0].find("STATUS").unwrap();
    assert!(lines[0].starts_with("  ID  "), "{stdout}");
    let short = |id: &str| format!("{}…", &id[..10]);
    // Claude, Codex, Devin order; the first account added is the default.
    for (line, id, provider, plan, status) in [
        (lines[1], &claude, "claude", "Max", "needs sign-in"),
        (lines[2], &codex, "codex", "ChatGPT", "off"),
        (lines[3], &devin, "devin", "Imported", "ready"),
    ] {
        assert!(line.contains(&short(id)), "{line}");
        assert!(line.contains(&format!("  {provider}  ")), "{line}");
        assert!(line.contains(&format!("  {plan}  ")), "{line}");
        // Every character here is one column wide, `…` included.
        let column = line[..line.find(status).unwrap()].chars().count();
        assert_eq!(column, status_at, "{stdout}");
        assert!(line.chars().count() <= 100, "{line}");
    }
    assert!(lines[1].starts_with("> "), "{stdout}");
    assert!(!stdout.contains("unmeasured"), "{stdout}");
    assert!(!stdout.contains("subscripti"), "{stdout}");
    assert_eq!(
        text(&output.stderr),
        format!("Next: xcb accounts login {claude}\n")
    );
    // --json keeps its documented shape.
    let json = sandbox.run(&["--json", "accounts"], &[]);
    let value: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    let keys = |value: &serde_json::Value| {
        let mut keys: Vec<String> = value.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        keys
    };
    assert_eq!(
        keys(&value),
        [
            "accounts",
            "estimatedPoolSeconds",
            "localOnly",
            "measuredPools",
            "totalPools",
            "version"
        ]
    );
    assert_eq!(
        keys(&value["accounts"][0]),
        [
            "authenticationRequired",
            "busy",
            "email",
            "enabled",
            "id",
            "name",
            "provider",
            "quotaBlockedUntilMs",
            "remainingPercent",
            "resetsAtMs",
            "runway",
            "subscription"
        ]
    );
    assert!(
        value["accounts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["subscription"] == "ChatGPT subscription"),
        "{value}"
    );
}

/// `update disable` and `update enable --policy disable` only record the
/// policy: no LaunchAgent, no login-item notice, on every platform. The
/// sandbox HOME has no LaunchAgent, so nothing reaches launchd.
#[test]
fn update_policy_changes_without_installing_anything_to_turn_off() {
    let sandbox = Sandbox::new("update-policy");
    let agents = sandbox.root.join("home/Library/LaunchAgents");
    for args in [
        &["update", "disable"][..],
        &["update", "enable", "--policy", "disable"],
    ] {
        let output = sandbox.run(args, &[("HRANESS_AUDIENCE", "human")]);
        assert_eq!(output.status.code(), Some(0), "{args:?}: {output:?}");
        assert_eq!(text(&output.stdout), "xcb updates disabled\n", "{args:?}");
        assert_eq!(text(&output.stderr), "", "{args:?}");
        let status = sandbox.run(&["--json", "update", "status"], &[]);
        let value: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
        assert_eq!(value["policy"], "disable", "{args:?}");
        assert!(!agents.exists(), "{args:?} created {}", agents.display());
    }
    // Without a systemd user manager (the sandbox PATH has no systemctl),
    // enable records the policy and names the timer to add instead of
    // failing.
    if cfg!(target_os = "linux") {
        let output = sandbox.run(
            &["update", "enable", "--policy", "notify"],
            &[("HRANESS_AUDIENCE", "human")],
        );
        assert_eq!(output.status.code(), Some(0), "{output:?}");
        assert_eq!(text(&output.stdout), "xcb updates: notify\n");
        assert!(
            text(&output.stderr).contains("xcb update daemon"),
            "{output:?}"
        );
        let json = sandbox.run(&["--json", "update", "enable", "--policy", "auto"], &[]);
        let value: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
        assert!(
            !json.status.success(),
            "a source test executable cannot enable automatic installation"
        );
        assert!(
            value["error"]["message"]
                .as_str()
                .unwrap()
                .contains("verified release install record")
        );
    }
}

#[test]
fn upgrading_to_an_older_release_needs_an_explicit_flag() {
    let sandbox = Sandbox::new("downgrade");
    let output = sandbox.run(&["upgrade", "0.0.1"], &[]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(
        text(&output.stderr),
        format!(
            "✗ xcb 0.0.1 is older than the installed {}; installing it would downgrade xcb.\n→ xcb upgrade 0.0.1 --allow-downgrade\n",
            env!("CARGO_PKG_VERSION")
        )
    );
    let json = sandbox.run(&["--json", "update", "install", "v0.0.1"], &[]);
    let value: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(
        value["error"]["next"],
        "xcb upgrade 0.0.1 --allow-downgrade"
    );
}

#[test]
fn json_upgrade_reports_the_installed_release_without_installer_output_on_stdout() {
    let sandbox = Sandbox::new("upgrade-json");
    let share = xcb_runtime::private::directory(&sandbox.root.join("share/xcb")).unwrap();
    let installer = share.join("install-native.sh");
    let binary = sandbox.root.join("bin/xcb");
    std::fs::copy(env!("CARGO_BIN_EXE_xcb"), &binary).unwrap();
    let replacement = "#!/bin/sh\nprintf 'xcb 99.0.1\\n'\n";
    std::fs::write(sandbox.root.join("next-binary"), replacement).unwrap();
    sandbox.script(
        &installer,
        "#!/bin/sh\n[ \"$XCB_VERSION\" = '99.0.1' ] || exit 42\n[ \"$XCB_ADD_PATH\" = no ] || exit 43\n[ \"$XCB_GITHUB\" = hraness/xcb ] || exit 45\nprintf '%s\\n' \"$XCB_VERSION\" > \"$XCB_INSTALL_PREFIX/installed-version\"\n/bin/cp \"$XCB_INSTALL_PREFIX/next-binary\" \"$XCB_INSTALL_PREFIX/bin/xcb-next\"\n/bin/chmod 755 \"$XCB_INSTALL_PREFIX/bin/xcb-next\"\n/bin/mv -f \"$XCB_INSTALL_PREFIX/bin/xcb-next\" \"$XCB_INSTALL_PREFIX/bin/xcb\"\n/bin/cp \"$XCB_INSTALL_PREFIX/next-record\" \"$XCB_INSTALL_PREFIX/share/xcb/install.json\"\nprintf 'installer fixture output\\n'\n",
    );
    let record = serde_json::json!({
        "version":2,"installMethod":"release","channel":"stable","sourceRoot":"",
        "versionString":env!("CARGO_PKG_VERSION"),"versionPinned":false,
        "prefix":sandbox.root,"helperPath":installer,"binaryPath":binary,
        "binarySha256":xcb_runtime::process::executable_digest(&binary).unwrap(),
        "helperSha256":xcb_runtime::process::executable_digest(&installer).unwrap(),
    });
    let mut next_record = record.clone();
    next_record["versionString"] = serde_json::json!("99.0.1");
    next_record["binarySha256"] = serde_json::json!(xcb_runtime::digest(replacement));
    next_record["versionPinned"] = serde_json::json!(true);
    std::fs::write(sandbox.root.join("next-record"), next_record.to_string()).unwrap();
    xcb_runtime::private::create(&share.join("update-use.lock"), b"").unwrap();
    let os = if cfg!(target_os = "macos") {
        "darwin"
    } else {
        std::env::consts::OS
    };
    let asset = format!("xcb-99.0.1-{os}-{}.tar.gz", std::env::consts::ARCH);
    let release = serde_json::json!({
        "tag_name": "v99.0.1", "draft": false, "prerelease": false, "immutable": true,
        "assets": [{"name": asset}, {"name": format!("{asset}.sha256")}],
    });
    sandbox.script(
        &sandbox.root.join("bin/curl"),
        &format!(
            "#!/bin/sh\nfor arg do url=\"$arg\"; done\n[ \"$url\" = 'https://api.github.com/repos/hraness/xcb/releases/tags/v99.0.1' ] || exit 44\nprintf '%s\\n' '{release}'\n"
        ),
    );
    // Clearing an exact-version pin must not silently downgrade to an older latest.
    let mut pinned = record.clone();
    pinned["versionPinned"] = serde_json::json!(true);
    let manifest = share.join("install.json");
    std::fs::write(&manifest, pinned.to_string()).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&manifest, std::fs::Permissions::from_mode(0o600)).unwrap();
    let old_asset = format!("xcb-0.0.1-{os}-{}.tar.gz", std::env::consts::ARCH);
    let old_release = serde_json::json!([{
        "tag_name":"v0.0.1","draft":false,"prerelease":false,"immutable":true,
        "assets":[{"name":old_asset},{"name":format!("{old_asset}.sha256")}]
    }]);
    let curl = sandbox.root.join("bin/curl");
    let original_curl = std::fs::read_to_string(&curl).unwrap();
    sandbox.script(
        &curl,
        &format!("#!/bin/sh\nprintf '%s\\n' '{old_release}'\n"),
    );
    let output = Command::new(&binary)
        .env_clear()
        .env("HOME", sandbox.root.join("home"))
        .env("PATH", sandbox.root.join("bin"))
        .arg("--state")
        .arg(sandbox.state())
        .args(["--json", "upgrade"])
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "a pinned install must not downgrade: {output:?}"
    );
    let error: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        error["error"]["next"]
            .as_str()
            .unwrap()
            .contains("--allow-downgrade"),
        "{error}"
    );
    assert!(!sandbox.root.join("installed-version").exists());
    sandbox.script(&curl, &original_curl);
    for args in [
        &["--json", "upgrade", "99.0.1"][..],
        &["--json", "update", "install", "v99.0.1"],
    ] {
        std::fs::copy(env!("CARGO_BIN_EXE_xcb"), &binary).unwrap();
        let path = share.join("install.json");
        std::fs::write(&path, record.to_string()).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let output = Command::new(&binary)
            .env_clear()
            .env("HOME", sandbox.root.join("home"))
            .env("PATH", sandbox.root.join("bin"))
            .env("XCB_GITHUB", "foreign/blocked")
            .arg("--state")
            .arg(sandbox.state())
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{args:?}: {output:?}");
        let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            result,
            serde_json::json!({
                "version": 1, "previous": env!("CARGO_PKG_VERSION"),
                "current": "99.0.1", "changed": true,
            })
        );
        assert!(text(&output.stderr).contains("installer fixture output"));
        assert_eq!(
            std::fs::read_to_string(sandbox.root.join("installed-version")).unwrap(),
            "99.0.1\n"
        );
    }
    // A helper cannot claim success by changing only its install record.
    std::fs::copy(env!("CARGO_BIN_EXE_xcb"), &binary).unwrap();
    sandbox.script(&installer, "#!/bin/sh\n/bin/cp \"$XCB_INSTALL_PREFIX/lying-record\" \"$XCB_INSTALL_PREFIX/share/xcb/install.json\"\n");
    let mut original = record.clone();
    original["helperSha256"] =
        serde_json::json!(xcb_runtime::process::executable_digest(&installer).unwrap());
    std::fs::write(share.join("install.json"), original.to_string()).unwrap();
    let mut lying = original;
    lying["versionString"] = serde_json::json!("99.0.1");
    std::fs::write(sandbox.root.join("lying-record"), lying.to_string()).unwrap();
    let output = Command::new(&binary)
        .env_clear()
        .env("HOME", sandbox.root.join("home"))
        .env("PATH", sandbox.root.join("bin"))
        .arg("--state")
        .arg(sandbox.state())
        .args(["--json", "upgrade", "99.0.1"])
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "a no-op installer must fail: {output:?}"
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        result["error"]["message"]
            .as_str()
            .unwrap()
            .contains("does not report the requested xcb version"),
        "{result}"
    );
}

#[test]
fn killed_updater_parent_cannot_admit_work_while_its_helper_is_live() {
    use std::os::unix::fs::PermissionsExt;
    use std::time::{Duration, Instant};

    struct UpdaterParent {
        child: Option<std::process::Child>,
        release: PathBuf,
    }

    impl UpdaterParent {
        fn cleanup(&mut self) -> std::io::Result<Option<Output>> {
            let Some(mut child) = self.child.take() else {
                return Ok(None);
            };
            let released = std::fs::write(&self.release, b"");
            let _ = child.kill();
            // The helper inherits stderr. Draining that pipe also waits for
            // the released helper to close it, including on assertion unwind.
            let output = child.wait_with_output();
            released?;
            output.map(Some)
        }
    }

    impl Drop for UpdaterParent {
        fn drop(&mut self) {
            let _ = self.cleanup();
        }
    }

    let sandbox = Sandbox::new("update-parent-death");
    let share = xcb_runtime::private::directory(&sandbox.root.join("share/xcb")).unwrap();
    let installer = share.join("install-native.sh");
    let binary = sandbox.root.join("bin/xcb");
    std::fs::copy(env!("CARGO_BIN_EXE_xcb"), &binary).unwrap();
    sandbox.script(&installer, "#!/bin/sh\nprintf ready > \"$XCB_INSTALL_PREFIX/helper-started\"\ni=0\nwhile [ ! -f \"$XCB_INSTALL_PREFIX/helper-release\" ]; do i=$((i+1)); [ \"$i\" -lt 100 ] || exit 77; /bin/sleep 0.05; done\n/bin/rm \"$XCB_INSTALL_PREFIX/share/xcb/update-in-progress\"\nprintf done > \"$XCB_INSTALL_PREFIX/helper-finished\"\n");
    let record = serde_json::json!({
        "version":2,"installMethod":"release","channel":"stable","sourceRoot":"",
        "versionString":env!("CARGO_PKG_VERSION"),"versionPinned":false,
        "prefix":sandbox.root,"helperPath":installer,"binaryPath":binary,
        "binarySha256":xcb_runtime::process::executable_digest(&binary).unwrap(),
        "helperSha256":xcb_runtime::process::executable_digest(&installer).unwrap(),
    });
    std::fs::write(share.join("install.json"), record.to_string()).unwrap();
    std::fs::set_permissions(
        share.join("install.json"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    xcb_runtime::private::create(&share.join("update-use.lock"), b"").unwrap();
    let asset = format!("xcb-99.0.1-{}.tar.gz", xcb_runtime::update::platform());
    let release = serde_json::json!({"tag_name":"v99.0.1","draft":false,"prerelease":false,"immutable":true,
        "assets":[{"name":asset},{"name":format!("{asset}.sha256")}]});
    sandbox.script(
        &sandbox.root.join("bin/curl"),
        &format!("#!/bin/sh\nprintf '%s\\n' '{release}'\n"),
    );
    let child = Command::new(&binary)
        .env_clear()
        .env("HOME", sandbox.root.join("home"))
        .env("PATH", sandbox.root.join("bin"))
        .arg("--state")
        .arg(sandbox.state())
        .args(["--json", "upgrade", "99.0.1"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut parent = UpdaterParent {
        child: Some(child),
        release: sandbox.root.join("helper-release"),
    };
    // This deadline detects a hung fixture, not a startup performance limit:
    // verifying the debug executable can contend with the other CLI tests.
    let started = Instant::now();
    while !sandbox.root.join("helper-started").exists() {
        let status = parent.child.as_mut().unwrap().try_wait().unwrap();
        if status.is_some() || started.elapsed() >= Duration::from_secs(10) {
            let elapsed = started.elapsed();
            let guarded = share.join("update-in-progress").exists();
            let output = parent.cleanup().unwrap().unwrap();
            panic!(
                "helper did not start after {elapsed:?}; parent status: {status:?}; \
                 update guard present: {guarded}; stdout: {}; stderr: {}",
                text(&output.stdout),
                text(&output.stderr),
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    eprintln!("updater helper ready after {:?}", started.elapsed());
    parent.child.as_mut().unwrap().kill().unwrap();
    parent.child.as_mut().unwrap().wait().unwrap();
    let output = Command::new(&binary)
        .env_clear()
        .env("HOME", sandbox.root.join("home"))
        .env("PATH", sandbox.root.join("bin"))
        .arg("--state")
        .arg(sandbox.state())
        .args(["--json", "accounts"])
        .output()
        .unwrap();
    // Release the owned fixture child before making assertions that can panic.
    let updater_output = parent.cleanup().unwrap().unwrap();
    assert!(
        sandbox.root.join("helper-finished").exists(),
        "helper did not finish: {updater_output:?}"
    );
    assert!(
        !output.status.success(),
        "work was admitted during interrupted replacement: {output:?}"
    );
    let error: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("update is unfinished"),
        "{error}"
    );
    assert!(
        !sandbox.state().exists(),
        "the refused command must not open product state"
    );
    assert!(!share.join("update-in-progress").exists());
}

#[test]
fn json_update_daemon_reports_a_disabled_check_without_network_access() {
    let sandbox = Sandbox::new("update-daemon-json");
    assert!(sandbox.run(&["update", "disable"], &[]).status.success());
    let output = sandbox.run(&["--json", "update", "daemon"], &[]);
    assert!(output.status.success(), "{output:?}");
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        result,
        serde_json::json!({"version": 1, "checked": false, "upgrade": null})
    );
    assert!(output.stderr.is_empty(), "{output:?}");
}

#[test]
fn empty_sessions_say_so() {
    let sandbox = Sandbox::new("sessions");
    let output = sandbox.run(&["sessions"], &[("HRANESS_AUDIENCE", "human")]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(text(&output.stdout), "No provider sessions yet.\n");
    assert_eq!(text(&output.stderr), "Next: xcb\n");
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
    assert_eq!(text(&human.stderr), "Next: xcb setup <provider>\n");
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
    let dumb = sandbox.run(&["accounts", "login", &id], &[("TERM", "dumb")]);
    assert!(
        text(&dumb.stderr).starts_with("-> Checking Claude Code first"),
        "{dumb:?}"
    );
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

#[test]
fn workspaces_list_add_hide_and_upgrade_preview_on_a_scratch_state() {
    let sandbox = Sandbox::new("workspaces");
    let preview = sandbox.run(&["doctor", "--upgrade-plan"], &[]);
    assert!(preview.status.success(), "{preview:?}");
    assert_eq!(
        text(&preview.stdout),
        "No managed state yet; there is nothing to upgrade.\n"
    );
    let json = |args: &[&str]| -> serde_json::Value {
        let mut all = vec!["--json"];
        all.extend_from_slice(args);
        let output = sandbox.run(&all, &[]);
        assert!(output.status.success(), "{args:?}: {output:?}");
        serde_json::from_slice(&output.stdout).unwrap()
    };
    // Listing never creates the thread.
    assert_eq!(json(&["conversations"]), serde_json::json!([]));
    assert_eq!(json(&["workspaces", "list"]), serde_json::json!([]));
    let work = sandbox.root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let work = xcb_core::canonical(work).unwrap();
    let added = sandbox.run(
        &[
            "workspaces",
            "add",
            work.to_str().unwrap(),
            "--name",
            "proj",
        ],
        &[],
    );
    assert!(added.status.success(), "{added:?}");
    assert_eq!(
        text(&added.stdout),
        format!("Added proj · {}\n", work.display())
    );
    let rows = json(&["workspaces", "list"]);
    assert_eq!(rows[0]["name"], "proj", "{rows}");
    assert_eq!(rows[0]["admittedBy"], "command", "{rows}");
    assert_eq!(rows[0]["status"], "ok", "{rows}");
    assert_eq!(rows[0]["openConflicts"], 0, "{rows}");
    assert!(
        sandbox
            .run(&["workspaces", "hide", "proj"], &[])
            .status
            .success()
    );
    assert_eq!(json(&["workspaces", "list"])[0]["status"], "hidden");
    assert!(
        sandbox
            .run(&["workspaces", "show", "proj"], &[])
            .status
            .success()
    );
    assert_eq!(json(&["workspaces", "list"])[0]["status"], "ok");
    // Entries that no longer validate can still be hidden, which is what the
    // supervisor's notice asks for: a deleted directory by path or by name,
    // and a directory that is now refused (here it became $HOME).
    let mut extra = Vec::new();
    for name in ["gone-path", "gone-name", "later-home"] {
        let dir = sandbox.root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let dir = xcb_core::canonical(dir).unwrap();
        let added = sandbox.run(
            &["workspaces", "add", dir.to_str().unwrap(), "--name", name],
            &[],
        );
        assert!(added.status.success(), "{added:?}");
        extra.push(dir);
    }
    std::fs::remove_dir(&extra[0]).unwrap();
    std::fs::remove_dir(&extra[1]).unwrap();
    for (args, env) in [
        (
            vec!["workspaces", "hide", extra[0].to_str().unwrap()],
            vec![],
        ),
        (vec!["workspaces", "hide", "gone-name"], vec![]),
        (
            vec!["workspaces", "hide", extra[2].to_str().unwrap()],
            vec![("HOME", extra[2].to_str().unwrap())],
        ),
    ] {
        let hid = sandbox.run(&args, &env);
        assert!(hid.status.success(), "{args:?}: {hid:?}");
    }
    let rows = json(&["workspaces", "list"]);
    for name in ["gone-path", "gone-name", "later-home"] {
        let row = rows
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["name"] == name)
            .unwrap();
        assert_eq!(row["status"], "hidden", "{rows}");
    }
    let home = sandbox.run(
        &[
            "workspaces",
            "add",
            sandbox.root.join("home").to_str().unwrap(),
        ],
        &[],
    );
    assert_eq!(home.status.code(), Some(1), "{home:?}");
    let plan = json(&["doctor", "--upgrade-plan"]);
    assert_eq!(plan["toVersion"], 7, "{plan}");
    assert_eq!(plan["workspaces"], 4, "{plan}");
    let leftovers: Vec<_> = std::fs::read_dir(sandbox.state())
        .unwrap()
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("upgrade-plan-")
        })
        .collect();
    assert!(leftovers.is_empty(), "the preview copy is removed");
    assert_eq!(json(&["conversations"]), serde_json::json!([]));
}

/// xcb's internal delivery vocabulary, which public copy translates (see
/// "Public copy" in AGENTS.md). Matched as whole words in any case, with
/// their plural and negated forms.
const INTERNAL_TERMS: [&str; 20] = [
    "admission",
    "admissions",
    "admitted",
    "admit",
    "admits",
    "qualification",
    "qualifications",
    "qualified",
    "unqualified",
    "custody",
    "settled",
    "unsettled",
    "settlement",
    "settlements",
    "joined",
    "unjoined",
    "receipt",
    "receipts",
    "bounded",
    "promoted",
];

/// Documented names that contain one of those words: the `xcb remote admit`
/// command, in usage lines and in `xcb remote --help`'s command list.
const DOCUMENTED_NAMES: [&str; 2] = ["remote admit", "\n  admit "];

/// Internal words in `text`. `XCB` is matched exactly, so environment
/// variable names such as `XCB_STATE` (one token) stay allowed.
fn internal_words(text: &str) -> Vec<String> {
    let mut text = format!("\n{text}");
    for name in DOCUMENTED_NAMES {
        text = text.replace(name, "\n");
    }
    text.split(|ch: char| !(ch.is_alphanumeric() || ch == '_'))
        .filter(|word| word == &"XCB" || INTERNAL_TERMS.contains(&word.to_lowercase().as_str()))
        .map(str::to_owned)
        .collect()
}

#[test]
fn internal_word_check_matches_whole_words_only() {
    assert_eq!(internal_words("No unsettled runs."), ["unsettled"]);
    assert_eq!(internal_words("Bounded, ephemeral"), ["Bounded"]);
    assert_eq!(internal_words("No XCB messages"), ["XCB"]);
    assert!(internal_words("$XCB_STATE or ~/.local/share/xcb").is_empty());
    assert!(internal_words("Usage: xcb remote admit [OPTIONS] <DEVICE>").is_empty());
    assert!(internal_words("Commands:\n  admit    Post an account-key wrap").is_empty());
    assert!(internal_words("promote notes; settle labels; adjoined").is_empty());
}

/// Every command's help, recursively, from the grouped root screens.
fn every_help_screen() -> Vec<(String, String)> {
    let run = |args: &[&str]| text(&plain(args).stdout);
    let mut screens = vec![
        ("xcb --help".to_owned(), run(&["--help"])),
        ("xcb help advanced".to_owned(), run(&["help", "advanced"])),
    ];
    let mut pending: Vec<Vec<String>> = screens
        .iter()
        .flat_map(|(_, screen)| {
            screen
                .lines()
                .filter(|line| line.starts_with("  ") && !line.starts_with("   "))
                .filter_map(|line| line.split_whitespace().next())
                .filter(|word| {
                    !word.starts_with('-')
                        && word.chars().all(|ch| ch.is_ascii_lowercase() || ch == '-')
                })
                .map(|word| vec![word.to_owned()])
                .collect::<Vec<_>>()
        })
        .collect();
    while let Some(path) = pending.pop() {
        let mut args: Vec<&str> = path.iter().map(String::as_str).collect();
        args.push("--help");
        let output = plain(&args);
        assert!(output.status.success(), "{args:?}: {output:?}");
        let screen = text(&output.stdout);
        if let Some((_, listed)) = screen.split_once("\nCommands:\n") {
            // Entries start two spaces in; wrapped descriptions start deeper.
            for line in listed.lines().take_while(|line| !line.is_empty()) {
                if let Some(name) = line
                    .strip_prefix("  ")
                    .filter(|rest| !rest.starts_with(' '))
                    .and_then(|rest| rest.split_whitespace().next())
                    && name != "help"
                {
                    let mut child = path.clone();
                    child.push(name.to_owned());
                    pending.push(child);
                }
            }
        }
        screens.push((format!("xcb {}", path.join(" ")), screen));
    }
    screens
}

/// Help and the everyday outputs speak the reader's language: none of the
/// internal words above, and never "XCB" in prose.
#[test]
fn help_and_everyday_output_avoid_internal_words() {
    let mut checked = every_help_screen();
    assert!(
        checked.len() > 80,
        "only {} help screens found",
        checked.len()
    );
    let sandbox = Sandbox::new("public-words");
    let work = sandbox.root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let work = work.to_str().unwrap().to_owned();
    sandbox.fake_provider("codex", "codex-cli 0.0.1");
    sandbox.fake_provider("devin", "devin 0.0.1 (fixture)");
    let codex = sandbox.add(&["codex", "--plan", "ChatGPT subscription"]);
    sandbox.add(&["claude"]);
    assert!(
        sandbox
            .run(&["accounts", "disable", &codex], &[])
            .status
            .success()
    );
    let added = sandbox.run(&["--json", "backlog", "add", &work, "Fix the parser"], &[]);
    assert!(added.status.success(), "{added:?}");
    let task: serde_json::Value = serde_json::from_slice(&added.stdout).unwrap();
    let task = task["id"].as_str().unwrap().to_owned();
    let human = [("HRANESS_AUDIENCE", "human")];
    for args in [
        vec![],
        vec!["doctor"],
        vec!["accounts"],
        vec!["setup", "devin"],
        vec!["sessions"],
        vec!["sessions", "prune"],
        vec!["sessions", "rm", "s_missing"],
        vec!["recover"],
        vec!["recover", "--launch-artifacts"],
        vec!["command", "prune"],
        vec!["tasks"],
        vec!["tasks", "verify", &task],
        vec!["tasks", "messages", &task],
        vec!["backlog"],
        vec!["conversations"],
        vec!["workspaces"],
        vec!["inbox"],
        vec!["attention"],
        vec!["schedules"],
        vec!["daemons"],
        vec!["projects"],
        vec!["reflex"],
        vec!["models"],
        vec!["update", "status"],
        vec!["update", "disable"],
        vec!["judge", "status"],
        vec!["fleet"],
        vec!["run"],
        vec!["upgrade", "0.0.1"],
        vec![
            "rename",
            "c_global",
            "Parser work",
            "--expected-title",
            "Thread",
        ],
    ] {
        let output = sandbox.run(&args, &human);
        checked.push((
            format!("xcb {}", args.join(" ")),
            format!("{}{}", text(&output.stdout), text(&output.stderr)),
        ));
    }
    let leaks: Vec<String> = checked
        .iter()
        .filter_map(|(what, screen)| {
            let words = internal_words(screen);
            (!words.is_empty()).then(|| format!("{what}: {words:?}"))
        })
        .collect();
    assert!(
        leaks.is_empty(),
        "internal words in public output:\n{}",
        leaks.join("\n")
    );
}

#[test]
fn advanced_is_the_same_screen_as_help_advanced() {
    let expected = text(&plain(&["help", "advanced"]).stdout);
    assert!(expected.starts_with("Advanced xcb commands."), "{expected}");
    for args in [
        &["advanced"][..],
        &["advanced", "--help"],
        &["advanced", "-h"],
        &["--json", "advanced"],
    ] {
        let output = plain(args);
        assert!(output.status.success(), "{args:?}: {output:?}");
        assert_eq!(text(&output.stdout), expected, "{args:?}");
    }
    let sandbox = Sandbox::new("advanced");
    let output = sandbox.run(&["advanced"], &[]);
    assert_eq!(text(&output.stdout), expected);
    assert!(!sandbox.state().exists(), "the screen opens no state");
}

/// Setup checks the provider build before any account or sign-in: Devin
/// gets no empty account, and an unsupported Claude build stops before the
/// browser sign-in.
#[test]
fn setup_checks_the_build_is_supported_before_sign_in() {
    let sandbox = Sandbox::new("setup-unsupported");
    sandbox.fake_provider("devin", "devin 0.0.1 (fixture)");
    let devin = sandbox.run(&["setup", "devin"], &[]);
    assert_eq!(devin.status.code(), Some(1), "{devin:?}");
    assert_eq!(text(&devin.stdout), "");
    let stderr = text(&devin.stderr);
    assert!(stderr.contains("Devin"), "{stderr}");
    if cfg!(target_os = "linux") {
        assert!(stderr.contains("macOS ARM64"), "{stderr}");
        assert!(stderr.contains("use Claude on Linux"), "{stderr}");
        assert!(
            stderr.contains("xcb.sh/docs/providers#claude-on-linux"),
            "{stderr}"
        );
    } else if cfg!(target_os = "macos") {
        assert!(stderr.contains("0.0.1"), "{stderr}");
        assert!(stderr.contains("xcb.sh/docs/providers"), "{stderr}");
    }
    assert!(!stderr.contains("qualified"), "{stderr}");
    let accounts = sandbox.run(&["--json", "accounts"], &[]);
    let list: serde_json::Value = serde_json::from_slice(&accounts.stdout).unwrap();
    assert_eq!(
        list["accounts"],
        serde_json::json!([]),
        "no empty Devin account"
    );
    sandbox.fake_provider("claude", "0.1.0 (Claude Code)");
    let claude = sandbox.run(&["setup", "claude"], &[]);
    assert_eq!(claude.status.code(), Some(1), "{claude:?}");
    assert!(
        text(&claude.stdout).starts_with("✓ Added claude/a_"),
        "{claude:?}"
    );
    let stderr = text(&claude.stderr);
    assert!(stderr.contains("✗ xcb can't run Claude Code"), "{stderr}");
    assert!(!stderr.contains("Opening your browser"), "{stderr}");
    if cfg!(target_os = "linux") {
        assert!(stderr.contains("sandbox checks"), "{stderr}");
        assert!(
            stderr.contains("xcb.sh/docs/providers#claude-on-linux"),
            "{stderr}"
        );
        assert!(stderr.contains("xcb doctor --provider claude"), "{stderr}");
    }
}

#[test]
fn task_and_rename_commands_print_sentences_unless_json() {
    let sandbox = Sandbox::new("task-output");
    let work = sandbox.root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let added = sandbox.run(
        &[
            "--json",
            "backlog",
            "add",
            work.to_str().unwrap(),
            "Fix the parser",
        ],
        &[],
    );
    assert!(added.status.success(), "{added:?}");
    let task: serde_json::Value = serde_json::from_slice(&added.stdout).unwrap();
    let task = task["id"].as_str().unwrap().to_owned();
    let verify = sandbox.run(&["tasks", "verify", &task], &[]);
    assert!(verify.status.success(), "{verify:?}");
    assert_eq!(
        text(&verify.stdout),
        format!("✓ {task}: its 1 recorded step replays and matches the task.\n")
    );
    let json = sandbox.run(&["--json", "tasks", "verify", &task], &[]);
    let value: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(value["verified"], true, "{value}");
    assert_eq!(value["revisions"], 1, "{value}");
    let messages = sandbox.run(&["tasks", "messages", &task, "--limit", "5"], &[]);
    assert_eq!(
        text(&messages.stdout),
        format!("No messages for {task} after sequence 0.\n")
    );
    let renamed = sandbox.run(
        &[
            "rename",
            "c_global",
            "Parser work",
            "--expected-title",
            "Thread",
        ],
        &[],
    );
    assert!(renamed.status.success(), "{renamed:?}");
    assert_eq!(
        text(&renamed.stdout),
        "✓ Renamed c_global to \u{201c}Parser work\u{201d}\n"
    );
    let json = sandbox.run(
        &[
            "--json",
            "rename",
            "c_global",
            "Thread",
            "--expected-title",
            "Parser work",
        ],
        &[],
    );
    let value: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(value["title"], "Thread", "{value}");
    // A dry-run session removal answers in JSON too.
    let json = sandbox.run(&["--json", "sessions", "rm", "s_missing"], &[]);
    assert!(json.status.success(), "{json:?}");
    let value: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(
        value,
        serde_json::json!({"version":1,"applied":false,"session":"s_missing","found":false})
    );
    let prune = sandbox.run(&["command", "prune"], &[]);
    assert!(prune.status.success(), "{prune:?}");
    assert!(
        text(&prune.stdout).starts_with("No offline command jobs"),
        "{prune:?}"
    );
}

/// A terminal subprocess with no provider binaries or credentials. Python's
/// stdlib opens the PTY so the real CLI's terminal detection is exercised.
fn account_terminal(sandbox: &Sandbox, args: &[&str], input: &str) -> String {
    let python = ["/usr/bin/python3", "/usr/local/bin/python3"]
        .into_iter()
        .find(|path| Path::new(path).is_file())
        .expect("Python 3 is required for the terminal CLI regression tests");
    let script = r#"
import os, pty, select, subprocess, sys, time
master, slave = pty.openpty()
child = subprocess.Popen(sys.argv[1:], stdin=slave, stdout=slave, stderr=slave)
os.close(slave)
data = bytearray()
sent = False
deadline = time.monotonic() + 10
try:
    while time.monotonic() < deadline:
        if select.select([master], [], [], .1)[0]:
            try:
                chunk = os.read(master, 65536)
            except OSError:
                break
            if not chunk:
                break
            data.extend(chunk)
            if not sent and b'Choose an account:' in data:
                os.write(master, os.environ['XCB_TEST_INPUT'].encode())
                sent = True
        elif child.poll() is not None:
            break
    try:
        child.wait(timeout=2)
    except subprocess.TimeoutExpired:
        child.kill()
        child.wait()
        raise RuntimeError('terminal CLI timed out: ' + data.decode(errors='replace'))
finally:
    os.close(master)
    if child.poll() is None:
        child.kill()
        child.wait()
sys.stdout.buffer.write(data)
"#;
    let output = Command::new(python)
        .env_clear()
        .env("HOME", sandbox.root.join("home"))
        .env("PATH", sandbox.root.join("bin"))
        .env("LANG", "en_US.UTF-8")
        .env("XCB_TEST_INPUT", input)
        .args(["-c", script, env!("CARGO_BIN_EXE_xcb"), "--state"])
        .arg(sandbox.state())
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    text(&output.stdout)
}

#[test]
fn account_terminal_add_continues_without_copying_the_id() {
    let sandbox = Sandbox::new("account-terminal-add");
    sandbox.fake_provider("claude", "0.1.0 (Claude Code)");
    let output = account_terminal(&sandbox, &["accounts", "add", "claude"], "");
    assert!(output.contains("Added claude/a_"), "{output}");
    assert!(output.contains("xcb can't run Claude Code"), "{output}");
    assert!(!output.contains("Next: xcb accounts login"), "{output}");
    assert!(output.contains("xcb setup claude --account a_"), "{output}");
    let store = xcb_runtime::store::Store::open(&sandbox.state()).unwrap();
    assert_eq!(store.accounts().unwrap().len(), 1);
    assert!(store.unsettled_runs().unwrap().is_empty());
}

#[test]
fn account_terminal_setup_cancel_preserves_existing_accounts() {
    let sandbox = Sandbox::new("account-terminal-cancel");
    let id = sandbox.add(&["codex"]);
    for input in ["0\n", "\x04"] {
        let output = account_terminal(&sandbox, &["setup", "codex"], input);
        assert!(output.contains("Choose an account:"), "{output}");
        assert!(!output.contains("Added"), "{output}");
        assert!(!output.contains("installed"), "{output}");
    }
    let store = xcb_runtime::store::Store::open(&sandbox.state()).unwrap();
    let accounts = store.accounts().unwrap();
    assert_eq!(accounts.len(), 1);
    assert_eq!(accounts[0].id.as_str(), id);
    assert!(store.unsettled_runs().unwrap().is_empty());
}

#[test]
fn account_add_json_and_nonterminal_only_create_accounts() {
    let sandbox = Sandbox::new("account-add-automation");
    let id = sandbox.add(&["codex"]);
    let output = sandbox.run(&["accounts", "add", "claude"], &[]);
    assert!(output.status.success(), "{output:?}");
    assert!(text(&output.stdout).contains("Added claude/a_"));
    assert!(output.stderr.is_empty(), "{output:?}");
    let store = xcb_runtime::store::Store::open(&sandbox.state()).unwrap();
    assert_eq!(store.accounts().unwrap().len(), 2);
    assert_eq!(
        store
            .account(&xcb_core::Id::new(&id).unwrap())
            .unwrap()
            .id
            .as_str(),
        id
    );
    assert!(store.unsettled_runs().unwrap().is_empty());
}

#[test]
fn account_setup_new_preserves_existing_account_and_can_retry_exact_row() {
    let sandbox = Sandbox::new("account-setup-new");
    let original = sandbox.add(&["claude"]);
    sandbox.fake_provider("claude", "0.1.0 (Claude Code)");
    let created = sandbox.run(&["setup", "claude", "--new"], &[]);
    assert!(!created.status.success(), "{created:?}");
    let store = xcb_runtime::store::Store::open(&sandbox.state()).unwrap();
    let accounts = store.accounts().unwrap();
    assert_eq!(accounts.len(), 2);
    let added = accounts
        .iter()
        .find(|account| account.id.as_str() != original)
        .unwrap();
    assert!(text(&created.stderr).contains(&format!("xcb setup claude --account {}", added.id)));
    let retry = sandbox.run(&["setup", "claude", "--account", added.id.as_str()], &[]);
    assert!(!retry.status.success(), "{retry:?}");
    assert_eq!(store.accounts().unwrap().len(), 2);
    assert!(!text(&retry.stdout).contains("Added"));
    assert!(store.unsettled_runs().unwrap().is_empty());
}
