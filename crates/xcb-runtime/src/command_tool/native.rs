#[cfg(any(target_os = "macos", test))]
use crate::native_backend::NativeScope;
use crate::{
    Error, Result,
    broker::Workspace,
    command::{CommandNetwork, CommandRequest},
    store::{RunRecord, Store},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
#[cfg(target_os = "macos")]
use serde_json::json;
use std::{path::Path, sync::Arc};
use tokio::sync::watch;
use xcb_core::policy::EffectState;

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Request {
    pub argv: Vec<String>,
    pub cwd: String,
    pub timeout_ms: u32,
    pub network: Network,
    /// `false` runs this command without the grant's GitHub credentials, so
    /// a timeout can settle. `true` requires them. Omitted follows the grant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github_credentials: Option<bool>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum Network {
    Https,
}

impl Request {
    pub(super) fn parse(arguments: &Value, root: &Path) -> Result<Self> {
        let mut request: Self = serde_json::from_value(arguments.clone())?;
        let _ = request.network;
        let checked = CommandRequest {
            argv: request.argv.clone(),
            cwd: request.cwd.clone(),
            timeout_ms: request.timeout_ms,
            network: CommandNetwork::None,
        }
        .relative_to(root);
        checked.validate()?;
        request.cwd = checked.cwd;
        Ok(request)
    }
}

/// Whether a command runs with the grant's GitHub credentials.
#[cfg(any(target_os = "macos", test))]
fn github_credentials(granted: bool, requested: Option<bool>) -> Result<bool> {
    match requested {
        Some(true) if !granted => Err(Error::Unavailable(
            "GitHub credentials are not granted to this native workspace",
        )),
        Some(requested) => Ok(requested),
        None => Ok(granted),
    }
}

/// Read-only GitHub CLI queries, the usual way a worker waits on CI. Their
/// remote effects are known (none) even when they hold credentials and are
/// stopped at a deadline. Only `gh` found on the command's PATH qualifies:
/// the PATH holds host toolchain and granted read-only roots, never the
/// writable workspace. Shells, `gh api`, aliases and extensions never match.
#[cfg(any(target_os = "macos", test))]
fn read_only_github_query(argv: &[String]) -> bool {
    let [program, group, action, ..] = argv else {
        return false;
    };
    program == "gh"
        && matches!(
            (group.as_str(), action.as_str()),
            ("pr", "checks" | "view" | "list" | "status" | "diff")
                | ("run", "watch" | "view" | "list")
                | ("issue", "view" | "list")
                | ("release", "view" | "list")
                | ("repo", "view")
                | ("workflow", "view" | "list")
        )
        && argv[3..].iter().all(|argument| {
            !matches!(argument.as_str(), "--web" | "-w") && !argument.starts_with("--web=")
        })
}

/// A timed-out native command whose group xcb stopped and proved absent has
/// known effects when it could not have changed anything but local files:
/// it ran without GitHub credentials (the only remote authority a native
/// command can hold), or it was a read-only GitHub query.
#[cfg(any(target_os = "macos", test))]
fn timeout_settles(github: bool, argv: &[String]) -> bool {
    !github || read_only_github_query(argv)
}

#[cfg(any(target_os = "macos", test))]
pub(crate) fn policy(scope: &NativeScope, home: &Path, state: &Path) -> Result<String> {
    crate::native_backend::validate_scope(scope, state)?;
    let quote = |path: &Path| -> Result<String> {
        Ok(serde_json::to_string(
            path.to_str().ok_or(Error::PrivateState)?,
        )?)
    };
    let paths = |roots: &[std::path::PathBuf], kind: &str| -> Result<String> {
        roots
            .iter()
            .map(|path| Ok(format!("({kind} {})", quote(path)?)))
            .collect::<Result<Vec<_>>>()
            .map(|parts| parts.join(" "))
    };
    let mut protected = crate::native_backend::protected_paths(state)?;
    if scope.host_read {
        protected.extend(host_private_paths(home_dir()?));
    }
    let mut ancestors = scope.read_only_roots.clone();
    ancestors.extend(scope.git_metadata.clone());
    let bindings = [
        ("workspace", quote(&scope.workspace)?),
        ("home", quote(home)?),
        ("read_only", paths(&scope.read_only_roots, "subpath")?),
        ("git_write", paths(&scope.git_metadata, "subpath")?),
        ("ancestors", paths(&ancestors, "path-ancestors")?),
        (
            "host_read",
            if scope.host_read {
                "(allow file-read* process-exec file-map-executable (subpath \"/\"))\n".into()
            } else {
                String::new()
            },
        ),
        ("protected", paths(&protected, "subpath")?),
    ];
    include_str!("../native-command.sb")
        .split('@')
        .enumerate()
        .map(|(index, part)| {
            if index % 2 == 0 {
                return Ok(part.to_owned());
            }
            bindings
                .iter()
                .find(|(name, _)| *name == part)
                .map(|(_, value)| value.clone())
                .ok_or(Error::Protocol("unknown native confinement template field"))
        })
        .collect::<Result<Vec<_>>>()
        .map(|parts| parts.join(""))
}

#[cfg(any(target_os = "macos", test))]
fn home_dir() -> Result<std::path::PathBuf> {
    Ok(xcb_core::canonical(
        xcb_core::home_dir().ok_or(Error::PrivateState)?,
    )?)
}

/// Personal data a host-read grant still hides, beyond xcb's protected
/// credentials: configuration, browser, mail and message stores, and shell
/// history. Toolchains never live here.
#[cfg(any(target_os = "macos", test))]
fn host_private_paths(home: std::path::PathBuf) -> Vec<std::path::PathBuf> {
    [
        ".config",
        ".zsh_history",
        ".bash_history",
        ".zsh_sessions",
        ".docker",
        ".kube",
        ".gnupg",
        "Library/Application Support",
        "Library/Containers",
        "Library/Group Containers",
        "Library/Cookies",
        "Library/Mail",
        "Library/Messages",
        "Library/Safari",
        "Library/Accounts",
        "Library/Mobile Documents",
    ]
    .map(|path| home.join(path))
    .into()
}

/// Host toolchain directories a host-read command searches before system
/// directories, in the order a login shell usually puts them.
#[cfg(target_os = "macos")]
fn host_toolchain_paths(home: &Path) -> Vec<std::path::PathBuf> {
    let mut node = std::fs::read_dir(home.join(".nvm/versions/node"))
        .map(|entries| {
            entries
                .filter_map(|entry| entry.ok().map(|entry| entry.path().join("bin")))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    node.sort_by_key(|path| {
        path.parent()
            .and_then(|version| version.file_name())
            .and_then(|name| name.to_str())
            .map(|name| {
                name.trim_start_matches('v')
                    .split('.')
                    .map(|part| part.parse::<u64>().unwrap_or(0))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    });
    [home.join(".cargo/bin"), home.join(".bun/bin")]
        .into_iter()
        .chain(node.pop())
        .chain(
            ["/opt/homebrew/bin", "/opt/homebrew/sbin", "/usr/local/bin"]
                .map(std::path::PathBuf::from),
        )
        .filter(|path| path.is_dir())
        .collect()
}

#[cfg(target_os = "macos")]
pub(super) async fn call(
    store: Arc<Store>,
    run: RunRecord,
    call: String,
    workspace: Arc<Workspace>,
    request: Request,
    cancel: watch::Receiver<bool>,
) -> (Result<Value>, EffectState) {
    let result = prepare_and_execute(&store, &run, &call, &workspace, request, cancel).await;
    result.unwrap_or_else(|error| {
        let effects = if error.is_cleanup_unproven() {
            EffectState::Uncertain
        } else {
            EffectState::None
        };
        (Err(error), effects)
    })
}

#[cfg(not(target_os = "macos"))]
pub(super) async fn call(
    _store: Arc<Store>,
    _run: RunRecord,
    _call: String,
    _workspace: Arc<Workspace>,
    _request: Request,
    _cancel: watch::Receiver<bool>,
) -> (Result<Value>, EffectState) {
    (
        Err(Error::Unavailable(
            "native command confinement is not qualified on this platform",
        )),
        EffectState::None,
    )
}

#[cfg(target_os = "macos")]
async fn prepare_and_execute(
    store: &Store,
    run: &RunRecord,
    call: &str,
    workspace: &Workspace,
    request: Request,
    cancel: watch::Receiver<bool>,
) -> Result<(Result<Value>, EffectState)> {
    use crate::{private, process, runner::LaunchArtifacts};
    use std::time::Duration;
    use tokio::process::Command;
    if *cancel.borrow() {
        return Err(Error::Unavailable("native command cancelled before launch"));
    }
    let _writer = workspace.native_writer()?;
    let arguments = serde_json::to_value(&request)?;
    let scope = crate::native_backend::admit(store, run, workspace.root(), &arguments, call, true)?;
    let cwd = xcb_core::canonical(workspace.root().join(&request.cwd))?;
    if !cwd.starts_with(workspace.root()) || !cwd.is_dir() {
        return Err(Error::Unavailable(
            "native command cwd escaped its workspace",
        ));
    }
    let base = private::directory(&super::default_root()?.join("native"))?;
    let mut artifacts = LaunchArtifacts::create(&base)?;
    let home = private::directory(&artifacts.path().join("home"))?;
    private::directory(&home.join("tmp"))?;
    let policy_path = artifacts.path().join("command.sb");
    private::create(
        &policy_path,
        policy(&scope, &home, store.root())?.as_bytes(),
    )?;
    let mut command = Command::new("/usr/bin/sandbox-exec");
    command
        .arg("-f")
        .arg(policy_path)
        .args(&request.argv)
        .current_dir(cwd);
    let host = home_dir()?;
    let mut paths = Vec::new();
    if scope.host_read {
        paths.extend(
            host_toolchain_paths(&host)
                .into_iter()
                .map(|path| path.to_string_lossy().into_owned()),
        );
    }
    for root in &scope.read_only_roots {
        paths.push(root.to_string_lossy().into_owned());
        if root.join("bin").is_dir() {
            paths.push(root.join("bin").to_string_lossy().into_owned());
        }
    }
    paths.extend([
        "/usr/bin".to_string(),
        "/bin".into(),
        "/usr/sbin".into(),
        "/sbin".into(),
    ]);
    let github = github_credentials(scope.github_credentials, request.github_credentials)?;
    let custody = format!("native_{}", &crate::digest(call)[..32]);
    store.mark_capability_starting(run, &custody)?;
    artifacts.retain_before_launch();
    let token = match super::host::native_environment(
        &mut command,
        artifacts.path(),
        &home,
        &paths.join(":"),
        github,
        cancel.clone(),
    )
    .await
    {
        Ok(token) => {
            // The command's HOME is private scratch, so point rustup at the
            // host's installed toolchains for rust-toolchain.toml pins.
            if scope.host_read && host.join(".rustup").is_dir() {
                command.env("RUSTUP_HOME", host.join(".rustup"));
            }
            token
        }
        Err(error) => {
            let uncertain = error.is_cleanup_unproven();
            if !uncertain {
                store
                    .clear_capability_custody(run, &custody)
                    .map_err(|_| Error::CleanupUnproven)?;
            }
            artifacts.release_after_join(
                !uncertain,
                if uncertain {
                    EffectState::Uncertain
                } else {
                    EffectState::None
                },
            );
            return Err(error);
        }
    };
    let outcome = process::capture_command(
        command,
        32 * 1024,
        Duration::from_millis(u64::from(request.timeout_ms)),
        cancel.clone(),
        |pid| store.mark_capability_spawned(run, &custody, pid),
    )
    .await;
    let redact = |bytes: &[u8]| {
        let text = zeroize::Zeroizing::new(String::from_utf8_lossy(bytes).into_owned());
        let text = if token.is_empty() {
            text.to_string()
        } else {
            text.replace(token.as_str(), "[credential redacted]")
        };
        xcb_core::display_text(&text, 16384)
    };
    let (output, effects, joined) = match outcome {
        process::CommandOutcome::NeverStarted(error) => (Err(error), EffectState::None, true),
        // A command that ran to its own exit has a known result whatever its
        // status: return it so the worker can read the failure and continue.
        process::CommandOutcome::Exited {
            code,
            stdout,
            stderr,
            truncated,
        } if !*cancel.borrow() => (
            Ok(
                json!({"stdout":redact(&stdout),"stderr":redact(&stderr),"exitCode":code,"truncated":truncated,"network":"https","joined":true,"published":true,"sandbox":"native-workspace"}),
            ),
            EffectState::Settled,
            true,
        ),
        // The deadline passed, xcb stopped the whole group and proved it
        // gone. Without remote authority its only effects are the workspace
        // files it changed, which the worker can inspect: a settled result,
        // so one slow build does not leave the whole task uncertain.
        process::CommandOutcome::TimedOut { stdout, stderr }
            if !*cancel.borrow() && timeout_settles(github, &request.argv) =>
        {
            (
                Ok(json!({
                    "status": "timed_out",
                    "timedOut": true,
                    "timeoutMs": request.timeout_ms,
                    "exitCode": null,
                    "stdout": redact(&stdout),
                    "stderr": redact(&stderr),
                    "truncated": true,
                    "network": "https",
                    "joined": true,
                    "published": true,
                    "sandbox": "native-workspace",
                    "note": "xcb stopped the command at timeoutMs and confirmed every process in its group exited. Any workspace file changes it made before stopping are kept; inspect them (for example git status) before continuing. Run a narrower command or give it a larger timeoutMs (up to 600000).",
                })),
                EffectState::Settled,
                true,
            )
        }
        process::CommandOutcome::TimedOut { .. } if !*cancel.borrow() => (
            Err(Error::Unavailable(
                "native command timed out while holding GitHub credentials, so its remote effects are unknown; reconcile effects before retrying. Run builds and tests with githubCredentials false so a timeout settles",
            )),
            EffectState::Uncertain,
            true,
        ),
        process::CommandOutcome::Exited { .. }
        | process::CommandOutcome::TimedOut { .. }
        | process::CommandOutcome::Interrupted => (
            Err(Error::Unavailable(
                "native command timed out or was cancelled; reconcile effects before retrying",
            )),
            EffectState::Uncertain,
            true,
        ),
        process::CommandOutcome::Unproven => {
            (Err(Error::CleanupUnproven), EffectState::Uncertain, false)
        }
    };
    if joined {
        store
            .clear_capability_custody(run, &custody)
            .map_err(|_| Error::CleanupUnproven)?;
    }
    artifacts.release_after_join(joined, effects);
    Ok((output, effects))
}

#[cfg(target_os = "macos")]
async fn process_privacy_probe(policy_path: &Path, home: &Path, workspace: &Path) -> Result<()> {
    use crate::{private, process};
    use std::time::Duration;
    use tokio::{process::Command, sync::oneshot};
    let source = home.with_file_name("private-process.c");
    let executable = home.with_file_name("private-process");
    private::create(
        &source,
        b"extern int pause(void); int main(void) { for (;;) pause(); }\n",
    )?;
    let (_control, cancelled) = watch::channel(false);
    let mut compiler = Command::new("/usr/bin/clang");
    compiler
        .arg(&source)
        .arg("-o")
        .arg(&executable)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", home);
    match process::capture_supervised(compiler, 4096, Duration::from_secs(30), cancelled, |_| {
        Ok(())
    })
    .await
    {
        process::CaptureOutcome::Joined(Ok(_)) => (),
        process::CaptureOutcome::Unproven => return Err(Error::CleanupUnproven),
        _ => {
            return Err(Error::Unavailable(
                "synthetic process privacy fixture could not compile",
            ));
        }
    }
    let (stop, cancelled) = watch::channel(false);
    let (started, pid) = oneshot::channel();
    let owner = tokio::spawn(async move {
        let mut child = Command::new(executable);
        child
            .env_clear()
            .env("XCB_NATIVE_PRIVATE_PROBE", "synthetic-not-a-credential");
        process::capture_supervised(
            child,
            4096,
            Duration::from_secs(60),
            cancelled,
            move |group| {
                let _ = started.send(group);
                Ok(())
            },
        )
        .await
    });
    let result = async {
        let pid = pid.await.map_err(|_| Error::Unavailable("synthetic process privacy control did not start"))?.to_string();
        let (_control, cancelled) = watch::channel(false);
        let mut command = Command::new("/bin/ps");
        command.args(["eww","-p",&pid]).env_clear();
        match process::capture_supervised(command,16384,Duration::from_secs(10),cancelled.clone(),|_|Ok(())).await {
            process::CaptureOutcome::Joined(Ok(bytes)) if String::from_utf8_lossy(&bytes).contains("XCB_NATIVE_PRIVATE_PROBE") => (),
            process::CaptureOutcome::Unproven => return Err(Error::CleanupUnproven),
            process::CaptureOutcome::Joined(Ok(bytes)) => return Err(Error::Guided { message: format!("synthetic process privacy control did not expose its synthetic marker: {}", xcb_core::display_text(&String::from_utf8_lossy(&bytes), 256)), next: None }),
            process::CaptureOutcome::Joined(Err(error)) | process::CaptureOutcome::NeverStarted(error) => return Err(error),
        }
        let mut command = Command::new("/usr/bin/sandbox-exec");
        command.arg("-f").arg(policy_path).args(["/bin/sh","-c","value=$(ps eww -p \"$1\" 2>/dev/null || :); case \"$value\" in *XCB_NATIVE_PRIVATE_PROBE*) exit 20;; esac; printf native-private-process-ok","probe",&pid]).env_clear().env("HOME",home).env("PATH","/usr/bin:/bin").current_dir(workspace);
        match process::capture_supervised(command,4096,Duration::from_secs(10),cancelled,|_|Ok(())).await {
            process::CaptureOutcome::Joined(Ok(bytes)) if bytes.as_slice() == b"native-private-process-ok" => Ok(()),
            process::CaptureOutcome::Unproven => Err(Error::CleanupUnproven),
            _ => Err(Error::Unavailable("native commands could inspect another process's synthetic private environment")),
        }
    }.await;
    let _ = stop.send(true);
    match owner.await {
        Ok(process::CaptureOutcome::Joined(_)) | Ok(process::CaptureOutcome::NeverStarted(_)) => {
            result
        }
        _ => Err(Error::CleanupUnproven),
    }
}

pub async fn qualify(state: &Path) -> Result<crate::native_backend::Qualification> {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = state;
        Err(Error::Unavailable(
            "native command confinement is not qualified on this platform",
        ))
    }
    #[cfg(target_os = "macos")]
    {
        use crate::{private, process};
        use std::time::Duration;
        use tokio::process::Command;
        if !crate::sandbox::available() {
            return Err(Error::Unavailable("native OS confinement is unavailable"));
        }
        let temp = tempfile::tempdir()?;
        let root = private::directory(&xcb_core::canonical(temp.path())?.join("probe"))?;
        let workspace = private::directory(&root.join("workspace"))?;
        let home = private::directory(&root.join("home"))?;
        private::directory(&home.join("tmp"))?;
        let hidden = root.join("hidden");
        private::create(&hidden, b"synthetic private marker")?;
        let scope = NativeScope {
            workspace: workspace.clone(),
            providers: xcb_core::Provider::SUPPORTED.to_vec(),
            github_credentials: false,
            read_only_roots: vec![],
            git_metadata: vec![],
            host_read: false,
        };
        let policy_path = root.join("command.sb");
        private::create(&policy_path, policy(&scope, &home, state)?.as_bytes())?;
        for argv in [
            vec!["/bin/sh".into(), "-c".into(), "set -eu; printf native-ok > marker; test \"$(cat marker)\" = native-ok; if cat \"$1\" 2>/dev/null; then exit 20; fi; if printf denied > \"$1\" 2>/dev/null; then exit 21; fi; ln -s \"$1\" escaped; if cat escaped 2>/dev/null; then exit 22; fi; if printf denied > escaped 2>/dev/null; then exit 23; fi; printf native-confinement-ok".into(), "probe".into(), hidden.to_string_lossy().into_owned()],
            vec!["/usr/bin/curl".into(), "--silent".into(), "--show-error".into(), "--fail".into(), "--max-time".into(), "20".into(), "--output".into(), "/dev/null".into(), "--write-out".into(), "native-dns-https-ok".into(), "https://api.github.com/meta".into()],
        ] {
            let mut command = Command::new("/usr/bin/sandbox-exec");
            command.arg("-f").arg(&policy_path).args(&argv).env_clear().env("HOME", &home).env("TMPDIR", home.join("tmp")).env("PATH", "/usr/bin:/bin").current_dir(&workspace);
            let (_owner, cancel) = watch::channel(false);
            match process::capture_supervised(command, 4096, Duration::from_secs(30), cancel, |_| Ok(())).await {
                process::CaptureOutcome::Joined(Ok(bytes)) if bytes.as_slice() == b"native-confinement-ok" || bytes.as_slice() == b"native-dns-https-ok" => (),
                process::CaptureOutcome::Unproven => { let _ = temp.keep(); return Err(Error::CleanupUnproven); }
                _ => return Err(Error::Unavailable("native confinement or DNS/HTTPS probe failed")),
            }
        }
        match process_privacy_probe(&policy_path, &home, &workspace).await {
            Err(Error::CleanupUnproven) => {
                let _ = temp.keep();
                return Err(Error::CleanupUnproven);
            }
            result => result?,
        }
        if std::fs::read(&hidden)? != b"synthetic private marker" {
            return Err(Error::Unavailable("native probe escaped confinement"));
        }
        let receipt = crate::native_backend::Qualification {
            version: 1,
            policy_sha256: crate::digest(include_bytes!("../native-command.sb")),
            host_sha256: process::host_identity()?.1,
            platform: std::env::consts::OS.into(),
            observed_at_ms: crate::now_ms(),
            passed: true,
        };
        private::directory(&state.join("qualification"))?;
        let path = state.join("qualification/native-command.json");
        let bytes = serde_json::to_vec_pretty(&receipt)?;
        match private::read(&path, 16384) {
            Ok(previous) => private::replace(&path, &bytes, &crate::digest(previous))?,
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                private::create(&path, &bytes)?
            }
            Err(error) => return Err(error),
        }
        Ok(receipt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn native_requests_never_accept_environment_or_offline_fallback() {
        let root = Path::new("/tmp/workspace");
        let valid = json!({"argv":["/bin/sh","-c","printf ok"],"cwd":".","timeoutMs":1000,"network":"https"});
        Request::parse(&valid, root).unwrap();
        for (key, value) in [
            ("env", json!({"GH_TOKEN":"injected"})),
            ("network", json!("none")),
            ("network", json!("host")),
            ("cwd", json!("../peer")),
            ("timeoutMs", json!(600001)),
            ("argv", json!([])),
        ] {
            let mut bad = valid.clone();
            bad[key] = value;
            assert!(Request::parse(&bad, root).is_err(), "{key}");
        }
    }

    #[test]
    fn native_requests_may_withhold_or_require_granted_github_credentials() {
        let root = Path::new("/tmp/workspace");
        let base = json!({"argv":["cargo","build"],"cwd":".","timeoutMs":600000,"network":"https"});
        let request = Request::parse(&base, root).unwrap();
        assert_eq!(request.github_credentials, None);
        // Omitted, the field leaves the request (and its permit digest) as before.
        assert!(
            serde_json::to_value(&request)
                .unwrap()
                .get("githubCredentials")
                .is_none()
        );
        for value in [false, true] {
            let mut with = base.clone();
            with["githubCredentials"] = json!(value);
            assert_eq!(
                Request::parse(&with, root).unwrap().github_credentials,
                Some(value)
            );
        }
        let mut bad = base.clone();
        bad["githubCredentials"] = json!("no");
        assert!(Request::parse(&bad, root).is_err());
        assert!(github_credentials(true, None).unwrap());
        assert!(!github_credentials(true, Some(false)).unwrap());
        assert!(github_credentials(true, Some(true)).unwrap());
        assert!(!github_credentials(false, None).unwrap());
        assert!(!github_credentials(false, Some(false)).unwrap());
        assert!(github_credentials(false, Some(true)).is_err());
    }

    #[test]
    fn only_uncredentialed_commands_and_read_only_github_queries_settle_timeouts() {
        let argv = |items: &[&str]| {
            items
                .iter()
                .map(|item| item.to_string())
                .collect::<Vec<_>>()
        };
        // Without credentials a stopped command can only have changed local files.
        for command in [
            argv(&["cargo", "build", "--workspace"]),
            argv(&["/bin/sh", "-c", "git push origin HEAD"]),
            argv(&["gh", "pr", "merge", "1"]),
        ] {
            assert!(timeout_settles(false, &command), "{command:?}");
        }
        for command in [
            argv(&["gh", "pr", "checks", "12", "--watch"]),
            argv(&["gh", "run", "watch", "123", "--exit-status"]),
            argv(&["gh", "pr", "view", "12", "--json", "state"]),
            argv(&["gh", "release", "list"]),
        ] {
            assert!(timeout_settles(true, &command), "{command:?}");
        }
        // With credentials, anything that could write remotely stays uncertain.
        for command in [
            argv(&["cargo", "build"]),
            argv(&["bun", "test"]),
            argv(&["git", "push"]),
            argv(&["gh", "pr", "merge", "12", "--auto"]),
            argv(&["gh", "pr", "create"]),
            argv(&["gh", "api", "repos/o/r/pulls"]),
            argv(&["gh", "run", "rerun", "1"]),
            argv(&["gh", "pr", "view", "12", "--web"]),
            argv(&["gh", "pr", "view", "--web=true"]),
            argv(&["gh", "pr"]),
            argv(&["./gh", "pr", "view"]),
            argv(&["/opt/homebrew/bin/gh", "pr", "checks"]),
            argv(&["/bin/sh", "-c", "gh pr checks 12 --watch"]),
        ] {
            assert!(!timeout_settles(true, &command), "{command:?}");
        }
    }

    #[test]
    fn native_confinement_quotes_paths_without_expanding_path_contents() {
        let directory = tempfile::tempdir().unwrap();
        let base = xcb_core::canonical(directory.path()).unwrap();
        // Windows forbids double quotes in file names. Its canonical path
        // supplies backslashes to exercise JSON escaping; both platforms
        // retain the template token and literal quotation characters.
        let name = if cfg!(windows) {
            "workspace_@home@_'quoted'"
        } else {
            "workspace_@home@_\"quoted\""
        };
        let workspace = crate::private::directory(&base.join(name)).unwrap();
        let home = crate::private::directory(&base.join("home")).unwrap();
        let state = crate::private::directory(&base.join("state")).unwrap();
        let scope = NativeScope {
            workspace: workspace.clone(),
            providers: xcb_core::Provider::SUPPORTED.to_vec(),
            github_credentials: false,
            read_only_roots: vec![],
            git_metadata: vec![],
            host_read: false,
        };
        let rendered = policy(&scope, &home, &state).unwrap();
        assert!(rendered.contains(&serde_json::to_string(&workspace).unwrap()));
        assert!(rendered.contains(&format!(
            "(subpath {})",
            serde_json::to_string(&state).unwrap()
        )));
        assert!(rendered.contains("(deny default)"));
        assert!(rendered.contains("(remote tcp \"*:443\")"));
        assert!(rendered.contains("(deny network-inbound)"));
        assert!(!rendered.contains("@protected@"));
        assert!(!rendered.contains("(allow network-outbound)"));
        assert!(!rendered.contains("(subpath \"/\"))"));
    }

    #[test]
    fn host_read_opens_host_paths_but_still_hides_private_state_last() {
        let directory = tempfile::tempdir().unwrap();
        let base = xcb_core::canonical(directory.path()).unwrap();
        let workspace = crate::private::directory(&base.join("workspace")).unwrap();
        let home = crate::private::directory(&base.join("home")).unwrap();
        let state = crate::private::directory(&base.join("state")).unwrap();
        let scope = NativeScope {
            workspace,
            providers: xcb_core::Provider::SUPPORTED.to_vec(),
            github_credentials: false,
            read_only_roots: vec![],
            git_metadata: vec![],
            host_read: true,
        };
        let rendered = policy(&scope, &home, &state).unwrap();
        let open = rendered
            .find("(allow file-read* process-exec file-map-executable (subpath \"/\"))")
            .expect("host read rule");
        let hidden = rendered.find("(deny file-read* file-write*").unwrap();
        // Seatbelt applies the last matching rule, so private state must follow the host rule.
        assert!(open < hidden);
        let deny = &rendered[hidden..];
        let real_home = home_dir().unwrap();
        for private in [
            ".ssh",
            ".codex",
            ".claude",
            ".config",
            "Library/Keychains",
            "Library/Application Support",
        ] {
            assert!(
                deny.contains(&serde_json::to_string(&real_home.join(private)).unwrap()),
                "{private}"
            );
        }
        assert!(deny.contains(&serde_json::to_string(&state).unwrap()));
        assert!(!rendered.contains("(allow file-write* (subpath \"/\"))"));
        assert!(!rendered.contains("@host_read@"));
    }
}
