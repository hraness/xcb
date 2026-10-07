//! Automatic application admission.
//!
//! The manual `qualify-application --evidence` receipt re-proved facts about
//! the xcb build (source gates, unit and contract tests, the provider-boundary
//! fixture) once per account and model. Those facts belong to the build:
//!
//! - Release binaries come only from a verified `main` commit whose `Required`
//!   check ran the workspace tests, and carry a build-provenance attestation
//!   bound to their digest.
//! - The provider boundary of a pinned provider build is a release fact too:
//!   provider admission accepts only builds xcb's source or its reviewed
//!   `qualified-builds.json` catalog names.
//! - What can differ on this host is the kernel confinement. That is checked
//!   by a credential-free local probe (`host_boundary`), recorded once per xcb
//!   digest, provider and policy. A host where it can't be proven refuses
//!   application traffic with a reason; nothing falls back to unconfined runs.
//!
//! What remains per account is admission: one fixed harmless challenge through
//! the same zero-tool executor, under exclusive account custody, bound to the
//! tuple (account, sign-in generation, model, xcb digest, provider digest,
//! policy and configuration digests). The first `generate` for a tuple without
//! a record runs it automatically; any binding change makes the record stale
//! and the next call runs it again. Records hold the binding, the outcome and
//! a time. They never hold a prompt, the challenge nonce, or a reply.
//!
//! The owner can turn application access off globally or per account with
//! `xcb application disable [--account ID]`.

use crate::{
    Error, Result,
    application::FailureCode,
    application_qualification as qualification, digest, now_ms, private,
    process::Pin,
    store::{RunRecord, Store},
};
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    path::{Path, PathBuf},
    sync::Arc,
};
use xcb_core::{Id, Provider, models::ModelChoice, session::State};

const ACCESS_FILE: &str = "application-access.json";
const AUTO_DIR: &str = "qualification/application-auto";
const MAX_RECORD: usize = 16 * 1024;
const MAX_DISABLED_ACCOUNTS: usize = 1024;
/// A failed challenge keeps the model unavailable for this long, unless a
/// binding changes first. The challenge costs one provider turn; retrying it on
/// every call would spend quota against an account that just refused it.
pub(crate) const FAILED_RETRY_MS: u64 = 15 * 60 * 1000;
/// The fixed challenge's own deadline. The caller's request deadline starts
/// only after admission, so the first call can take up to this much longer.
pub(crate) const CHALLENGE_TIMEOUT_MS: u64 = 60_000;
/// How much longer than one challenge a waiting caller holds on before `busy`.
const LOCK_GRACE_MS: u64 = 15_000;
pub(crate) const CHALLENGE_PREFIX: &str = "xcb-application-v1:";

// ---------------------------------------------------------------------------
// Owner kill switch

/// Owner-controlled application access. Absent means on for every signed-in
/// account; the switch only ever removes access.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Access {
    pub version: u32,
    pub disabled: bool,
    pub disabled_accounts: Vec<Id>,
}

impl Access {
    pub fn load(root: &Path) -> Result<Self> {
        match private::read(&root.join(ACCESS_FILE), MAX_RECORD) {
            Ok(bytes) => {
                let access: Access = serde_json::from_slice(&bytes)?;
                if access.version != 1 || access.disabled_accounts.len() > MAX_DISABLED_ACCOUNTS {
                    return Err(Error::PrivateState);
                }
                Ok(access)
            }
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self {
                version: 1,
                ..Self::default()
            }),
            Err(error) => Err(error),
        }
    }
    /// An unreadable or malformed switch fails closed.
    pub fn allows(root: &Path, account: &Id) -> bool {
        Self::load(root).is_ok_and(|access| access.permits(account))
    }
    pub fn permits(&self, account: &Id) -> bool {
        !self.disabled && !self.disabled_accounts.contains(account)
    }
    /// Change the switch: `account: None` is global.
    pub fn set(root: &Path, account: Option<&Id>, enabled: bool) -> Result<Self> {
        let path = root.join(ACCESS_FILE);
        let previous = match private::read(&path, MAX_RECORD) {
            Ok(bytes) => Some(bytes),
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        let mut access = Self::load(root)?;
        match account {
            None => access.disabled = !enabled,
            Some(account) => {
                access.disabled_accounts.retain(|id| id != account);
                if !enabled {
                    if access.disabled_accounts.len() >= MAX_DISABLED_ACCOUNTS {
                        return Err(Error::Unavailable("too many disabled application accounts"));
                    }
                    access.disabled_accounts.push(account.clone());
                    access.disabled_accounts.sort();
                }
            }
        }
        let bytes = serde_json::to_vec_pretty(&access)?;
        match previous {
            Some(previous) => private::replace(&path, &bytes, &digest(previous))?,
            None => private::create(&path, &bytes)?,
        }
        Ok(access)
    }
}

// ---------------------------------------------------------------------------
// Host boundary (build/host fact, not per account)

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct HostBoundary {
    pub version: u32,
    pub probe: String,
    pub runtime_sha256: String,
    pub provider: Provider,
    pub os: String,
    pub arch: String,
    pub policy_sha256: String,
    pub passed: bool,
    pub observed_at_ms: u64,
}

impl HostBoundary {
    fn expected(pin: &Pin, policy_sha256: &str, passed: bool) -> Self {
        Self {
            version: 1,
            probe: probe_name().into(),
            runtime_sha256: pin.host_sha256.clone(),
            provider: pin.provider,
            os: std::env::consts::OS.into(),
            arch: std::env::consts::ARCH.into(),
            policy_sha256: policy_sha256.into(),
            passed,
            observed_at_ms: 0,
        }
    }
    fn matches(&self, pin: &Pin, policy_sha256: &str) -> bool {
        let wanted = Self::expected(pin, policy_sha256, self.passed);
        Self {
            observed_at_ms: 0,
            ..self.clone()
        } == wanted
    }
}

fn probe_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos-seatbelt"
    } else if cfg!(target_os = "linux") {
        "linux-sandbox"
    } else {
        "unsupported"
    }
}

fn auto_dir(root: &Path) -> PathBuf {
    root.join(AUTO_DIR)
}

/// How many builds' records are kept per provider or account/model, so two xcb
/// binaries sharing one state (an app's bundled copy and the installed one, or
/// old and new during an update) don't keep overwriting each other.
const KEPT_BUILDS: usize = 4;

/// Records are keyed by build so a second binary on the same state keeps its own.
fn build_key(runtime_sha256: &str, provider_sha256: &str) -> String {
    digest(format!("{runtime_sha256}:{provider_sha256}"))[..32].to_owned()
}

fn host_path(root: &Path, pin: &Pin) -> PathBuf {
    auto_dir(root).join(format!(
        "host-{}-{}.json",
        pin.provider.as_str(),
        build_key(&pin.host_sha256, "")
    ))
}

/// Best effort: drop all but the newest `KEPT_BUILDS` records sharing `prefix`.
fn prune(directory: &Path, prefix: &str) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    let mut records: Vec<_> = entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name.starts_with(prefix) && name.ends_with(".json")
        })
        .filter_map(|entry| Some((entry.metadata().ok()?.modified().ok()?, entry.path())))
        .collect();
    records.sort_by_key(|record| std::cmp::Reverse(record.0));
    for (_, path) in records.into_iter().skip(KEPT_BUILDS) {
        let _ = std::fs::remove_file(path);
    }
}

/// The cached host verdict for this exact xcb digest, provider and policy.
/// `None` means not yet probed (or probed for different bytes). Read-only.
pub(crate) fn cached_host_boundary(root: &Path, pin: &Pin, policy_sha256: &str) -> Option<bool> {
    let bytes = private::read(&host_path(root, pin), MAX_RECORD).ok()?;
    let record: HostBoundary = serde_json::from_slice(&bytes).ok()?;
    if !record.matches(pin, policy_sha256) || record.observed_at_ms > now_ms() {
        return None;
    }
    // A failure is retried after a while, so fixing the host takes effect
    // without an xcb update; a pass lasts as long as its binding.
    (record.passed || now_ms() - record.observed_at_ms < FAILED_RETRY_MS).then_some(record.passed)
}

/// Probe the host boundary once per binding, then reuse the verdict. A failed
/// or unavailable probe refuses application traffic; there is no fallback.
pub(crate) async fn host_boundary(
    root: &Path,
    pin: &Pin,
    policy_sha256: &str,
    probe: &Probe,
) -> bool {
    if let Some(verdict) = cached_host_boundary(root, pin, policy_sha256) {
        return verdict;
    }
    // Concurrent first calls share one probe; a later process reuses the record.
    static PROBING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _probing = PROBING.lock().await;
    if let Some(verdict) = cached_host_boundary(root, pin, policy_sha256) {
        return verdict;
    }
    let passed = probe(root.to_owned(), pin.provider).await;
    let record = HostBoundary {
        observed_at_ms: now_ms(),
        ..HostBoundary::expected(pin, policy_sha256, passed)
    };
    // Recording is best effort: an unrecorded verdict is probed again next time.
    if write_record(&host_path(root, pin), &record).is_ok() {
        prune(&auto_dir(root), &format!("host-{}-", pin.provider.as_str()));
    }
    passed
}

pub(crate) type Probe = Arc<
    dyn Fn(PathBuf, Provider) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send>>
        + Send
        + Sync,
>;

/// The production probe. macOS runs this binary under the provider's exact
/// Seatbelt profile; Linux reads the `xcb doctor --qualify-sandbox` receipt
/// for these bytes. Other hosts fail; there is no unconfined fallback.
pub(crate) fn native_probe() -> Probe {
    Arc::new(|root, provider| {
        Box::pin(async move {
            #[cfg(target_os = "macos")]
            {
                let _ = root;
                crate::qualification::probe::seatbelt(provider).await.passed
            }
            #[cfg(target_os = "linux")]
            {
                let _ = provider;
                crate::sandbox::linux_sandbox(&root).qualified
            }
            #[cfg(not(any(target_os = "macos", target_os = "linux")))]
            {
                let _ = (root, provider);
                false
            }
        })
    })
}

// ---------------------------------------------------------------------------
// Per-account admission

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct AutoBinding {
    pub runtime_version: String,
    pub runtime_sha256: String,
    pub provider: Provider,
    pub provider_version: String,
    pub provider_sha256: String,
    pub os: String,
    pub arch: String,
    pub policy_sha256: String,
    pub config_sha256: String,
    pub account: Id,
    pub credential_generation: String,
    pub model: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Outcome {
    Admitted,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct AutoRecord {
    version: u32,
    binding: AutoBinding,
    outcome: Outcome,
    observed_at_ms: u64,
}

/// What `--capabilities` and `generate` see for one account and model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AdmissionState {
    /// The strict manual receipt covers it.
    Qualified,
    /// The automatic challenge passed for the current binding.
    Admitted,
    /// No record for the current binding; the next generate admits it.
    Pending,
    /// The challenge failed for the current binding less than
    /// `FAILED_RETRY_MS` ago.
    Failed,
}

pub(crate) struct Context<'a> {
    pub pin: &'a Pin,
    pub account: &'a Id,
    pub model: &'a ModelChoice,
    pub policy_sha256: &'a str,
    pub config_sha256: &'a str,
}

impl Context<'_> {
    fn binding(&self, generation: String) -> AutoBinding {
        AutoBinding {
            runtime_version: env!("CARGO_PKG_VERSION").into(),
            runtime_sha256: self.pin.host_sha256.clone(),
            provider: self.pin.provider,
            provider_version: self.pin.version.clone(),
            provider_sha256: self.pin.sha256.clone(),
            os: std::env::consts::OS.into(),
            arch: std::env::consts::ARCH.into(),
            policy_sha256: self.policy_sha256.into(),
            config_sha256: self.config_sha256.into(),
            account: self.account.clone(),
            credential_generation: generation,
            model: self.model.key(),
        }
    }
}

fn account_dir(root: &Path, account: &Id) -> PathBuf {
    auto_dir(root).join("accounts").join(account.as_str())
}

fn model_prefix(model: &str) -> String {
    format!("{}-", &digest(model)[..32])
}

fn record_path(root: &Path, binding: &AutoBinding) -> PathBuf {
    account_dir(root, &binding.account).join(format!(
        "{}{}.json",
        model_prefix(&binding.model),
        build_key(&binding.runtime_sha256, &binding.provider_sha256)
    ))
}

/// Read-only: no directory, generation or record is created.
pub(crate) fn state(root: &Path, context: &Context<'_>, now: u64) -> AdmissionState {
    let Ok(Some(generation)) = qualification::read_generation(root, context.account) else {
        return AdmissionState::Pending;
    };
    let wanted = context.binding(generation);
    let Ok(bytes) = private::read(&record_path(root, &wanted), MAX_RECORD) else {
        return AdmissionState::Pending;
    };
    let Ok(record) = serde_json::from_slice::<AutoRecord>(&bytes) else {
        return AdmissionState::Pending;
    };
    if record.version != 1 || record.binding != wanted || record.observed_at_ms > now {
        return AdmissionState::Pending;
    }
    match record.outcome {
        Outcome::Admitted => AdmissionState::Admitted,
        Outcome::Failed if now - record.observed_at_ms < FAILED_RETRY_MS => AdmissionState::Failed,
        Outcome::Failed => AdmissionState::Pending,
    }
}

/// The fixed harmless challenge shared with manual qualification: a fresh
/// nonce the model must echo exactly. Returns (nonce, wanted reply, prompt).
pub(crate) fn challenge_prompt() -> (String, String, String) {
    let nonce = digest(format!(
        "{}:{}",
        crate::new_id("challenge"),
        crate::new_id("challenge")
    ));
    let wanted = format!("{CHALLENGE_PREFIX}{nonce}");
    let prompt = format!(
        "Return exactly the following text, with no quotes, explanation, or extra whitespace: {wanted}"
    );
    (nonce, wanted, prompt)
}

/// Serializes challenges per account across processes. Held from the record
/// check through publication; a second caller gets `busy` instead of a second
/// challenge.
struct ChallengeLock {
    _file: File,
}
impl ChallengeLock {
    fn try_acquire(root: &Path, account: &Id) -> std::result::Result<Self, FailureCode> {
        let directory = (|| -> Result<PathBuf> {
            private::directory(&root.join("qualification"))?;
            private::directory(&auto_dir(root))?;
            private::directory(&auto_dir(root).join("accounts"))?;
            private::directory(&account_dir(root, account))
        })()
        .map_err(|_| FailureCode::Unavailable)?;
        let path = directory.join("challenge.lock");
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(&path).map_err(|_| FailureCode::Unavailable)?;
        match file.try_lock() {
            Ok(()) => Ok(Self { _file: file }),
            Err(std::fs::TryLockError::WouldBlock) => Err(FailureCode::Busy),
            Err(_) => Err(FailureCode::Unavailable),
        }
    }
}

/// Ensure the account/model is admitted for the current binding, running the
/// fixed challenge if needed. Never returns a payload; failures are closed codes.
///
/// `challenge` runs the prompt inside the reserved, authenticated `run` and
/// must settle it: production passes the shared application executor, tests a
/// synthetic provider.
pub(crate) async fn ensure<F, Fut>(
    store: &Arc<Store>,
    context: &Context<'_>,
    challenge: F,
    cancel: &tokio::sync::watch::Receiver<bool>,
) -> std::result::Result<AdmissionState, FailureCode>
where
    F: FnOnce(RunRecord, String) -> Fut,
    Fut: std::future::Future<
            Output = std::result::Result<String, crate::application::GenerateFailure>,
        >,
{
    match state(store.root(), context, now_ms()) {
        AdmissionState::Admitted => return Ok(AdmissionState::Admitted),
        AdmissionState::Failed => return Err(FailureCode::Unavailable),
        AdmissionState::Qualified | AdmissionState::Pending => {}
    }
    // Another caller's challenge for this account finishes within its own
    // deadline; wait for it rather than starting a second one.
    let wait_until = std::time::Instant::now()
        + std::time::Duration::from_millis(CHALLENGE_TIMEOUT_MS + LOCK_GRACE_MS);
    let _lock = loop {
        match ChallengeLock::try_acquire(store.root(), context.account) {
            Ok(lock) => break lock,
            Err(FailureCode::Busy) if std::time::Instant::now() < wait_until => {
                if *cancel.borrow() {
                    return Err(FailureCode::Cancelled);
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            Err(code) => return Err(code),
        }
    };
    // The owner may have switched access off while we waited.
    if !Access::allows(store.root(), context.account) {
        return Err(FailureCode::Unavailable);
    }
    // Another caller may have finished while we waited for the lock.
    match state(store.root(), context, now_ms()) {
        AdmissionState::Admitted => return Ok(AdmissionState::Admitted),
        AdmissionState::Failed => return Err(FailureCode::Unavailable),
        AdmissionState::Qualified | AdmissionState::Pending => {}
    }
    if *cancel.borrow() {
        return Err(FailureCode::Cancelled);
    }
    let run = store
        .prepare_probe(context.account, Some(context.model.clone()), now_ms())
        .map_err(|error| match error {
            Error::Conflict(_) => FailureCode::Busy,
            _ => FailureCode::Unavailable,
        })?;
    let generation = match qualification::ensure_generation(store, &run) {
        Ok(generation) => generation.generation,
        Err(_) => {
            // The generation write is the only effect so far; its tool receipt
            // stays if it started, which keeps the account held for recovery.
            return Err(
                if store.require_settled_tools(&run).is_ok()
                    && store.settle(&run, State::Failed, now_ms()).is_ok()
                {
                    FailureCode::Unavailable
                } else {
                    FailureCode::CustodyUnproven
                },
            );
        }
    };
    let binding = context.binding(generation);
    let (_, wanted, prompt) = challenge_prompt();
    let reply = challenge(run, prompt).await;
    let outcome = match reply {
        Ok(text) if text == wanted => Outcome::Admitted,
        Ok(_) => Outcome::Failed,
        Err(failure) => match failure.code {
            // The model answered, but past the challenge's tiny output bound.
            FailureCode::OutputLimit => Outcome::Failed,
            // Transient (including provider errors such as rate limits) or
            // local: record nothing so the next call retries.
            code => return Err(code),
        },
    };
    publish(store, context, &binding, outcome)?;
    match outcome {
        Outcome::Admitted => Ok(AdmissionState::Admitted),
        Outcome::Failed => Err(FailureCode::Unavailable),
    }
}

/// Publish under a fresh exclusive lease, rejecting an intervening sign-in.
fn publish(
    store: &Arc<Store>,
    context: &Context<'_>,
    binding: &AutoBinding,
    outcome: Outcome,
) -> std::result::Result<(), FailureCode> {
    let run = store
        .prepare_probe(context.account, Some(context.model.clone()), now_ms())
        .map_err(|error| match error {
            Error::Conflict(_) => FailureCode::Busy,
            _ => FailureCode::Unavailable,
        })?;
    let mut started = false;
    let result: Result<()> = (|| {
        let current = qualification::read_generation(store.root(), context.account)?;
        if current.as_deref() != Some(binding.credential_generation.as_str()) {
            return Err(Error::Conflict(
                "account sign-in changed during application admission",
            ));
        }
        let record = AutoRecord {
            version: 1,
            binding: binding.clone(),
            outcome,
            observed_at_ms: now_ms(),
        };
        let bytes = serde_json::to_vec(&record)?;
        store.begin_tool(
            &run,
            "xcb_application_admission",
            "host_admission",
            &digest(&bytes),
        )?;
        started = true;
        write_bytes(&record_path(store.root(), binding), &bytes)?;
        store.settle_tool(&run, "xcb_application_admission")?;
        prune(
            &account_dir(store.root(), context.account),
            &model_prefix(&binding.model),
        );
        Ok(())
    })();
    match result {
        Ok(()) => store
            .settle(&run, State::Idle, now_ms())
            .map_err(|_| FailureCode::CustodyUnproven),
        Err(_) if started => Err(FailureCode::CustodyUnproven),
        Err(_) => {
            store
                .settle(&run, State::Failed, now_ms())
                .map_err(|_| FailureCode::CustodyUnproven)?;
            Err(FailureCode::Unavailable)
        }
    }
}

fn write_record<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    write_bytes(path, &serde_json::to_vec(value)?)
}

fn write_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().ok_or(Error::PrivateState)?;
    // Create the private chain below the state root.
    let mut chain = Vec::new();
    let mut cursor = parent;
    while !cursor.exists() {
        chain.push(cursor.to_owned());
        cursor = cursor.parent().ok_or(Error::PrivateState)?;
    }
    for directory in chain.into_iter().rev() {
        private::directory(&directory)?;
    }
    match private::read(path, MAX_RECORD) {
        Ok(previous) if previous == bytes => Ok(()),
        Ok(previous) => private::replace(path, bytes, &digest(previous)),
        Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            match private::create(path, bytes) {
                Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    let previous = private::read(path, MAX_RECORD)?;
                    private::replace(path, bytes, &digest(previous))
                }
                other => other,
            }
        }
        Err(error) => Err(error),
    }
}

// Synthetic store, pin, probe and provider only: nothing here launches a
// provider or reaches a real account.
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::application::{
        self, AdmissionHooks, GenerateFailure, GenerateOutcome, GenerateRequest, GenerateResponse,
        capabilities_with, configuration_digest, generate_with, policy_digest,
    };
    use crate::application_qualification::{Binding, Expected, tests as receipts};
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };
    use tokio::sync::watch;
    use xcb_core::models::Mode;
    use xcb_core::policy::{EffectState, Terminal, TurnFacts};

    const SECRET_PROMPT: &str = "application-only secret prompt fixture";
    const SECRET_REPLY: &str = "application-only secret reply fixture";

    #[derive(Clone, Copy, PartialEq)]
    enum Answer {
        Echo,
        Wrong,
        Transient,
        ProviderError,
    }

    struct Harness {
        _temp: tempfile::TempDir,
        store: Arc<Store>,
        account: Id,
        model: ModelChoice,
        pin: Arc<Mutex<Pin>>,
        manual: Arc<Mutex<bool>>,
        host: Arc<Mutex<bool>>,
        answer: Arc<Mutex<Answer>>,
        challenges: Arc<AtomicUsize>,
        requests: Arc<AtomicUsize>,
        probes: Arc<AtomicUsize>,
        delay_ms: u64,
    }

    impl Harness {
        fn new() -> Self {
            Self::with_delay(0)
        }
        fn with_delay(delay_ms: u64) -> Self {
            let temp = tempfile::tempdir().unwrap();
            let store = Arc::new(
                Store::open(&xcb_core::canonical(temp.path()).unwrap().join("state")).unwrap(),
            );
            let account = store
                .add_account(Provider::Claude, "Synthetic", 1, None)
                .unwrap()
                .id;
            crate::auth::store_token(&store, &account, b"sk-ant-oat01-synthetic_fixture_token")
                .unwrap();
            let model = ModelChoice {
                provider: Provider::Claude,
                id: Id::new("synthetic-model").unwrap(),
                label: "Synthetic".into(),
                mode: Mode::Fixed,
                resolved: None,
                effort: None,
                observed_at_ms: now_ms(),
            };
            store
                .set_account_models(&account, std::slice::from_ref(&model))
                .unwrap();
            let pin = Pin {
                provider: Provider::Claude,
                executable: store.root().join("must-never-launch"),
                sha256: "2".repeat(64),
                version: "synthetic-provider-1".into(),
                host_sha256: "1".repeat(64),
                observed_at_ms: now_ms(),
            };
            Self {
                _temp: temp,
                store,
                account,
                model,
                pin: Arc::new(Mutex::new(pin)),
                manual: Arc::new(Mutex::new(false)),
                host: Arc::new(Mutex::new(true)),
                answer: Arc::new(Mutex::new(Answer::Echo)),
                challenges: Arc::default(),
                requests: Arc::default(),
                probes: Arc::default(),
                delay_ms,
            }
        }

        fn hooks(&self) -> AdmissionHooks {
            let pin = self.pin.clone();
            let manual = self.manual.clone();
            let host = self.host.clone();
            let probes = self.probes.clone();
            let answer = self.answer.clone();
            let challenges = self.challenges.clone();
            let requests = self.requests.clone();
            let delay = self.delay_ms;
            AdmissionHooks {
                pin: Arc::new(move |_, provider| {
                    let pin = pin.lock().unwrap().clone();
                    (pin.provider == provider).then_some(pin)
                }),
                receipt: Arc::new(move |store, pin, account, observed| {
                    if !*manual.lock().unwrap() {
                        return None;
                    }
                    let keys: Vec<_> = observed.iter().map(ModelChoice::key).collect();
                    receipts::load_synthetic(
                        store.root(),
                        &Expected {
                            pin,
                            account,
                            policy_sha256: &policy_digest(),
                            config_sha256: &configuration_digest(),
                            observed_models: &keys,
                        },
                        now_ms(),
                    )
                    .ok()
                }),
                probe: Arc::new(move |_, _| {
                    probes.fetch_add(1, Ordering::SeqCst);
                    let passed = *host.lock().unwrap();
                    Box::pin(async move { passed })
                }),
                execute: Arc::new(move |store, request, _, id, run, _, _, _| {
                    let answer = *answer.lock().unwrap();
                    let challenge = request
                        .prompt
                        .rsplit_once(": ")
                        .map(|(_, wanted)| wanted.to_owned())
                        .filter(|wanted| wanted.starts_with(CHALLENGE_PREFIX));
                    if challenge.is_some() {
                        challenges.fetch_add(1, Ordering::SeqCst);
                    } else {
                        requests.fetch_add(1, Ordering::SeqCst);
                    }
                    Box::pin(async move {
                        tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                        // Same settlement contract as the real executor.
                        let facts = TurnFacts {
                            terminal: Terminal::Completed,
                            joined: true,
                            effects: EffectState::None,
                            pending_attention: false,
                            failure: None,
                        };
                        store.settle_application(&run, &facts, now_ms()).unwrap();
                        let text = match (challenge, answer) {
                            (_, Answer::Transient) => {
                                return Err(GenerateFailure::unstarted(FailureCode::Deadline));
                            }
                            (_, Answer::ProviderError) => {
                                return Err(GenerateFailure::unstarted(FailureCode::ProviderError));
                            }
                            (Some(wanted), Answer::Echo) => wanted,
                            (Some(_), Answer::Wrong) => "not the challenge".into(),
                            (None, _) => SECRET_REPLY.into(),
                        };
                        Ok(GenerateResponse {
                            version: 1,
                            status: "completed",
                            request_id: id.to_string(),
                            account: request.account.clone(),
                            model: request.model.clone(),
                            text,
                            outcome: GenerateOutcome {
                                terminal: "completed",
                                joined: true,
                                effects: "none",
                            },
                        })
                    })
                }),
            }
        }

        fn request(&self) -> GenerateRequest {
            GenerateRequest {
                version: 1,
                account: self.account.clone(),
                model: self.model.key(),
                prompt: SECRET_PROMPT.into(),
                timeout_ms: 1000,
                max_output_bytes: 4096,
            }
        }

        async fn generate(&self) -> std::result::Result<GenerateResponse, GenerateFailure> {
            let (_tx, cancel) = watch::channel(false);
            generate_with(self.store.clone(), self.request(), cancel, &self.hooks()).await
        }

        fn capabilities(&self) -> serde_json::Value {
            let value =
                serde_json::to_value(capabilities_with(&self.store, &self.hooks()).unwrap())
                    .unwrap();
            assert_eq!(value["version"], 1);
            value
        }
        fn row(&self) -> serde_json::Value {
            self.capabilities()["accounts"][0].clone()
        }

        fn idle(&self) {
            assert!(self.store.unsettled_runs().unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn first_generate_admits_automatically_then_serves() {
        let harness = Harness::new();
        let row = harness.row();
        assert_eq!(row["available"], true, "{row}");
        assert_eq!(row["reason"], serde_json::Value::Null);
        assert_eq!(row["admission"], "pending");
        assert_eq!(row["models"][0]["admission"], "pending");

        let response = harness.generate().await.unwrap();
        assert_eq!(response.text, SECRET_REPLY);
        assert_eq!(harness.probes.load(Ordering::SeqCst), 1);
        assert_eq!(harness.challenges.load(Ordering::SeqCst), 1);
        assert_eq!(harness.requests.load(Ordering::SeqCst), 1);
        harness.idle();

        let row = harness.row();
        assert_eq!(row["available"], true);
        assert_eq!(row["admission"], "admitted");
        assert_eq!(row["models"][0]["admission"], "admitted");

        // Admitted tuples serve directly: no second probe or challenge.
        harness.generate().await.unwrap();
        assert_eq!(harness.probes.load(Ordering::SeqCst), 1);
        assert_eq!(harness.challenges.load(Ordering::SeqCst), 1);
        assert_eq!(harness.requests.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn any_binding_change_readmits_automatically() {
        let harness = Harness::new();
        harness.generate().await.unwrap();
        let changes: [fn(&mut Pin); 3] = [
            // xcb self-update: the running binary's digest changes.
            |pin| pin.host_sha256 = "3".repeat(64),
            // Provider update: new bytes and version.
            |pin| {
                pin.sha256 = "4".repeat(64);
                pin.version = "synthetic-provider-2".into();
            },
            |pin| pin.version = "synthetic-provider-3".into(),
        ];
        for (index, change) in changes.into_iter().enumerate() {
            change(&mut harness.pin.lock().unwrap());
            assert_eq!(harness.row()["admission"], "pending", "change {index}");
            harness.generate().await.unwrap();
            assert_eq!(harness.challenges.load(Ordering::SeqCst), index + 2);
            assert_eq!(harness.row()["admission"], "admitted");
        }
        // The host boundary is a build fact: a new xcb digest re-probes it,
        // a provider update does not (the challenge covers the provider).
        assert_eq!(harness.probes.load(Ordering::SeqCst), 2);

        // A sign-in rotation (new credential generation) readmits as well.
        let run = harness
            .store
            .prepare_probe(&harness.account, None, now_ms())
            .unwrap();
        qualification::rotate_generation(&harness.store, &run).unwrap();
        harness.store.settle(&run, State::Idle, now_ms()).unwrap();
        assert_eq!(harness.row()["admission"], "pending");
        harness.generate().await.unwrap();
        assert_eq!(harness.challenges.load(Ordering::SeqCst), 5);
    }

    #[tokio::test]
    async fn a_failed_challenge_leaves_the_account_unavailable_with_a_reason() {
        let harness = Harness::new();
        *harness.answer.lock().unwrap() = Answer::Wrong;
        let failure = harness.generate().await.unwrap_err();
        assert_eq!(failure.code, FailureCode::Unavailable);
        assert_eq!(harness.requests.load(Ordering::SeqCst), 0);
        harness.idle();
        let row = harness.row();
        assert_eq!(row["available"], false);
        assert_eq!(row["reason"], "admission_failed");
        assert_eq!(row["admission"], serde_json::Value::Null);
        assert_eq!(row["models"], serde_json::json!([]));
        // No retry storm: the failure holds until it ages out or a binding changes.
        *harness.answer.lock().unwrap() = Answer::Echo;
        assert_eq!(
            harness.generate().await.unwrap_err().code,
            FailureCode::Unavailable
        );
        assert_eq!(harness.challenges.load(Ordering::SeqCst), 1);
        harness.pin.lock().unwrap().version = "synthetic-provider-2".into();
        harness.generate().await.unwrap();
        assert_eq!(harness.challenges.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_transient_challenge_failure_records_nothing() {
        let harness = Harness::new();
        // A deadline, and a provider error such as a rate limit, are transient.
        for (answer, code) in [
            (Answer::Transient, FailureCode::Deadline),
            (Answer::ProviderError, FailureCode::ProviderError),
        ] {
            *harness.answer.lock().unwrap() = answer;
            assert_eq!(harness.generate().await.unwrap_err().code, code);
            harness.idle();
            assert_eq!(harness.row()["admission"], "pending");
        }
        *harness.answer.lock().unwrap() = Answer::Echo;
        harness.generate().await.unwrap();
        assert_eq!(harness.challenges.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn two_builds_sharing_state_keep_their_own_admission() {
        let harness = Harness::new();
        let first = harness.pin.lock().unwrap().clone();
        harness.generate().await.unwrap();
        // A second xcb (say an app's bundled copy) on the same state.
        harness.pin.lock().unwrap().host_sha256 = "5".repeat(64);
        harness.generate().await.unwrap();
        assert_eq!(harness.challenges.load(Ordering::SeqCst), 2);
        // Switching back finds the first build's record intact.
        *harness.pin.lock().unwrap() = first;
        assert_eq!(harness.row()["admission"], "admitted");
        harness.generate().await.unwrap();
        assert_eq!(harness.challenges.load(Ordering::SeqCst), 2);

        // Records for old builds are bounded.
        for index in 0..(KEPT_BUILDS + 3) {
            harness.pin.lock().unwrap().host_sha256 = format!("{index:x}").repeat(64)[..64].into();
            harness.generate().await.unwrap();
        }
        let records = std::fs::read_dir(account_dir(harness.store.root(), &harness.account))
            .unwrap()
            .filter(|entry| {
                entry
                    .as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".json")
            })
            .count();
        assert!(records <= KEPT_BUILDS, "{records} records");
    }

    #[tokio::test]
    async fn an_unproven_host_boundary_refuses_without_a_challenge() {
        let harness = Harness::new();
        *harness.host.lock().unwrap() = false;
        assert_eq!(
            harness.generate().await.unwrap_err().code,
            FailureCode::Unavailable
        );
        assert_eq!(harness.challenges.load(Ordering::SeqCst), 0);
        assert_eq!(harness.requests.load(Ordering::SeqCst), 0);
        let row = harness.row();
        assert_eq!(row["available"], false);
        assert_eq!(row["reason"], "sandbox_unproven");
        // The verdict is cached for this binding: no re-probe on every call.
        let _ = harness.generate().await;
        assert_eq!(harness.probes.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_first_generates_run_exactly_one_challenge() {
        let harness = Arc::new(Harness::with_delay(300));
        let calls: Vec<_> = (0..4)
            .map(|_| {
                let harness = harness.clone();
                tokio::spawn(async move { harness.generate().await })
            })
            .collect();
        let mut served = 0;
        for call in calls {
            match call.await.unwrap() {
                Ok(_) => served += 1,
                Err(failure) => assert_eq!(failure.code, FailureCode::Busy),
            }
        }
        assert!(served >= 1);
        assert_eq!(harness.challenges.load(Ordering::SeqCst), 1);
        assert_eq!(harness.probes.load(Ordering::SeqCst), 1);
        harness.idle();
    }

    #[tokio::test]
    async fn the_owner_kill_switch_turns_access_off_globally_or_per_account() {
        let harness = Harness::new();
        let root = harness.store.root();
        assert!(Access::allows(root, &harness.account));

        Access::set(root, None, false).unwrap();
        assert_eq!(
            harness.generate().await.unwrap_err().code,
            FailureCode::Unavailable
        );
        let row = harness.row();
        assert_eq!(row["available"], false);
        assert_eq!(row["reason"], "application_disabled");
        // Never `pending` beside a refusal, on the account or its models.
        assert_eq!(row["admission"], serde_json::Value::Null);
        assert_eq!(row["models"][0]["admission"], serde_json::Value::Null);
        assert_eq!(row["models"][0]["key"], harness.model.key());
        assert_eq!(harness.capabilities()["supported"], false);
        assert_eq!(harness.challenges.load(Ordering::SeqCst), 0);
        Access::set(root, None, true).unwrap();
        harness.generate().await.unwrap();

        // Per account, and it also blocks an already admitted tuple.
        Access::set(root, Some(&harness.account), false).unwrap();
        assert!(!Access::allows(root, &harness.account));
        assert!(Access::allows(root, &Id::new("a_other").unwrap()));
        assert_eq!(
            harness.generate().await.unwrap_err().code,
            FailureCode::Unavailable
        );
        let row = harness.row();
        assert_eq!(row["reason"], "application_disabled");
        assert_eq!(row["admission"], serde_json::Value::Null);
        assert_eq!(harness.requests.load(Ordering::SeqCst), 1);
        Access::set(root, Some(&harness.account), true).unwrap();
        harness.generate().await.unwrap();
        assert_eq!(harness.challenges.load(Ordering::SeqCst), 1);

        // An unreadable switch fails closed.
        std::fs::write(root.join(ACCESS_FILE), b"{").unwrap();
        assert!(!Access::allows(root, &harness.account));
        assert_eq!(harness.row()["reason"], "application_disabled");
    }

    #[tokio::test]
    async fn the_strict_manual_receipt_is_still_accepted() {
        let harness = Harness::new();
        let pin = harness.pin.lock().unwrap().clone();
        let binding = Binding {
            runtime_version: env!("CARGO_PKG_VERSION").into(),
            runtime_sha256: pin.host_sha256.clone(),
            provider: pin.provider,
            provider_version: pin.version.clone(),
            provider_sha256: pin.sha256.clone(),
            os: std::env::consts::OS.into(),
            arch: std::env::consts::ARCH.into(),
            policy_sha256: policy_digest(),
            config_sha256: configuration_digest(),
            account: harness.account.clone(),
            credential_generation: "5".repeat(64),
            models: vec![harness.model.key()],
        };
        receipts::write_synthetic_receipt(harness.store.root(), &binding, now_ms());
        *harness.manual.lock().unwrap() = true;
        let row = harness.row();
        assert_eq!(row["admission"], "qualified");
        assert_eq!(row["models"][0]["admission"], "qualified");
        assert_eq!(row["qualification"]["runtimeDigest"], pin.host_sha256);
        assert_eq!(harness.row()["available"], true);
        harness.generate().await.unwrap();
        // The receipt admits on its own: no probe and no automatic challenge.
        assert_eq!(harness.probes.load(Ordering::SeqCst), 0);
        assert_eq!(harness.challenges.load(Ordering::SeqCst), 0);
        // It also outranks a host whose automatic probe would fail.
        *harness.host.lock().unwrap() = false;
        harness.generate().await.unwrap();
        assert_eq!(harness.row()["reason"], serde_json::Value::Null);
    }

    #[tokio::test]
    async fn admission_stores_no_prompt_reply_or_challenge_text() {
        let harness = Harness::new();
        harness.generate().await.unwrap();
        harness.generate().await.unwrap();
        let mut files = vec![harness.store.root().to_owned()];
        let mut scanned = 0;
        while let Some(path) = files.pop() {
            if path.is_dir() {
                files.extend(std::fs::read_dir(&path).unwrap().map(|e| e.unwrap().path()));
                continue;
            }
            let bytes = std::fs::read(&path).unwrap();
            scanned += 1;
            for needle in [
                SECRET_PROMPT,
                SECRET_REPLY,
                CHALLENGE_PREFIX,
                "Return exactly",
            ] {
                assert!(
                    !bytes.windows(needle.len()).any(|w| w == needle.as_bytes()),
                    "{needle} stored in {}",
                    path.display()
                );
            }
        }
        assert!(scanned > 0);
        // The admission record holds only the binding and outcome.
        let record = std::fs::read_dir(account_dir(harness.store.root(), &harness.account))
            .unwrap()
            .map(|e| e.unwrap().path())
            .find(|p| p.extension().is_some_and(|e| e == "json"))
            .unwrap();
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(record).unwrap()).unwrap();
        let mut keys: Vec<_> = value.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(keys, ["binding", "observedAtMs", "outcome", "version"]);
    }

    #[test]
    fn capability_rows_add_fields_without_repurposing_old_ones() {
        let value = serde_json::to_value(application::empty_capabilities()).unwrap();
        assert_eq!(value["version"], 1);
        for key in [
            "supported",
            "zeroTools",
            "zeroHooks",
            "ephemeral",
            "limits",
            "accounts",
        ] {
            assert!(value.get(key).is_some(), "{key}");
        }
        let harness = Harness::new();
        let row = harness.row();
        // Every version-one field keeps its name and type; admission is new.
        for key in [
            "id",
            "name",
            "provider",
            "enabled",
            "busy",
            "connected",
            "runtimeAdmitted",
            "available",
            "reason",
            "models",
            "admission",
        ] {
            assert!(row.get(key).is_some(), "{key}: {row}");
        }
        assert!(row["available"].is_boolean() && row["runtimeAdmitted"].is_boolean());
        let model = &row["models"][0];
        for key in ["key", "label", "observedAtMs", "admission"] {
            assert!(model.get(key).is_some(), "{key}: {model}");
        }
        assert!(model["observedAtMs"].is_u64());
    }

    #[test]
    fn a_large_catalog_lists_fewer_pending_models_instead_of_failing() {
        let harness = Harness::new();
        let catalog: Vec<_> = (0..100)
            .map(|index| ModelChoice {
                id: Id::new(format!("synthetic-{index}")).unwrap(),
                ..harness.model.clone()
            })
            .collect();
        harness
            .store
            .set_account_models(&harness.account, &catalog)
            .unwrap();
        let row = harness.row();
        assert_eq!(row["available"], true, "{row}");
        let models = row["models"].as_array().unwrap();
        assert_eq!(models.len(), 64);
        // The store's catalog order is kept.
        let stored = harness.store.account_models(&harness.account).unwrap();
        let listed: Vec<_> = models.iter().map(|model| model["key"].clone()).collect();
        let expected: Vec<_> = stored[..64]
            .iter()
            .map(|model| serde_json::Value::from(model.key()))
            .collect();
        assert_eq!(listed, expected);
    }
}
