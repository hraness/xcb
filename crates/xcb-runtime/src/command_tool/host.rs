//! Opt-in host command lane. Unlike VM replay this runs in the real worktree
//! with ordinary host network access and Git credentials.
use crate::{
    Error, Result,
    command::{CommandNetwork, CommandRequest},
    private, process,
    store::{RunRecord, Store},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{process::Command, sync::watch};
use xcb_core::{Provider, policy::EffectState};
use zeroize::Zeroizing;

const OUTPUT_LIMIT: usize = 32 * 1024;
const ENV_LIMIT: usize = 16 * 1024;
const HOST_POLICY: &str = "XCB_HOST_CREDENTIALS_TASK";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Request {
    argv: Vec<String>,
    cwd: String,
    timeout_ms: u32,
    network: HostNetwork,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum HostNetwork {
    Host,
}

impl Request {
    pub(super) fn parse(value: &Value, root: &Path) -> Result<Self> {
        let mut request: Self = serde_json::from_value(value.clone())?;
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

    fn directory(&self, root: &Path) -> Result<PathBuf> {
        let path = xcb_core::canonical(root.join(&self.cwd))?;
        if !path.starts_with(root) || !path.is_dir() || xcb_core::canonical(root)? != root {
            return Err(Error::Unavailable(
                "host command cwd changed or escaped the workspace",
            ));
        }
        Ok(path)
    }
}

fn policy_allows(
    provider: Provider,
    workspace: &str,
    root: &Path,
    task: Option<&str>,
    policy: &str,
) -> bool {
    provider == Provider::Codex
        && workspace == root.to_string_lossy()
        && task.is_some_and(|task| !task.is_empty() && task.len() <= 160 && task == policy)
}

/// Only trusted host policy may nominate this exact managed task. A prompt,
/// argument or workspace file cannot grant the lane. The ordinary owned run
/// and account lease are verified before any credential lookup or child spawn.
pub(super) fn admit(store: &Store, run: &RunRecord, root: &Path) -> Result<()> {
    store.verify_owned_run(run)?;
    let session = run
        .session
        .as_ref()
        .and_then(|id| store.session(id).ok().flatten())
        .ok_or(Error::Unavailable(
            "host exec requires a managed Codex session",
        ))?;
    let policy = std::env::var(HOST_POLICY).unwrap_or_default();
    if !policy_allows(
        session.model.provider,
        &session.workspace,
        root,
        session.managed_task.as_ref().map(xcb_core::Id::as_str),
        &policy,
    ) {
        return Err(Error::Unavailable(
            "host credentials are not admitted for this task",
        ));
    }
    Ok(())
}

struct Credentials {
    token: Zeroizing<String>,
    author_name: Option<String>,
    author_email: Option<String>,
    ssh_socket: Option<PathBuf>,
}

fn host_command(program: &str, args: &[&str], home: &Path, path: &str) -> Command {
    let mut command = Command::new(program);
    command
        .args(args)
        .env_clear()
        .env("HOME", home)
        .env("PATH", path)
        .env("LANG", "en_US.UTF-8")
        .env("NO_COLOR", "1");
    for key in ["GH_TOKEN", "GITHUB_TOKEN", "XDG_CONFIG_HOME"] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    command
}

async fn host_value(
    program: &str,
    args: &[&str],
    home: &Path,
    path: &str,
    limit: usize,
    cancel: watch::Receiver<bool>,
) -> Result<Option<Zeroizing<String>>> {
    if *cancel.borrow() {
        return Err(Error::Unavailable("host credential lookup cancelled"));
    }
    let output = match process::capture_supervised(
        host_command(program, args, home, path),
        limit,
        Duration::from_secs(10),
        cancel,
        |_| Ok(()),
    )
    .await
    {
        process::CaptureOutcome::Joined(Ok(bytes)) => bytes,
        process::CaptureOutcome::Unproven => return Err(Error::CleanupUnproven),
        _ => return Ok(None),
    };
    let value = Zeroizing::new(
        String::from_utf8(output.to_vec())
            .map_err(|_| Error::Unavailable("host identity encoding invalid"))?,
    );
    let trimmed = value.trim_end_matches(['\r', '\n']);
    if trimmed.is_empty() || trimmed.len() > limit || trimmed.contains(['\r', '\n', '\0']) {
        return Ok(None);
    }
    Ok(Some(Zeroizing::new(trimmed.to_owned())))
}

fn host_path(current: Option<String>) -> Result<String> {
    let mut path =
        current.unwrap_or_else(|| "/usr/bin:/bin:/usr/local/bin:/opt/homebrew/bin".into());
    if path.len() > 4096
        || path.contains(['\0', '\r', '\n'])
        || path
            .split(':')
            .any(|root| !xcb_core::absolute_clean(Path::new(root)))
    {
        return Err(Error::PrivateState);
    }
    for root in [
        "/usr/bin",
        "/bin",
        "/usr/sbin",
        "/sbin",
        "/usr/local/bin",
        "/opt/homebrew/bin",
    ] {
        if !path.split(':').any(|existing| existing == root) {
            path.push(':');
            path.push_str(root);
        }
    }
    if path.len() > 4096 {
        return Err(Error::PrivateState);
    }
    Ok(path)
}

async fn credentials(cancel: watch::Receiver<bool>) -> Result<(Credentials, PathBuf, String)> {
    let home = PathBuf::from(std::env::var_os("HOME").ok_or(Error::PrivateState)?);
    if !home.is_absolute() || xcb_core::canonical(&home)? != home {
        return Err(Error::PrivateState);
    }
    let path = host_path(std::env::var("PATH").ok())?;
    let token = host_value("gh", &["auth", "token"], &home, &path, 4096, cancel.clone())
        .await?
        .ok_or(Error::Unavailable(
            "host GitHub authentication is not ready",
        ))?;
    if !token.bytes().all(|byte| byte.is_ascii_graphic()) {
        return Err(Error::Unavailable(
            "host GitHub authentication is not ready",
        ));
    }
    let name = host_value(
        "git",
        &["config", "--global", "--get", "user.name"],
        &home,
        &path,
        512,
        cancel.clone(),
    )
    .await?
    .map(|value| value.to_string());
    let email = host_value(
        "git",
        &["config", "--global", "--get", "user.email"],
        &home,
        &path,
        512,
        cancel,
    )
    .await?
    .map(|value| value.to_string());
    let ssh_socket = std::env::var_os("SSH_AUTH_SOCK")
        .map(PathBuf::from)
        .filter(|socket| {
            socket.is_absolute() && socket.as_os_str().len() <= 4096 && socket.exists()
        });
    Ok((
        Credentials {
            token,
            author_name: name,
            author_email: email,
            ssh_socket,
        },
        home,
        path,
    ))
}

fn append(body: &mut Zeroizing<Vec<u8>>, key: &str, value: &str) -> Result<()> {
    if value.contains(['\0', '\r', '\n']) || value.len() > 4096 {
        return Err(Error::Unavailable("host credential environment invalid"));
    }
    body.extend_from_slice(key.as_bytes());
    body.push(b'=');
    body.extend_from_slice(value.as_bytes());
    body.push(b'\n');
    if body.len() > ENV_LIMIT {
        return Err(Error::Unavailable("host credential environment limit"));
    }
    Ok(())
}

/// A mode-0600 host-owned env file is consumed immediately before launch. The
/// serialized and readback buffers are zeroized; no env bytes enter a receipt.
fn child_environment(
    command: &mut Command,
    directory: &Path,
    credentials: &Credentials,
    home: &Path,
    path: &str,
) -> Result<()> {
    let mut body = Zeroizing::new(Vec::new());
    append(&mut body, "HOME", home.to_str().ok_or(Error::PrivateState)?)?;
    append(&mut body, "PATH", path)?;
    append(&mut body, "GH_TOKEN", &credentials.token)?;
    append(&mut body, "GITHUB_TOKEN", &credentials.token)?;
    append(&mut body, "GIT_TERMINAL_PROMPT", "0")?;
    // Process-scoped Git helper; never rewrite the owner's global config.
    append(&mut body, "GIT_CONFIG_COUNT", "1")?;
    append(
        &mut body,
        "GIT_CONFIG_KEY_0",
        "credential.https://github.com.helper",
    )?;
    append(&mut body, "GIT_CONFIG_VALUE_0", "!gh auth git-credential")?;
    if let Some(name) = &credentials.author_name {
        append(&mut body, "GIT_AUTHOR_NAME", name)?;
        append(&mut body, "GIT_COMMITTER_NAME", name)?;
    }
    if let Some(email) = &credentials.author_email {
        append(&mut body, "GIT_AUTHOR_EMAIL", email)?;
        append(&mut body, "GIT_COMMITTER_EMAIL", email)?;
    }
    if let Some(socket) = &credentials.ssh_socket {
        append(
            &mut body,
            "SSH_AUTH_SOCK",
            socket.to_str().ok_or(Error::PrivateState)?,
        )?;
    }
    let env_file = directory.join(format!("{}.env", crate::new_id("host")));
    private::create(&env_file, &body).map_err(|_| Error::CleanupUnproven)?;
    let loaded = private::read(&env_file, ENV_LIMIT);
    // Removal is attempted even when readback fails. An unremoved credential
    // file is uncertain and must not release this task's account custody.
    let removed = std::fs::remove_file(&env_file);
    if removed.is_err() {
        return Err(Error::CleanupUnproven);
    }
    let loaded = Zeroizing::new(loaded?);
    if loaded.as_slice() != body.as_slice() {
        return Err(Error::Conflict("host credential env changed during launch"));
    }
    command
        .env_clear()
        .env("LANG", "en_US.UTF-8")
        .env("NO_COLOR", "1");
    for line in loaded
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        let separator = line
            .iter()
            .position(|byte| *byte == b'=')
            .ok_or(Error::PrivateState)?;
        let (key, value) = line.split_at(separator);
        let value = &value[1..];
        command.env(
            std::str::from_utf8(key).map_err(|_| Error::PrivateState)?,
            std::str::from_utf8(value).map_err(|_| Error::PrivateState)?,
        );
    }
    Ok(())
}

fn scrub(bytes: &[u8], token: &str) -> String {
    // A host command can transmit credentials over the network by design;
    // this protects xcb's result and receipt against literal token echo.
    let text = Zeroizing::new(String::from_utf8_lossy(bytes).into_owned());
    let redacted = Zeroizing::new(text.replace(token, "[credential redacted]"));
    xcb_core::display_text(&redacted, 16_384)
}

async fn execute(
    root: &Path,
    request: Request,
    credentials: Credentials,
    home: PathBuf,
    path: String,
    cancel: watch::Receiver<bool>,
    env_root: &Path,
) -> (Result<Value>, EffectState) {
    if *cancel.borrow() {
        return (
            Err(Error::Unavailable("host command cancelled before launch")),
            EffectState::None,
        );
    }
    let cwd = match request.directory(root) {
        Ok(cwd) => cwd,
        Err(error) => return (Err(error), EffectState::None),
    };
    let mut command = Command::new(&request.argv[0]);
    command.args(&request.argv[1..]).current_dir(cwd);
    if let Err(error) = child_environment(&mut command, env_root, &credentials, &home, &path) {
        let effects = if error.is_cleanup_unproven() {
            EffectState::Uncertain
        } else {
            EffectState::None
        };
        return (Err(error), effects);
    }
    match process::capture_supervised(
        command,
        OUTPUT_LIMIT,
        Duration::from_millis(u64::from(request.timeout_ms)),
        cancel.clone(),
        |_| Ok(()),
    )
    .await
    {
        process::CaptureOutcome::NeverStarted(_) => (
            Err(Error::Unavailable("host command did not start")),
            EffectState::None,
        ),
        process::CaptureOutcome::Joined(Ok(_)) if *cancel.borrow() => (
            Err(Error::Unavailable("host command was cancelled")),
            EffectState::Uncertain,
        ),
        process::CaptureOutcome::Joined(Ok(bytes)) => (
            Ok(json!({
                "stdout": scrub(&bytes, &credentials.token),
                "joined": true,
                "network": "host",
                "exitCode": 0,
                "published": true
            })),
            EffectState::Settled,
        ),
        process::CaptureOutcome::Joined(Err(_)) => (
            Err(Error::Unavailable(
                "host command failed or timed out; reconcile effects before retrying",
            )),
            EffectState::Uncertain,
        ),
        process::CaptureOutcome::Unproven => (
            Err(Error::Unavailable(
                "host command stop is unproven; custody retained",
            )),
            EffectState::Uncertain,
        ),
    }
}

pub(super) async fn call(
    root: &Path,
    request: Request,
    cancel: watch::Receiver<bool>,
) -> (Result<Value>, EffectState) {
    if *cancel.borrow() {
        return (
            Err(Error::Unavailable(
                "host command cancelled before admission",
            )),
            EffectState::None,
        );
    }
    let (credentials, home, path) = match credentials(cancel.clone()).await {
        Ok(values) => values,
        Err(error) => {
            let effects = if error.is_cleanup_unproven() {
                EffectState::Uncertain
            } else {
                EffectState::None
            };
            return (Err(error), effects);
        }
    };
    let env_root = match crate::command_tool::default_root()
        .and_then(|base| private::directory(&base.join("host-env")))
    {
        Ok(root) => root,
        Err(error) => return (Err(error), EffectState::None),
    };
    execute(root, request, credentials, home, path, cancel, &env_root).await
}

#[cfg(target_os = "macos")]
pub(super) async fn native_environment(
    command: &mut Command,
    directory: &Path,
    home: &Path,
    path: &str,
    github: bool,
    cancel: watch::Receiver<bool>,
) -> Result<Zeroizing<String>> {
    command
        .env_clear()
        .env("HOME", home)
        .env("PATH", path)
        .env("TMPDIR", home.join("tmp"))
        .env("LANG", "en_US.UTF-8")
        .env("NO_COLOR", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_NOSYSTEM", "1");
    if !github {
        return Ok(Zeroizing::new(String::new()));
    }
    let (mut credentials, _, _) = credentials(cancel).await?;
    credentials.ssh_socket = None;
    child_environment(command, directory, &credentials, home, path)?;
    command
        .env("TMPDIR", home.join("tmp"))
        .env("GIT_CONFIG_NOSYSTEM", "1");
    Ok(credentials.token)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn credential_helpers_complete_service_paths_without_losing_host_toolchains() {
        let path = host_path(Some("/usr/bin:/bin:/usr/sbin:/sbin:/trusted/bin".into())).unwrap();
        let roots: Vec<_> = path.split(':').collect();
        assert_eq!(
            &roots[..5],
            &["/usr/bin", "/bin", "/usr/sbin", "/sbin", "/trusted/bin"]
        );
        for root in ["/usr/local/bin", "/opt/homebrew/bin"] {
            assert!(roots.contains(&root), "{root}");
        }
        assert_eq!(host_path(Some(path.clone())).unwrap(), path);
        for path in [
            "",
            "/usr/bin\n/bin",
            "/usr/bin\r/bin",
            "/usr/bin::/bin",
            "/usr/bin:.",
            "relative",
        ] {
            assert!(host_path(Some(path.to_string())).is_err(), "{path:?}");
        }
        for path in ["x".repeat(4097), format!("/{}", "x".repeat(4090))] {
            assert!(host_path(Some(path)).is_err());
        }
    }

    #[test]
    fn request_is_closed_and_bounded() {
        let root = Path::new("/tmp/workspace");
        let good = json!({"argv":["git","status"],"cwd":".","timeoutMs":1000,"network":"host"});
        Request::parse(&good, root).unwrap();
        for (field, value) in [
            ("network", json!("none")),
            ("env", json!({"TOKEN":"evil"})),
            ("cwd", json!("../peer")),
            ("timeoutMs", json!(600001)),
        ] {
            let mut bad = good.clone();
            bad[field] = value;
            assert!(Request::parse(&bad, root).is_err(), "{field}");
        }
    }

    #[test]
    fn policy_is_exact_task_workspace_and_codex_only() {
        let root = Path::new("/tmp/workspace");
        assert!(policy_allows(
            Provider::Codex,
            "/tmp/workspace",
            root,
            Some("task_one"),
            "task_one"
        ));
        for (provider, workspace, task, policy) in [
            (
                Provider::Claude,
                "/tmp/workspace",
                Some("task_one"),
                "task_one",
            ),
            (Provider::Codex, "/tmp/peer", Some("task_one"), "task_one"),
            (Provider::Codex, "/tmp/workspace", None, "task_one"),
            (
                Provider::Codex,
                "/tmp/workspace",
                Some("task_one"),
                "task_two",
            ),
            (Provider::Codex, "/tmp/workspace", Some("task_one"), ""),
        ] {
            assert!(!policy_allows(provider, workspace, root, task, policy));
        }
    }

    #[test]
    fn private_env_file_is_consumed_and_output_scrubbed() {
        let dir = tempfile::tempdir().unwrap();
        let private =
            private::directory(&xcb_core::canonical(dir.path()).unwrap().join("private")).unwrap();
        let secret = "github_pat_SECRET_987654321";
        let credentials = Credentials {
            token: Zeroizing::new(secret.into()),
            author_name: Some("Test User".into()),
            author_email: Some("test@example.invalid".into()),
            ssh_socket: None,
        };
        let mut command = Command::new("/bin/true");
        child_environment(
            &mut command,
            &private,
            &credentials,
            Path::new("/tmp"),
            "/usr/bin:/bin",
        )
        .unwrap();
        assert!(std::fs::read_dir(&private).unwrap().next().is_none());
        assert!(!scrub(format!("before {secret} after").as_bytes(), secret).contains(secret));
        assert!(scrub(secret.as_bytes(), secret).contains("[credential redacted]"));
        let sample = private.join("mode");
        private::create(&sample, b"x").unwrap();
        assert_eq!(
            std::fs::metadata(sample).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[tokio::test]
    async fn host_exec_uses_real_worktree_and_never_returns_the_token() {
        let dir = tempfile::tempdir().unwrap();
        let root = xcb_core::canonical(dir.path()).unwrap();
        let env_root = private::directory(&root.join("private")).unwrap();
        let token = "github_pat_SYNTHETIC_123456";
        let credentials = Credentials {
            token: Zeroizing::new(token.into()),
            author_name: None,
            author_email: None,
            ssh_socket: None,
        };
        let request = Request::parse(
            &json!({"argv":["/bin/sh","-c","printf '%s\\n' \"$GH_TOKEN\"; printf changed > actual.txt"],"cwd":".","timeoutMs":5000,"network":"host"}),
            &root,
        )
        .unwrap();
        let (_sender, cancel) = watch::channel(false);
        let (result, effects) = execute(
            &root,
            request,
            credentials,
            PathBuf::from("/tmp"),
            "/usr/bin:/bin".into(),
            cancel,
            &env_root,
        )
        .await;
        assert_eq!(effects, EffectState::Settled, "result: {result:?}");
        let output = result.unwrap();
        assert!(!output.to_string().contains(token));
        assert_eq!(
            std::fs::read_to_string(root.join("actual.txt")).unwrap(),
            "changed"
        );
        assert!(std::fs::read_dir(env_root).unwrap().next().is_none());
    }
}
