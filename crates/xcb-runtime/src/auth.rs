use crate::{
    Error, Result, private,
    process::{CaptureOutcome, Pin, capture_supervised, environment},
    runner::LaunchArtifacts,
    store::{RunRecord, Store},
};
use regex::Regex;
use std::{path::Path, sync::OnceLock, time::Duration};
use tokio::{process::Command, sync::watch};
use xcb_core::{Id, Provider};
use zeroize::Zeroizing;

pub fn valid_token(text: &str) -> bool {
    static TOKEN: OnceLock<Regex> = OnceLock::new();
    TOKEN
        .get_or_init(|| {
            Regex::new(r"^sk-ant-oat[0-9]{2}-[A-Za-z0-9_-]{16,1024}$").expect("static token shape")
        })
        .is_match(text)
}

fn validated_claude_token(bytes: &[u8]) -> Result<&str> {
    if bytes.len() > 2048 {
        return Err(Error::Unavailable("invalid subscription token"));
    }
    let token = std::str::from_utf8(bytes)
        .map_err(|_| Error::Unavailable("invalid token"))?
        .trim();
    if !valid_token(token) {
        return Err(Error::Unavailable("invalid subscription token"));
    }
    Ok(token)
}

// This host-only publication plan binds the credential revision observed before
// a potentially long login. Never replace a token changed while login was open.
struct ClaudeTokenPublication {
    account: Id,
    run: Id,
    path: std::path::PathBuf,
    revision: Option<String>,
}

fn claude_token_publication(store: &Store, run: &RunRecord) -> Result<ClaudeTokenPublication> {
    if store.account(&run.account)?.provider != Provider::Claude {
        return Err(Error::Conflict("subscription token provider mismatch"));
    }
    let path = store.account_root(&run.account)?.join("subscription-token");
    let revision = match private::read(&path, 2048) {
        Ok(previous) => {
            let previous = Zeroizing::new(previous);
            Some(crate::digest(previous.as_slice()))
        }
        Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    Ok(ClaudeTokenPublication {
        account: run.account.clone(),
        run: run.id.clone(),
        path,
        revision,
    })
}

fn publish_claude_token(
    store: &Store,
    run: &RunRecord,
    plan: &ClaudeTokenPublication,
    token: &str,
    publication_attempted: &mut bool,
) -> Result<()> {
    if plan.account != run.account || plan.run != run.id {
        return Err(Error::Conflict("credential publication authority changed"));
    }
    store.begin_tool(
        run,
        "xcb_claude_auth_store",
        "host_auth_import",
        &crate::digest(token),
    )?;
    *publication_attempted = true;
    crate::application_qualification::rotate_generation(store, run)?;
    if let Some(revision) = &plan.revision {
        private::replace(&plan.path, token.as_bytes(), revision)?;
    } else {
        private::create(&plan.path, token.as_bytes())?;
    }
    store.settle_tool(run, "xcb_claude_auth_store")?;
    if plan.revision.as_deref() != Some(crate::digest(token).as_str()) {
        store.clear_authentication_failure(run)?;
    }
    Ok(())
}

/// Store or rotate a Claude token only under the account's exclusive lease.
/// Publication or receipt uncertainty deliberately retains that custody.
pub fn store_token(store: &Store, id: &Id, bytes: &[u8]) -> Result<()> {
    if store.account(id)?.provider != Provider::Claude {
        return Err(Error::Unavailable(
            "token input is only supported for Claude",
        ));
    }
    let token = validated_claude_token(bytes)?;
    let run = store.prepare_probe(id, None, crate::now_ms())?;
    let mut publication_attempted = false;
    let result = (|| {
        let plan = claude_token_publication(store, &run)?;
        publish_claude_token(store, &run, &plan, token, &mut publication_attempted)
    })();
    if result.is_ok() || !publication_attempted {
        store.settle(&run, xcb_core::session::State::Idle, crate::now_ms())?;
    }
    result
}

pub(crate) fn token(store: &Store, id: &Id) -> Result<Zeroizing<String>> {
    if store.account(id)?.provider != Provider::Claude {
        return Err(Error::Conflict("subscription token provider mismatch"));
    }
    let bytes = Zeroizing::new(private::read(
        &store.account_root(id)?.join("subscription-token"),
        2048,
    )?);
    let token = std::str::from_utf8(&bytes)
        .map_err(|_| Error::Unavailable("invalid stored credential"))?
        .trim();
    if !valid_token(token) {
        return Err(Error::Unavailable(
            "no valid stored token; use xcb accounts login",
        ));
    }
    Ok(Zeroizing::new(token.to_owned()))
}

pub fn has_token(store: &Store, id: &Id) -> Result<bool> {
    if store.account(id)?.provider != Provider::Claude {
        return Ok(false);
    }
    let path = store.account_root(id)?.join("subscription-token");
    match private::read(&path, 2048) {
        Ok(bytes) => {
            let bytes = Zeroizing::new(bytes);
            Ok(std::str::from_utf8(&bytes).is_ok_and(|text| valid_token(text.trim())))
        }
        Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

/// Embedded callers retain this future until completion. CLI callers use
/// login_with_cancel so their signal handlers can request graceful cleanup.
pub async fn login(store: &Store, id: &Id, pin: &Pin) -> Result<()> {
    let (_sender, cancel) = watch::channel(false);
    login_with_cancel(store, id, pin, cancel).await
}

fn captured_claude_token(bytes: &[u8]) -> Result<Zeroizing<String>> {
    let output =
        std::str::from_utf8(bytes).map_err(|_| Error::Protocol("login output encoding"))?;
    let ansi = Regex::new(r"\x1b\[[0-9;?]*[a-zA-Z]").expect("static ANSI pattern");
    let cleaned = Zeroizing::new(ansi.replace_all(output, "").into_owned());
    let pattern =
        Regex::new(r"sk-ant-oat[0-9]{2}-[A-Za-z0-9_-]+(?:\n[ \t]*[A-Za-z0-9_-]{40,}[ \t]*)*")
            .expect("static token capture");
    let mut matches = pattern.find_iter(&cleaned);
    let found = matches.next().ok_or(Error::Unavailable(
        "sign-in did not return a subscription token",
    ))?;
    if matches.next().is_some() {
        return Err(Error::Unavailable("sign-in returned ambiguous credentials"));
    }
    let value = Zeroizing::new(
        found
            .as_str()
            .chars()
            .filter(|ch| !ch.is_whitespace())
            .collect::<String>(),
    );
    validated_claude_token(value.as_bytes())?;
    Ok(value)
}

/// Best-effort account identity: browser sign-in makes Claude Code write
/// `oauthAccount.emailAddress` into `.claude.json` under its config/home dir.
/// Reading our own launch artifacts is the only ambient-free source — the
/// control protocol reports no account identity.
pub(crate) fn claude_profile_email(dirs: &[&Path]) -> Option<String> {
    for dir in dirs {
        let Ok(bytes) = private::read(&dir.join(".claude.json"), 256 * 1024) else {
            continue;
        };
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            continue;
        };
        let Some(email) = value
            .pointer("/oauthAccount/emailAddress")
            .and_then(serde_json::Value::as_str)
        else {
            continue;
        };
        if !email.is_empty()
            && email.len() <= 320
            && email.contains('@')
            && !email.chars().any(char::is_control)
            && email.trim() == email
        {
            return Some(email.to_owned());
        }
    }
    None
}

fn finish_claude_login(
    store: &Store,
    run: &RunRecord,
    publication: &ClaudeTokenPublication,
    artifacts: &mut LaunchArtifacts,
    outcome: CaptureOutcome,
) -> Result<()> {
    use xcb_core::{policy::EffectState, session::State};
    let output = match outcome {
        CaptureOutcome::NeverStarted(error) => {
            store.settle(run, State::Failed, crate::now_ms())?;
            artifacts.release_after_join(true, EffectState::None);
            return Err(error);
        }
        CaptureOutcome::Unproven => {
            return Err(Error::Unavailable(
                "Claude sign-in process stop unproven; account custody retained",
            ));
        }
        CaptureOutcome::Joined(output) => output,
    };
    let mut publication_attempted = false;
    let result = (|| {
        let bytes = output?;
        let value = captured_claude_token(&bytes)?;
        publish_claude_token(store, run, publication, &value, &mut publication_attempted)?;
        // A successfully joined provider login is stronger evidence than a
        // token-shaped import, even if it returned the same token material.
        store.clear_authentication_failure(run)?;
        // The provider wrote this file inside our own launch profile; a
        // missing or unparseable one just leaves the fixed account name.
        let base = artifacts.path();
        if let Some(email) = claude_profile_email(&[&base.join("home"), &base.join("profile")]) {
            store.set_account_identity(&run.account, Some(email), None)?;
        }
        Ok(())
    })();
    if result.is_ok() || !publication_attempted {
        store.settle(
            run,
            if result.is_ok() {
                State::Idle
            } else {
                State::Failed
            },
            crate::now_ms(),
        )?;
        artifacts.release_after_join(true, EffectState::None);
    }
    result
}

/// One supervised host sign-in. Signals request cancellation through the watch
/// channel; account custody and private launch artifacts survive an uncertain
/// stop or a dropped future. Secret stdout is parsed only after physical join.
pub async fn login_with_cancel(
    store: &Store,
    id: &Id,
    pin: &Pin,
    cancel: watch::Receiver<bool>,
) -> Result<()> {
    let account = store.account(id)?;
    if account.provider != pin.provider {
        return Err(Error::Conflict("login provider mismatch"));
    }
    if account.provider != Provider::Claude {
        return Err(Error::Unavailable(
            "use the supervised Codex sign-in or explicit Devin credential import",
        ));
    }
    if *cancel.borrow() {
        return Err(Error::Unavailable("sign-in cancelled before launch"));
    }
    pin.verify()?;
    let run = store.prepare_probe(id, None, crate::now_ms())?;
    let planned = (|| {
        let publication = claude_token_publication(store, &run)?;
        let artifacts = LaunchArtifacts::create(store.root())?;
        let executable = pin.snapshot(artifacts.path())?;
        let home = private::directory(&artifacts.path().join("home"))?;
        private::directory(&home.join("tmp"))?;
        let profile = private::directory(&artifacts.path().join("profile"))?;
        let mut env = environment(&home);
        env.insert(
            "CLAUDE_CONFIG_DIR".into(),
            profile.to_string_lossy().into_owned(),
        );
        let mut command = Command::new(executable);
        command
            .arg("setup-token")
            .env_clear()
            .envs(env)
            .current_dir(&home);
        Ok::<_, Error>((command, publication, artifacts))
    })();
    let (command, publication, mut artifacts) = match planned {
        Ok(plan) => plan,
        Err(error) => {
            store.settle(&run, xcb_core::session::State::Failed, crate::now_ms())?;
            return Err(error);
        }
    };
    artifacts.retain_before_launch();
    let outcome = capture_supervised(
        command,
        64 * 1024,
        Duration::from_secs(600),
        cancel,
        |pid| store.mark_spawned(&run, pid).map(|_| ()),
    )
    .await;
    finish_claude_login(store, &run, &publication, &mut artifacts, outcome)
}

/// Explicit legacy import: reads a pre-0.4.0 AgentMixer `claude-oauth-token`
/// file from `source` and stores it as a new Claude account. The legacy state
/// root is never a live default; the source directory is left untouched.
pub fn import_agentmixer_token(store: &Store, source: &Path) -> Result<Id> {
    private::check_directory(source)?;
    let bytes = Zeroizing::new(private::read(&source.join("claude-oauth-token"), 2048)?);
    if !std::str::from_utf8(&bytes).is_ok_and(|text| valid_token(text.trim())) {
        return Err(Error::Unavailable(
            "legacy subscription token is missing or invalid",
        ));
    }
    let account = store.add_account(
        Provider::Claude,
        "Imported subscription",
        crate::now_ms(),
        None,
    )?;
    store_token(store, &account.id, &bytes)?;
    Ok(account.id)
}

const MAX_CODEX_AUTH_BYTES: usize = 64 * 1024;
const CODEX_AUTH_CALL: &str = "xcb_auth_snapshot";

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CodexAuthFile<'a> {
    auth_mode: Option<&'a str>,
    #[serde(rename = "OPENAI_API_KEY")]
    api_key: Option<&'a str>,
    #[serde(borrow)]
    tokens: CodexTokens<'a>,
    last_refresh: Option<&'a str>,
}

#[derive(serde::Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct CodexTokens<'a> {
    id_token: &'a str,
    access_token: &'a str,
    refresh_token: &'a str,
    account_id: Option<&'a str>,
}

fn same_codex_credentials(left: &[u8], right: &[u8]) -> bool {
    match (
        serde_json::from_slice::<CodexAuthFile<'_>>(left),
        serde_json::from_slice::<CodexAuthFile<'_>>(right),
    ) {
        (Ok(left), Ok(right)) => left.tokens == right.tokens,
        _ => false,
    }
}

#[derive(serde::Deserialize)]
struct CodexClaims<'a> {
    sub: Option<&'a str>,
    email: Option<&'a str>,
    #[serde(rename = "https://api.openai.com/auth", borrow)]
    auth: Option<CodexIdentityClaims<'a>>,
}

/// What the credential proves about itself: a continuity digest plus the
/// account email the provider signed into the identity token.
pub struct CodexIdentity {
    pub digest: String,
    pub email: Option<String>,
}

#[derive(serde::Deserialize)]
struct CodexIdentityClaims<'a> {
    chatgpt_account_id: Option<&'a str>,
    chatgpt_user_id: Option<&'a str>,
    user_id: Option<&'a str>,
}

/// Validate the supported ChatGPT file shape and return a continuity digest
/// plus the identity-token email. This is not token verification: the provider
/// must authenticate it. Borrow parsed secrets from zeroized input; never
/// return parser diagnostics.
fn codex_identity(bytes: &[u8]) -> Result<CodexIdentity> {
    use base64::Engine;
    let invalid = || Error::Unavailable("invalid Codex ChatGPT credential file");
    if bytes.is_empty() || bytes.len() > MAX_CODEX_AUTH_BYTES {
        return Err(invalid());
    }
    let auth: CodexAuthFile<'_> = serde_json::from_slice(bytes).map_err(|_| invalid())?;
    if auth.auth_mode.is_some_and(|mode| mode != "chatgpt") || auth.api_key.is_some() {
        return Err(invalid());
    }
    let token = |text: &str| {
        !text.is_empty()
            && text.len() <= 32 * 1024
            && text.bytes().all(|byte| byte.is_ascii_graphic())
    };
    if !token(auth.tokens.id_token)
        || !token(auth.tokens.access_token)
        || !token(auth.tokens.refresh_token)
    {
        return Err(invalid());
    }
    if let Some(last_refresh) = auth.last_refresh {
        time::OffsetDateTime::parse(last_refresh, &time::format_description::well_known::Rfc3339)
            .map_err(|_| invalid())?;
    }
    let parts: Vec<_> = auth.tokens.id_token.split('.').collect();
    if parts.len() != 3 || parts.iter().any(|part| part.is_empty()) {
        return Err(invalid());
    }
    let payload = Zeroizing::new(
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(parts[1])
            .map_err(|_| invalid())?,
    );
    let claims: CodexClaims<'_> = serde_json::from_slice(&payload).map_err(|_| invalid())?;
    let account = auth
        .tokens
        .account_id
        .or_else(|| {
            claims
                .auth
                .as_ref()
                .and_then(|value| value.chatgpt_account_id)
        })
        .ok_or_else(invalid)?;
    let user = claims
        .sub
        .or_else(|| {
            claims
                .auth
                .as_ref()
                .and_then(|value| value.chatgpt_user_id.or(value.user_id))
        })
        .ok_or_else(invalid)?;
    if [account, user].iter().any(|value| {
        value.is_empty()
            || value.len() > 512
            || value.chars().any(char::is_control)
            || value.trim() != *value
    }) {
        return Err(invalid());
    }
    // The signed email claim becomes the account's fixed display identity.
    // Keep it optional: an older token shape without it stays importable.
    let email = claims
        .email
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 320
                && value.contains('@')
                && !value.chars().any(char::is_control)
                && value.trim() == *value
        })
        .map(str::to_owned);
    let identity = Zeroizing::new(serde_json::to_vec(&(account, user))?);
    Ok(CodexIdentity {
        digest: crate::digest(&identity),
        email,
    })
}

fn codex_auth_path(store: &Store, id: &Id) -> Result<std::path::PathBuf> {
    if store.account(id)?.provider != Provider::Codex {
        return Err(Error::Conflict("Codex credential provider mismatch"));
    }
    let profile = private::check_directory(&store.account_root(id)?.join("profile"))?;
    Ok(profile.join("auth.json"))
}

/// Local credential presence/shape only; runtime qualification and live account
/// authentication remain separate.
pub fn has_credentials(store: &Store, id: &Id) -> Result<bool> {
    match store.account(id)?.provider {
        Provider::Claude => has_token(store, id),
        Provider::Codex => {
            match private::read(&codex_auth_path(store, id)?, MAX_CODEX_AUTH_BYTES) {
                Ok(bytes) => Ok(codex_identity(&Zeroizing::new(bytes)).is_ok()),
                Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
                Err(error) => Err(error),
            }
        }
        Provider::Devin => crate::devin::auth::has_credentials(store, id),
    }
}

/// Import only the explicitly selected private file. Never read the ambient
/// CODEX_HOME or copy global configuration, sessions, MCP state or API keys.
pub fn import_codex_auth(store: &Store, id: &Id, source: &Path) -> Result<()> {
    let target = codex_auth_path(store, id)?;
    if source.file_name().and_then(|name| name.to_str()) != Some("auth.json") || source == target {
        return Err(Error::Unavailable(
            "select an external private Codex auth.json file",
        ));
    }
    let bytes = Zeroizing::new(private::read(source, MAX_CODEX_AUTH_BYTES)?);
    codex_identity(&bytes)?;
    import_codex_bytes(store, id, &bytes)
}

/// Create a Codex account only after the explicitly selected credential file
/// has passed private-file and ChatGPT shape validation. Source bytes are read
/// once; no global configuration or provider sessions are imported. The
/// account's display identity comes from the credential's own email claim.
pub fn import_codex_account(store: &Store, source: &Path) -> Result<Id> {
    if source.file_name().and_then(|name| name.to_str()) != Some("auth.json") {
        return Err(Error::Unavailable("select a private Codex auth.json file"));
    }
    let bytes = Zeroizing::new(private::read(source, MAX_CODEX_AUTH_BYTES)?);
    let identity = codex_identity(&bytes)?;
    let account = store.add_account(
        Provider::Codex,
        "ChatGPT subscription",
        crate::now_ms(),
        identity.email,
    )?;
    import_codex_bytes(store, &account.id, &bytes)?;
    Ok(account.id)
}

fn import_codex_bytes(store: &Store, id: &Id, bytes: &[u8]) -> Result<()> {
    let target = codex_auth_path(store, id)?;
    let run = store.prepare_probe(id, None, crate::now_ms())?;
    let mut publication_attempted = false;
    let result = (|| {
        let previous = match private::read(&target, MAX_CODEX_AUTH_BYTES) {
            Ok(bytes) => Some(Zeroizing::new(bytes)),
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        if let Some(previous) = &previous
            && codex_identity(previous)?.digest != codex_identity(bytes)?.digest
        {
            return Err(Error::Conflict("Codex credential account identity changed"));
        }
        let changed = previous
            .as_ref()
            .is_none_or(|previous| !same_codex_credentials(previous, bytes));
        store.begin_tool(
            &run,
            "xcb_auth_import",
            "host_auth_import",
            &crate::digest(bytes),
        )?;
        publication_attempted = true;
        crate::application_qualification::rotate_generation(store, &run)?;
        if let Some(previous) = previous {
            private::replace(&target, bytes, &crate::digest(&previous))?;
        } else {
            private::create(&target, bytes)?;
        }
        store.settle_tool(&run, "xcb_auth_import")?;
        if changed {
            store.clear_authentication_failure(&run)?;
        }
        // The credential's signed email claim becomes the display identity.
        if let Ok(identity) = codex_identity(bytes) {
            store.set_account_identity(id, identity.email, None)?;
        }
        Ok(())
    })();
    if result.is_ok() || !publication_attempted {
        store.settle(&run, xcb_core::session::State::Idle, crate::now_ms())?;
    }
    // Publication/receipt failures deliberately retain account custody.
    result
}

/// No Debug/Serialize implementation: a snapshot is host-only credential
/// authority. Its profile contains secrets and must remain outside workspaces.
pub struct CodexAuthSnapshot {
    account: Id,
    run: Id,
    state_root: std::path::PathBuf,
    profile: std::path::PathBuf,
    directory_identity: (u64, u64),
    original_revision: Option<String>,
    account_identity: Option<String>,
}
impl CodexAuthSnapshot {
    pub fn profile(&self) -> &Path {
        &self.profile
    }
}

/// A recovery record is private evidence, not launch authority. Its exact
/// serialized digest is bound into the durable auth receipt before any child
/// starts. Never deserialize the host-only CodexAuthSnapshot itself.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CodexAuthRecovery {
    version: u8,
    run: Id,
    account: Id,
    owner_instance: String,
    owner_pid: u32,
    state_root: std::path::PathBuf,
    state_identity: (u64, u64),
    profile: std::path::PathBuf,
    profile_identity: (u64, u64),
    persistent_identity: (u64, u64),
    original_revision: Option<String>,
    account_identity: Option<String>,
    allow_missing: bool,
}

fn directory_identity(path: &Path) -> Result<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    private::check_directory(path)?;
    let metadata = std::fs::symlink_metadata(path)?;
    Ok((metadata.dev(), metadata.ino()))
}

fn recovery_path(root: &Path, run: &Id) -> std::path::PathBuf {
    root.join("runs")
        .join(format!("{}.codex-auth-recovery.json", run.as_str()))
}

fn register_codex_recovery(
    store: &Store,
    run: &crate::store::RunRecord,
    snapshot: &CodexAuthSnapshot,
    allow_missing: bool,
) -> Result<()> {
    let owner = run
        .owner
        .as_ref()
        .ok_or(Error::Conflict("credential run owner missing"))?;
    let persistent = codex_auth_path(store, &run.account)?;
    let metadata = CodexAuthRecovery {
        version: 1,
        run: run.id.clone(),
        account: run.account.clone(),
        owner_instance: owner.instance.clone(),
        owner_pid: owner.pid,
        state_root: store.root().to_owned(),
        state_identity: directory_identity(store.root())?,
        profile: snapshot.profile.clone(),
        profile_identity: snapshot.directory_identity,
        persistent_identity: directory_identity(persistent.parent().ok_or(Error::PrivateState)?)?,
        original_revision: snapshot.original_revision.clone(),
        account_identity: snapshot.account_identity.clone(),
        allow_missing,
    };
    let bytes = serde_json::to_vec(&metadata)?;
    private::create(&recovery_path(store.root(), &run.id), &bytes)?;
    store.begin_tool(
        run,
        CODEX_AUTH_CALL,
        "host_auth_refresh",
        &crate::digest(bytes),
    )
}

/// Called only by Store::recover_run while holding its immediate transaction,
/// after exact run/lease validation and independent host/group stop proof. This
/// helper never reacquires the store mutex or settles database authority.
pub(crate) fn recover_codex_auth(
    root: &Path,
    run: &crate::store::RunRecord,
    metadata_digest: &str,
) -> Result<()> {
    let raw = private::read(&recovery_path(root, &run.id), 32 * 1024)?;
    if crate::digest(&raw) != metadata_digest {
        return Err(Error::Conflict("credential recovery metadata changed"));
    }
    let metadata: CodexAuthRecovery = serde_json::from_slice(&raw)
        .map_err(|_| Error::Conflict("invalid credential recovery metadata"))?;
    let owner = run
        .owner
        .as_ref()
        .ok_or(Error::Conflict("credential recovery owner missing"))?;
    let runs = root.join("runs");
    let hex_digest = xcb_core::hex64;
    if metadata.version != 1
        || metadata.run != run.id
        || metadata.account != run.account
        || metadata.owner_instance != owner.instance
        || metadata.owner_pid != owner.pid
        || metadata.state_root != root
        || directory_identity(root)? != metadata.state_identity
        || !metadata.profile.starts_with(&runs)
        || metadata.profile == runs
        || directory_identity(&metadata.profile)? != metadata.profile_identity
        || metadata
            .original_revision
            .as_deref()
            .is_some_and(|value| !hex_digest(value))
        || metadata
            .account_identity
            .as_deref()
            .is_some_and(|value| !hex_digest(value))
        || metadata.original_revision.is_some() != metadata.account_identity.is_some()
        || (!metadata.allow_missing && metadata.original_revision.is_none())
    {
        return Err(Error::Conflict("credential recovery authority changed"));
    }
    let persistent = root
        .join("accounts")
        .join(run.account.as_str())
        .join("profile");
    if directory_identity(&persistent)? != metadata.persistent_identity {
        return Err(Error::Conflict("persistent credential directory changed"));
    }
    let target = persistent.join("auth.json");
    let current = match private::read(&target, MAX_CODEX_AUTH_BYTES) {
        Ok(bytes) => Some(Zeroizing::new(bytes)),
        Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    let current_revision = current.as_ref().map(crate::digest);
    let refreshed = match private::read(&metadata.profile.join("auth.json"), MAX_CODEX_AUTH_BYTES) {
        Ok(bytes) => Zeroizing::new(bytes),
        Err(Error::Io(error))
            if error.kind() == std::io::ErrorKind::NotFound && metadata.allow_missing =>
        {
            // Device login was handed no existing token. A stopped flow that
            // produced none can release custody only if persistent auth agrees.
            if current_revision != metadata.original_revision {
                return Err(Error::Conflict(
                    "persistent credentials changed during login",
                ));
            }
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    let identity = codex_identity(&refreshed)?;
    if metadata
        .account_identity
        .as_ref()
        .is_some_and(|expected| *expected != identity.digest)
    {
        return Err(Error::Conflict("Codex credential account identity changed"));
    }
    let refreshed_revision = crate::digest(&refreshed);
    if current_revision.as_ref() == Some(&refreshed_revision) {
        // A prior recovery/normal persistence may have published and crashed
        // before SQLite commit. The exact refreshed bytes make this retry safe.
        private::open_file(&target, MAX_CODEX_AUTH_BYTES as u64)?.sync_all()?;
        std::fs::File::open(&persistent)?.sync_all()?;
        return Ok(());
    }
    if current_revision != metadata.original_revision {
        return Err(Error::Conflict(
            "persistent credentials changed before recovery",
        ));
    }
    if let Some(revision) = &metadata.original_revision {
        private::replace(&target, &refreshed, revision)
    } else {
        private::create(&target, &refreshed)
    }
}

fn current_codex_run(
    store: &Store,
    run: &crate::store::RunRecord,
) -> Result<crate::store::RunRecord> {
    let current = store
        .run(&run.id)?
        .ok_or(Error::Conflict("credential run missing"))?;
    if current.account != run.account
        || !matches!(current.phase.as_str(), "prepared" | "running")
        || current.owner.as_ref().map(|owner| owner.instance.as_str())
            != run.owner.as_ref().map(|owner| owner.instance.as_str())
    {
        return Err(Error::Conflict("credential run authority changed"));
    }
    codex_auth_path(store, &run.account)?;
    Ok(current)
}

/// Call after preparing the exclusive account run, before provider spawn. The
/// destination must be a private launch profile under this store's runs tree.
/// Errors after begin_tool require discard_unstarted_codex_auth when the caller
/// has independently established that no child launched; do not drop custody.
pub fn snapshot_codex_auth(
    store: &Store,
    run: &crate::store::RunRecord,
    profile: &Path,
) -> Result<CodexAuthSnapshot> {
    use std::os::unix::fs::MetadataExt;
    current_codex_run(store, run)?;
    let runs = store.root().join("runs");
    if !profile.starts_with(&runs) || profile == runs {
        return Err(Error::PrivateState);
    }
    let profile = private::directory(profile)?;
    let target = profile.join("auth.json");
    match std::fs::symlink_metadata(&target) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Ok(_) => return Err(Error::Conflict("credential snapshot already exists")),
        Err(error) => return Err(error.into()),
    }
    let bytes = Zeroizing::new(private::read(
        &codex_auth_path(store, &run.account)?,
        MAX_CODEX_AUTH_BYTES,
    )?);
    let identity = codex_identity(&bytes)?;
    let revision = crate::digest(&bytes);
    let metadata = std::fs::symlink_metadata(&profile)?;
    let snapshot = CodexAuthSnapshot {
        account: run.account.clone(),
        run: run.id.clone(),
        state_root: store.root().to_owned(),
        profile,
        directory_identity: (metadata.dev(), metadata.ino()),
        original_revision: Some(revision),
        account_identity: Some(identity.digest),
    };
    register_codex_recovery(store, run, &snapshot, false)?;
    private::create(&target, &bytes)?;
    Ok(snapshot)
}

/// Settle only the credential snapshot receipt after independent proof no child
/// started. The caller still owns run settlement and disposable launch cleanup.
pub fn discard_unstarted_codex_auth(
    store: &Store,
    run: &crate::store::RunRecord,
    no_child_started: bool,
) -> Result<()> {
    let current = current_codex_run(store, run)?;
    if !no_child_started || current.phase != "prepared" || current.pid.is_some() {
        return Err(Error::Conflict(
            "credential snapshot has no unstarted proof",
        ));
    }
    store.discard_unstarted_tool(run, CODEX_AUTH_CALL)
}

/// Persist refreshed credentials only after the provider, bridge and handlers
/// are fully joined, while the run still owns its account. CAS rejects rotation
/// by another actor; identity checks reject an unintended account/user switch.
pub fn persist_codex_auth(
    store: &Store,
    run: &crate::store::RunRecord,
    snapshot: &CodexAuthSnapshot,
    joined: bool,
) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    if !joined
        || snapshot.run != run.id
        || snapshot.account != run.account
        || snapshot.state_root != store.root()
    {
        return Err(Error::Conflict(
            "credential refresh requires joined account custody",
        ));
    }
    current_codex_run(store, run)?;
    private::check_directory(&snapshot.profile)?;
    let metadata = std::fs::symlink_metadata(&snapshot.profile)?;
    if (metadata.dev(), metadata.ino()) != snapshot.directory_identity {
        return Err(Error::Conflict("credential snapshot directory changed"));
    }
    let bytes = Zeroizing::new(private::read(
        &snapshot.profile.join("auth.json"),
        MAX_CODEX_AUTH_BYTES,
    )?);
    let identity = codex_identity(&bytes)?;
    if snapshot
        .account_identity
        .as_ref()
        .is_some_and(|expected| *expected != identity.digest)
    {
        return Err(Error::Conflict("Codex credential account identity changed"));
    }
    let target = codex_auth_path(store, &run.account)?;
    if let Some(revision) = &snapshot.original_revision {
        private::replace(&target, &bytes, revision)?;
    } else {
        private::create(&target, &bytes)?;
    }
    store.settle_tool(run, CODEX_AUTH_CALL)?;
    // Identity was proven unchanged above; the email claim just fills in the
    // display identity for accounts imported before it was captured.
    store.set_account_identity(&run.account, identity.email, None)
}

/// The caller owns process-group supervision, bounded wait/cancellation and
/// independent join proof. Device-auth output is deliberately inherited so the
/// user sees the verification URI/code immediately; never use this for Claude
/// setup-token, whose stdout contains a reusable secret.
pub struct CodexLoginPlan {
    pub command: Command,
    pub credentials: CodexAuthSnapshot,
}

pub fn prepare_codex_login(
    store: &Store,
    run: &crate::store::RunRecord,
    pin: &Pin,
    profile: &Path,
) -> Result<CodexLoginPlan> {
    use std::os::unix::{fs::MetadataExt, process::CommandExt};
    current_codex_run(store, run)?;
    if pin.provider != Provider::Codex {
        return Err(Error::Conflict("login provider mismatch"));
    }
    pin.verify()?;
    // The caller must retain custody if generation publication/receipt fails,
    // even though no provider was started by this preparation function.
    crate::application_qualification::rotate_generation(store, run)
        .map_err(|_| Error::CleanupUnproven)?;
    let runs = store.root().join("runs");
    if !profile.starts_with(&runs) || profile == runs {
        return Err(Error::PrivateState);
    }
    let profile = private::directory(profile)?;
    if std::fs::read_dir(&profile)?.next().is_some() {
        return Err(Error::Conflict("Codex login profile must be empty"));
    }
    let original = match private::read(&codex_auth_path(store, &run.account)?, MAX_CODEX_AUTH_BYTES)
    {
        Ok(bytes) => Some(Zeroizing::new(bytes)),
        Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    let account_identity = original
        .as_ref()
        .map(|bytes| codex_identity(bytes).map(|identity| identity.digest))
        .transpose()?;
    let original_revision = original.as_ref().map(crate::digest);
    let home = private::directory(&profile.join("login-home"))?;
    private::directory(&home.join("tmp"))?;
    let mut env = environment(&home);
    env.insert("CODEX_HOME".into(), profile.to_string_lossy().into_owned());
    let mut command = Command::new(&pin.executable);
    command.args([
        "-c",
        "cli_auth_credentials_store=\"file\"",
        "-c",
        "forced_login_method=\"chatgpt\"",
        "login",
        "--device-auth",
    ]);
    command
        .env_clear()
        .envs(env)
        .current_dir(&home)
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .kill_on_drop(true);
    command.as_std_mut().process_group(0);
    let metadata = std::fs::symlink_metadata(&profile)?;
    let credentials = CodexAuthSnapshot {
        account: run.account.clone(),
        run: run.id.clone(),
        state_root: store.root().to_owned(),
        profile,
        directory_identity: (metadata.dev(), metadata.ino()),
        original_revision,
        account_identity,
    };
    register_codex_recovery(store, run, &credentials, true)?;
    Ok(CodexLoginPlan {
        command,
        credentials,
    })
}

#[cfg(test)]
mod auth_custody_tests {
    use super::*;

    #[test]
    fn claude_unproven_login_capture_retains_account_and_artifacts() {
        let directory = tempfile::tempdir().unwrap();
        let base = directory.path().canonicalize().unwrap();
        let store = Store::open(&base.join("state")).unwrap();
        let account = store.add_account(Provider::Claude, "Max", 1, None).unwrap();
        let original = b"sk-ant-oat01-original_synthetic_fixture_not_real";
        store_token(&store, &account.id, original).unwrap();
        let run = store.prepare_probe(&account.id, None, 2).unwrap();
        let publication = claude_token_publication(&store, &run).unwrap();
        let mut artifacts = LaunchArtifacts::create(store.root()).unwrap();
        let path = artifacts.path().to_owned();
        artifacts.retain_before_launch();
        assert!(
            finish_claude_login(
                &store,
                &run,
                &publication,
                &mut artifacts,
                CaptureOutcome::Unproven
            )
            .is_err()
        );
        drop(artifacts);
        assert!(path.exists());
        assert_eq!(store.unsettled_runs().unwrap().len(), 1);
        assert_eq!(private::read(&publication.path, 2048).unwrap(), original);
    }

    #[test]
    fn unstarted_auth_receipt_cleanup_rejects_started_and_foreign_owned_runs() {
        let directory = tempfile::tempdir().unwrap();
        let base = directory.path().canonicalize().unwrap();
        let store = Store::open(&base.join("state")).unwrap();
        let account = store
            .add_account(Provider::Codex, "ChatGPT", 1, None)
            .unwrap();
        let run = store.prepare_probe(&account.id, None, 2).unwrap();
        store
            .begin_tool(
                &run,
                CODEX_AUTH_CALL,
                "host_auth_refresh",
                &crate::digest(b"fixture"),
            )
            .unwrap();
        let other = Store::open(store.root()).unwrap();
        assert!(discard_unstarted_codex_auth(&other, &run, true).is_err());
        let started = store.mark_spawned(&run, std::process::id()).unwrap();
        assert!(discard_unstarted_codex_auth(&store, &started, true).is_err());
        assert!(discard_unstarted_codex_auth(&store, &run, true).is_err());
        assert_eq!(store.unsettled_runs().unwrap().len(), 1);
    }
}
