//! Account-owned Devin credentials. The provider receives only the opaque token
//! through WINDSURF_API_KEY; no credential file belongs in its disposable HOME.
use crate::{Error, Result, digest, private, store::Store};
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
use xcb_core::{Id, Provider, session::State};
use zeroize::Zeroizing;

const MAX_TOKEN_BYTES: usize = 8192;
const MAX_CREDENTIAL_FILE_BYTES: usize = 64 * 1024;
const API_SERVER: &str = "https://server.codeium.com";
const WEBAPP: &str = "https://app.devin.ai";
const DEVIN_API: &str = "https://api.devin.ai";

fn valid_token(token: &str) -> bool {
    !token.is_empty()
        && token.len() <= MAX_TOKEN_BYTES
        && token.bytes().all(|byte| byte.is_ascii_graphic())
}

fn token_path(store: &Store, account: &Id) -> Result<PathBuf> {
    if store.account(account)?.provider != Provider::Devin {
        return Err(Error::Conflict("Devin credential provider mismatch"));
    }
    Ok(store.account_root(account)?.join("windsurf-token"))
}

/// Presence and local shape only; this neither qualifies a runtime nor proves
/// that the provider will authenticate the opaque token.
pub fn has_credentials(store: &Store, account: &Id) -> Result<bool> {
    if store.account(account)?.provider != Provider::Devin {
        return Ok(false);
    }
    match private::read(&token_path(store, account)?, MAX_TOKEN_BYTES) {
        Ok(bytes) => {
            let bytes = Zeroizing::new(bytes);
            Ok(std::str::from_utf8(&bytes).is_ok_and(valid_token))
        }
        Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

pub fn token(store: &Store, account: &Id) -> Result<Zeroizing<String>> {
    let bytes = Zeroizing::new(private::read(
        &token_path(store, account)?,
        MAX_TOKEN_BYTES,
    )?);
    let token = std::str::from_utf8(&bytes)
        .map_err(|_| Error::Unavailable("invalid stored Devin credential"))?;
    if !valid_token(token) {
        return Err(Error::Unavailable("invalid stored Devin credential"));
    }
    Ok(Zeroizing::new(token.to_owned()))
}

/// Store or rotate credentials only while owning the account's exclusive probe
/// lease. An uncertain publication/receipt failure intentionally keeps custody.
pub fn store_token(store: &Store, account: &Id, bytes: &[u8]) -> Result<()> {
    token_path(store, account)?;
    if bytes.len() > MAX_TOKEN_BYTES + 2 {
        return Err(Error::Unavailable("invalid Devin token"));
    }
    let token = std::str::from_utf8(bytes)
        .map_err(|_| Error::Unavailable("invalid Devin token"))?
        .trim();
    if !valid_token(token) {
        return Err(Error::Unavailable("invalid Devin token"));
    }
    let run = store.prepare_probe(account, None, crate::now_ms())?;
    store_token_in_run(store, &run, token, false)
}

fn store_token_in_run(
    store: &Store,
    run: &crate::store::RunRecord,
    token: &str,
    signed_in: bool,
) -> Result<()> {
    let target = token_path(store, &run.account)?;
    let mut publication_attempted = false;
    let result = (|| {
        let previous = match private::read(&target, MAX_TOKEN_BYTES) {
            Ok(bytes) => Some(Zeroizing::new(bytes)),
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        let changed = previous
            .as_ref()
            .is_none_or(|previous| previous.as_slice() != token.as_bytes());
        store.begin_tool(
            run,
            "xcb_devin_auth_store",
            "host_auth_import",
            &digest(token),
        )?;
        publication_attempted = true;
        crate::application_qualification::rotate_generation(store, run)?;
        if let Some(previous) = previous {
            private::replace(&target, token.as_bytes(), &digest(&previous))?;
        } else {
            private::create(&target, token.as_bytes())?;
        }
        store.settle_tool(run, "xcb_devin_auth_store")?;
        if changed || signed_in {
            store.clear_authentication_failure(run)?;
        }
        Ok(())
    })();
    if result.is_ok() || !publication_attempted {
        store.settle(run, State::Idle, crate::now_ms())?;
    }
    result
}

/// Parse the exact native CredentialsFile's flat string fields, not general
/// user-authored TOML. Reject escapes, tables, comments, duplicate/unknown keys
/// and extra fields rather than guessing at an alternate credential format.
/// All returned slices borrow the caller's zeroized file buffer.
fn credential_token(bytes: &[u8]) -> Result<&str> {
    let invalid = || Error::Unavailable("unsupported Devin credential file");
    if bytes.is_empty() || bytes.len() > MAX_CREDENTIAL_FILE_BYTES {
        return Err(invalid());
    }
    let text = std::str::from_utf8(bytes).map_err(|_| invalid())?;
    let mut fields = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (key, quoted) = line.split_once('=').ok_or_else(invalid)?;
        let key = key.trim();
        if !matches!(
            key,
            "windsurf_api_key" | "api_server_url" | "devin_webapp_host" | "devin_api_url"
        ) {
            return Err(invalid());
        }
        let value = quoted
            .trim()
            .strip_prefix('"')
            .and_then(|value| value.strip_suffix('"'))
            .ok_or_else(invalid)?;
        if value
            .bytes()
            .any(|byte| !byte.is_ascii_graphic() || matches!(byte, b'"' | b'\\'))
            || fields.insert(key, value).is_some()
        {
            return Err(invalid());
        }
    }
    let expected_endpoint = |key, expected: &str| {
        fields
            .get(key)
            .is_some_and(|actual| actual.trim_end_matches('/') == expected)
    };
    if fields.len() != 4
        || !expected_endpoint("api_server_url", API_SERVER)
        // The native CredentialsFile stores a hostname here, while older
        // exports used an HTTPS origin. Neither admits an alternate host.
        || !(fields.get("devin_webapp_host") == Some(&"app.devin.ai")
            || expected_endpoint("devin_webapp_host", WEBAPP))
        || !expected_endpoint("devin_api_url", DEVIN_API)
    {
        return Err(Error::Unavailable(
            "Devin credential endpoints are not admitted",
        ));
    }
    let token = fields["windsurf_api_key"];
    if !valid_token(token) {
        return Err(invalid());
    }
    Ok(token)
}

/// The official CLI may create credentials.toml as 0644. Accept that explicit
/// import source without modifying its mode; persistent xcb state remains 0600.
/// Reject foreign/writable-by-others/link/nonregular sources and unstable reads.
#[cfg(windows)]
fn read_import_source(_source: &Path) -> Result<Zeroizing<Vec<u8>>> {
    Err(Error::providers_unsupported())
}

#[cfg(unix)]
fn read_import_source(source: &Path) -> Result<Zeroizing<Vec<u8>>> {
    let before = std::fs::symlink_metadata(source)?;
    if before.mode() & 0o022 != 0 {
        return Err(Error::PrivateState);
    }
    let read = local_custody::stable_read(
        source,
        &local_custody::StableReadOptions {
            exact_mode: Some(before.mode() & 0o7777),
            owner_only: false,
            maximum_bytes: MAX_CREDENTIAL_FILE_BYTES as u64,
            minimum_bytes: Some(1),
            links: Some(1),
            nonblocking: true,
        },
    )
    .map_err(|_| Error::PrivateState)?;
    let bytes = Zeroizing::new(read.bytes);
    let after = std::fs::symlink_metadata(source)?;
    if xcb_core::canonical(source)? != source
        || !before.is_file()
        || !after.is_file()
        || before.dev() != read.identity.dev
        || before.ino() != read.identity.ino
        || before.dev() != after.dev()
        || before.ino() != after.ino()
        || before.size() != after.size()
        || before.size() != bytes.len() as u64
        || before.mode() != after.mode()
        || before.uid() != after.uid()
        || before.nlink() != after.nlink()
        || before.mtime() != after.mtime()
        || before.mtime_nsec() != after.mtime_nsec()
        || before.ctime() != after.ctime()
        || before.ctime_nsec() != after.ctime_nsec()
    {
        return Err(Error::Conflict(
            "Devin credential source changed during import",
        ));
    }
    Ok(bytes)
}

/// Explicit source import only. Preserve the source, copy only its opaque
/// token, and never inherit endpoint overrides, plugins or provider state.
fn imported_token(source: &Path) -> Result<Zeroizing<String>> {
    if !source.is_absolute()
        || source.file_name().and_then(|name| name.to_str()) != Some("credentials.toml")
        || xcb_core::canonical(source)? != source
    {
        return Err(Error::PrivateState);
    }
    let bytes = read_import_source(source)?;
    Ok(Zeroizing::new(credential_token(&bytes)?.to_owned()))
}

/// Connect a reviewed native sign-in to the chosen account without creating a
/// second account. Preserve source bytes and existing account custody.
pub fn import_into_account(store: &Store, account: &Id, source: &Path) -> Result<()> {
    token_path(store, account)?;
    let token = imported_token(source)?;
    store_token(store, account, token.as_bytes())
}

/// Native browser sign-in in an empty, private provider home. No credentials
/// from the user's native profile or this account are passed to the child.
#[cfg(unix)]
pub async fn login(store: &Store, account: &Id, pin: &crate::process::Pin) -> Result<()> {
    use tokio::signal::unix::{SignalKind, signal};
    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut terminate = signal(SignalKind::terminate())?;
    let (sender, cancel) = tokio::sync::watch::channel(false);
    let login = login_with_cancel(store, account, pin, cancel);
    tokio::pin!(login);
    tokio::select! {
        result = &mut login => result,
        _ = async { tokio::select! { _ = interrupt.recv() => {}, _ = terminate.recv() => {} } } => {
            let _ = sender.send(true);
            login.await
        }
    }
}

#[cfg(unix)]
pub async fn login_with_cancel(
    store: &Store,
    account: &Id,
    pin: &crate::process::Pin,
    cancel: tokio::sync::watch::Receiver<bool>,
) -> Result<()> {
    super::runtime_admitted(pin)?;
    login_inner(store, account, pin, cancel).await
}

#[cfg(unix)]
async fn login_inner(
    store: &Store,
    account: &Id,
    pin: &crate::process::Pin,
    mut cancel: tokio::sync::watch::Receiver<bool>,
) -> Result<()> {
    use crate::process::Group;
    use std::{os::fd::AsFd, process::Stdio, time::Duration};
    use tokio::process::Command;
    use xcb_core::policy::EffectState;

    token_path(store, account)?;
    let mut artifacts = crate::runner::LaunchArtifacts::create(store.root())?;
    let run = store.prepare_probe(account, None, crate::now_ms())?;
    // Already connected accounts must not silently acquire a different identity.
    let connected = (|| {
        Ok::<_, Error>(has_credentials(store, account)? && !store.authentication_required(account)?)
    })();
    let connected = match connected {
        Ok(connected) => connected,
        Err(error) => {
            store.settle(&run, State::Failed, crate::now_ms())?;
            return Err(error);
        }
    };
    if connected {
        store.settle(&run, State::Idle, crate::now_ms())?;
        return Err(Error::Conflict(
            "Devin account is already signed in; add another account to use another sign-in",
        ));
    }
    let prepared = (|| {
        let directory = artifacts.path();
        let executable = pin.snapshot(directory)?;
        let home = private::directory(&directory.join("home"))?;
        private::directory(&home.join("tmp"))?;
        let env = crate::process::environment(&home);
        let source = home.join(".local/share/devin/credentials.toml");
        let mut command = Command::new(executable);
        command
            .args(["auth", "login"])
            .env_clear()
            .envs(env)
            .current_dir(&home)
            .stdin(Stdio::inherit())
            .stdout(Stdio::from(std::io::stderr().as_fd().try_clone_to_owned()?))
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        Group::prepare(&mut command);
        Ok::<_, Error>((command, source))
    })();
    let (mut command, source) = match prepared {
        Ok(value) => value,
        Err(error) => {
            store.settle(&run, State::Failed, crate::now_ms())?;
            return Err(error);
        }
    };
    if *cancel.borrow() {
        store.settle(&run, State::Failed, crate::now_ms())?;
        return Err(Error::Unavailable("Devin sign-in cancelled before launch"));
    }
    artifacts.retain_before_launch();
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            store.settle(&run, State::Failed, crate::now_ms())?;
            artifacts.release_after_join(true, EffectState::None);
            return Err(Error::LaunchNotStarted(error));
        }
    };
    let group = Group::adopt(&mut child).ok_or(Error::CleanupUnproven)?;
    let pid = group.pid();
    struct Custody(Option<Group>);
    impl Drop for Custody {
        fn drop(&mut self) {
            if let Some(group) = &self.0 {
                let _ = group.kill();
            }
        }
    }
    let mut custody = Custody(Some(group.clone()));
    let result = match store.mark_spawned(&run, pid) {
        Err(error) => Err(error),
        Ok(_) => tokio::select! {
            _ = async { while !*cancel.borrow() { if cancel.changed().await.is_err() { std::future::pending::<()>().await; } } } => Err(Error::Unavailable("Devin sign-in cancelled")),
            _ = tokio::time::sleep(Duration::from_secs(600)) => Err(Error::Unavailable("Devin sign-in timed out")),
            status = child.wait() => match status {
                Ok(status) if status.success() => Ok(()),
                Ok(_) => Err(Error::Unavailable("Devin sign-in did not complete")),
                Err(error) => Err(error.into()),
            },
        },
    };
    if child.id() == Some(pid) {
        let _ = group.kill();
    }
    custody.0 = None;
    let joined = tokio::time::timeout(Duration::from_secs(5), async {
        if child.wait().await.is_err() {
            return false;
        }
        loop {
            if group.empty() == Some(true) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or(false);
    if !joined {
        return Err(Error::CleanupUnproven);
    }
    artifacts.release_after_join(true, EffectState::None);
    if let Err(error) = result {
        // The account was never exposed. Failed login files stay private and
        // are not promoted, even if upstream wrote a credential before failing.
        store.settle(&run, State::Failed, crate::now_ms())?;
        return Err(error);
    }
    let token = match imported_token(&source) {
        Ok(token) => token,
        Err(error) => {
            store.settle(&run, State::Failed, crate::now_ms())?;
            return Err(error);
        }
    };
    // Only the account-owned token persists after a completed sign-in.
    if let Err(error) = std::fs::remove_file(&source) {
        store.settle(&run, State::Failed, crate::now_ms())?;
        return Err(error.into());
    }
    // Retain the original probe throughout credential publication. There is
    // no release/reacquire window in which another login can replace identity.
    artifacts.retain_before_launch();
    let result = store_token_in_run(store, &run, &token, true);
    if result.is_ok() {
        artifacts.release_after_join(true, EffectState::Settled);
    }
    result
}

#[cfg(not(unix))]
pub async fn login(_store: &Store, _account: &Id, _pin: &crate::process::Pin) -> Result<()> {
    Err(Error::providers_unsupported())
}

#[cfg(not(unix))]
pub async fn login_with_cancel(
    _store: &Store,
    _account: &Id,
    _pin: &crate::process::Pin,
    _cancel: tokio::sync::watch::Receiver<bool>,
) -> Result<()> {
    Err(Error::providers_unsupported())
}

pub fn import_account(store: &Store, source: &Path) -> Result<Id> {
    let token = imported_token(source)?;
    let account = store.add_account(
        Provider::Devin,
        "Imported subscription",
        crate::now_ms(),
        None,
    )?;
    store_token(store, &account.id, token.as_bytes())?;
    Ok(account.id)
}

#[cfg(all(test, unix))]
mod login_tests {
    use super::*;
    use std::{os::unix::fs::PermissionsExt, time::Duration};

    async fn fixture(base: &Path, body: &str) -> crate::process::Pin {
        let fixture_home = private::directory(&base.join("fixture")).unwrap();
        let executable = fixture_home.join("fake-devin");
        private::create(&executable, format!("#!/bin/sh\nif [ \"$1\" = --version ]; then echo 'devin 3000.11.3 (fixture)'; exit 0; fi\n{body}\n").as_bytes()).unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        crate::process::inspect(Provider::Devin, Some(&executable), &fixture_home)
            .await
            .unwrap()
    }

    const WRITE_TOKEN: &str = "test \"$1 $2\" = 'auth login' || exit 8\ntest \"$XDG_DATA_HOME\" = \"$HOME/.local/share\" || exit 9\nmkdir -p \"$XDG_DATA_HOME/devin\"\ncat > \"$XDG_DATA_HOME/devin/credentials.toml\" <<'CREDENTIAL'\nwindsurf_api_key = \"synthetic-native-login\"\napi_server_url = \"https://server.codeium.com\"\ndevin_webapp_host = \"https://app.devin.ai\"\ndevin_api_url = \"https://api.devin.ai\"\nCREDENTIAL";

    #[tokio::test]
    async fn native_login_publishes_only_selected_account_after_join_and_cleans_profile() {
        let directory = tempfile::tempdir().unwrap();
        let base = xcb_core::canonical(directory.path()).unwrap();
        let pin = fixture(&base, WRITE_TOKEN).await;
        let store = Store::open(&base.join("state")).unwrap();
        let account = store.add_account(Provider::Devin, "Core", 1, None).unwrap();
        let other = store.add_account(Provider::Devin, "Core", 1, None).unwrap();
        let (_sender, cancel) = tokio::sync::watch::channel(false);
        login_inner(&store, &account.id, &pin, cancel)
            .await
            .unwrap();
        assert_eq!(
            &*token(&store, &account.id).unwrap(),
            "synthetic-native-login"
        );
        assert!(!has_credentials(&store, &other.id).unwrap());
        assert_eq!(store.accounts().unwrap().len(), 2);
        assert!(store.unsettled_runs().unwrap().is_empty());
        assert_eq!(
            std::fs::read_dir(store.root().join("runs"))
                .unwrap()
                .count(),
            0
        );
        assert!(!base.join(".local/share/devin/credentials.toml").exists());
    }

    #[tokio::test]
    async fn native_failed_login_never_promotes_written_credential() {
        let directory = tempfile::tempdir().unwrap();
        let base = xcb_core::canonical(directory.path()).unwrap();
        let pin = fixture(&base, &format!("{WRITE_TOKEN}\nexit 1")).await;
        let store = Store::open(&base.join("state")).unwrap();
        let account = store.add_account(Provider::Devin, "Core", 1, None).unwrap();
        let (_sender, cancel) = tokio::sync::watch::channel(false);
        assert!(
            login_inner(&store, &account.id, &pin, cancel)
                .await
                .is_err()
        );
        assert!(!has_credentials(&store, &account.id).unwrap());
        assert!(store.unsettled_runs().unwrap().is_empty());
        assert_eq!(
            std::fs::read_dir(store.root().join("runs"))
                .unwrap()
                .count(),
            0
        );
    }

    #[tokio::test]
    async fn native_sign_in_repairs_rejected_credentials_without_silent_healthy_replacement() {
        let directory = tempfile::tempdir().unwrap();
        let base = xcb_core::canonical(directory.path()).unwrap();
        let pin = fixture(&base, WRITE_TOKEN).await;
        let store = Store::open(&base.join("state")).unwrap();
        let account = store.add_account(Provider::Devin, "Core", 1, None).unwrap();
        store_token(&store, &account.id, b"synthetic-native-login").unwrap();
        let (_sender, cancel) = tokio::sync::watch::channel(false);
        assert!(
            login_inner(&store, &account.id, &pin, cancel)
                .await
                .is_err()
        );
        assert!(store.unsettled_runs().unwrap().is_empty());
        crate::authentication_tests::fail_authentication(&store, &account.id);
        assert!(store.authentication_required(&account.id).unwrap());
        let (_sender, cancel) = tokio::sync::watch::channel(false);
        login_inner(&store, &account.id, &pin, cancel)
            .await
            .unwrap();
        assert!(!store.authentication_required(&account.id).unwrap());
        assert_eq!(
            &*token(&store, &account.id).unwrap(),
            "synthetic-native-login"
        );
        assert!(store.unsettled_runs().unwrap().is_empty());
    }

    #[tokio::test]
    async fn native_cancellation_awaits_process_join_before_releasing_account() {
        let directory = tempfile::tempdir().unwrap();
        let base = xcb_core::canonical(directory.path()).unwrap();
        let pin = fixture(&base, "exec /bin/sleep 30").await;
        let store = Store::open(&base.join("state")).unwrap();
        let account = store.add_account(Provider::Devin, "Core", 1, None).unwrap();
        let (sender, cancel) = tokio::sync::watch::channel(false);
        let login = login_inner(&store, &account.id, &pin, cancel);
        let request = async {
            loop {
                let runs = store.unsettled_runs().unwrap();
                if runs.first().is_some_and(|run| run.pid.is_some()) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            sender.send(true).unwrap();
        };
        let (result, ()) = tokio::time::timeout(Duration::from_secs(10), async {
            tokio::join!(login, request)
        })
        .await
        .unwrap();
        assert!(result.is_err());
        assert!(store.unsettled_runs().unwrap().is_empty());
        assert!(!has_credentials(&store, &account.id).unwrap());
    }

    #[tokio::test]
    async fn native_login_output_child() {
        if std::env::var_os("XCB_DEVIN_LOGIN_OUTPUT_CHILD").is_none() {
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let base = xcb_core::canonical(directory.path()).unwrap();
        let pin = fixture(
            &base,
            &format!("{WRITE_TOKEN}\necho synthetic-login-progress"),
        )
        .await;
        let store = Store::open(&base.join("state")).unwrap();
        let account = store.add_account(Provider::Devin, "Core", 1, None).unwrap();
        let (_sender, cancel) = tokio::sync::watch::channel(false);
        login_inner(&store, &account.id, &pin, cancel)
            .await
            .unwrap();
    }

    #[test]
    fn native_login_progress_preserves_machine_readable_stdout() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "devin::auth::login_tests::native_login_output_child",
            ])
            .env("XCB_DEVIN_LOGIN_OUTPUT_CHILD", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stderr).contains("synthetic-login-progress"));
        assert!(!String::from_utf8_lossy(&output.stdout).contains("synthetic-login-progress"));
    }
}
