//! Opt-in full Claude login. Only this trusted host reads the account-specific
//! Keychain item or gives a refresh token to the pinned authentication command.
//! Provider children receive the short-lived access token, never this profile.
use super::ClaudeLoginEvent;
#[cfg(unix)]
use super::ClaudeLoginObserver;
use crate::{
    Error, Result, private,
    process::{CaptureOutcome, LoginInteraction, Pin, environment},
    runner::LaunchArtifacts,
    store::{RunRecord, Store},
};
use icu_normalizer::ComposingNormalizer;
use rustls::{ClientConfig, RootCertStore, pki_types::ServerName};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{io::AsyncWriteExt, net::TcpStream, process::Command, sync::watch};
use tokio_rustls::TlsConnector;
use xcb_core::{Id, Provider, policy::EffectState, session::State};
use zeroize::{Zeroize, Zeroizing};

pub(crate) mod recovery;
#[cfg(test)]
mod tests;

const ACTIVE: &str = "claude-oauth.json";
const AUTH_CUSTODY: &str = "claude_oauth_auth";
const KEYCHAIN_CUSTODY: &str = "claude_oauth_keychain";
const MAX_RECORD: usize = 64 * 1024;
const REFRESH_MARGIN_MS: u64 = 5 * 60 * 1000;
const SUPPORTED_VERSION: &str = "2.1.285";

// The refresh token remains in the exact official Keychain namespace. This
// private record is both the active-generation pointer and the access cache,
// making activation/replacement a single compare-and-swap publication.
#[derive(Serialize)]
struct Active {
    version: u8,
    account: Id,
    generation: String,
    account_uuid: String,
    email: String,
    scopes: Vec<String>,
    expires_at_ms: u64,
    access_token: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ActiveWire<'a> {
    version: u8,
    account: Id,
    generation: String,
    account_uuid: String,
    email: String,
    scopes: Vec<String>,
    expires_at_ms: u64,
    #[serde(borrow)]
    access_token: &'a str,
}

impl Drop for Active {
    fn drop(&mut self) {
        self.access_token.zeroize();
    }
}

fn invalid_record() -> Error {
    Error::Unavailable(
        "invalid full Claude sign-in; preserve this account and create a new one with xcb setup claude --new, then connect it with xcb tools setup-browser --account <new account>",
    )
}

fn valid_email(email: &str) -> bool {
    !email.is_empty()
        && email.len() <= 320
        && email.contains('@')
        && email.trim() == email
        && !email.chars().any(char::is_control)
}

fn valid_uuid(value: &str) -> bool {
    value.len() == 36
        && uuid::Uuid::parse_str(value).is_ok_and(|id| id.hyphenated().to_string() == value)
}

fn valid_scopes(scopes: &[String]) -> bool {
    !scopes.is_empty()
        && scopes.len() <= 32
        && scopes.iter().all(|scope| {
            !scope.is_empty()
                && scope.len() <= 128
                && scope.bytes().all(|byte| byte.is_ascii_graphic())
        })
        && ["user:profile", "user:inference"]
            .iter()
            .all(|required| scopes.iter().any(|scope| scope == required))
}

impl Active {
    fn validate(&self, account: &Id) -> Result<()> {
        if self.version != 1
            || self.account != *account
            || !valid_uuid(&self.generation)
            || !valid_uuid(&self.account_uuid)
            || !valid_email(&self.email)
            || !valid_scopes(&self.scopes)
            || self.expires_at_ms == 0
            || !super::valid_token(&self.access_token)
        {
            return Err(invalid_record());
        }
        Ok(())
    }
}

struct ObservedActive {
    value: Active,
    revision: String,
}

fn read_active(store: &Store, account: &Id) -> Result<Option<ObservedActive>> {
    if store.account(account)?.provider != Provider::Claude {
        return Err(Error::Conflict("Claude sign-in provider mismatch"));
    }
    read_active_at(&store.account_root(account)?, account)
}

fn read_active_at(root: &std::path::Path, account: &Id) -> Result<Option<ObservedActive>> {
    let bytes = match private::read(&root.join(ACTIVE), MAX_RECORD) {
        Ok(bytes) => Zeroizing::new(bytes),
        Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let wire: ActiveWire<'_> = serde_json::from_slice(&bytes).map_err(|_| invalid_record())?;
    let value = Active {
        version: wire.version,
        account: wire.account,
        generation: wire.generation,
        account_uuid: wire.account_uuid,
        email: wire.email,
        scopes: wire.scopes,
        expires_at_ms: wire.expires_at_ms,
        access_token: wire.access_token.to_owned(),
    };
    value.validate(account)?;
    Ok(Some(ObservedActive {
        value,
        revision: crate::digest(&bytes),
    }))
}

/// Local mode inspection only: never unlocks Keychain, starts a process, or
/// refreshes a credential while rendering accounts or selecting a route.
pub fn has_claude_browser_credentials(store: &Store, account: &Id) -> Result<bool> {
    if store.account(account)?.provider != Provider::Claude {
        return Ok(false);
    }
    Ok(read_active(store, account)?.is_some())
}

pub(super) fn cached_token(store: &Store, account: &Id) -> Result<Option<Zeroizing<String>>> {
    let Some(active) = read_active(store, account)? else {
        return Ok(None);
    };
    if active.value.expires_at_ms <= crate::now_ms() {
        return Err(Error::Unavailable(
            "Claude browser sign-in needs refresh before this account can run",
        ));
    }
    Ok(Some(Zeroizing::new(active.value.access_token.clone())))
}

struct Generation {
    id: String,
    root: PathBuf,
    home: PathBuf,
    profile: PathBuf,
    username: String,
    service: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EffectIntent {
    version: u8,
    run: Id,
    account: Id,
    generation: String,
    call: String,
    operation: String,
    active_revision: Option<String>,
    publication_revision: Option<String>,
}

struct AuthEffect {
    call: String,
    digest: String,
}

fn effect_intent(
    store: &Store,
    run: &RunRecord,
    generation: &Generation,
    operation: &str,
    publication_revision: Option<String>,
) -> Result<AuthEffect> {
    let call = format!("xcb_auth_claude_{}_{}", generation.id, uuid::Uuid::new_v4());
    let intent = EffectIntent {
        version: 1,
        run: run.id.clone(),
        account: run.account.clone(),
        generation: generation.id.clone(),
        call: call.clone(),
        operation: operation.into(),
        active_revision: read_active(store, &run.account)?.map(|record| record.revision),
        publication_revision,
    };
    let bytes = serde_json::to_vec(&intent)?;
    let directory = generation.root.join("effects");
    private::directory(&directory)?;
    private::create(&directory.join(format!("{call}.json")), &bytes)?;
    Ok(AuthEffect {
        call,
        digest: crate::digest(&bytes),
    })
}

impl Generation {
    fn resolve(store: &Store, account: &Id, id: &str) -> Result<Self> {
        Self::resolve_at(store.account_root(account)?, id)
    }

    fn resolve_at(account_root: PathBuf, id: &str) -> Result<Self> {
        if !valid_uuid(id) {
            return Err(invalid_record());
        }
        if xcb_core::canonical(&account_root)? != account_root {
            return Err(Error::PrivateState);
        }
        let root = account_root.join("claude-oauth").join(id);
        let home = root.join("home");
        let profile = root.join("profile");
        let path = profile.to_str().ok_or(Error::PrivateState)?;
        // Official 2.1.285 normalizes the storage path before hashing it.
        let normalized = ComposingNormalizer::new_nfc().normalize(path);
        let digest = crate::digest(normalized.as_bytes());
        // Upstream's service suffix has only 32 bits. An independently unique
        // username isolates both its credentials and companion API-key items.
        let username = format!("xcb-{}", crate::digest(path.as_bytes()));
        let service = format!("Claude Code-credentials-{}", &digest[..8]);
        Ok(Self {
            id: id.to_owned(),
            root,
            home,
            profile,
            username,
            service,
        })
    }

    fn create(store: &Store, run: &RunRecord) -> Result<Self> {
        let generation = Self::resolve(store, &run.account, &uuid::Uuid::new_v4().to_string())?;
        private::directory(&generation.root)?;
        private::directory(&generation.home)?;
        private::directory(&generation.home.join("tmp"))?;
        private::directory(&generation.profile)?;
        // No secrets in the intent. It identifies the only namespaces this
        // generation is allowed to touch, including after interrupted login.
        private::create(
            &generation.root.join("intent.json"),
            &serde_json::to_vec(&serde_json::json!({
                "version":1, "account":run.account, "run":run.id, "generation":generation.id,
                "username":generation.username, "service":generation.service,
                "profile":generation.profile,
            }))?,
        )?;
        Ok(generation)
    }

    fn check(&self) -> Result<()> {
        for path in [&self.root, &self.home, &self.profile] {
            private::check_directory(path)?;
        }
        // The official CLI can fall back to this file when Keychain fails.
        // Preserve it for recovery, but never silently activate that backend.
        match std::fs::symlink_metadata(self.profile.join(".credentials.json")) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
            Ok(_) => Err(Error::Unavailable(
                "Claude could not keep this sign-in in its dedicated Keychain entry; full sign-in was not activated and the private profile was retained",
            )),
        }
    }
}

fn supported(pin: &Pin) -> Result<()> {
    if !cfg!(target_os = "macos") {
        return Err(Error::Unavailable(
            "Claude browser sign-in currently requires macOS",
        ));
    }
    if pin.provider != Provider::Claude || pin.version != SUPPORTED_VERSION {
        return Err(Error::Unavailable(
            "Claude browser sign-in requires supported Claude Code 2.1.285",
        ));
    }
    pin.verify()
}

fn cancelled(cancel: &watch::Receiver<bool>) -> Result<()> {
    if *cancel.borrow() {
        Err(Error::Unavailable("Claude sign-in cancelled"))
    } else {
        Ok(())
    }
}

fn unproven(error: Error) -> Error {
    match error {
        Error::Unavailable(message) | Error::AuthUnproven(message) => Error::AuthUnproven(message),
        _ => Error::CleanupUnproven,
    }
}

// On a mutating child, custody survives process exit until the caller proves
// the resulting credential and settles its mutation receipt. This also fences
// future drops during a subsequent in-process identity request/publication.
#[allow(clippy::too_many_arguments)]
async fn command_capture(
    store: &Store,
    run: &RunRecord,
    command: Command,
    custody: &str,
    effect: Option<&AuthEffect>,
    cancel: watch::Receiver<bool>,
    deadline: Duration,
    interaction: Option<LoginInteraction>,
) -> Result<Zeroizing<Vec<u8>>> {
    cancelled(&cancel)?;
    store.mark_capability_starting(run, custody)?;
    if let Some(effect) = effect
        && let Err(error) =
            store.begin_tool(run, &effect.call, "host_auth_claude_oauth", &effect.digest)
    {
        store.clear_capability_custody(run, custody)?;
        return Err(error);
    }
    let outcome = crate::process::capture_supervised_interactive_diagnosed(
        command,
        MAX_RECORD,
        deadline,
        cancel,
        |pid| store.mark_capability_spawned(run, custody, pid),
        interaction,
        (custody == AUTH_CUSTODY)
            .then_some(super::claude_auth_failure as fn(&[u8], &[u8]) -> &'static str),
    )
    .await;
    match outcome {
        CaptureOutcome::NeverStarted(error) => {
            if let Some(effect) = effect {
                store
                    .finish_claude_auth_helper(run, &effect.call, custody)
                    .map_err(|_| Error::CleanupUnproven)?;
            } else {
                store.clear_capability_custody(run, custody)?;
            }
            Err(error)
        }
        CaptureOutcome::Unproven => Err(Error::CleanupUnproven),
        CaptureOutcome::Joined(result) => {
            if custody != AUTH_CUSTODY {
                if let Some(effect) = effect {
                    store
                        .finish_claude_auth_helper(run, &effect.call, custody)
                        .map_err(|_| Error::CleanupUnproven)?;
                } else {
                    store.clear_capability_custody(run, custody)?;
                }
                result.map_err(|_| Error::Unavailable("Claude's dedicated Keychain entry is unavailable; full sign-in was not activated"))
            } else {
                result.map_err(unproven)
            }
        }
    }
}

fn finish_effect(store: &Store, run: &RunRecord, call: &str) -> Result<()> {
    store
        .finish_claude_auth_helper(run, call, AUTH_CUSTODY)
        .map_err(|_| Error::CleanupUnproven)
}

fn caller_keychain_unavailable() -> Error {
    Error::Unavailable(
        "Claude sign-in cannot access the default Keychain under the caller's HOME; run xcb from your normal macOS terminal before signing in",
    )
}

/// Apple's security helper resolves user Keychain preferences through HOME.
/// Honor the caller's selected home, including intentional isolation; never
/// discover another home or broaden the provider/model process environment.
fn helper_environment(generation: &Generation) -> Result<BTreeMap<String, String>> {
    helper_environment_at(generation, xcb_core::home_dir().as_deref())
}

fn helper_environment_at(
    generation: &Generation,
    caller_home: Option<&Path>,
) -> Result<BTreeMap<String, String>> {
    let home = caller_home
        .filter(|home| home.is_absolute())
        .ok_or_else(caller_keychain_unavailable)?;
    let home = xcb_core::canonical(home).map_err(|_| caller_keychain_unavailable())?;
    if !home.is_dir() {
        return Err(caller_keychain_unavailable());
    }
    let home = home.to_str().ok_or_else(caller_keychain_unavailable)?;
    let mut env = environment(&generation.home);
    env.insert("HOME".into(), home.into());
    env.insert(
        "CLAUDE_CONFIG_DIR".into(),
        generation.profile.to_string_lossy().into_owned(),
    );
    env.insert(
        "CLAUDE_SECURESTORAGE_CONFIG_DIR".into(),
        generation.profile.to_string_lossy().into_owned(),
    );
    env.insert(
        "ANTHROPIC_CONFIG_DIR".into(),
        generation
            .home
            .join(".config/anthropic")
            .to_string_lossy()
            .into_owned(),
    );
    env.insert("USER".into(), generation.username.clone());
    Ok(env)
}

fn keychain_preflight_command(generation: &Generation, env: &BTreeMap<String, String>) -> Command {
    let mut command = Command::new("/usr/bin/security");
    command
        .args(["default-keychain", "-d", "user"])
        .env_clear()
        .envs(env)
        .current_dir(&generation.home);
    command
}

fn official_auth_command(
    executable: &Path,
    generation: &Generation,
    env: BTreeMap<String, String>,
) -> Command {
    let mut command = Command::new(executable);
    // Official 2.1.285's hidden global option is parsed before auth preAction:
    // it confines both project and local settings during auth initialization.
    command
        .arg("--project-config-root")
        .arg(&generation.home)
        .args(["auth", "login", "--claudeai"])
        .env_clear()
        .envs(env)
        .env("BROWSER", "/usr/bin/true")
        .env("COLUMNS", "4096")
        .current_dir(&generation.home);
    command
}

async fn keychain_preflight(
    store: &Store,
    run: &RunRecord,
    generation: &Generation,
    cancel: watch::Receiver<bool>,
    command: Command,
) -> Result<()> {
    // This reads only default-Keychain metadata. It does not unlock or alter
    // preferences, inspect an account credential, or start provider login.
    let effect = effect_intent(store, run, generation, "keychain-preflight", None)?;
    let result = command_capture(
        store,
        run,
        command,
        KEYCHAIN_CUSTODY,
        Some(&effect),
        cancel.clone(),
        Duration::from_secs(15),
        None,
    )
    .await;
    match result {
        Err(error) if error.is_cleanup_unproven() => Err(error),
        Err(_) => {
            cancelled(&cancel)?;
            Err(caller_keychain_unavailable())
        }
        Ok(bytes) if bytes.iter().any(|byte| !byte.is_ascii_whitespace()) => Ok(()),
        Ok(_) => Err(caller_keychain_unavailable()),
    }
}

async fn official_auth(
    store: &Store,
    run: &RunRecord,
    pin: &Pin,
    generation: &Generation,
    refresh: Option<&Bundle<'_>>,
    cancel: watch::Receiver<bool>,
    interaction: Option<LoginInteraction>,
) -> Result<String> {
    generation.check()?;
    let env = helper_environment(generation)?;
    keychain_preflight(
        store,
        run,
        generation,
        cancel.clone(),
        keychain_preflight_command(generation, &env),
    )
    .await?;
    let mut artifacts = LaunchArtifacts::create(store.root())?;
    let executable = pin.snapshot(artifacts.path())?;
    let mut command = official_auth_command(&executable, generation, env);
    if let Some(bundle) = refresh {
        command
            .env("CLAUDE_CODE_OAUTH_REFRESH_TOKEN", bundle.refresh_token)
            .env("CLAUDE_CODE_OAUTH_SCOPES", bundle.scopes.join(" "));
    }
    let effect = effect_intent(
        store,
        run,
        generation,
        if refresh.is_some() {
            "refresh"
        } else {
            "login"
        },
        None,
    )?;
    artifacts.retain_before_launch();
    let result = command_capture(
        store,
        run,
        command,
        AUTH_CUSTODY,
        Some(&effect),
        cancel,
        if refresh.is_some() {
            Duration::from_secs(60)
        } else {
            Duration::from_secs(600)
        },
        interaction,
    )
    .await;
    // Launch artifacts contain only the executable; the durable generation
    // holds the profile. Clean the former only with an independent join.
    if !result
        .as_ref()
        .is_err_and(|error| error.is_cleanup_unproven())
    {
        artifacts.release_after_join(true, EffectState::None);
    }
    result.map(|_| effect.call)
}

#[derive(Deserialize)]
struct Keychain<'a> {
    #[serde(rename = "claudeAiOauth", borrow)]
    oauth: Bundle<'a>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Bundle<'a> {
    #[serde(borrow)]
    access_token: &'a str,
    #[serde(borrow)]
    refresh_token: &'a str,
    expires_at: u64,
    scopes: Vec<String>,
}

fn bundle(bytes: &[u8]) -> Result<Bundle<'_>> {
    let value: Keychain<'_> = serde_json::from_slice(bytes).map_err(|_| invalid_record())?;
    let value = value.oauth;
    if !super::valid_token(value.access_token)
        || value.refresh_token.is_empty()
        || value.refresh_token.len() > 4096
        || !value
            .refresh_token
            .bytes()
            .all(|byte| byte.is_ascii_graphic())
        || value.expires_at == 0
        || !valid_scopes(&value.scopes)
    {
        return Err(invalid_record());
    }
    Ok(value)
}

async fn keychain(
    store: &Store,
    run: &RunRecord,
    generation: &Generation,
    cancel: watch::Receiver<bool>,
) -> Result<Zeroizing<Vec<u8>>> {
    generation.check()?;
    let mut command = Command::new("/usr/bin/security");
    command
        .args([
            "find-generic-password",
            "-a",
            &generation.username,
            "-w",
            "-s",
            &generation.service,
        ])
        .env_clear()
        .envs(helper_environment(generation)?)
        .current_dir(&generation.home);
    let effect = effect_intent(store, run, generation, "keychain-read", None)?;
    command_capture(
        store,
        run,
        command,
        KEYCHAIN_CUSTODY,
        Some(&effect),
        cancel,
        Duration::from_secs(15),
        None,
    )
    .await
}

#[derive(Deserialize)]
struct Profile<'a> {
    #[serde(borrow)]
    account: ProfileAccount<'a>,
}

#[derive(Deserialize)]
struct ProfileAccount<'a> {
    #[serde(borrow)]
    uuid: &'a str,
    #[serde(borrow)]
    email: &'a str,
}

fn profile_identity(status: u16, bytes: &[u8]) -> Result<(String, String)> {
    if status != 200 {
        return Err(Error::Unavailable(
            "Claude could not verify this sign-in's account and browser permission",
        ));
    }
    let profile: Profile<'_> = serde_json::from_slice(bytes).map_err(|_| invalid_record())?;
    if !valid_uuid(profile.account.uuid) || !valid_email(profile.account.email) {
        return Err(invalid_record());
    }
    Ok((
        profile.account.uuid.to_owned(),
        profile.account.email.to_owned(),
    ))
}

async fn fetch_identity(token: &str) -> Result<(String, String)> {
    const HOST: &str = "api.anthropic.com";
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let connector = TlsConnector::from(Arc::new(config));
    let addresses = tokio::net::lookup_host((HOST, 443)).await?;
    let mut stream = None;
    for address in addresses.take(8) {
        if let Ok(tcp) = TcpStream::connect(address).await {
            stream = Some(tcp);
            break;
        }
    }
    let mut tls = connector
        .connect(
            ServerName::try_from(HOST).expect("fixed identity host"),
            stream.ok_or(Error::Unavailable(
                "Claude account verification connection failed",
            ))?,
        )
        .await?;
    let request = Zeroizing::new(format!(
        "GET /api/oauth/profile HTTP/1.1\r\nhost: {HOST}\r\nauthorization: Bearer {token}\r\ncontent-type: application/json\r\naccept: application/json\r\ncache-control: no-cache\r\nconnection: close\r\n\r\n"
    ));
    tls.write_all(request.as_bytes()).await?;
    tls.flush().await?;
    let (status, body) = crate::jev::read_response(&mut tls).await?;
    profile_identity(status, &Zeroizing::new(body))
}

async fn verified_identity(
    token: &str,
    mut cancel: watch::Receiver<bool>,
) -> Result<(String, String)> {
    cancelled(&cancel)?;
    tokio::select! {
        biased;
        _=async { while !*cancel.borrow() { if cancel.changed().await.is_err() { break; } } } => Err(Error::Unavailable("Claude sign-in cancelled")),
        result=tokio::time::timeout(Duration::from_secs(10), fetch_identity(token)) => {
            result.map_err(|_| Error::Unavailable("Claude account verification timed out"))?
                .map_err(|_| Error::Unavailable("Claude could not verify this sign-in's account and browser permission"))
        }
    }
}

fn check_identity(
    identity: &(String, String),
    previous: Option<&Active>,
    known_email: Option<&str>,
) -> Result<()> {
    if previous.is_some_and(|previous| {
        previous.account_uuid != identity.0 || !previous.email.eq_ignore_ascii_case(&identity.1)
    }) || known_email.is_some_and(|email| !email.eq_ignore_ascii_case(&identity.1))
    {
        return Err(Error::Unavailable(
            "Claude sign-in used a different account; the previous sign-in was preserved",
        ));
    }
    Ok(())
}

fn active_record(
    run: &RunRecord,
    generation: &Generation,
    bundle: &Bundle<'_>,
    identity: (String, String),
) -> Active {
    Active {
        version: 1,
        account: run.account.clone(),
        generation: generation.id.clone(),
        account_uuid: identity.0,
        email: identity.1,
        scopes: bundle.scopes.clone(),
        expires_at_ms: bundle.expires_at,
        access_token: bundle.access_token.to_owned(),
    }
}

struct LoginCustody<'a> {
    store: &'a Store,
    run: &'a RunRecord,
    active: bool,
}

impl Drop for LoginCustody<'_> {
    fn drop(&mut self) {
        if self.active {
            // Never release a dropped login while a helper or credential
            // receipt is outstanding. Store::settle checks child custody.
            let _ = self
                .store
                .require_settled_tools(self.run)
                .and_then(|_| self.store.settle(self.run, State::Failed, crate::now_ms()));
        }
    }
}

fn publish(
    store: &Store,
    run: &RunRecord,
    value: &Active,
    previous: Option<&ObservedActive>,
    new_login: bool,
) -> Result<()> {
    value.validate(&run.account)?;
    let bytes = Zeroizing::new(serde_json::to_vec(value)?);
    let path = store.account_root(&run.account)?.join(ACTIVE);
    let generation = Generation::resolve(store, &run.account, &value.generation)?;
    let effect = effect_intent(
        store,
        run,
        &generation,
        "publish",
        Some(crate::digest(&bytes)),
    )?;
    store.begin_tool(
        run,
        &effect.call,
        "host_auth_claude_publish",
        &effect.digest,
    )?;
    if new_login {
        crate::application_qualification::rotate_generation(store, run)?;
    }
    if let Some(previous) = previous {
        private::replace(&path, &bytes, &previous.revision)?;
    } else {
        private::create(&path, &bytes)?;
    }
    store.set_account_identity(&run.account, Some(value.email.clone()), None)?;
    store.clear_authentication_failure(run)?;
    store.settle_tool(run, &effect.call)
}

#[cfg(unix)]
async fn login(
    store: &Store,
    id: &Id,
    pin: &Pin,
    cancel: watch::Receiver<bool>,
    interaction: LoginInteraction,
) -> Result<()> {
    supported(pin)?;
    cancelled(&cancel)?;
    let account = store.account(id)?;
    if account.provider != Provider::Claude {
        return Err(Error::Conflict("Claude sign-in provider mismatch"));
    }
    let run = store.prepare_probe(id, None, crate::now_ms())?;
    let mut custody = LoginCustody {
        store,
        run: &run,
        active: true,
    };
    let result = async {
        // Observe the activation revision after acquiring exclusive custody.
        let previous = read_active(store, id)?;
        let generation = Generation::create(store, &run)?;
        let login_call = official_auth(
            store,
            &run,
            pin,
            &generation,
            None,
            cancel.clone(),
            Some(interaction),
        )
        .await?;
        let bytes = keychain(store, &run, &generation, cancel.clone())
            .await
            .map_err(unproven)?;
        let bundle = bundle(&bytes).map_err(unproven)?;
        if bundle.expires_at <= crate::now_ms() {
            return Err(unproven(invalid_record()));
        }
        let identity = verified_identity(bundle.access_token, cancel.clone())
            .await
            .map_err(unproven)?;
        // This exact staged namespace is now proven and inactive. A known
        // wrong-account choice can be refused without blocking the old login.
        finish_effect(store, &run, &login_call)?;
        check_identity(
            &identity,
            previous.as_ref().map(|record| &record.value),
            account.email.as_deref(),
        )?;
        // Qualify refresh ownership before activating the new generation.
        let refresh_call = official_auth(
            store,
            &run,
            pin,
            &generation,
            Some(&bundle),
            cancel.clone(),
            None,
        )
        .await?;
        let result = async {
            let refreshed = keychain(store, &run, &generation, cancel.clone()).await?;
            let refreshed = bundle_from_refresh(&refreshed, &bundle)?;
            let confirmed = verified_identity(refreshed.access_token, cancel.clone()).await?;
            if confirmed.0 != identity.0 || !confirmed.1.eq_ignore_ascii_case(&identity.1) {
                return Err(Error::Unavailable(
                    "Claude account changed while refreshing sign-in",
                ));
            }
            cancelled(&cancel)?;
            let value = active_record(&run, &generation, &refreshed, confirmed);
            publish(store, &run, &value, previous.as_ref(), true)?;
            finish_effect(store, &run, &refresh_call)
        }
        .await;
        result.map_err(unproven)
    }
    .await;
    custody.active = false;
    if !result
        .as_ref()
        .is_err_and(|error| error.is_cleanup_unproven())
    {
        store.settle(
            &run,
            if result.is_ok() {
                State::Idle
            } else {
                State::Failed
            },
            crate::now_ms(),
        )?;
    }
    result
}

fn bundle_from_refresh<'a>(bytes: &'a [u8], previous: &Bundle<'_>) -> Result<Bundle<'a>> {
    let refreshed = bundle(bytes)?;
    // A successful CLI exit without a new token is not proof of refresh.
    if refreshed.access_token == previous.access_token || refreshed.expires_at <= crate::now_ms() {
        return Err(Error::Unavailable(
            "Claude did not save its refreshed sign-in",
        ));
    }
    Ok(refreshed)
}

/// Full-scope browser login is explicit and macOS-only. URL events use the
/// existing allowlisted/redacting observer; raw provider output stays private.
pub async fn login_claude_browser_with_interaction(
    store: &Store,
    id: &Id,
    pin: &Pin,
    cancel: watch::Receiver<bool>,
    events: tokio::sync::mpsc::Sender<ClaudeLoginEvent>,
    codes: tokio::sync::mpsc::Receiver<Zeroizing<String>>,
) -> Result<()> {
    supported(pin)?;
    #[cfg(not(unix))]
    {
        let _ = (store, id, cancel, events, codes);
        Err(Error::providers_unsupported())
    }
    #[cfg(unix)]
    {
        let (stdin, terminal) = crate::process::login_terminal()?;
        let mut observer = ClaudeLoginObserver::default();
        let interaction = LoginInteraction {
            stdin,
            terminal,
            codes,
            observer: Box::new(move |bytes| {
                for event in observer.observe(bytes) {
                    let _ = events.try_send(event);
                }
                if observer.failed {
                    Err(Error::Unavailable("Claude browser sign-in failed"))
                } else {
                    Ok(())
                }
            }),
        };
        login(store, id, pin, cancel, interaction).await
    }
}

/// Call only under the account's existing exclusive run. Errors after a
/// potentially mutating refresh deliberately retain custody and its receipt.
pub(crate) async fn refresh_claude_credentials(
    store: &Store,
    run: &RunRecord,
    pin: &Pin,
    cancel: watch::Receiver<bool>,
) -> Result<()> {
    if store.account(&run.account)?.provider != Provider::Claude {
        return Ok(());
    }
    let Some(previous) = read_active(store, &run.account)? else {
        return Ok(());
    };
    supported(pin)?;
    store.require_authenticated_run(run)?;
    cancelled(&cancel)?;
    if previous.value.expires_at_ms > crate::now_ms().saturating_add(REFRESH_MARGIN_MS) {
        return Ok(());
    }
    let generation = Generation::resolve(store, &run.account, &previous.value.generation)?;
    let bytes = keychain(store, run, &generation, cancel.clone()).await?;
    let current = bundle(&bytes)?;
    // A changed namespace may reflect an interrupted external rotation. It is
    // never another automatic grant to spend a refresh token.
    if current.access_token != previous.value.access_token {
        return Err(Error::Unavailable(
            "Claude's saved sign-in changed; sign in again before refreshing",
        ));
    }
    let call = official_auth(
        store,
        run,
        pin,
        &generation,
        Some(&current),
        cancel.clone(),
        None,
    )
    .await?;
    let result = async {
        let bytes = keychain(store, run, &generation, cancel.clone()).await?;
        let refreshed = bundle_from_refresh(&bytes, &current)?;
        let identity = verified_identity(refreshed.access_token, cancel.clone()).await?;
        check_identity(
            &identity,
            Some(&previous.value),
            store.account(&run.account)?.email.as_deref(),
        )?;
        cancelled(&cancel)?;
        let value = active_record(run, &generation, &refreshed, identity);
        publish(store, run, &value, Some(&previous), false)?;
        finish_effect(store, run, &call)
    }
    .await;
    result.map_err(unproven)
}
