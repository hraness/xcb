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

mod claude_oauth;
pub(crate) use claude_oauth::recovery as claude_recovery;
pub(crate) use claude_oauth::refresh_claude_credentials;
pub use claude_oauth::{has_claude_browser_credentials, login_claude_browser_with_interaction};

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
    if has_claude_browser_credentials(store, &run.account)? {
        return Err(Error::guided(
            "this account uses full Claude sign-in; reconnect it to replace its credentials",
            format!("xcb accounts login {} --browser", run.account),
        ));
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

/// The provider's own subscription meter for browser-signed-in Claude
/// accounts. `None` means the account holds a setup token, which the usage
/// endpoint refuses by scope — those accounts stay passively metered by
/// rate-limit events observed during runs.
pub(crate) async fn claude_usage(store: &Store, id: &Id) -> Result<Option<serde_json::Value>> {
    let Some(token) = claude_oauth::cached_token(store, id)? else {
        return Ok(None);
    };
    claude_oauth::fetch_usage(&token).await.map(Some)
}

pub(crate) fn token(store: &Store, id: &Id) -> Result<Zeroizing<String>> {
    if store.account(id)?.provider != Provider::Claude {
        return Err(Error::Conflict("subscription token provider mismatch"));
    }
    if let Some(token) = claude_oauth::cached_token(store, id)? {
        return Ok(token);
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
    if has_claude_browser_credentials(store, id)? {
        return Ok(true);
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
/// Reading our own launch artifacts is an ambient-free source for launches
/// whose control-protocol startup report omits the account email.
pub(crate) fn claude_profile_email<P: AsRef<Path>>(dirs: &[P]) -> Option<String> {
    for dir in dirs {
        let Ok(bytes) =
            private::read_provider_written(&dir.as_ref().join(".claude.json"), 256 * 1024)
        else {
            continue;
        };
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            continue;
        };
        if let Some(email) = value
            .pointer("/oauthAccount/emailAddress")
            .and_then(serde_json::Value::as_str)
            .and_then(crate::store::observed_email)
        {
            return Some(email);
        }
    }
    None
}

/// The email xcb verified with the provider when this account last completed
/// a full browser sign-in. Local read only: no Keychain, process, or network.
pub(crate) fn claude_browser_email(store: &Store, id: &Id) -> Result<Option<String>> {
    claude_oauth::verified_email(store, id)
}

/// Every local, token-free source of a Claude account's email, in order of
/// freshness: the provider's own startup report, the `.claude.json` profile
/// the provider wrote inside this launch's private directories, then the
/// identity verified at browser sign-in. Persists the first valid one.
pub(crate) fn observe_claude_email(
    store: &Store,
    id: &Id,
    reported: Option<&str>,
    profile_dirs: &[impl AsRef<Path>],
) -> Result<Option<String>> {
    // The browser record is a display fallback; an unreadable one is
    // reported by the credential path, never by identity observation.
    let email = reported
        .and_then(crate::store::observed_email)
        .or_else(|| claude_profile_email(profile_dirs))
        .or_else(|| claude_browser_email(store, id).ok().flatten());
    if let Some(email) = &email
        && store.account(id)?.email.as_ref() != Some(email)
    {
        store.set_account_identity(id, Some(email.clone()), None)?;
    }
    Ok(email)
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
        observe_claude_email(
            store,
            &run.account,
            None,
            &[&base.join("home"), &base.join("profile")],
        )?;
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
    login_claude(store, id, pin, cancel, None).await
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaudeLoginEvent {
    AuthorizationUrl(String),
    CodeRequested,
}

/// Pinned Claude `auth login` failures are classified only after the helper
/// stops. Never return captured text: errors can contain tokens, URLs or PII.
pub(crate) fn claude_auth_failure(stdout: &[u8], stderr: &[u8]) -> &'static str {
    const UNKNOWN: &str =
        "Claude sign-in stopped with an error; saved credentials need verification";
    for bytes in [stderr, stdout] {
        if bytes.len() > 64 * 1024 {
            continue;
        }
        let Ok(text) = std::str::from_utf8(bytes) else {
            continue;
        };
        for line in text.lines().map(str::trim) {
            if matches!(
                line,
                "Managed settings on this machine configure a Cloud gateway sign-in; run interactive /login to authenticate."
                    | "Unable to read managed policy settings."
                    | "Unable to read managed policy settings, which may restrict the API providers this machine may use (allowedProviders). Contact your administrator."
                    | "forceLoginOrgUUID in managed settings is set to an empty array."
            ) {
                return "Claude sign-in stopped: managed policy blocked authentication; saved credentials need verification";
            }
            let Some(reason) = line.strip_prefix("Login failed: ") else {
                continue;
            };
            let message = match reason {
                "Authentication failed: Invalid authorization code" => Some(
                    "Claude sign-in stopped: the authorization code was rejected; saved credentials need verification",
                ),
                "Invalid state parameter" => Some(
                    "Claude sign-in stopped: the authorization state did not match this attempt; saved credentials need verification",
                ),
                "No authorization code received" => Some(
                    "Claude sign-in stopped: no authorization code was received; saved credentials need verification",
                ),
                "Couldn't save your login. Try logging in again."
                | "Couldn't save your login. If your Mac's keychain is locked, unlock it and log in again." => {
                    Some(
                        "Claude sign-in stopped: Claude could not save its credentials; check Keychain access and recover this sign-in",
                    )
                }
                "socket hang up" | "Network Error" => Some(
                    "Claude sign-in stopped: its network connection failed; saved credentials need verification",
                ),
                "Request failed with status code 400" => Some(
                    "Claude sign-in stopped: the service rejected the request (HTTP 400); saved credentials need verification",
                ),
                "Request failed with status code 401" => Some(
                    "Claude sign-in stopped: the service rejected authorization (HTTP 401); saved credentials need verification",
                ),
                "Request failed with status code 403" => Some(
                    "Claude sign-in stopped: the service refused access (HTTP 403); saved credentials need verification",
                ),
                "Request failed with status code 429" => Some(
                    "Claude sign-in stopped: the service limited requests (HTTP 429); saved credentials need verification",
                ),
                _ => None,
            };
            if let Some(message) = message {
                return message;
            }
            if [
                "getaddrinfo ENOTFOUND ",
                "getaddrinfo EAI_AGAIN ",
                "connect ECONNREFUSED ",
                "connect ETIMEDOUT ",
                "read ECONNRESET",
            ]
            .iter()
            .any(|prefix| reason.starts_with(prefix))
                || reason
                    .strip_prefix("timeout of ")
                    .and_then(|value| value.strip_suffix("ms exceeded"))
                    .is_some_and(|value| {
                        !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
                    })
            {
                return "Claude sign-in stopped: its network connection failed; saved credentials need verification";
            }
            if reason
                .strip_prefix("Request failed with status code ")
                .filter(|status| {
                    status.len() == 3 && status.bytes().all(|byte| byte.is_ascii_digit())
                })
                .and_then(|status| status.parse::<u16>().ok())
                .is_some_and(|status| (500..=599).contains(&status))
            {
                return "Claude sign-in stopped: the service returned an error (HTTP 5xx); saved credentials need verification";
            }
        }
    }
    UNKNOWN
}

pub async fn login_with_interaction(
    store: &Store,
    id: &Id,
    pin: &Pin,
    cancel: watch::Receiver<bool>,
    events: tokio::sync::mpsc::Sender<ClaudeLoginEvent>,
    codes: tokio::sync::mpsc::Receiver<Zeroizing<String>>,
) -> Result<()> {
    #[cfg(not(unix))]
    {
        let _ = (store, id, pin, cancel, events, codes);
        Err(Error::providers_unsupported())
    }
    #[cfg(unix)]
    {
        let (stdin, terminal) = crate::process::login_terminal()?;
        let mut observer = ClaudeLoginObserver::default();
        let interaction = crate::process::LoginInteraction {
            stdin,
            terminal,
            codes,
            observer: Box::new(move |bytes| {
                for event in observer.observe(bytes) {
                    let _ = events.try_send(event);
                }
                if observer.failed {
                    Err(Error::Unavailable(observer.failure_message()))
                } else {
                    Ok(())
                }
            }),
        };
        login_claude(store, id, pin, cancel, Some(interaction)).await
    }
}

#[cfg(any(unix, test))]
#[derive(Default)]
struct ClaudeLoginObserver {
    bytes: Zeroizing<Vec<u8>>,
    url_sent: bool,
    prompt_sent: bool,
    prompt_seen: bool,
    failed: bool,
    malformed_code: bool,
}

#[cfg(any(unix, test))]
fn oauth_url_has_secret(url: &str) -> bool {
    let mut decoded = Zeroizing::new(url.as_bytes().to_vec());
    for _ in 0..3 {
        if decoded.windows(7).any(|window| window == b"sk-ant-") {
            return true;
        }
        let mut next = Zeroizing::new(Vec::with_capacity(decoded.len()));
        let mut offset = 0;
        while offset < decoded.len() {
            if decoded[offset] == b'%' && offset + 2 < decoded.len() {
                let hex = |b: u8| (b as char).to_digit(16).map(|n| n as u8);
                if let (Some(a), Some(b)) = (hex(decoded[offset + 1]), hex(decoded[offset + 2])) {
                    next.push(a * 16 + b);
                    offset += 3;
                    continue;
                }
            }
            next.push(decoded[offset]);
            offset += 1;
        }
        if *next == *decoded {
            return false;
        }
        decoded = next;
    }
    decoded.windows(7).any(|window| window == b"sk-ant-")
}

#[cfg(any(unix, test))]
impl ClaudeLoginObserver {
    fn failure_message(&self) -> &'static str {
        if self.malformed_code {
            "Claude rejected an incomplete sign-in code; retry sign-in and copy the full code"
        } else {
            "Claude browser sign-in failed; retry sign-in or paste a setup token"
        }
    }

    fn observe(&mut self, bytes: &[u8]) -> Vec<ClaudeLoginEvent> {
        // Provider output contains the reusable token. It stays private here;
        // only a known OAuth endpoint and a fixed prompt cross the boundary.
        if self.bytes.len() + bytes.len() > 128 * 1024 {
            return Vec::new();
        }
        self.bytes.extend_from_slice(bytes);
        static ANSI: OnceLock<Regex> = OnceLock::new();
        static OSC: OnceLock<Regex> = OnceLock::new();
        static URL: OnceLock<Regex> = OnceLock::new();
        let raw = Zeroizing::new(String::from_utf8_lossy(&self.bytes).into_owned());
        // Ink wraps visible links in OSC 8 hyperlinks. Remove the control
        // payload and terminators, retaining only the visible URL. CSI-only
        // stripping leaves an ESC after the URL, hiding it from the scanner.
        let without_osc = Zeroizing::new(
            OSC.get_or_init(|| Regex::new(r"\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)").unwrap())
                .replace_all(&raw, "")
                .into_owned(),
        );
        let text = Zeroizing::new(
            ANSI.get_or_init(|| Regex::new(r"\x1b\[[0-9;?]*[ -/]*[@-~]").unwrap())
                .replace_all(&without_osc, "")
                .into_owned(),
        );
        // The official auth command reports malformed manual input without
        // exiting. Stop our supervised attempt rather than waiting ten minutes
        // for an exchange that never started. Only this fixed error escapes.
        self.malformed_code |=
            text.contains("Invalid code. Please make sure the full code was copied.");
        self.failed |= self.malformed_code || text.contains("OAuth error:");
        let mut events = Vec::new();
        if !self.url_sent {
            let urls = URL.get_or_init(|| Regex::new(r"https://(?:claude\.com/cai/oauth/authorize|claude\.ai/oauth/authorize|platform\.claude\.com/oauth/authorize|console\.anthropic\.com/oauth/authorize)\?[A-Za-z0-9_~%=&.+:/-]+[\s]").unwrap());
            for url in urls.find_iter(&text) {
                let url = url.as_str().trim();
                if url.len() <= 8192
                    && !oauth_url_has_secret(url)
                    && url.contains("client_id=")
                    && url.contains("state=")
                    && url.contains("code_challenge=")
                {
                    self.url_sent = true;
                    events.push(ClaudeLoginEvent::AuthorizationUrl(url.into()));
                    break;
                }
            }
        }
        self.prompt_seen |= text.contains("Paste code here if prompted");
        // A provider may render its prompt before its link. The terminal must
        // receive the verified URL before waiting for optional manual input.
        if self.url_sent && self.prompt_seen && !self.prompt_sent {
            self.prompt_sent = true;
            events.push(ClaudeLoginEvent::CodeRequested);
        }
        events
    }
}

async fn login_claude(
    store: &Store,
    id: &Id,
    pin: &Pin,
    cancel: watch::Receiver<bool>,
    interaction: Option<crate::process::LoginInteraction>,
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
            .env("COLUMNS", "4096")
            .current_dir(&home);
        // The provider completes browser sign-in through its own loopback
        // channel only when it can actually launch a browser; a suppressed
        // BROWSER leaves the paste prompt as an unmounted fallback that
        // never reads input, so no BROWSER override is set here.
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
    let outcome = if let Some(interaction) = interaction {
        crate::process::capture_supervised_interactive(
            command,
            64 * 1024,
            Duration::from_secs(600),
            cancel,
            |pid| store.mark_spawned(&run, pid).map(|_| ()),
            Some(interaction),
        )
        .await
    } else {
        capture_supervised(
            command,
            64 * 1024,
            Duration::from_secs(600),
            cancel,
            |pid| store.mark_spawned(&run, pid).map(|_| ()),
        )
        .await
    };
    finish_claude_login(store, &run, &publication, &mut artifacts, outcome)
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
    private::check_directory(path)?;
    let metadata = crate::os::lstat(path)?;
    Ok((metadata.dev, metadata.ino))
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
        // A stopped login may have completed under another ChatGPT account.
        // Its isolated profile has no authority to replace the saved account.
        // Once the saved credential is proven unchanged, reconciliation may
        // settle the receipt and release the lease without publishing it.
        if current_revision != metadata.original_revision {
            return Err(Error::Conflict(
                "persistent credentials changed during login",
            ));
        }
        return Ok(());
    }
    let refreshed_revision = crate::digest(&refreshed);
    if current_revision.as_ref() == Some(&refreshed_revision) {
        // A prior recovery/normal persistence may have published and crashed
        // before SQLite commit. The exact refreshed bytes make this retry safe.
        private::open_file(&target, MAX_CODEX_AUTH_BYTES as u64)?.sync_all()?;
        private::sync_directory(&persistent)?;
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
    let metadata = crate::os::lstat(&profile)?;
    let snapshot = CodexAuthSnapshot {
        account: run.account.clone(),
        run: run.id.clone(),
        state_root: store.root().to_owned(),
        profile,
        directory_identity: (metadata.dev, metadata.ino),
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
    let metadata = crate::os::lstat(&snapshot.profile)?;
    if (metadata.dev, metadata.ino) != snapshot.directory_identity {
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
    if cfg!(windows) {
        return Err(Error::providers_unsupported());
    }
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
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .kill_on_drop(true);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.as_std_mut().process_group(0);
    }
    let metadata = crate::os::lstat(&profile)?;
    let credentials = CodexAuthSnapshot {
        account: run.account.clone(),
        run: run.id.clone(),
        state_root: store.root().to_owned(),
        profile,
        directory_identity: (metadata.dev, metadata.ino),
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
mod claude_login_observer_tests {
    use super::*;
    const TEST_URL: &str =
        "https://claude.com/cai/oauth/authorize?client_id=test&state=test&code_challenge=test";

    #[test]
    fn official_auth_failures_use_static_diagnostics_without_private_output() {
        let poison = "sk-ant-oat01-private-sentinel secret@example.test https://private.test/code?secret=sentinel";
        for (line, expected) in [
            (
                "Login failed: Invalid state parameter",
                "authorization state",
            ),
            (
                "Login failed: No authorization code received",
                "no authorization code",
            ),
            (
                "Login failed: Authentication failed: Invalid authorization code",
                "authorization code was rejected",
            ),
            (
                "Login failed: Couldn't save your login. Try logging in again.",
                "could not save its credentials",
            ),
            (
                "Login failed: Couldn't save your login. If your Mac's keychain is locked, unlock it and log in again.",
                "could not save its credentials",
            ),
            (
                "Login failed: timeout of 30000ms exceeded",
                "network connection failed",
            ),
            ("Login failed: Network Error", "network connection failed"),
            (
                "Login failed: getaddrinfo ENOTFOUND private.test",
                "network connection failed",
            ),
            (
                "Login failed: Request failed with status code 400",
                "request (HTTP 400)",
            ),
            (
                "Login failed: Request failed with status code 401",
                "authorization (HTTP 401)",
            ),
            (
                "Login failed: Request failed with status code 403",
                "access (HTTP 403)",
            ),
            (
                "Login failed: Request failed with status code 429",
                "requests (HTTP 429)",
            ),
            (
                "Login failed: Request failed with status code 503",
                "error (HTTP 5xx)",
            ),
            (
                "Unable to read managed policy settings.",
                "managed policy blocked",
            ),
            (
                "Managed settings on this machine configure a Cloud gateway sign-in; run interactive /login to authenticate.",
                "managed policy blocked",
            ),
        ] {
            let output = Zeroizing::new(format!("{line}\n{poison}\n"));
            for (stdout, stderr) in [(output.as_bytes(), &[][..]), (&[][..], output.as_bytes())] {
                let message = claude_auth_failure(stdout, stderr);
                assert!(message.contains(expected), "{line}: {message}");
                let error = Error::AuthUnproven(message);
                let rendered = format!("{error} {error:?}");
                for secret in [
                    "sk-ant-",
                    "secret@example.test",
                    "https://",
                    "private.test",
                    "sentinel",
                ] {
                    assert!(!rendered.contains(secret));
                }
                assert!(error.is_cleanup_unproven());
            }
        }
    }

    #[test]
    fn official_auth_unknown_partial_or_oversize_output_stays_generic() {
        let generic = claude_auth_failure(&[], &[]);
        for output in [
            "Login failed: confidential sk-ant-oat01-private example@example.test https://private.test",
            "provider body says Login failed: Request failed with status code 403",
            "Login failed: Request failed with status code 400 confidential",
            "Login failed: timeout of privatems exceeded",
            "Invalid code. Please make sure the full code was copied.",
            "Login failed: Request failed with status code 5999",
            "Login failed: Request failed with status code +503",
            "Login failed: Request failed with status code 4",
        ] {
            assert_eq!(claude_auth_failure(&[], output.as_bytes()), generic);
        }
        let oversized = format!(
            "Login failed: Invalid state parameter\n{}",
            "x".repeat(64 * 1024)
        );
        assert_eq!(claude_auth_failure(&[], oversized.as_bytes()), generic);
        assert_eq!(claude_auth_failure(&[], &[0xff]), generic);
        assert!(
            !claude_auth_failure(&[], b"Login failed: Request failed with status code 400")
                .contains("code was rejected")
        );
    }

    #[test]
    fn native_osc_hyperlinks_emit_visible_oauth_link_before_prompt() {
        for terminator in ["\x07", "\x1b\\"] {
            let output = format!(
                "\x1b[32m\x1b]8;;{TEST_URL}{terminator}{TEST_URL}\x1b]8;;{terminator}\x1b[0m\nPaste code here if prompted > "
            );
            for split in 0..output.len() {
                let mut observer = ClaudeLoginObserver::default();
                let mut events = observer.observe(&output.as_bytes()[..split]);
                events.extend(observer.observe(&output.as_bytes()[split..]));
                assert_eq!(
                    events,
                    vec![
                        ClaudeLoginEvent::AuthorizationUrl(TEST_URL.into()),
                        ClaudeLoginEvent::CodeRequested
                    ],
                    "OSC terminator {terminator:?}, split {split}"
                );
            }
        }
    }

    #[test]
    fn prompt_before_url_waits_for_valid_authorization_link() {
        let mut observer = ClaudeLoginObserver::default();
        assert!(
            observer
                .observe(b"Paste code here if prompted > \n")
                .is_empty()
        );
        assert!(
            observer
                .observe(b"https://claude.com/cai/oauth/authorize?client_id=x&state=x\n")
                .is_empty()
        );
        assert_eq!(
            observer.observe(format!("{TEST_URL}\n").as_bytes()),
            vec![
                ClaudeLoginEvent::AuthorizationUrl(TEST_URL.into()),
                ClaudeLoginEvent::CodeRequested
            ]
        );
        assert!(
            observer
                .observe(b"Paste code here if prompted > ")
                .is_empty()
        );
    }

    #[test]
    fn hidden_hyperlink_payload_never_becomes_the_visible_sign_in_link() {
        let mut observer = ClaudeLoginObserver::default();
        let output =
            format!("\x1b]8;;{TEST_URL}\x07Sign in\x1b]8;;\x07\nPaste code here if prompted > ");
        assert!(observer.observe(output.as_bytes()).is_empty());
    }

    #[test]
    fn private_provider_errors_fail_without_relaying_details() {
        let mut observer = ClaudeLoginObserver::default();
        assert!(
            observer
                .observe(b"OAuth error: confidential provider details")
                .is_empty()
        );
        assert!(observer.failed);
    }

    #[test]
    fn incomplete_manual_code_rejection_is_private_across_output_fragments() {
        let output = b"Invalid code. Please make sure the full code was copied.\nprivate-code#private-state private@example.test\n";
        for split in 0..=output.len() {
            let mut observer = ClaudeLoginObserver::default();
            assert!(observer.observe(&output[..split]).is_empty());
            assert!(observer.observe(&output[split..]).is_empty());
            assert!(observer.failed, "split {split}");
            assert_eq!(
                observer.failure_message(),
                "Claude rejected an incomplete sign-in code; retry sign-in and copy the full code"
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn incomplete_manual_code_stops_and_joins_the_waiting_auth_helper() {
        let (stdin, terminal) = crate::process::login_terminal().unwrap();
        let (_codes, receiver) = tokio::sync::mpsc::channel(1);
        let mut observer = ClaudeLoginObserver::default();
        let interaction = crate::process::LoginInteraction {
            stdin,
            terminal,
            codes: receiver,
            observer: Box::new(move |bytes| {
                observer.observe(bytes);
                if observer.failed {
                    Err(Error::Unavailable(observer.failure_message()))
                } else {
                    Ok(())
                }
            }),
        };
        let mut command = tokio::process::Command::new("/bin/sh");
        command.args([
            "-c",
            // exec keeps the helper a single group member; a forked `sleep`
            // orphan can outlive the group-absent proof on a loaded host.
            "printf 'Invalid code. Please make sure the full code was copied.\\n' >&2; exec sleep 30",
        ]);
        let (_cancel, cancel) = tokio::sync::watch::channel(false);
        let outcome = crate::process::capture_supervised_interactive(
            command,
            1024,
            // Below the helper's own 30-second wait; generous enough that a
            // loaded scheduler cannot race the child into the deadline.
            std::time::Duration::from_secs(20),
            cancel,
            |_| Ok(()),
            Some(interaction),
        )
        .await;
        let debug = match &outcome {
            crate::process::CaptureOutcome::Joined(Err(e)) => e.to_string(),
            crate::process::CaptureOutcome::Joined(Ok(_)) => "joined-ok".into(),
            crate::process::CaptureOutcome::NeverStarted(e) => format!("never-started:{e}"),
            crate::process::CaptureOutcome::Unproven => "unproven".into(),
        };
        assert!(
            matches!(
                outcome,
                crate::process::CaptureOutcome::Joined(Err(Error::Unavailable(
                    "Claude rejected an incomplete sign-in code; retry sign-in and copy the full code"
                )))
            ),
            "{debug}"
        );
    }
    /// A provider whose input listener mounts after the paste prompt renders
    /// can discard a code written during that mount. A silent child must see
    /// the retained code redelivered without operator action.
    #[cfg(unix)]
    #[tokio::test]
    async fn silent_child_after_submit_receives_the_code_again() {
        let (stdin, terminal) = crate::process::login_terminal().unwrap();
        let (codes, receiver) = tokio::sync::mpsc::channel(1);
        let mut observer = ClaudeLoginObserver::default();
        let interaction = crate::process::LoginInteraction {
            stdin,
            terminal,
            codes: receiver,
            observer: Box::new(move |bytes| {
                observer.observe(bytes);
                Ok(())
            }),
        };
        let mut command = tokio::process::Command::new("/bin/sh");
        command.args(["-c", "sleep 10; head -n 2"]);
        let (_cancel, cancel) = tokio::sync::watch::channel(false);
        codes
            .send(zeroize::Zeroizing::new("resend-code#st".to_string()))
            .await
            .unwrap();
        drop(codes);
        let outcome = crate::process::capture_supervised_interactive(
            command,
            1024,
            std::time::Duration::from_secs(30),
            cancel,
            |_| Ok(()),
            Some(interaction),
        )
        .await;
        let crate::process::CaptureOutcome::Joined(Ok(output)) = outcome else {
            panic!("the auth helper must join with captured output")
        };
        assert_eq!(
            String::from_utf8_lossy(&output)
                .matches("resend-code")
                .count(),
            2,
            "the code is written once at submit and once more while the child stays silent"
        );
    }

    #[test]
    fn only_complete_oauth_links_and_fixed_prompt_leave_capture() {
        let mut observer = ClaudeLoginObserver::default();
        assert!(
            observer
                .observe(b"secret sk-ant-oat01-neverpublish\nhttps://claude.com/cai/oauth/auth")
                .is_empty()
        );
        let events = observer.observe(
            b"orize?client_id=test&state=test&code_challenge=test\nPaste code here if prompted > ",
        );
        assert_eq!(events, vec![ClaudeLoginEvent::AuthorizationUrl("https://claude.com/cai/oauth/authorize?client_id=test&state=test&code_challenge=test".into()), ClaudeLoginEvent::CodeRequested]);
        assert!(
            observer
                .observe(b"Paste code here if prompted > ")
                .is_empty()
        );
    }
    #[test]
    fn untrusted_endpoints_and_missing_pkce_are_not_forwarded() {
        for text in [
            "https://claude.com.evil/cai/oauth/authorize?client_id=x&state=x&code_challenge=x\n",
            "https://claude.com/cai/oauth/authorize/evil?client_id=x&state=x&code_challenge=x\n",
            "https://claude.com/cai/oauth/authorize?client_id=x&state=x\n",
            "https://claude.com/cai/oauth/authorize?client_id=x&state=x&code_challenge=x@evil\n",
            "https://claude.com/cai/oauth/authorize?client_id=x&state=x&code_challenge=sk-ant-oat01-private\n",
            "https://claude.com/cai/oauth/authorize?client_id=x&state=x&code_challenge=sk%2Dant-oat01-private\n",
        ] {
            assert!(
                ClaudeLoginObserver::default()
                    .observe(text.as_bytes())
                    .is_empty()
            );
        }
    }
}

#[cfg(test)]
mod claude_email_tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, Store, Id) {
        let directory = tempfile::tempdir().unwrap();
        let base = xcb_core::canonical(directory.path()).unwrap();
        let store = Store::open(&base.join("state")).unwrap();
        let id = store
            .add_account(Provider::Claude, "Personal Max", 1, None)
            .unwrap()
            .id;
        (directory, store, id)
    }

    fn profile(dir: &Path, email: &str) {
        private::directory(dir).unwrap();
        let body = serde_json::json!({
            "oauthAccount": {"emailAddress": email, "accountUuid": "synthetic"},
            "primaryApiKey": "SYNTHETIC_PRIVATE_VALUE",
        });
        // Claude Code writes this file under the default umask (0644), not
        // xcb's owner-only mode; the private launch directory still holds it.
        let path = dir.join(".claude.json");
        std::fs::write(&path, body.to_string()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
    }

    #[test]
    fn no_observed_email_keeps_the_placeholder_name() {
        let (_directory, store, id) = fixture();
        let missing = store.root().join("missing-profile");
        assert_eq!(
            observe_claude_email(&store, &id, None, &[&missing]).unwrap(),
            None
        );
        // A symlinked profile is never followed, even to a valid email.
        #[cfg(unix)]
        {
            let elsewhere = store.root().join("elsewhere");
            profile(&elsewhere, "fixture@example.invalid");
            let linked = store.root().join("linked-profile");
            private::directory(&linked).unwrap();
            std::os::unix::fs::symlink(elsewhere.join(".claude.json"), linked.join(".claude.json"))
                .unwrap();
            assert_eq!(
                observe_claude_email(&store, &id, None, &[&linked]).unwrap(),
                None
            );
        }
        let account = store.account(&id).unwrap();
        assert_eq!(account.email, None);
        assert_eq!(account.name(), account.fixed_name());
        assert!(account.fixed_name().starts_with("claude/a_"));
    }

    #[test]
    fn profile_email_becomes_the_display_name_and_keeps_the_plan() {
        let (_directory, store, id) = fixture();
        let empty = store.root().join("launch-config");
        let home = store.root().join("launch-home");
        private::directory(&empty).unwrap();
        profile(&home, "fixture@example.invalid");
        let before = store.account(&id).unwrap();
        assert_eq!(
            observe_claude_email(&store, &id, None, &[&empty, &home])
                .unwrap()
                .as_deref(),
            Some("fixture@example.invalid")
        );
        let account = store.account(&id).unwrap();
        assert_eq!(account.name(), "fixture@example.invalid");
        assert_eq!(account.label, before.label);
        assert_eq!(account.subscription, "Personal Max");
        assert!(
            !serde_json::to_string(&account)
                .unwrap()
                .contains("SYNTHETIC_PRIVATE_VALUE")
        );
    }

    #[test]
    fn invalid_reported_email_falls_back_to_the_profile_and_never_fails() {
        let (_directory, store, id) = fixture();
        let home = store.root().join("launch-home");
        profile(&home, "fixture@example.invalid");
        let long = format!("{}@example.invalid", "x".repeat(320));
        for reported in [
            "",
            " fixture@example.invalid",
            "no-at-sign",
            "x\n@example.invalid",
            long.as_str(),
        ] {
            assert_eq!(
                observe_claude_email(&store, &id, Some(reported), &[&home])
                    .unwrap()
                    .as_deref(),
                Some("fixture@example.invalid"),
                "{reported:?}"
            );
        }
        assert_eq!(
            observe_claude_email(&store, &id, Some("startup@example.invalid"), &[&home])
                .unwrap()
                .as_deref(),
            Some("startup@example.invalid")
        );
        assert_eq!(
            store.account(&id).unwrap().email.as_deref(),
            Some("startup@example.invalid")
        );
    }
}

#[cfg(test)]
mod auth_custody_tests {
    use super::*;
    use base64::Engine;

    fn synthetic_codex_auth(account: &str, access: &str) -> Vec<u8> {
        let claims = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&serde_json::json!({
                "sub": "synthetic-user",
                "https://api.openai.com/auth": {"chatgpt_account_id": account}
            }))
            .unwrap(),
        );
        serde_json::to_vec(&serde_json::json!({
            "auth_mode": "chatgpt",
            "tokens": {
                "id_token": format!("synthetic.{claims}.signature"),
                "access_token": access,
                "refresh_token": "synthetic-refresh",
                "account_id": account,
            },
        }))
        .unwrap()
    }

    #[test]
    fn stopped_codex_login_can_discard_a_different_identity_only_with_unchanged_saved_auth() {
        let directory = tempfile::tempdir().unwrap();
        let base = xcb_core::canonical(directory.path()).unwrap();
        let store = Store::open(&base.join("state")).unwrap();
        let account = store
            .add_account(Provider::Codex, "ChatGPT", 1, None)
            .unwrap();
        let saved = synthetic_codex_auth("original-account", "original-access");
        let other = synthetic_codex_auth("different-account", "other-access");
        let target = codex_auth_path(&store, &account.id).unwrap();
        private::create(&target, &saved).unwrap();
        let run = store.prepare_probe(&account.id, None, 2).unwrap();
        let profile = store.root().join("runs/synthetic-login");
        let snapshot = snapshot_codex_auth(&store, &run, &profile).unwrap();
        private::replace(
            &snapshot.profile().join("auth.json"),
            &other,
            &crate::digest(&saved),
        )
        .unwrap();
        let metadata = private::read(&recovery_path(store.root(), &run.id), 32 * 1024).unwrap();
        let metadata_digest = crate::digest(&metadata);

        recover_codex_auth(store.root(), &run, &metadata_digest).unwrap();
        assert_eq!(private::read(&target, MAX_CODEX_AUTH_BYTES).unwrap(), saved);

        let concurrent = synthetic_codex_auth("original-account", "concurrent-access");
        private::replace(&target, &concurrent, &crate::digest(&saved)).unwrap();
        assert!(recover_codex_auth(store.root(), &run, &metadata_digest).is_err());
        assert_eq!(
            private::read(&target, MAX_CODEX_AUTH_BYTES).unwrap(),
            concurrent
        );
    }

    #[test]
    fn claude_unproven_login_capture_retains_account_and_artifacts() {
        let directory = tempfile::tempdir().unwrap();
        let base = xcb_core::canonical(directory.path()).unwrap();
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
        let base = xcb_core::canonical(directory.path()).unwrap();
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
