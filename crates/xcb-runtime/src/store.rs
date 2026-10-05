use crate::{Error, Result, digest, new_id, private};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard},
    time::Duration,
};
use xcb_core::{
    Id, Provider, label,
    models::ModelChoice,
    session::{Message, Session, State},
    usage::{Counters, QuotaPoint},
};

const MAX_ACCOUNTS: i64 = 128;
/// One provider catalog (fallback or one account's own) holds at most this many rows.
const MAX_ACCOUNT_MODELS: usize = 4096;
/// Every account's own catalog together holds at most this many rows.
const MAX_CATALOG_ROWS: usize = 16_384;

/// Observed models per account, with the provider-wide catalog as the
/// fallback for accounts that have not reported their own list yet.
#[derive(Debug, Clone, Default)]
pub struct ModelCatalog {
    accounts: BTreeMap<Id, Provider>,
    fallback: Vec<ModelChoice>,
    observed: BTreeMap<Id, Vec<ModelChoice>>,
}

impl ModelCatalog {
    /// The models `account` can be routed with: its own observation, else
    /// the provider-wide fallback for its provider.
    pub fn for_account(&self, account: &Id, provider: Provider) -> &[ModelChoice] {
        match self.observed.get(account) {
            Some(own) if !own.is_empty() => own,
            _ => {
                let start = self.fallback.partition_point(|m| m.provider < provider);
                let end = self.fallback.partition_point(|m| m.provider <= provider);
                &self.fallback[start..end]
            }
        }
    }
    /// True when `account` of `provider` has `model` in its catalog.
    pub fn offers(&self, account: &Id, provider: Provider, model: &ModelChoice) -> bool {
        model.provider == provider
            && self
                .for_account(account, provider)
                .iter()
                .any(|choice| choice.key() == model.key())
    }
    /// Accounts whose catalog contains `model`, in id order.
    pub fn accounts_offering(&self, model: &ModelChoice) -> Vec<Id> {
        self.accounts
            .iter()
            .filter(|(id, provider)| self.offers(id, **provider, model))
            .map(|(id, _)| id.clone())
            .collect()
    }
    fn fallback_reachable(&self, provider: Provider) -> bool {
        let mut accounts = self.accounts.iter().filter(|(_, p)| **p == provider);
        let mut any = false;
        let reachable = accounts.any(|(id, _)| {
            any = true;
            self.observed.get(id).is_none_or(Vec::is_empty)
        });
        reachable || !any
    }
    /// One row per model key across every reachable catalog, ordered by
    /// provider then key. The provider-wide fallback is left out only when
    /// every account of that provider reported its own list.
    pub fn union(&self) -> Vec<ModelChoice> {
        let mut rows: BTreeMap<(Provider, String), ModelChoice> = BTreeMap::new();
        for choice in &self.fallback {
            if self.fallback_reachable(choice.provider) {
                rows.entry((choice.provider, choice.key()))
                    .or_insert_with(|| choice.clone());
            }
        }
        for (account, own) in &self.observed {
            if !self.accounts.contains_key(account) {
                continue;
            }
            for choice in own {
                rows.entry((choice.provider, choice.key()))
                    .or_insert_with(|| choice.clone());
            }
        }
        rows.into_values().collect()
    }
}
const MAX_SESSIONS: i64 = 10_000;
const MAX_MESSAGES: i64 = 10_000;
pub(crate) const AUTHENTICATION_REQUIRED: &str = "account authentication or subscription access failed; restore account access (administrator action may be needed), then reconnect before running tasks";

#[path = "store_overview.rs"]
mod overview;
#[path = "store_recovery.rs"]
mod recovery;
pub use crate::retry::AccountRecovery;

#[path = "store_claude_recovery.rs"]
mod claude_recovery;
pub use claude_recovery::ClaudeAuthRecoveryInfo;

#[path = "host_contract.rs"]
pub mod host_contract;

fn authentication_required_from(db: &Connection, account: &Id) -> Result<bool> {
    let available: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='account_auth_failures')",
        [], |row| row.get(0),
    )?;
    if !available {
        return Ok(false);
    }
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM account_auth_failures WHERE account=?1)",
        [account.as_str()],
        |row| row.get(0),
    )?)
}

/// A terminal report is committed in the same transaction as custody release.
/// Its transcript boundary prevents a later turn from being mistaken for the
/// managed dispatch that is being reconciled after a supervisor restart.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SettledOutcome {
    version: u32,
    run: Id,
    session: Id,
    input: Id,
    input_sequence: u64,
    message_count: u64,
    session_revision: u64,
    /// Some(true) after successful protocol submission; Some(false) only if
    /// submission was never attempted. Failed attempts and legacy are unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    prompt_submission: Option<bool>,
    outcome: crate::runner::Outcome,
}

fn validate_outcome(outcome: &crate::runner::Outcome) -> Result<()> {
    xcb_core::bounded_text(&outcome.text, xcb_core::MAX_TEXT_BYTES)?;
    if !outcome.facts.joined
        || outcome.facts.effects == xcb_core::policy::EffectState::Uncertain
        || matches!(outcome.state, State::Working | State::Uncertain)
    {
        return Err(Error::Conflict("terminal outcome settlement is unproven"));
    }
    Ok(())
}

fn generation_pool(root: &Path, account: &Account) -> Result<Option<Id>> {
    if account.provider != Provider::Claude {
        return Ok(None);
    }
    crate::application_qualification::read_generation(root, &account.id)?
        .map(|generation| {
            let bytes = serde_json::to_vec(&("xcb-account-quota-v1", &account.id, generation))?;
            Ok(Id::new(format!("q_{}", digest(bytes)))?)
        })
        .transpose()
}

/// Provider-reported meters: the windows usage projections read.
const QUOTAS: &str = "quotas";
/// Cooldowns for usage limits without a reported reset, one synthetic
/// [`xcb_core::usage::limit_window`] per provider. Kept apart from the meters
/// so no percentage, reset, or runway projection reads a cooldown as
/// telemetry; only admission consults both.
const QUOTA_LIMITS: &str = "quota_limits";

fn quota_points_from(db: &Connection, table: &str, pool: &Id) -> Result<Vec<QuotaPoint>> {
    let mut query = db.prepare(&format!(
        "SELECT payload FROM {table} WHERE pool=?1 ORDER BY observed_at LIMIT 2049"
    ))?;
    let rows = query.query_map([pool.as_str()], |row| row.get::<_, String>(0))?;
    let mut points = Vec::new();
    for row in rows {
        let point: QuotaPoint = decode(&row?)?;
        point.validate()?;
        if &point.pool != pool {
            return Err(Error::Conflict("stored quota pool mismatch"));
        }
        points.push(point);
    }
    if points.len() > 2048 {
        return Err(xcb_core::Error::Limit("quota windows").into());
    }
    Ok(points)
}

fn quotas_from(db: &Connection, pool: &Id) -> Result<Vec<QuotaPoint>> {
    quota_points_from(db, QUOTAS, pool)
}

/// Additive table: a database written by an older xcb has none, and then
/// carries no cooldowns.
fn quota_limits_from(db: &Connection, pool: &Id) -> Result<Vec<QuotaPoint>> {
    let available: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='quota_limits')",
        [],
        |row| row.get(0),
    )?;
    if !available {
        return Ok(Vec::new());
    }
    quota_points_from(db, QUOTA_LIMITS, pool)
}

fn current_quota_points_from(
    db: &Connection,
    root: &Path,
    account: &Account,
) -> Result<Option<Vec<QuotaPoint>>> {
    // Claude binds quota to the live credential generation so a rotated
    // sign-in cannot inherit another identity's block; an absent generation
    // stays unbound. Other providers use the account's own stable pool.
    let pool = if account.provider == Provider::Claude {
        match generation_pool(root, account)? {
            Some(pool) => pool,
            None => return Ok(None),
        }
    } else {
        account.quota_pool.clone()
    };
    if pool != account.quota_pool {
        return Ok(None);
    }
    let mut points = quotas_from(db, &pool)?;
    points.extend(quota_limits_from(db, &pool)?);
    if account.provider == Provider::Claude
        && generation_pool(root, account)?.as_ref() != Some(&pool)
    {
        return Err(Error::Conflict("account credential generation changed"));
    }
    Ok(Some(points))
}

fn blocked_until_from(
    db: &Connection,
    root: &Path,
    account: &Account,
    now: u64,
) -> Result<Option<u64>> {
    Ok(
        current_quota_points_from(db, root, account)?.and_then(|points| {
            xcb_core::usage::quota_blocked_until(
                &points,
                &account.quota_pool,
                account.provider,
                now,
            )
        }),
    )
}

/// A quota meter update buffered during streaming. The account's pool is
/// bound inside the recording transaction, not at observation time.
pub(crate) struct PendingQuota {
    pub window: Id,
    pub used_percent: f64,
    pub observed_at_ms: u64,
    pub resets_at_ms: u64,
}

fn insert_quota(tx: &Transaction<'_>, table: &str, point: &QuotaPoint) -> Result<()> {
    if !store_quota(tx, table, point)? {
        return Err(Error::Conflict("conflicting quota observation"));
    }
    Ok(())
}

/// Store one quota point unless a different payload already holds the same
/// (pool, window, instant); returns false for that conflict and writes
/// nothing. Stored history is never rewritten. `table` is one of the two
/// constants above, never caller input.
fn store_quota(tx: &Transaction<'_>, table: &str, point: &QuotaPoint) -> Result<bool> {
    point.validate()?;
    let json = serde_json::to_string(point)?;
    let prior: Option<String> = tx
        .query_row(
            &format!("SELECT payload FROM {table} WHERE pool=?1 AND window=?2 AND observed_at=?3"),
            params![
                point.pool.as_str(),
                point.window.as_str(),
                sql(point.observed_at_ms)?
            ],
            |row| row.get(0),
        )
        .optional()?;
    if prior.as_ref().is_some_and(|old| old != &json) {
        return Ok(false);
    }
    tx.execute(
        &format!("INSERT OR IGNORE INTO {table} VALUES(?1,?2,?3,?4)"),
        params![
            point.pool.as_str(),
            point.window.as_str(),
            sql(point.observed_at_ms)?,
            json
        ],
    )?;
    tx.execute(&format!("DELETE FROM {table} WHERE pool=?1 AND window=?2 AND observed_at NOT IN (SELECT observed_at FROM {table} WHERE pool=?1 AND window=?2 ORDER BY observed_at DESC LIMIT 128)"), params![point.pool.as_str(), point.window.as_str()])?;
    Ok(true)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Account {
    pub id: Id,
    pub provider: Provider,
    /// Legacy display label retained for resolution compatibility only; the
    /// rendered account name is always system-derived via `name()`.
    pub label: String,
    /// Provider-reported account email, captured at import or during a probe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// Provider-reported plan when known (e.g. ChatGPT planType); otherwise
    /// the plan text supplied at creation.
    pub subscription: String,
    pub quota_pool: Id,
    pub enabled: bool,
    pub created_at_ms: u64,
}
impl Account {
    pub fn validate(&self) -> Result<()> {
        label(&self.label, 80)?;
        label(&self.subscription, 80)?;
        if let Some(email) = &self.email {
            email_label(email)?;
        }
        Ok(())
    }
    /// Fixed, system-derived display identity: the provider account email once
    /// observed, otherwise `provider/<id prefix>`. Never a user-authored label.
    pub fn name(&self) -> String {
        self.email.clone().unwrap_or_else(|| self.fixed_name())
    }
    /// The stable non-email identity; also stored as `label` for new accounts.
    pub fn fixed_name(&self) -> String {
        let short: String = self.id.as_str().chars().take(10).collect();
        format!("{}/{short}", self.provider)
    }
}

fn email_label(value: &str) -> Result<()> {
    label(value, 320)?;
    if !value.contains('@') {
        return Err(xcb_core::Error::Invalid("account email").into());
    }
    Ok(())
}

/// Identity of the xcb process instance that owns a run. Persisted on the run
/// row so sibling terminals can tell "working in another terminal" apart from
/// a genuinely unsettled run.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunOwner {
    pub instance: String,
    pub pid: u32,
}
impl RunOwner {
    /// True while the recorded owning process still exists. A signal-permission
    /// failure also proves presence; only an absent or invalid pid does not.
    pub fn alive(&self) -> bool {
        if self.pid == 0 || i32::try_from(self.pid).is_err() {
            return false;
        }
        crate::os::process_exists(self.pid) == Some(true)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunRecord {
    /// Always emitted, including for legacy records, so older strict decoders
    /// cannot release custody using recovery rules that predate auth receipts.
    #[serde(default)]
    pub custody_version: u32,
    pub id: Id,
    pub session: Option<Id>,
    pub account: Id,
    pub revision: u64,
    pub phase: String,
    pub pid: Option<u32>,
    pub created_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelChoice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<RunOwner>,
    /// A guest worker has independent custody from its host provider process.
    /// Present fields make older strict readers fail closed until reconciliation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_custody: Option<crate::command::CommandCustody>,
    /// Host tool servers are independent process groups. The launch intent is
    /// durable before spawning; older strict readers cannot release this run.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub capability_processes: BTreeMap<String, Option<u32>>,
}

impl RunRecord {
    /// Recovery is unavailable while the owning host could still be joining
    /// processes or persisting credentials. Unknown identities fail closed.
    /// A refusal because a recorded process still exists names that process
    /// and what to check; the account stays held either way.
    pub fn verify_recovery_stop(&self) -> Result<()> {
        if self.phase != "running" {
            return Err(Error::Conflict("run is not in running phase"));
        }
        let owner = self.owner.as_ref().ok_or(Error::Conflict(
            "run has no recorded owner; recovery custody cannot be proven",
        ))?;
        if owner.pid <= 1 || i32::try_from(owner.pid).is_err() {
            return Err(Error::Conflict("run owner identity is invalid"));
        }
        match crate::os::process_exists(owner.pid) {
            Some(false) => (),
            // A number that exists is never proof that this run's owner
            // exited, even if it now names another program: xcb cannot
            // tell a reused process number from the owner itself.
            Some(true) => {
                return Err(Error::guided(
                    format!(
                        "process {}, which started this run, is still running, so xcb keeps the account held. Check it with `ps -p {}`: if it is xcb, let its turn finish or quit that xcb; if it is another program, the number was reused, so restart your computer to prove the run stopped",
                        owner.pid, owner.pid
                    ),
                    format!("xcb recover {} --yes", self.id),
                ));
            }
            None => {
                return Err(Error::Conflict(
                    "run owner is still present or its stop is unproven",
                ));
            }
        }
        let pid = self
            .pid
            .filter(|pid| *pid > 1)
            .ok_or(Error::Conflict("run has no valid recorded process group"))?;
        match crate::process::prove_process_group_absent(pid) {
            Err(Error::Conflict(_)) => Err(Error::guided(
                format!(
                    "the provider's processes (process group {pid}) are still running, so xcb keeps the account held. Wait until `pgrep -g {pid}` prints nothing, or stop those processes yourself"
                ),
                format!("xcb recover {} --yes", self.id),
            )),
            proof => proof,
        }?;
        for pid in self.capability_processes.values() {
            let pid = pid.ok_or(Error::Conflict(
                "tool server launch has no recorded process group; stop cannot be proven",
            ))?;
            crate::process::prove_process_group_absent(pid)?;
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<()> {
        if !matches!(self.custody_version, 0 | 1) {
            return Err(xcb_core::Error::Invalid("run custody version").into());
        }
        if let Some(custody) = &self.command_custody {
            if self.custody_version != 1 || !matches!(self.phase.as_str(), "prepared" | "running") {
                return Err(xcb_core::Error::Invalid("command run custody").into());
            }
            validate_command_custody(&self.id, custody)?;
        }
        if !self.capability_processes.is_empty() {
            if self.custody_version != 1
                || !matches!(self.phase.as_str(), "prepared" | "running")
                || self.capability_processes.len() > 32
            {
                return Err(xcb_core::Error::Invalid("tool server custody").into());
            }
            for (server, pid) in &self.capability_processes {
                Id::new(server.clone())?;
                if pid.is_some_and(|pid| pid <= 1 || i32::try_from(pid).is_err()) {
                    return Err(xcb_core::Error::Invalid("tool server process group").into());
                }
            }
        }
        if let Some(model) = &self.model {
            model.validate()?;
        }
        if self
            .owner
            .as_ref()
            .is_some_and(|owner| owner.instance.is_empty() || owner.instance.len() > 160)
        {
            return Err(xcb_core::Error::Invalid("run owner").into());
        }
        Ok(())
    }
}

fn validate_command_custody(run_id: &Id, custody: &crate::command::CommandCustody) -> Result<()> {
    let hash = xcb_core::hex64;
    let id = custody.command_id.as_str();
    if custody.version != 1
        || custody.run_id != *run_id
        || id.len() > 80
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
        || !hash(&custody.workspace_id)
        || !hash(&custody.snapshot_sha256)
        || !hash(&custody.request_sha256)
        || !hash(&custody.backend_sha256)
        || custody.boot_id.len() != 36
        || custody.boot_id.chars().any(char::is_control)
    {
        return Err(xcb_core::Error::Invalid("command custody").into());
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageObservation {
    pub id: Id,
    pub session: Id,
    pub account: Id,
    pub model: ModelChoice,
    pub counters: Counters,
    pub at_ms: u64,
}

#[derive(Debug)]
pub(crate) struct SessionRunReceipt {
    pub run: RunRecord,
    pub lease_held: bool,
}

#[derive(Debug)]
pub(crate) struct ToolEffectRecord {
    pub call: String,
    pub operation: String,
    pub input_digest: String,
    pub settled: bool,
}

#[derive(Debug)]
pub(crate) struct SessionReceipts {
    pub run_count: u64,
    pub runs: Vec<SessionRunReceipt>,
    pub effect_count: u64,
    pub effects: BTreeMap<Id, Vec<ToolEffectRecord>>,
}

/// Exact, deterministic selectors for the local status projection. Text search
/// is deliberately absent; greppable output is a rendering concern.
#[derive(Debug, Clone, Default)]
pub(crate) struct StatusFilter {
    pub provider: Option<Provider>,
    pub account: Option<Id>,
    pub session: Option<Id>,
    pub state: Option<State>,
    /// Exact canonical workspace path; linked-worktree families are a herd
    /// concern, not a record filter.
    pub workspace: Option<String>,
    pub has_lease: bool,
    pub unsettled_effects: bool,
    pub pending_command_custody: bool,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct StatusPage {
    pub limit: u64,
    pub offset: u64,
}

#[derive(Debug)]
pub(crate) struct StatusSection<T> {
    pub matched: u64,
    pub offset: u64,
    pub records: Vec<T>,
}

#[derive(Debug)]
pub(crate) struct StatusAccount {
    pub account: Account,
    pub active_runs: u64,
    pub lease_held: bool,
    pub authentication_required: bool,
    pub remaining_percent: Option<f64>,
    pub resets_at_ms: Option<u64>,
    pub quota_blocked_until_ms: Option<u64>,
}

#[derive(Debug)]
pub(crate) struct StatusSession {
    pub session: Session,
    pub run_count: u64,
    pub unsettled_run_count: u64,
    pub effect_count: u64,
    pub unsettled_effect_count: u64,
    pub lease_held: bool,
    pub pending_command_custody: bool,
    pub capability_process_count: u64,
}

#[derive(Debug)]
pub(crate) struct StatusRun {
    pub run: RunRecord,
    pub provider: Option<Provider>,
    pub lease_held: bool,
    pub effect_count: u64,
    pub unsettled_effect_count: u64,
}

#[derive(Debug)]
pub(crate) struct StatusEffect {
    pub session: Option<Id>,
    pub account: Option<Id>,
    pub provider: Option<Provider>,
    pub run: Id,
    pub call: String,
    pub operation: String,
    pub input_digest: String,
    pub settled: bool,
}

#[derive(Debug)]
pub(crate) struct StatusTotals {
    pub accounts: u64,
    pub sessions: u64,
    pub runs: u64,
    pub tool_effects: u64,
    pub leases: u64,
    pub held_accounts: u64,
    pub unsettled_runs: u64,
    pub unsettled_effects: u64,
    pub unlinked_tool_effects: u64,
    pub pending_command_custody: u64,
}

#[derive(Debug)]
pub(crate) struct StatusSnapshot {
    pub totals: StatusTotals,
    pub accounts: StatusSection<StatusAccount>,
    pub sessions: StatusSection<StatusSession>,
    pub runs: StatusSection<StatusRun>,
    pub effects: StatusSection<StatusEffect>,
}

pub struct Store {
    root: PathBuf,
    /// Test-only count of fsync'd observability commits, proving a batch of
    /// N stream events lands in one transaction rather than N.
    #[cfg(test)]
    pub(crate) observation_commits: std::sync::atomic::AtomicUsize,
    /// Unique identity of this open handle — one per terminal process — stamped
    /// on every run this store prepares so other terminals can recognise
    /// foreign-owned live runs.
    instance: String,
    connection: Mutex<Connection>,
    /// Lazily opened managed store: session probes reuse one connection and
    /// its migration probe instead of paying a fresh open on every call.
    /// `None` means not opened yet — or the managed database absent at the
    /// last check — so a later created managed root is still discovered.
    managed: Mutex<Option<crate::managed::ManagedStore>>,
    /// Opened by `open_read_only`: the managed store is read the same way,
    /// never migrated, cleaned or waited on.
    read_only: bool,
    /// Telemetry observations the batched recorder dropped because their
    /// clock or payload contradicted stored telemetry. Diagnostic only.
    dropped_observations: std::sync::atomic::AtomicU64,
}

fn decode<T: DeserializeOwned>(text: &str) -> Result<T> {
    if text.len() > 1024 * 1024 {
        return Err(xcb_core::Error::Limit("stored record").into());
    }
    Ok(serde_json::from_str(text)?)
}
fn session_from(connection: &Connection, id: &Id) -> Result<Option<Session>> {
    let json: Option<String> = connection
        .query_row(
            "SELECT payload FROM sessions WHERE id=?1",
            [id.as_str()],
            |row| row.get(0),
        )
        .optional()?;
    json.map(|json| {
        let session: Session = decode(&json)?;
        session.validate()?;
        Ok(session)
    })
    .transpose()
}
fn state_value(state: State) -> &'static str {
    match state {
        State::Idle => "idle",
        State::Working => "working",
        State::NeedsAnswer => "needs_answer",
        State::NeedsAction => "needs_action",
        State::NeedsApproval => "needs_approval",
        State::Limited => "limited",
        State::Failed => "failed",
        State::Cancelled => "cancelled",
        State::Uncertain => "uncertain",
    }
}
fn sql(value: u64) -> Result<i64> {
    i64::try_from(value).map_err(|_| xcb_core::Error::Invalid("database integer").into())
}

fn settle_tool_in(tx: &Transaction<'_>, run: &RunRecord, call: &str) -> Result<()> {
    if tx.execute(
        "UPDATE tool_effects SET settled=1 WHERE run=?1 AND call=?2 AND settled=0",
        params![run.id.as_str(), call],
    )? != 1
    {
        return Err(Error::Conflict("tool receipt changed"));
    }
    Ok(())
}
fn append_message_in(
    tx: &Transaction<'_>,
    id: &Id,
    expected_revision: Option<u64>,
    message: &Message,
) -> Result<Session> {
    let mut session = session_from(tx, id)?.ok_or(Error::Unavailable("session not found"))?;
    let expected = session.revision;
    if expected_revision.is_some_and(|expected_revision| expected_revision != expected) {
        return Err(Error::Conflict("session revision changed"));
    }
    let count: i64 = tx.query_row(
        "SELECT count(*) FROM messages WHERE session=?1",
        [id.as_str()],
        |row| row.get(0),
    )?;
    if count >= MAX_MESSAGES {
        return Err(xcb_core::Error::Limit("session messages").into());
    }
    tx.execute(
        "INSERT INTO messages VALUES(?1,?2,?3,?4)",
        params![
            message.id.as_str(),
            id.as_str(),
            count + 1,
            serde_json::to_string(message)?
        ],
    )?;
    session.revision = session
        .revision
        .checked_add(1)
        .ok_or(Error::Conflict("revision overflow"))?;
    session.last_active_at_ms = session.last_active_at_ms.max(message.at_ms);
    if session.title == "New session" && message.role == xcb_core::session::Role::User {
        session.title = message
            .text
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(80)
            .collect();
        session.title = xcb_core::display_text(&session.title, 160);
        if session.title.is_empty() {
            session.title = "Image message".into();
        }
    }
    update_session(tx, &session, expected)?;
    Ok(session)
}
fn update_session(transaction: &Transaction<'_>, session: &Session, expected: u64) -> Result<()> {
    session.validate()?;
    if transaction.execute(
        "UPDATE sessions SET payload=?1, revision=?2, last_active=?3 WHERE id=?4 AND revision=?5",
        params![
            serde_json::to_string(session)?,
            sql(session.revision)?,
            sql(session.last_active_at_ms)?,
            session.id.as_str(),
            sql(expected)?
        ],
    )? != 1
    {
        return Err(Error::Conflict("session revision changed"));
    }
    Ok(())
}

impl Store {
    /// Read existing state without initialization, migration, recovery, or a
    /// writable database connection. SQLite may maintain its normal reader
    /// coordination sidecars; no application records are changed.
    pub fn open_read_only(root: &Path) -> Result<Self> {
        crate::process::initialize_host()?;
        let root = private::check_directory(root)?;
        let path = root.join("xcb.sqlite");
        let database = private::open_file(&path, 8 * 1024 * 1024 * 1024)?;
        for suffix in ["xcb.sqlite-wal", "xcb.sqlite-shm", "xcb.sqlite-journal"] {
            private::open_file_maybe_vanished(&root.join(suffix), 1024 * 1024 * 1024)?;
        }
        let connection = Connection::open_with_flags(
            &path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        private::same_file(&path, &database)?;
        connection.busy_timeout(Duration::from_secs(2))?;
        connection.pragma_update(None, "query_only", true)?;
        let version: u32 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        // Versions one and two read identically: the lease rekey to
        // (run, account) only changes which rows writers may insert.
        if !(1..=2).contains(&version) {
            return Err(Error::Unavailable(
                "existing xcb database schema is unavailable",
            ));
        }
        Ok(Self {
            root,
            instance: new_id("i").to_string(),
            connection: Mutex::new(connection),
            managed: Mutex::new(None),
            read_only: true,
            dropped_observations: std::sync::atomic::AtomicU64::new(0),
            #[cfg(test)]
            observation_commits: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    pub fn open(root: &Path) -> Result<Self> {
        crate::process::initialize_host()?;
        let root = private::directory(root)?;
        let lock_path = root.join(".initialize.lock");
        let initialization = crate::os::no_follow(
            crate::os::owner_only(
                fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(true)
                    .truncate(false),
            ),
            true,
        )
        .open(&lock_path)?;
        private::check_file(&initialization, 0)?;
        private::lock(&initialization)?;
        let initialization = private::ExclusiveLock::held(initialization);
        private::same_file(&lock_path, &initialization)?;
        for name in [
            "accounts",
            "panes",
            "hooks",
            "runs",
            "attachments",
            "exports",
        ] {
            private::directory(&root.join(name))?;
        }
        let path = root.join("xcb.sqlite");
        match fs::symlink_metadata(&path) {
            Ok(_) => {
                private::open_file(&path, 8 * 1024 * 1024 * 1024)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                private::create(&path, &[])?;
            }
            Err(error) => return Err(error.into()),
        }
        for suffix in ["xcb.sqlite-wal", "xcb.sqlite-shm", "xcb.sqlite-journal"] {
            // SQLite retires these sidecars when the last connection closes;
            // a sibling store can legitimately unlink one after this process
            // releases the initialization lock but before it exits.
            private::open_file_maybe_vanished(&root.join(suffix), 1024 * 1024 * 1024)?;
        }
        let mut connection = Connection::open(&path)?;
        // Writers serialize on the WAL writer lock; readers never block.
        // Twenty parallel terminals × short transactions still fit well under
        // this bound on a loaded host, and a dead process's locks are released
        // by the kernel, so a generous ceiling cannot deadlock the store.
        connection.busy_timeout(Duration::from_secs(30))?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        let journal: String =
            connection.pragma_query_value(None, "journal_mode", |row| row.get(0))?;
        if !journal.eq_ignore_ascii_case("wal") {
            connection.pragma_update(None, "journal_mode", "WAL")?;
        }
        connection.pragma_update(None, "synchronous", "FULL")?;
        let version: u32 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version > 2 {
            return Err(Error::Unavailable("database was written by a newer xcb"));
        }
        if version == 0 {
            let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let current: u32 = tx.pragma_query_value(None, "user_version", |row| row.get(0))?;
            if current > 1 {
                return Err(Error::Unavailable("database was written by a newer xcb"));
            }
            if current == 0 {
                tx.execute_batch("CREATE TABLE accounts(id TEXT PRIMARY KEY, payload TEXT NOT NULL);
                CREATE TABLE sessions(id TEXT PRIMARY KEY, account TEXT NOT NULL REFERENCES accounts(id), payload TEXT NOT NULL, revision INTEGER NOT NULL, last_active INTEGER NOT NULL);
                CREATE INDEX sessions_activity ON sessions(last_active DESC, id);
                CREATE TABLE messages(id TEXT PRIMARY KEY, session TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE, sequence INTEGER NOT NULL, payload TEXT NOT NULL, UNIQUE(session, sequence));
                CREATE TABLE runs(id TEXT PRIMARY KEY, session TEXT REFERENCES sessions(id) ON DELETE CASCADE, account TEXT NOT NULL REFERENCES accounts(id), phase TEXT NOT NULL, payload TEXT NOT NULL);
                CREATE TABLE leases(account TEXT PRIMARY KEY REFERENCES accounts(id), run TEXT NOT NULL UNIQUE REFERENCES runs(id));
                CREATE TABLE usage(id TEXT PRIMARY KEY, session TEXT NOT NULL, account TEXT NOT NULL, observed_at INTEGER NOT NULL, payload TEXT NOT NULL);
                CREATE INDEX usage_activity ON usage(session, observed_at DESC);
                CREATE TABLE quotas(pool TEXT NOT NULL, window TEXT NOT NULL, observed_at INTEGER NOT NULL, payload TEXT NOT NULL, PRIMARY KEY(pool, window, observed_at));
                CREATE TABLE models(provider TEXT NOT NULL, id TEXT NOT NULL, payload TEXT NOT NULL, PRIMARY KEY(provider, id));
                CREATE TABLE tool_effects(run TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE, call TEXT NOT NULL, operation TEXT NOT NULL, input_digest TEXT NOT NULL, settled INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(run,call));
                CREATE TABLE velocity(session TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE, at_ms INTEGER NOT NULL, output_total INTEGER NOT NULL, PRIMARY KEY(session,at_ms));
                PRAGMA user_version=1;")?;
            }
            tx.commit()?;
        }
        let version: u32 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version == 1 {
            let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            // Version two rekeys custody by run so one account may hold
            // several concurrent unsettled runs (config
            // `max_runs_per_account`). Every other lease read and write was
            // already (account, run)-scoped, so only this primary key moved.
            tx.execute_batch(
                "CREATE TABLE leases_v2(
                run TEXT PRIMARY KEY REFERENCES runs(id),
                account TEXT NOT NULL REFERENCES accounts(id));
                INSERT INTO leases_v2(run,account) SELECT run,account FROM leases;
                DROP TABLE leases;
                ALTER TABLE leases_v2 RENAME TO leases;
                PRAGMA user_version=2;",
            )?;
            tx.commit()?;
        }
        // Additive extension: older readers can still inspect version-one
        // state; missing terminal records never authorize inferred completion.
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS run_outcomes(
            run TEXT PRIMARY KEY REFERENCES runs(id) ON DELETE CASCADE,
            session TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
            input_sequence INTEGER NOT NULL,
            payload TEXT NOT NULL,
            UNIQUE(session,input_sequence));",
        )?;
        // Independent of session/run pruning. Generation changes alone cannot
        // clear this record: login rotates before credentials are published.
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS account_auth_failures(
            account TEXT PRIMARY KEY REFERENCES accounts(id),
            generation TEXT,
            run TEXT NOT NULL);",
        )?;
        // Additive: each account's own observed catalog. The version-one
        // `models` table stays the provider-wide catalog that account-less
        // writers maintain and that older readers keep using; accounts with
        // no rows here fall back to it (`ModelCatalog::for_account`).
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS account_models(
            account TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
            id TEXT NOT NULL,
            payload TEXT NOT NULL,
            PRIMARY KEY(account, id));",
        )?;
        connection.execute_batch("CREATE TABLE IF NOT EXISTS run_recovery_generation(run TEXT PRIMARY KEY REFERENCES runs(id) ON DELETE CASCADE, generation TEXT);")?;
        connection.execute_batch("CREATE TABLE IF NOT EXISTS account_recovery(account TEXT PRIMARY KEY REFERENCES accounts(id) ON DELETE CASCADE, generation TEXT, payload TEXT NOT NULL);")?;
        // Additive: cooldowns for usage limits the provider refused without
        // a reset time (see `QUOTA_LIMITS`). Older readers ignore it and
        // simply do not see the cooldown.
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS quota_limits(
            pool TEXT NOT NULL,
            window TEXT NOT NULL,
            observed_at INTEGER NOT NULL,
            payload TEXT NOT NULL,
            PRIMARY KEY(pool, window, observed_at));",
        )?;
        Ok(Self {
            root,
            instance: new_id("i").to_string(),
            connection: Mutex::new(connection),
            managed: Mutex::new(None),
            read_only: false,
            dropped_observations: std::sync::atomic::AtomicU64::new(0),
            #[cfg(test)]
            observation_commits: std::sync::atomic::AtomicUsize::new(0),
        })
    }
    fn db(&self) -> Result<MutexGuard<'_, Connection>> {
        self.connection
            .lock()
            .map_err(|_| Error::Conflict("database lock poisoned"))
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    /// The identity this handle stamps on runs it prepares.
    pub fn instance(&self) -> &str {
        &self.instance
    }
    fn owner(&self) -> RunOwner {
        RunOwner {
            instance: self.instance.clone(),
            pid: std::process::id(),
        }
    }
    /// True when `session` has an unsettled run owned by a different — still
    /// living — process instance. Such a run is active work in another
    /// terminal, not a run needing recovery.
    pub fn remote_active(&self, session: &Id) -> Result<bool> {
        Ok(self.unsettled_runs()?.iter().any(|run| {
            run.session.as_ref() == Some(session)
                && run
                    .owner
                    .as_ref()
                    .is_some_and(|owner| owner.instance != self.instance && owner.alive())
        }))
    }

    pub fn add_account(
        &self,
        provider: Provider,
        subscription: &str,
        now: u64,
        email: Option<String>,
    ) -> Result<Account> {
        label(subscription, 80)?;
        if let Some(email) = &email {
            email_label(email)?;
        }
        let id = new_id("a");
        let mut account = Account {
            quota_pool: id.clone(),
            id,
            provider,
            label: String::new(),
            email,
            subscription: subscription.to_owned(),
            enabled: true,
            created_at_ms: now,
        };
        // The stored label is the fixed system-derived identity; it is never
        // user-authored and keeps older strict readers seeing a valid label.
        account.label = account.fixed_name();
        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let count: i64 = tx.query_row("SELECT count(*) FROM accounts", [], |row| row.get(0))?;
        if count >= MAX_ACCOUNTS {
            return Err(xcb_core::Error::Limit("accounts").into());
        }
        tx.execute(
            "INSERT INTO accounts(id,payload) VALUES(?1,?2)",
            params![account.id.as_str(), serde_json::to_string(&account)?],
        )?;
        let root = private::directory(&self.root.join("accounts").join(account.id.as_str()))?;
        for name in ["profile", "home"] {
            private::directory(&root.join(name))?;
        }
        tx.commit()?;
        Ok(account)
    }
    pub fn accounts(&self) -> Result<Vec<Account>> {
        let db = self.db()?;
        let mut query = db.prepare("SELECT payload FROM accounts ORDER BY id LIMIT 129")?;
        let rows = query.query_map([], |row| row.get::<_, String>(0))?;
        let mut accounts = Vec::new();
        for row in rows {
            let account: Account = decode(&row?)?;
            account.validate()?;
            accounts.push(account);
        }
        if accounts.len() > MAX_ACCOUNTS as usize {
            return Err(xcb_core::Error::Limit("accounts").into());
        }
        Ok(accounts)
    }
    pub fn account(&self, id: &Id) -> Result<Account> {
        self.accounts()?
            .into_iter()
            .find(|account| &account.id == id)
            .ok_or(Error::Unavailable("account not found"))
    }
    /// Resolve what a person typed: an exact id, name or legacy label, else
    /// a unique id prefix. The accounts table shortens ids with a trailing
    /// `…`, so a pasted cell resolves too.
    pub fn resolve_account(&self, value: &str) -> Result<Account> {
        let accounts = self.accounts()?;
        let shown = xcb_core::display_text(value, 64);
        let exact: Vec<&Account> = accounts
            .iter()
            .filter(|account| {
                account.id.as_str() == value
                    || account.name() == value
                    // Legacy labels still resolve so stored references keep working.
                    || account.label == value
            })
            .collect();
        match exact.as_slice() {
            [only] => return Ok((*only).clone()),
            [] => {}
            several => {
                return Err(Error::guided(
                    format!(
                        "\"{shown}\" names {} accounts. Use the account id instead.",
                        several.len()
                    ),
                    "xcb accounts --json",
                ));
            }
        }
        let prefix = value.trim_end_matches('…');
        let matches: Vec<&Account> = if prefix.is_empty() {
            Vec::new()
        } else {
            accounts
                .iter()
                .filter(|account| account.id.as_str().starts_with(prefix))
                .collect()
        };
        match matches.as_slice() {
            [only] => Ok((*only).clone()),
            [] => Err(Error::guided(
                format!("No account matches \"{shown}\"."),
                "xcb accounts",
            )),
            several => Err(Error::guided(
                format!(
                    "\"{shown}\" matches {} accounts: {}. Type more of the id.",
                    several.len(),
                    several
                        .iter()
                        .take(4)
                        .map(|account| account.id.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                "xcb accounts",
            )),
        }
    }
    /// Record provider-observed identity: email and, when reported, the plan.
    /// Only fresh observations are written; a `None` email never clears a
    /// known one.
    pub fn set_account_identity(
        &self,
        id: &Id,
        email: Option<String>,
        subscription: Option<String>,
    ) -> Result<()> {
        if let Some(email) = &email {
            email_label(email)?;
        }
        if let Some(subscription) = &subscription {
            label(subscription, 80)?;
        }
        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let json: String = tx.query_row(
            "SELECT payload FROM accounts WHERE id=?1",
            [id.as_str()],
            |row| row.get(0),
        )?;
        let mut account: Account = decode(&json)?;
        if let Some(email) = email {
            account.email = Some(email);
        }
        if let Some(subscription) = subscription {
            account.subscription = subscription;
        }
        account.validate()?;
        tx.execute(
            "UPDATE accounts SET payload=?1 WHERE id=?2",
            params![serde_json::to_string(&account)?, id.as_str()],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn account_root(&self, id: &Id) -> Result<PathBuf> {
        self.account(id)?;
        private::check_directory(&self.root.join("accounts").join(id.as_str()))
    }

    /// Permanently remove an account and every account-owned record.
    ///
    /// The lease and run checks happen under the same immediate database
    /// transaction as the deletion. An account held by any unsettled run (or
    /// by a lease whose run record is inconsistent) is refused before any
    /// state or credential path is changed; removal never performs recovery or
    /// releases custody as a side effect.
    pub fn remove_account(&self, id: &Id) -> Result<Account> {
        let account_path = self.root.join("accounts").join(id.as_str());
        // Check the private credential directory before changing the database.
        // A missing or foreign directory is a custody failure, not permission
        // to delete only the database record and leave an unknown credential.
        private::check_directory(&account_path)?;

        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let json: String = tx.query_row(
            "SELECT payload FROM accounts WHERE id=?1",
            [id.as_str()],
            |row| row.get(0),
        )?;
        let account: Account = decode(&json)?;
        account.validate()?;
        let held: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM leases WHERE account=?1)
             OR EXISTS(SELECT 1 FROM runs WHERE account=?1 AND phase!='settled')",
            [id.as_str()],
            |row| row.get(0),
        )?;
        if held {
            return Err(Error::Conflict(
                "account has an unsettled run; exclusive custody remains held",
            ));
        }

        // Account-bound history cannot outlive the account foreign key. The
        // account removal is intentionally permanent, so remove those records
        // in the same transaction rather than leaving an unusable account id
        // behind in sessions, runs, usage, or quota history.
        tx.execute("DELETE FROM runs WHERE account=?1", [id.as_str()])?;
        tx.execute("DELETE FROM sessions WHERE account=?1", [id.as_str()])?;
        tx.execute("DELETE FROM usage WHERE account=?1", [id.as_str()])?;
        tx.execute(
            "DELETE FROM quotas WHERE pool=?1",
            [account.quota_pool.as_str()],
        )?;
        tx.execute(
            "DELETE FROM quota_limits WHERE pool=?1",
            [account.quota_pool.as_str()],
        )?;
        if tx.execute("DELETE FROM accounts WHERE id=?1", [id.as_str()])? != 1 {
            return Err(Error::Unavailable("account not found"));
        }
        tx.commit()?;

        // The database deletion is durable before removing the account tree;
        // no provider credential remains in the state root when this returns
        // successfully. A filesystem failure is reported rather than claimed
        // as a successful removal.
        fs::remove_dir_all(&account_path)?;
        private::sync_directory(account_path.parent().ok_or(Error::PrivateState)?)?;
        Ok(account)
    }

    pub fn set_account_enabled(&self, id: &Id, enabled: bool) -> Result<()> {
        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let held: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM leases WHERE account=?1)",
            [id.as_str()],
            |row| row.get(0),
        )?;
        if held {
            return Err(Error::Conflict("account has an unsettled run"));
        }
        let json: String = tx.query_row(
            "SELECT payload FROM accounts WHERE id=?1",
            [id.as_str()],
            |row| row.get(0),
        )?;
        let mut account: Account = decode(&json)?;
        account.enabled = enabled;
        account.validate()?;
        tx.execute(
            "UPDATE accounts SET payload=?1 WHERE id=?2",
            params![serde_json::to_string(&account)?, id.as_str()],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn create_session(
        &self,
        account_id: &Id,
        model: ModelChoice,
        workspace: &Path,
        now: u64,
    ) -> Result<Session> {
        self.create_session_inner(account_id, model, workspace, now, None)
    }
    /// The same custody checks as `create_session`, with the owning managed
    /// task recorded on the session atomically. The marker lets startup
    /// reconciliation prove custody of an orphan if the supervisor dies
    /// between session creation and managed `prepare`.
    pub fn create_managed_session(
        &self,
        account_id: &Id,
        model: ModelChoice,
        workspace: &Path,
        now: u64,
        task: &Id,
    ) -> Result<Session> {
        self.create_session_inner(account_id, model, workspace, now, Some(task))
    }
    fn create_session_inner(
        &self,
        account_id: &Id,
        model: ModelChoice,
        workspace: &Path,
        now: u64,
        managed_task: Option<&Id>,
    ) -> Result<Session> {
        let account = self.account(account_id)?;
        model.validate()?;
        let workspace = xcb_core::canonical(workspace)?;
        if !workspace.is_dir()
            || workspace.starts_with(&self.root)
            || self.root.starts_with(&workspace)
            || model.provider != account.provider
            || !account.enabled
        {
            return Err(Error::Conflict("account or workspace unavailable"));
        }
        let session = Session {
            route_pins: Default::default(),
            requirements: Default::default(),
            id: new_id("s"),
            account: account_id.clone(),
            model,
            workspace: workspace.to_str().ok_or(Error::PrivateState)?.to_owned(),
            title: "New session".into(),
            pane: Id::new("focus")?,
            state: State::Idle,
            managed_task: managed_task.cloned(),
            revision: 0,
            created_at_ms: now,
            last_active_at_ms: now,
        };
        session.validate()?;
        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let count: i64 = tx.query_row("SELECT count(*) FROM sessions", [], |row| row.get(0))?;
        if count >= MAX_SESSIONS {
            return Err(xcb_core::Error::Limit("sessions; prune old sessions").into());
        }
        tx.execute(
            "INSERT INTO sessions VALUES(?1,?2,?3,?4,?5)",
            params![
                session.id.as_str(),
                account_id.as_str(),
                serde_json::to_string(&session)?,
                sql(session.revision)?,
                sql(now)?
            ],
        )?;
        tx.commit()?;
        Ok(session)
    }
    pub fn session(&self, id: &Id) -> Result<Option<Session>> {
        let db = self.db()?;
        session_from(&db, id)
    }
    /// List readers tolerate one corrupt session row: it is skipped so the
    /// summary and routing snapshot keep working. Single-row reads and every
    /// run boundary stay strict (`session`, `session_from`).
    pub fn sessions(&self, limit: usize) -> Result<Vec<Session>> {
        if !(1..=256).contains(&limit) {
            return Err(xcb_core::Error::Invalid("session page limit").into());
        }
        let db = self.db()?;
        let mut query =
            db.prepare("SELECT payload FROM sessions ORDER BY last_active DESC,id LIMIT ?1")?;
        let rows = query.query_map([limit as i64], |row| row.get::<_, String>(0))?;
        let mut sessions = Vec::new();
        for row in rows {
            let Ok(session) = decode::<Session>(&row?).and_then(|session| {
                session.validate()?;
                Ok(session)
            }) else {
                continue;
            };
            sessions.push(session);
        }
        Ok(sessions)
    }
    /// Sessions carrying a managed-task ownership marker, decoded tolerantly
    /// like `sessions`: a row that cannot be proven marked is skipped rather
    /// than reported, so the orphan sweep never acts on unproven custody.
    pub fn managed_marked_sessions(&self) -> Result<Vec<Session>> {
        let db = self.db()?;
        let mut query = db.prepare(
            "SELECT payload FROM sessions WHERE payload LIKE '%\"managed_task\":%' ORDER BY last_active,id LIMIT ?1",
        )?;
        let rows = query.query_map([MAX_SESSIONS + 1], |row| row.get::<_, String>(0))?;
        let mut sessions = Vec::new();
        for row in rows {
            let Ok(session) = decode::<Session>(&row?).and_then(|session| {
                session.validate()?;
                Ok(session)
            }) else {
                continue;
            };
            if session.managed_task.is_some() {
                sessions.push(session);
            }
        }
        Ok(sessions)
    }
    pub fn append_message(
        &self,
        id: &Id,
        expected_revision: u64,
        message: &Message,
    ) -> Result<Session> {
        message.validate()?;
        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let session = append_message_in(&tx, id, Some(expected_revision), message)?;
        tx.commit()?;
        Ok(session)
    }
    /// Append a tool transcript message at the session's current revision.
    /// Tool results are appended by the run owner, so the revision read and
    /// the append share one transaction instead of two fsync'd commits.
    pub(crate) fn append_tool_message(&self, id: &Id, message: &Message) -> Result<Session> {
        message.validate()?;
        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let session = append_message_in(&tx, id, None, message)?;
        tx.commit()?;
        Ok(session)
    }
    /// Settle a tool receipt and append its transcript message in one
    /// durable transaction. Either both land or neither does, so a settled
    /// receipt is never separated from its recorded result.
    pub(crate) fn settle_tool_and_append(
        &self,
        run: &RunRecord,
        call: &str,
        id: &Id,
        message: &Message,
    ) -> Result<Session> {
        message.validate()?;
        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        settle_tool_in(&tx, run, call)?;
        let session = append_message_in(&tx, id, None, message)?;
        tx.commit()?;
        Ok(session)
    }
    pub fn message_count(&self, id: &Id) -> Result<usize> {
        let count: i64 = self.db()?.query_row(
            "SELECT count(*) FROM messages WHERE session=?1",
            [id.as_str()],
            |row| row.get(0),
        )?;
        usize::try_from(count).map_err(|_| xcb_core::Error::Invalid("message count").into())
    }
    /// Prove the exact managed dispatch prompt at its durable transcript
    /// boundary. Message counts alone do not identify the inserted input.
    pub(crate) fn input_matches_digest(
        &self,
        session: &Id,
        before: usize,
        expected: &str,
    ) -> Result<bool> {
        let sequence = before
            .checked_add(1)
            .ok_or(xcb_core::Error::Limit("input sequence"))?;
        let payload: Option<(String, String)> = self
            .db()?
            .query_row(
                "SELECT id,payload FROM messages WHERE session=?1 AND sequence=?2",
                params![session.as_str(), sequence as i64],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((id, payload)) = payload else {
            return Ok(false);
        };
        let message: Message = decode(&payload)?;
        message.validate()?;
        Ok(message.id.as_str() == id
            && message.role == xcb_core::session::Role::User
            && crate::digest(&message.text) == expected)
    }
    pub fn messages(&self, id: &Id, limit: usize) -> Result<Vec<Message>> {
        Ok(self.transcript_page(id, None, limit)?.messages)
    }

    pub fn transcript_page(
        &self,
        id: &Id,
        before: Option<u64>,
        limit: usize,
    ) -> Result<xcb_core::ui::TranscriptPage> {
        crate::transcript::page(
            &*self.db()?,
            xcb_core::ui::TranscriptContext::Session(id.clone()),
            before,
            limit,
        )
    }

    /// Compare the observed title, then change metadata without advancing the
    /// transcript revision or invalidating an active worker's custody.
    pub fn rename_session(&self, id: &Id, expected_title: &str, title: &str) -> Result<Session> {
        let title = crate::transcript::title(title)?;
        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut session = session_from(&tx, id)?.ok_or(Error::Unavailable("session not found"))?;
        if session.id != *id || session.title != expected_title {
            return Err(Error::Conflict("session title changed"));
        }
        session.title = title;
        update_session(&tx, &session, session.revision)?;
        tx.commit()?;
        Ok(session)
    }
    pub fn set_session_route_pins(
        &self,
        id: &Id,
        mut pins: xcb_core::session::RoutePins,
    ) -> Result<()> {
        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut session = session_from(&tx, id)?.ok_or(Error::Unavailable("session not found"))?;
        if pins
            .model
            .as_deref()
            .is_some_and(|model| model == session.model.id.as_str() || model == session.model.label)
        {
            pins.model = Some(session.model.key());
        }
        session.route_pins = pins;
        update_session(&tx, &session, session.revision)?;
        tx.commit()?;
        Ok(())
    }

    /// Monotonic metadata update; preserves transcript revision and run custody.
    pub fn require_session_capabilities(
        &self,
        id: &Id,
        requirements: xcb_core::session::TaskRequirements,
    ) -> Result<Session> {
        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut session = session_from(&tx, id)?.ok_or(Error::Unavailable("session not found"))?;
        session.requirements = session.requirements.merge(requirements);
        update_session(&tx, &session, session.revision)?;
        tx.commit()?;
        Ok(session)
    }

    pub fn authentication_required(&self, account: &Id) -> Result<bool> {
        let db = self.db()?;
        authentication_required_from(&db, account)
    }

    pub fn require_authenticated_account(&self, account: &Id) -> Result<()> {
        if self.authentication_required(account)? {
            return Err(Error::Unavailable(AUTHENTICATION_REQUIRED));
        }
        Ok(())
    }

    /// Prompting probes recheck health under their exclusive account lease.
    /// Reconnect and metadata probes remain available without this admission.
    pub(crate) fn require_authenticated_run(&self, run: &RunRecord) -> Result<()> {
        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (current, _) = self.owned_run_from(&tx, run)?;
        if authentication_required_from(&tx, &current.account)? {
            return Err(Error::Unavailable(AUTHENTICATION_REQUIRED));
        }
        Ok(())
    }

    /// A never-started provider/bridge may still have changed credentials.
    /// Callers releasing such a lease must prove its receipts are settled;
    /// generic turn settlement intentionally has different effect semantics.
    pub(crate) fn require_settled_tools(&self, run: &RunRecord) -> Result<()> {
        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        self.owned_run_from(&tx, run)?;
        let pending: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM tool_effects WHERE run=?1 AND settled=0)",
            [run.id.as_str()],
            |row| row.get(0),
        )?;
        if pending {
            return Err(Error::CleanupUnproven);
        }
        Ok(())
    }

    /// Only successful explicit credential replacement or supervised reauth
    /// calls this, after publication while still holding exclusive custody.
    /// Metadata presence, generation rotation, and routine refresh do not.
    pub(crate) fn clear_authentication_failure(&self, run: &RunRecord) -> Result<()> {
        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (current, _) = self.owned_run_from(&tx, run)?;
        tx.execute(
            "DELETE FROM account_auth_failures WHERE account=?1",
            [current.account.as_str()],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn prepare_run(
        &self,
        session_id: &Id,
        expected_revision: u64,
        now: u64,
    ) -> Result<RunRecord> {
        let capacity = crate::config::Config::load(&self.root)?
            .0
            .max_runs_per_account;
        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut session =
            session_from(&tx, session_id)?.ok_or(Error::Unavailable("session not found"))?;
        if session.revision != expected_revision {
            return Err(Error::Conflict("session revision changed"));
        }
        // Sessions keep independent leases up to the configured account
        // capacity. Probe runs (runs with no session) hold the account alone:
        // they may rotate credentials or rewrite provider state that live
        // workers depend on.
        let (held, probes): (u32, u32) = tx.query_row(
            "SELECT COUNT(*), COALESCE(SUM(r.session IS NULL),0)
            FROM leases l JOIN runs r ON r.id=l.run AND r.account=l.account
            WHERE l.account=?1",
            [session.account.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if probes > 0 {
            return Err(Error::Conflict("account has an unsettled probe"));
        }
        if held >= capacity {
            return Err(Error::Conflict(
                "account is at its configured concurrent run limit",
            ));
        }
        let account_json: String = tx.query_row(
            "SELECT payload FROM accounts WHERE id=?1",
            [session.account.as_str()],
            |row| row.get(0),
        )?;
        let account: Account = decode(&account_json)?;
        if !account.enabled {
            return Err(Error::Conflict("account is disabled"));
        }
        if authentication_required_from(&tx, &account.id)? {
            return Err(Error::Unavailable(AUTHENTICATION_REQUIRED));
        }
        if !self.account_recovery_available_from(&tx, &account.id, now, held)? {
            return Err(Error::Unavailable(
                "account provider recovery is waiting for its next trial",
            ));
        }
        if blocked_until_from(&tx, &self.root, &account, now)?.is_some() {
            return Err(Error::Unavailable(
                "account quota exhausted until its reported reset; inspect xcb accounts list or refresh account metadata",
            ));
        }
        session.revision = session
            .revision
            .checked_add(1)
            .ok_or(Error::Conflict("revision overflow"))?;
        session.state = State::Working;
        session.last_active_at_ms = session.last_active_at_ms.max(now);
        let run = RunRecord {
            custody_version: 1,
            id: new_id("r"),
            session: Some(session_id.clone()),
            account: session.account.clone(),
            revision: session.revision,
            phase: "prepared".into(),
            pid: None,
            created_at_ms: now,
            model: Some(session.model.clone()),
            owner: Some(self.owner()),
            command_custody: None,
            capability_processes: BTreeMap::new(),
        };
        tx.execute(
            "INSERT INTO runs VALUES(?1,?2,?3,?4,?5)",
            params![
                run.id.as_str(),
                session_id.as_str(),
                session.account.as_str(),
                run.phase,
                serde_json::to_string(&run)?
            ],
        )?;
        self.capture_recovery_generation(&tx, &run)?;
        tx.execute(
            "INSERT INTO leases(run,account) VALUES(?1,?2)",
            params![run.id.as_str(), session.account.as_str()],
        )?;
        update_session(&tx, &session, expected_revision)?;
        tx.commit()?;
        Ok(run)
    }
    pub(crate) fn prepare_probe(
        &self,
        account: &Id,
        model: Option<ModelChoice>,
        now: u64,
    ) -> Result<RunRecord> {
        let target = self.account(account)?;
        if !target.enabled {
            return Err(Error::Conflict("account is disabled"));
        }
        if let Some(model) = &model {
            model.validate()?;
            if model.provider != target.provider {
                return Err(Error::Conflict("probe model provider mismatch"));
            }
        }
        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        // Probes keep exclusive custody at any configured run capacity: a
        // sign-in or health check may rewrite the credential and provider
        // state that live runs read.
        let held: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM leases WHERE account=?1)",
            [account.as_str()],
            |row| row.get(0),
        )?;
        if held {
            return Err(Error::Conflict("account has an unsettled run"));
        }
        // Model-bearing probes perform inference/application work and share
        // the same recovery circuit. Metadata and sign-in stay observational.
        if model.is_some() && !self.account_recovery_available_from(&tx, account, now, 0)? {
            return Err(Error::Unavailable(
                "account provider recovery is waiting for its next trial",
            ));
        }
        let run = RunRecord {
            custody_version: 1,
            id: new_id("probe"),
            session: None,
            account: account.clone(),
            revision: 0,
            phase: "prepared".into(),
            pid: None,
            created_at_ms: now,
            model,
            owner: Some(self.owner()),
            command_custody: None,
            capability_processes: BTreeMap::new(),
        };
        tx.execute(
            "INSERT INTO runs VALUES(?1,NULL,?2,'prepared',?3)",
            params![
                run.id.as_str(),
                account.as_str(),
                serde_json::to_string(&run)?
            ],
        )?;
        self.capture_recovery_generation(&tx, &run)?;
        tx.execute(
            "INSERT INTO leases(run,account) VALUES(?1,?2)",
            params![run.id.as_str(), account.as_str()],
        )?;
        tx.commit()?;
        Ok(run)
    }
    pub(crate) fn mark_spawned(&self, run: &RunRecord, pid: u32) -> Result<RunRecord> {
        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (current, payload) = self.owned_run_from(&tx, run)?;
        if current.phase != "prepared" {
            return Err(Error::Conflict("run authority changed"));
        }
        // A stale prepared handle must not erase guest custody persisted before
        // spawning. Preserve the authoritative row rather than cloning input.
        let next = RunRecord {
            phase: "running".into(),
            pid: Some(pid),
            ..current
        };
        if tx.execute("UPDATE runs SET phase='running',payload=?1 WHERE id=?2 AND phase='prepared' AND payload=?3", params![serde_json::to_string(&next)?, run.id.as_str(), payload])? != 1 {
            return Err(Error::Conflict("run authority changed"));
        }
        tx.commit()?;
        Ok(next)
    }

    /// Prepared handles remain valid after mark_spawned and custody updates.
    /// Only mutable phase/process/custody fields may differ from the handle.
    fn owned_run_from(&self, db: &Connection, run: &RunRecord) -> Result<(RunRecord, String)> {
        let row: Option<(String, String)> = db.query_row(
            "SELECT r.payload,r.phase FROM runs r JOIN leases l ON l.run=r.id AND l.account=r.account WHERE r.id=?1 AND r.account=?2 AND r.phase IN ('prepared','running')",
            params![run.id.as_str(), run.account.as_str()], |row| Ok((row.get(0)?, row.get(1)?)),
        ).optional()?;
        let (payload, phase) = row.ok_or(Error::Conflict("run authority changed"))?;
        let current: RunRecord = decode(&payload)?;
        current.validate()?;
        let owner = current
            .owner
            .as_ref()
            .ok_or(Error::Conflict("run owner missing"))?;
        let supplied = run
            .owner
            .as_ref()
            .ok_or(Error::Conflict("run owner missing"))?;
        if current.custody_version != 1
            || run.custody_version != 1
            || current.id != run.id
            || current.account != run.account
            || current.session != run.session
            || current.revision != run.revision
            || current.created_at_ms != run.created_at_ms
            || current.model != run.model
            || current.phase != phase
            || owner.instance != self.instance
            || owner.pid != std::process::id()
            || supplied.instance != owner.instance
            || supplied.pid != owner.pid
        {
            return Err(Error::Conflict("run authority changed"));
        }
        Ok((current, payload))
    }

    pub(crate) fn verify_owned_run(&self, run: &RunRecord) -> Result<()> {
        let db = self.db()?;
        self.owned_run_from(&db, run).map(|_| ())
    }

    fn update_capability_custody(
        &self,
        run: &RunRecord,
        server: &str,
        update: impl FnOnce(&mut BTreeMap<String, Option<u32>>) -> Result<()>,
    ) -> Result<()> {
        Id::new(server)?;
        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (mut current, payload) = self.owned_run_from(&tx, run)?;
        update(&mut current.capability_processes)?;
        current.validate()?;
        if tx.execute(
            "UPDATE runs SET payload=?1 WHERE id=?2 AND payload=?3",
            params![serde_json::to_string(&current)?, run.id.as_str(), payload],
        )? != 1
        {
            return Err(Error::Conflict("run authority changed"));
        }
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn mark_capability_starting(&self, run: &RunRecord, server: &str) -> Result<()> {
        self.update_capability_custody(run, server, |servers| {
            if servers.contains_key(server) {
                return Err(Error::Conflict("tool server already has custody"));
            }
            servers.insert(server.to_owned(), None);
            Ok(())
        })
    }

    pub(crate) fn mark_capability_spawned(
        &self,
        run: &RunRecord,
        server: &str,
        pid: u32,
    ) -> Result<()> {
        self.update_capability_custody(run, server, |servers| {
            if servers.get(server) != Some(&None) {
                return Err(Error::Conflict("tool server launch intent changed"));
            }
            servers.insert(server.to_owned(), Some(pid));
            Ok(())
        })
    }

    /// Only the owning manager calls this after its independent group join.
    pub(crate) fn clear_capability_custody(&self, run: &RunRecord, server: &str) -> Result<()> {
        self.update_capability_custody(run, server, |servers| {
            if servers.remove(server).is_none() {
                return Err(Error::Conflict("tool server custody is absent"));
            }
            Ok(())
        })
    }

    /// Persist before launching any guest command. A second pending command,
    /// even with the same ID, is not another grant to launch.
    pub(crate) fn record_command_custody(
        &self,
        run: &RunRecord,
        custody: &crate::command::CommandCustody,
    ) -> Result<()> {
        validate_command_custody(&run.id, custody)?;
        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (mut current, payload) = self.owned_run_from(&tx, run)?;
        if current.command_custody.is_some() {
            return Err(Error::Conflict("run has pending command custody"));
        }
        current.command_custody = Some(custody.clone());
        if tx.execute(
            "UPDATE runs SET payload=?1 WHERE id=?2 AND payload=?3",
            params![serde_json::to_string(&current)?, run.id.as_str(), payload],
        )? != 1
        {
            return Err(Error::Conflict("run authority changed"));
        }
        tx.commit()?;
        Ok(())
    }

    /// The trusted command owner calls this only after independently proving
    /// the exact guest receipt joined. It never settles effects or the run.
    pub(crate) fn clear_command_custody(
        &self,
        run: &RunRecord,
        custody: &crate::command::CommandCustody,
    ) -> Result<()> {
        validate_command_custody(&run.id, custody)?;
        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (mut current, payload) = self.owned_run_from(&tx, run)?;
        if current.command_custody.as_ref() != Some(custody) {
            return Err(Error::Conflict("command custody changed"));
        }
        current.command_custody = None;
        if tx.execute(
            "UPDATE runs SET payload=?1 WHERE id=?2 AND payload=?3",
            params![serde_json::to_string(&current)?, run.id.as_str(), payload],
        )? != 1
        {
            return Err(Error::Conflict("run authority changed"));
        }
        tx.commit()?;
        Ok(())
    }

    /// Called only by trusted backend recovery after its exact guest/stream
    /// join proof. Repeat host stop and complete run/custody identity under the
    /// writer lock. The account remains leased until ordinary run recovery.
    pub(crate) fn reconcile_command_custody(
        &self,
        run_id: &Id,
        expected_digest: &str,
        custody: &crate::command::CommandCustody,
    ) -> Result<RunRecord> {
        validate_command_custody(run_id, custody)?;
        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let payload: String = tx.query_row(
            "SELECT r.payload FROM runs r JOIN leases l ON l.run=r.id AND l.account=r.account WHERE r.id=?1 AND r.phase='running'",
            [run_id.as_str()], |row| row.get(0),
        ).optional()?.ok_or(Error::Conflict("run lease is absent or not running"))?;
        if digest(payload.as_bytes()) != expected_digest {
            return Err(Error::Conflict("run changed since command recovery proof"));
        }
        let mut current: RunRecord = decode(&payload)?;
        current.validate()?;
        if current.id != *run_id || current.command_custody.as_ref() != Some(custody) {
            return Err(Error::Conflict("command custody changed"));
        }
        current.verify_recovery_stop()?;
        current.command_custody = None;
        if tx.execute("UPDATE runs SET payload=?1 WHERE id=?2 AND account=?3 AND phase='running' AND payload=?4", params![serde_json::to_string(&current)?, run_id.as_str(), current.account.as_str(), payload])? != 1 {
            return Err(Error::Conflict("run changed since command recovery proof"));
        }
        tx.commit()?;
        Ok(current)
    }

    pub(crate) fn settle(&self, run: &RunRecord, state: State, now: u64) -> Result<()> {
        self.settle_inner(run, state, now, None, None)
    }

    #[cfg(test)]
    pub(crate) fn settle_outcome(
        &self,
        run: &RunRecord,
        input: &Id,
        outcome: &crate::runner::Outcome,
        now: u64,
    ) -> Result<()> {
        self.settle_outcome_submitted(run, input, outcome, None, now)
    }

    pub(crate) fn settle_outcome_submitted(
        &self,
        run: &RunRecord,
        input: &Id,
        outcome: &crate::runner::Outcome,
        prompt_submission: Option<bool>,
        now: u64,
    ) -> Result<()> {
        validate_outcome(outcome)?;
        self.settle_inner(
            run,
            outcome.state,
            now,
            Some((input, outcome, prompt_submission)),
            None,
        )
    }

    /// Application inference persists no prompt, output, or diagnostic payload.
    /// Only joined sessionless terminal facts can affect account health.
    pub(crate) fn settle_application(
        &self,
        run: &RunRecord,
        facts: &xcb_core::policy::TurnFacts,
        now: u64,
    ) -> Result<()> {
        use xcb_core::policy::{EffectState, Terminal};
        if run.session.is_some()
            || !facts.joined
            || facts.effects != EffectState::None
            || facts.pending_attention
            || !matches!(facts.terminal, Terminal::Completed | Terminal::Failed)
            || (facts.terminal == Terminal::Completed && facts.failure.is_some())
        {
            return Err(Error::Conflict("application settlement is unproven"));
        }
        let state = if facts.terminal == Terminal::Completed {
            State::Idle
        } else {
            State::Failed
        };
        self.settle_inner(run, state, now, None, Some(facts))
    }

    fn settle_inner(
        &self,
        run: &RunRecord,
        state: State,
        now: u64,
        outcome: Option<(&Id, &crate::runner::Outcome, Option<bool>)>,
        application: Option<&xcb_core::policy::TurnFacts>,
    ) -> Result<()> {
        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (current, payload) = self.owned_run_from(&tx, run)?;
        if !current.capability_processes.is_empty() {
            return Err(Error::Conflict(
                "tool server stop is unproven; account custody retained",
            ));
        }
        if current.command_custody.is_some() {
            return Err(Error::Conflict(
                "command guest stop is unproven; reconcile command custody before settling",
            ));
        }
        if let Some(facts) = outcome
            .map(|(_, outcome, _)| &outcome.facts)
            .or(application)
        {
            self.record_account_recovery(&tx, &current, facts, now)?;
            use xcb_core::policy::{Failure, Terminal};
            if facts.terminal == Terminal::Failed && facts.failure == Some(Failure::Authentication)
            {
                let generation = crate::application_qualification::read_generation(
                    &self.root,
                    &current.account,
                )?;
                tx.execute(
                    "INSERT INTO account_auth_failures(account,generation,run) VALUES(?1,?2,?3)
                    ON CONFLICT(account) DO UPDATE SET generation=excluded.generation,run=excluded.run",
                    params![current.account.as_str(), generation, current.id.as_str()],
                )?;
            } else if facts.terminal == Terminal::Completed
                && facts.failure.is_none()
                && !facts.pending_attention
                && state == State::Idle
            {
                tx.execute(
                    "DELETE FROM account_auth_failures WHERE account=?1",
                    [current.account.as_str()],
                )?;
            }
        }
        if let Some(id) = &current.session {
            let mut session =
                session_from(&tx, id)?.ok_or(Error::Unavailable("session not found"))?;
            let expected = session.revision;
            session.revision = expected
                .checked_add(1)
                .ok_or(Error::Conflict("revision overflow"))?;
            session.state = state;
            session.last_active_at_ms = session.last_active_at_ms.max(now);
            if let Some((input, outcome, prompt_submission)) = outcome {
                let input_sequence: u32 = tx.query_row(
                    "SELECT sequence FROM messages WHERE id=?1 AND session=?2",
                    params![input.as_str(), id.as_str()],
                    |row| row.get(0),
                )?;
                let message_count: u32 = tx.query_row(
                    "SELECT count(*) FROM messages WHERE session=?1",
                    [id.as_str()],
                    |row| row.get(0),
                )?;
                let record = SettledOutcome {
                    version: 1,
                    run: run.id.clone(),
                    session: id.clone(),
                    input: input.clone(),
                    input_sequence: input_sequence.into(),
                    message_count: message_count.into(),
                    session_revision: session.revision,
                    prompt_submission,
                    outcome: outcome.clone(),
                };
                tx.execute(
                    "INSERT INTO run_outcomes(run,session,input_sequence,payload) VALUES(?1,?2,?3,?4)",
                    params![run.id.as_str(), id.as_str(), input_sequence, serde_json::to_string(&record)?],
                )?;
            }
            update_session(&tx, &session, expected)?;
        } else if outcome.is_some() {
            return Err(Error::Conflict("terminal outcome requires a session run"));
        }
        let record = RunRecord {
            phase: "settled".into(),
            ..current
        };
        if tx.execute(
            "UPDATE runs SET phase='settled',payload=?1 WHERE id=?2 AND payload=?3",
            params![serde_json::to_string(&record)?, run.id.as_str(), payload],
        )? != 1
        {
            return Err(Error::Conflict("run authority changed"));
        }
        if tx.execute(
            "DELETE FROM leases WHERE account=?1 AND run=?2",
            params![run.account.as_str(), run.id.as_str()],
        )? != 1
        {
            return Err(Error::Conflict("run authority changed"));
        }
        tx.commit()?;
        Ok(())
    }

    /// Recover only the exact settled turn that started after this transcript
    /// boundary. Legacy runs and sessions changed by a later turn return None.
    pub fn settled_outcome(
        &self,
        session_id: &Id,
        message_count_before: usize,
    ) -> Result<Option<crate::runner::Outcome>> {
        Ok(self
            .settled_record(session_id, message_count_before)?
            .map(|record| record.outcome))
    }

    /// Reuse only the current, identity-checked terminal receipt. A later
    /// message, rebind, unfinished run, or revision invalidates this proof.
    pub(crate) fn latest_settled_outcome(
        &self,
        session_id: &Id,
    ) -> Result<Option<crate::runner::Outcome>> {
        let sequence: Option<i64> = self.db()?.query_row(
            "SELECT max(input_sequence) FROM run_outcomes WHERE session=?1",
            [session_id.as_str()],
            |row| row.get(0),
        )?;
        let Some(sequence) = sequence else {
            return Ok(None);
        };
        let before = sequence
            .checked_sub(1)
            .and_then(|before| usize::try_from(before).ok())
            .ok_or(Error::Protocol("terminal outcome sequence"))?;
        self.settled_outcome(session_id, before)
    }

    /// Prompt submission is proven only by an exact current terminal receipt.
    /// Legacy receipts and superseded turns cannot prove either submission
    /// or its absence. A failed submission attempt is likewise unknown.
    pub(crate) fn settled_input_submission(
        &self,
        session_id: &Id,
        message_count_before: usize,
    ) -> Result<Option<bool>> {
        Ok(self
            .settled_record(session_id, message_count_before)?
            .and_then(|record| record.prompt_submission))
    }

    fn settled_record(
        &self,
        session_id: &Id,
        message_count_before: usize,
    ) -> Result<Option<SettledOutcome>> {
        let before = u64::try_from(message_count_before)
            .map_err(|_| xcb_core::Error::Invalid("message count"))?;
        if before >= MAX_MESSAGES as u64 {
            return Err(xcb_core::Error::Invalid("message count").into());
        }
        let input_sequence = before + 1;
        let mut db = self.db()?;
        let tx = db.transaction()?;
        let available: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='run_outcomes')",
            [],
            |row| row.get(0),
        )?;
        if !available {
            return Ok(None);
        }
        let stored: Option<(String, String)> = tx.query_row(
            "SELECT o.payload,r.payload FROM run_outcomes o JOIN runs r ON r.id=o.run WHERE o.session=?1 AND o.input_sequence=?2 AND r.phase='settled'",
            params![session_id.as_str(), sql(input_sequence)?],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).optional()?;
        let Some((payload, run)) = stored else {
            return Ok(None);
        };
        let record: SettledOutcome = decode(&payload)?;
        let run: RunRecord = decode(&run)?;
        run.validate()?;
        validate_outcome(&record.outcome)?;
        if record.version != 1
            || record.run != run.id
            || &record.session != session_id
            || run.session.as_ref() != Some(session_id)
            || run.phase != "settled"
            || record.input_sequence != input_sequence
            || record.message_count < input_sequence
        {
            return Err(Error::Conflict("terminal outcome identity mismatch"));
        }
        let Some(session) = session_from(&tx, session_id)? else {
            return Ok(None);
        };
        let message_count: u32 = tx.query_row(
            "SELECT count(*) FROM messages WHERE session=?1",
            [session_id.as_str()],
            |row| row.get(0),
        )?;
        let input_matches: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM messages WHERE session=?1 AND sequence=?2 AND id=?3)",
            params![
                session_id.as_str(),
                sql(input_sequence)?,
                record.input.as_str()
            ],
            |row| row.get(0),
        )?;
        if !input_matches
            || session.revision != record.session_revision
            || u64::from(message_count) != record.message_count
            || session.state != record.outcome.state
            || session.account != run.account
            || run.model.as_ref() != Some(&session.model)
        {
            return Ok(None);
        }
        Ok(Some(record))
    }
    /// Bounded local receipts for one session. This reads xcb's custody
    /// records only; it does not attach a provider process to the account.
    pub(crate) fn session_receipts(&self, session: &Id) -> Result<SessionReceipts> {
        const RUN_LIMIT: i64 = 16;
        const EFFECT_LIMIT: i64 = 256;
        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let run_count: i64 = tx.query_row(
            "SELECT count(*) FROM runs WHERE session=?1",
            [session.as_str()],
            |row| row.get(0),
        )?;
        let mut query = tx.prepare(
            "SELECT id,payload, EXISTS(
                SELECT 1 FROM leases WHERE leases.run=runs.id AND leases.account=runs.account
            ) FROM runs WHERE session=?1 ORDER BY rowid DESC LIMIT ?2",
        )?;
        let rows = query.query_map(params![session.as_str(), RUN_LIMIT], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, bool>(2)?,
            ))
        })?;
        let mut runs = Vec::new();
        for row in rows {
            let (id, payload, lease_held) = row?;
            let run: RunRecord = decode(&payload)?;
            run.validate()?;
            if run.id.as_str() != id || run.session.as_ref() != Some(session) {
                return Err(Error::Conflict("run receipt session changed"));
            }
            runs.push(SessionRunReceipt { run, lease_held });
        }
        drop(query);
        let effect_count: i64 = tx.query_row(
            "SELECT count(*) FROM tool_effects t JOIN runs r ON r.id=t.run WHERE r.session=?1",
            [session.as_str()],
            |row| row.get(0),
        )?;
        let mut query = tx.prepare(
            "SELECT r.payload,t.run,t.call,t.operation,t.input_digest,t.settled
            FROM tool_effects t JOIN runs r ON r.id=t.run
            WHERE r.session=?1 ORDER BY t.run,t.call LIMIT ?2",
        )?;
        let rows = query.query_map(params![session.as_str(), EFFECT_LIMIT], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
            ))
        })?;
        let mut effects: BTreeMap<Id, Vec<ToolEffectRecord>> = BTreeMap::new();
        for row in rows {
            let (payload, run_id, call, operation, input_digest, settled) = row?;
            let run: RunRecord = decode(&payload)?;
            run.validate()?;
            if run.session.as_ref() != Some(session) || run.id.as_str() != run_id {
                return Err(Error::Conflict("tool receipt run changed"));
            }
            label(&call, 160)?;
            xcb_core::bounded_text(&operation, 160)?;
            xcb_core::bounded_text(&input_digest, 160)?;
            if !matches!(settled, 0 | 1) {
                return Err(Error::Conflict("tool receipt settlement changed"));
            }
            effects.entry(run.id).or_default().push(ToolEffectRecord {
                call,
                operation,
                input_digest,
                settled: settled == 1,
            });
        }
        drop(query);
        let receipts = SessionReceipts {
            run_count: u64::try_from(run_count)
                .map_err(|_| xcb_core::Error::Invalid("run count"))?,
            runs,
            effect_count: u64::try_from(effect_count)
                .map_err(|_| xcb_core::Error::Invalid("tool receipt count"))?,
            effects,
        };
        tx.finish()?;
        Ok(receipts)
    }

    /// One bounded local snapshot for the native status projection. All counts
    /// and records come from the same deferred transaction; provider processes,
    /// credentials and model prompts are not consulted.
    pub(crate) fn native_status(
        &self,
        filter: &StatusFilter,
        page: StatusPage,
        now: u64,
    ) -> Result<StatusSnapshot> {
        if !(1..=256).contains(&page.limit) {
            return Err(xcb_core::Error::Invalid("status limit").into());
        }
        let limit = sql(page.limit)?;
        let offset = sql(page.offset)?;
        let provider = filter.provider.map(|provider| provider.as_str().to_owned());
        let account = filter
            .account
            .as_ref()
            .map(|account| account.as_str().to_owned());
        let session = filter
            .session
            .as_ref()
            .map(|session| session.as_str().to_owned());
        let state = filter.state.map(state_value);
        let workspace = filter.workspace.clone();

        const ACCOUNT_FILTER: &str = "
            AND (?1 IS NULL OR CASE WHEN json_valid(a.payload) THEN json_extract(a.payload,'$.provider') END = ?1)
            AND (?2 IS NULL OR a.id = ?2)
            AND (?3 IS NULL OR EXISTS(SELECT 1 FROM sessions sx WHERE sx.account=a.id AND sx.id=?3))
            AND (?4 IS NULL OR EXISTS(SELECT 1 FROM sessions sx WHERE sx.account=a.id AND CASE WHEN json_valid(sx.payload) THEN json_extract(sx.payload,'$.state') END = ?4))
            AND (?5=0 OR EXISTS(SELECT 1 FROM leases l WHERE l.account=a.id))
            AND (?6=0 OR EXISTS(SELECT 1 FROM runs rx JOIN tool_effects tx ON tx.run=rx.id WHERE rx.account=a.id AND tx.settled=0))
            AND (?7=0 OR EXISTS(SELECT 1 FROM runs rx WHERE rx.account=a.id AND CASE WHEN json_valid(rx.payload) THEN json_extract(rx.payload,'$.command_custody') IS NOT NULL ELSE 0 END))
            AND (?8 IS NULL OR EXISTS(SELECT 1 FROM sessions sx WHERE sx.account=a.id AND CASE WHEN json_valid(sx.payload) THEN json_extract(sx.payload,'$.workspace') END = ?8))";
        const SESSION_FILTER: &str = "
            AND (?1 IS NULL OR CASE WHEN json_valid(s.payload) THEN json_extract(s.payload,'$.model.provider') END = ?1)
            AND (?2 IS NULL OR s.account = ?2)
            AND (?3 IS NULL OR s.id = ?3)
            AND (?4 IS NULL OR CASE WHEN json_valid(s.payload) THEN json_extract(s.payload,'$.state') END = ?4)
            AND (?5=0 OR EXISTS(SELECT 1 FROM runs rx JOIN leases l ON l.run=rx.id AND l.account=rx.account WHERE rx.session=s.id))
            AND (?6=0 OR EXISTS(SELECT 1 FROM runs rx JOIN tool_effects tx ON tx.run=rx.id WHERE rx.session=s.id AND tx.settled=0))
            AND (?7=0 OR EXISTS(SELECT 1 FROM runs rx WHERE rx.session=s.id AND CASE WHEN json_valid(rx.payload) THEN json_extract(rx.payload,'$.command_custody') IS NOT NULL ELSE 0 END))
            AND (?8 IS NULL OR CASE WHEN json_valid(s.payload) THEN json_extract(s.payload,'$.workspace') END = ?8)";
        const RUN_FILTER: &str = "
            AND (?1 IS NULL OR COALESCE(
                CASE WHEN json_valid(s.payload) THEN json_extract(s.payload,'$.model.provider') END,
                CASE WHEN json_valid(r.payload) THEN json_extract(r.payload,'$.model.provider') END) = ?1)
            AND (?2 IS NULL OR r.account = ?2)
            AND (?3 IS NULL OR r.session = ?3)
            AND (?4 IS NULL OR CASE WHEN json_valid(s.payload) THEN json_extract(s.payload,'$.state') END = ?4)
            AND (?5=0 OR EXISTS(SELECT 1 FROM leases l WHERE l.run=r.id AND l.account=r.account))
            AND (?6=0 OR EXISTS(SELECT 1 FROM tool_effects tx WHERE tx.run=r.id AND tx.settled=0))
            AND (?7=0 OR CASE WHEN json_valid(r.payload) THEN json_extract(r.payload,'$.command_custody') IS NOT NULL ELSE 0 END)
            AND (?8 IS NULL OR CASE WHEN json_valid(s.payload) THEN json_extract(s.payload,'$.workspace') END = ?8)";
        const EFFECT_FILTER: &str = "
            AND (?1 IS NULL OR COALESCE(
                CASE WHEN json_valid(s.payload) THEN json_extract(s.payload,'$.model.provider') END,
                CASE WHEN json_valid(r.payload) THEN json_extract(r.payload,'$.model.provider') END) = ?1)
            AND (?2 IS NULL OR r.account = ?2)
            AND (?3 IS NULL OR r.session = ?3)
            AND (?4 IS NULL OR CASE WHEN json_valid(s.payload) THEN json_extract(s.payload,'$.state') END = ?4)
            AND (?5=0 OR EXISTS(SELECT 1 FROM leases l WHERE l.run=r.id AND l.account=r.account))
            AND (?6=0 OR t.settled=0)
            AND (?7=0 OR CASE WHEN json_valid(r.payload) THEN json_extract(r.payload,'$.command_custody') IS NOT NULL ELSE 0 END)
            AND (?8 IS NULL OR CASE WHEN json_valid(s.payload) THEN json_extract(s.payload,'$.workspace') END = ?8)";

        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let count = |query: &str, params: &[&dyn rusqlite::ToSql]| -> Result<u64> {
            let count: i64 = tx.query_row(query, params, |row| row.get(0))?;
            u64::try_from(count).map_err(|_| xcb_core::Error::Invalid("status count").into())
        };
        let filter_params: [&dyn rusqlite::ToSql; 8] = [
            &provider,
            &account,
            &session,
            &state,
            &filter.has_lease,
            &filter.unsettled_effects,
            &filter.pending_command_custody,
            &workspace,
        ];
        let totals = StatusTotals {
            accounts: count("SELECT count(*) FROM accounts", &[])?,
            sessions: count("SELECT count(*) FROM sessions", &[])?,
            runs: count("SELECT count(*) FROM runs", &[])?,
            tool_effects: count("SELECT count(*) FROM tool_effects", &[])?,
            leases: count("SELECT count(*) FROM leases", &[])?,
            held_accounts: count("SELECT count(DISTINCT account) FROM leases", &[])?,
            unsettled_runs: count("SELECT count(*) FROM runs WHERE phase!='settled'", &[])?,
            unsettled_effects: count("SELECT count(*) FROM tool_effects WHERE settled=0", &[])?,
            unlinked_tool_effects: count(
                "SELECT count(*) FROM tool_effects t LEFT JOIN runs r ON r.id=t.run WHERE r.id IS NULL",
                &[],
            )?,
            pending_command_custody: count(
                "SELECT count(*) FROM runs WHERE CASE WHEN json_valid(payload) THEN json_extract(payload,'$.command_custody') IS NOT NULL ELSE 0 END",
                &[],
            )?,
        };

        let accounts = StatusSection {
            matched: count(
                &format!("SELECT count(*) FROM accounts a WHERE 1=1 {ACCOUNT_FILTER}"),
                &filter_params,
            )?,
            offset: page.offset,
            records: {
                let mut query = tx.prepare(&format!(
                    "SELECT a.id,a.payload,
                        (SELECT count(*) FROM runs rx WHERE rx.account=a.id AND rx.phase!='settled'),
                        EXISTS(SELECT 1 FROM leases l WHERE l.account=a.id)
                    FROM accounts a WHERE 1=1 {ACCOUNT_FILTER}
                    ORDER BY a.id LIMIT ?9 OFFSET ?10"
                ))?;
                let rows = query.query_map(
                    params![
                        provider,
                        account,
                        session,
                        state,
                        filter.has_lease,
                        filter.unsettled_effects,
                        filter.pending_command_custody,
                        workspace,
                        limit,
                        offset
                    ],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, i64>(2)?,
                            row.get::<_, bool>(3)?,
                        ))
                    },
                )?;
                let mut records = Vec::new();
                for row in rows {
                    let (id, payload, active_runs, lease_held) = row?;
                    let account: Account = decode(&payload)?;
                    account.validate()?;
                    if account.id.as_str() != id {
                        return Err(Error::Conflict("account status identity changed"));
                    }
                    let points = current_quota_points_from(&tx, &self.root, &account)?;
                    let mut by_window: BTreeMap<Id, Vec<QuotaPoint>> = BTreeMap::new();
                    for point in points.iter().flatten() {
                        by_window
                            .entry(point.window.clone())
                            .or_default()
                            .push(point.clone());
                    }
                    let fresh: Vec<_> = by_window
                        .values()
                        .filter_map(|points| points.last())
                        .filter(|point| point.fresh(now))
                        .collect();
                    records.push(StatusAccount {
                        authentication_required: authentication_required_from(&tx, &account.id)?,
                        remaining_percent: fresh
                            .iter()
                            .map(|point| 100.0 - point.used_percent)
                            .reduce(f64::min),
                        resets_at_ms: fresh.iter().map(|point| point.resets_at_ms).min(),
                        quota_blocked_until_ms: points.as_ref().and_then(|points| {
                            xcb_core::usage::quota_blocked_until(
                                points,
                                &account.quota_pool,
                                account.provider,
                                now,
                            )
                        }),
                        active_runs: u64::try_from(active_runs)
                            .map_err(|_| xcb_core::Error::Invalid("status run count"))?,
                        lease_held,
                        account,
                    });
                }
                records
            },
        };

        let sessions = StatusSection {
            matched: count(
                &format!("SELECT count(*) FROM sessions s WHERE 1=1 {SESSION_FILTER}"),
                &filter_params,
            )?,
            offset: page.offset,
            records: {
                let mut query = tx.prepare(&format!(
                    "SELECT s.id,s.account,s.payload,
                        (SELECT count(*) FROM runs rx WHERE rx.session=s.id),
                        (SELECT count(*) FROM runs rx WHERE rx.session=s.id AND rx.phase!='settled'),
                        (SELECT count(*) FROM tool_effects tx JOIN runs rx ON rx.id=tx.run WHERE rx.session=s.id),
                        (SELECT count(*) FROM tool_effects tx JOIN runs rx ON rx.id=tx.run WHERE rx.session=s.id AND tx.settled=0),
                        EXISTS(SELECT 1 FROM runs rx JOIN leases l ON l.run=rx.id AND l.account=rx.account WHERE rx.session=s.id),
                        EXISTS(SELECT 1 FROM runs rx WHERE rx.session=s.id AND CASE WHEN json_valid(rx.payload) THEN json_extract(rx.payload,'$.command_custody') IS NOT NULL ELSE 0 END),
                        (SELECT count(*) FROM runs rx JOIN json_each(rx.payload,'$.capability_processes') capability ON json_valid(rx.payload) AND json_type(rx.payload,'$.capability_processes')='object' WHERE rx.session=s.id)
                    FROM sessions s WHERE 1=1 {SESSION_FILTER}
                    ORDER BY s.last_active DESC,s.id LIMIT ?9 OFFSET ?10"
                ))?;
                let rows = query.query_map(
                    params![
                        provider,
                        account,
                        session,
                        state,
                        filter.has_lease,
                        filter.unsettled_effects,
                        filter.pending_command_custody,
                        workspace,
                        limit,
                        offset
                    ],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, i64>(3)?,
                            row.get::<_, i64>(4)?,
                            row.get::<_, i64>(5)?,
                            row.get::<_, i64>(6)?,
                            row.get::<_, bool>(7)?,
                            row.get::<_, bool>(8)?,
                            row.get::<_, i64>(9)?,
                        ))
                    },
                )?;
                let mut records = Vec::new();
                for row in rows {
                    let (
                        id,
                        account,
                        payload,
                        run_count,
                        unsettled_run_count,
                        effect_count,
                        unsettled_effect_count,
                        lease_held,
                        pending_command_custody,
                        capability_process_count,
                    ) = row?;
                    let session: Session = decode(&payload)?;
                    session.validate()?;
                    if session.id.as_str() != id || session.account.as_str() != account {
                        return Err(Error::Conflict("session status identity changed"));
                    }
                    records.push(StatusSession {
                        session,
                        run_count: u64::try_from(run_count)
                            .map_err(|_| xcb_core::Error::Invalid("status run count"))?,
                        unsettled_run_count: u64::try_from(unsettled_run_count)
                            .map_err(|_| xcb_core::Error::Invalid("status run count"))?,
                        effect_count: u64::try_from(effect_count)
                            .map_err(|_| xcb_core::Error::Invalid("status effect count"))?,
                        unsettled_effect_count: u64::try_from(unsettled_effect_count)
                            .map_err(|_| xcb_core::Error::Invalid("status effect count"))?,
                        lease_held,
                        pending_command_custody,
                        capability_process_count: u64::try_from(capability_process_count)
                            .map_err(|_| xcb_core::Error::Invalid("status process count"))?,
                    });
                }
                records
            },
        };

        let run_provider = "
            COALESCE(
                CASE WHEN json_valid(s.payload) THEN json_extract(s.payload,'$.model.provider') END,
                CASE WHEN json_valid(r.payload) THEN json_extract(r.payload,'$.model.provider') END)";
        let stored_provider = |value: Option<String>| -> Result<Option<Provider>> {
            value
                .map(|value| {
                    serde_json::from_value::<Provider>(serde_json::Value::String(value))
                        .map_err(Error::from)
                })
                .transpose()
        };
        let runs = StatusSection {
            matched: count(
                &format!(
                    "SELECT count(*) FROM runs r LEFT JOIN sessions s ON s.id=r.session WHERE 1=1 {RUN_FILTER}"
                ),
                &filter_params,
            )?,
            offset: page.offset,
            records: {
                let mut query = tx.prepare(&format!(
                    "SELECT r.id,r.session,r.payload,{run_provider},
                        EXISTS(SELECT 1 FROM leases l WHERE l.run=r.id AND l.account=r.account),
                        (SELECT count(*) FROM tool_effects tx WHERE tx.run=r.id),
                        (SELECT count(*) FROM tool_effects tx WHERE tx.run=r.id AND tx.settled=0)
                    FROM runs r LEFT JOIN sessions s ON s.id=r.session
                    WHERE 1=1 {RUN_FILTER}
                    ORDER BY r.rowid DESC LIMIT ?9 OFFSET ?10"
                ))?;
                let rows = query.query_map(
                    params![
                        provider,
                        account,
                        session,
                        state,
                        filter.has_lease,
                        filter.unsettled_effects,
                        filter.pending_command_custody,
                        workspace,
                        limit,
                        offset
                    ],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, Option<String>>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, Option<String>>(3)?,
                            row.get::<_, bool>(4)?,
                            row.get::<_, i64>(5)?,
                            row.get::<_, i64>(6)?,
                        ))
                    },
                )?;
                let mut records = Vec::new();
                for row in rows {
                    let (id, session, payload, provider, lease_held, effects, unsettled) = row?;
                    let run: RunRecord = decode(&payload)?;
                    run.validate()?;
                    if run.id.as_str() != id
                        || run.session.as_ref().map(Id::as_str) != session.as_deref()
                    {
                        return Err(Error::Conflict("run status identity changed"));
                    }
                    records.push(StatusRun {
                        run,
                        provider: stored_provider(provider)?,
                        lease_held,
                        effect_count: u64::try_from(effects)
                            .map_err(|_| xcb_core::Error::Invalid("status effect count"))?,
                        unsettled_effect_count: u64::try_from(unsettled)
                            .map_err(|_| xcb_core::Error::Invalid("status effect count"))?,
                    });
                }
                records
            },
        };

        let effects = StatusSection {
            matched: count(
                &format!(
                    "SELECT count(*) FROM tool_effects t LEFT JOIN runs r ON r.id=t.run LEFT JOIN sessions s ON s.id=r.session WHERE 1=1 {EFFECT_FILTER}"
                ),
                &filter_params,
            )?,
            offset: page.offset,
            records: {
                let mut query = tx.prepare(&format!(
                    "SELECT t.run,t.call,t.operation,t.input_digest,t.settled,
                        r.session,r.account,r.payload,{run_provider}
                    FROM tool_effects t
                    LEFT JOIN runs r ON r.id=t.run
                    LEFT JOIN sessions s ON s.id=r.session
                    WHERE 1=1 {EFFECT_FILTER}
                    ORDER BY r.rowid DESC,t.call LIMIT ?9 OFFSET ?10"
                ))?;
                let rows = query.query_map(
                    params![
                        provider,
                        account,
                        session,
                        state,
                        filter.has_lease,
                        filter.unsettled_effects,
                        filter.pending_command_custody,
                        workspace,
                        limit,
                        offset
                    ],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(3)?,
                            row.get::<_, i64>(4)?,
                            row.get::<_, Option<String>>(5)?,
                            row.get::<_, Option<String>>(6)?,
                            row.get::<_, Option<String>>(7)?,
                            row.get::<_, Option<String>>(8)?,
                        ))
                    },
                )?;
                let mut records = Vec::new();
                for row in rows {
                    let (
                        run_id,
                        call,
                        operation,
                        input_digest,
                        settled,
                        session,
                        account,
                        payload,
                        provider,
                    ) = row?;
                    let (session, account, run) = match payload {
                        Some(payload) => {
                            let run: RunRecord = decode(&payload)?;
                            run.validate()?;
                            if run.id.as_str() != run_id
                                || run.session.as_ref().map(Id::as_str) != session.as_deref()
                                || Some(run.account.as_str()) != account.as_deref()
                            {
                                return Err(Error::Conflict("tool status run changed"));
                            }
                            (run.session.clone(), Some(run.account.clone()), run.id)
                        }
                        None => {
                            if session.is_some() || account.is_some() {
                                return Err(Error::Conflict("tool status run changed"));
                            }
                            (
                                None,
                                None,
                                Id::new(&run_id)
                                    .map_err(|_| Error::Conflict("tool status run changed"))?,
                            )
                        }
                    };
                    label(&call, 160)?;
                    xcb_core::bounded_text(&operation, 160)?;
                    xcb_core::bounded_text(&input_digest, 160)?;
                    if !matches!(settled, 0 | 1) {
                        return Err(Error::Conflict("tool status settlement changed"));
                    }
                    records.push(StatusEffect {
                        session,
                        account,
                        provider: stored_provider(provider)?,
                        run,
                        call,
                        operation,
                        input_digest,
                        settled: settled == 1,
                    });
                }
                records
            },
        };

        let snapshot = StatusSnapshot {
            totals,
            accounts,
            sessions,
            runs,
            effects,
        };
        tx.finish()?;
        Ok(snapshot)
    }

    pub fn unsettled_runs(&self) -> Result<Vec<RunRecord>> {
        let db = self.db()?;
        let mut query =
            db.prepare("SELECT payload FROM runs WHERE phase!='settled' ORDER BY id LIMIT 129")?;
        let rows = query.query_map([], |row| row.get::<_, String>(0))?;
        let mut runs = Vec::new();
        for row in rows {
            let run: RunRecord = decode(&row?)?;
            run.validate()?;
            runs.push(run);
        }
        Ok(runs)
    }
    pub fn run(&self, id: &Id) -> Result<Option<RunRecord>> {
        self.recovery_candidate(id)
            .map(|candidate| candidate.map(|(run, _)| run))
    }
    pub fn recovery_candidate(&self, id: &Id) -> Result<Option<(RunRecord, String)>> {
        let db = self.db()?;
        let payload: Option<String> = db
            .query_row(
                "SELECT payload FROM runs WHERE id=?1",
                [id.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        payload
            .map(|payload| {
                let payload_digest = digest(payload.as_bytes());
                let run: RunRecord = decode(&payload)?;
                run.validate()?;
                Ok((run, payload_digest))
            })
            .transpose()
    }
    pub fn recover_run(&self, run_id: &Id, expected_digest: &str, now: u64) -> Result<RunRecord> {
        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let payload: String = tx
            .query_row(
                "SELECT payload FROM runs WHERE id=?1",
                [run_id.as_str()],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(Error::Unavailable("run not found"))?;
        let run: RunRecord = decode(&payload)?;
        run.validate()?;
        if run.phase != "running" {
            return Err(Error::Conflict("run is not in running phase"));
        }
        let Some(pid) = run.pid else {
            return Err(Error::Conflict("run has no recorded process group"));
        };
        if pid == 0 {
            return Err(Error::Conflict("recorded process group id must be nonzero"));
        }
        let held: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM leases WHERE account=?1 AND run=?2)",
            params![run.account.as_str(), run_id.as_str()],
            |row| row.get(0),
        )?;
        if !held {
            return Err(Error::Conflict("run lease is absent"));
        }
        if digest(payload.as_bytes()) != expected_digest {
            return Err(Error::Conflict("run changed since process-group proof"));
        }
        if run.command_custody.is_some() {
            return Err(Error::Conflict(
                "command guest stop is unproven; reconcile command custody before recovery",
            ));
        }
        run.verify_recovery_stop()?;
        let pending_auth: Vec<(String, String, String)> = {
            let mut query = tx.prepare("SELECT call,operation,input_digest FROM tool_effects WHERE run=?1 AND settled=0 AND (operation LIKE 'host_auth_%' OR call LIKE 'xcb_auth_%' OR call LIKE 'xcb_devin_auth_%')")?;
            query
                .query_map([run_id.as_str()], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                })?
                .collect::<std::result::Result<_, _>>()?
        };
        if !pending_auth.is_empty() {
            let [(call, operation, metadata_digest)] = pending_auth.as_slice() else {
                return Err(Error::Conflict(
                    "unsettled credential receipts require reconciliation",
                ));
            };
            if call != "xcb_auth_snapshot" || operation != "host_auth_refresh" {
                return Err(Error::Conflict(
                    "unsettled credential receipt has no safe recovery",
                ));
            }
            let account_json: String = tx.query_row(
                "SELECT payload FROM accounts WHERE id=?1",
                [run.account.as_str()],
                |row| row.get(0),
            )?;
            let account: Account = decode(&account_json)?;
            if account.provider != Provider::Codex {
                return Err(Error::Conflict("credential recovery provider mismatch"));
            }
            // Hold the same immediate transaction through filesystem CAS and
            // receipt/run settlement. Concurrent recoverers cannot release the
            // account while another still owns credential reconciliation.
            crate::auth::recover_codex_auth(&self.root, &run, metadata_digest)?;
            if tx.execute("UPDATE tool_effects SET settled=1 WHERE run=?1 AND call=?2 AND operation=?3 AND input_digest=?4 AND settled=0", params![run.id.as_str(), call, operation, metadata_digest])? != 1 {
                return Err(Error::Conflict("credential recovery receipt changed"));
            }
        }
        if let Some(session_id) = &run.session {
            let mut session =
                session_from(&tx, session_id)?.ok_or(Error::Unavailable("session not found"))?;
            let expected_revision = session.revision;
            session.revision = expected_revision
                .checked_add(1)
                .ok_or(Error::Conflict("revision overflow"))?;
            session.state = State::Uncertain;
            session.last_active_at_ms = session.last_active_at_ms.max(now);
            update_session(&tx, &session, expected_revision)?;
        }
        let settled = RunRecord {
            phase: "settled".into(),
            capability_processes: BTreeMap::new(),
            ..run.clone()
        };
        if tx.execute(
            "UPDATE runs SET phase='settled', payload=?1 WHERE id=?2 AND account=?3 AND phase='running' AND payload=?4",
            params![
                serde_json::to_string(&settled)?,
                run_id.as_str(),
                run.account.as_str(),
                payload
            ],
        )? != 1
        {
            return Err(Error::Conflict("run no longer matches recovery proof"));
        }
        if tx.execute(
            "DELETE FROM leases WHERE account=?1 AND run=?2",
            params![run.account.as_str(), run_id.as_str()],
        )? != 1
        {
            return Err(Error::Conflict("lease changed during recovery"));
        }
        tx.commit()?;
        Ok(settled)
    }
    pub fn remove_session(&self, id: &Id) -> Result<bool> {
        {
            let managed = self.managed_guard()?;
            if let Some(managed) = managed.as_ref()
                && managed.has_active_session(id)?
            {
                return Err(Error::Conflict("session belongs to an active managed task"));
            }
        }
        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let active: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM runs WHERE session=?1 AND phase!='settled')",
            [id.as_str()],
            |row| row.get(0),
        )?;
        if active {
            return Err(Error::Conflict("session has an unsettled run"));
        }
        let removed = tx.execute("DELETE FROM sessions WHERE id=?1", [id.as_str()])? > 0;
        tx.commit()?;
        Ok(removed)
    }
    pub fn prune_candidates(&self, before: u64, limit: usize) -> Result<Vec<Id>> {
        if !(1..=1000).contains(&limit) {
            return Err(xcb_core::Error::Invalid("prune limit").into());
        }
        let db = self.db()?;
        let mut query = db.prepare("SELECT id FROM sessions WHERE last_active<?1 AND NOT EXISTS(SELECT 1 FROM runs WHERE session=sessions.id AND phase!='settled') ORDER BY last_active,id LIMIT ?2")?;
        let rows = query.query_map(params![sql(before)?, limit as i64], |row| {
            row.get::<_, String>(0)
        })?;
        let candidates = rows
            .map(|row| Ok(Id::new(row?)?))
            .collect::<Result<Vec<_>>>()?;
        drop(query);
        drop(db);
        // Do not hold native database custody while inspecting the managed
        // store. Paused questions and between-turn queues still need history.
        // One managed handle and one active-task scan serve the whole pass.
        let managed = self.managed_guard()?;
        match managed.as_ref() {
            Some(managed) => {
                let active = managed.active_session_ids()?;
                Ok(candidates
                    .into_iter()
                    .filter(|id| !active.contains(id))
                    .collect())
            }
            None => Ok(candidates),
        }
    }

    /// The cached managed handle, opened on first use when
    /// `managed/managed.sqlite` exists. A failed open is retried on the next
    /// call rather than remembered; a still-missing database leaves `None`
    /// and is re-probed cheaply each call. A read-only store opens a
    /// read-only reader: no migration, cleanup or wait on the supervisor
    /// lock, and an unreadable or older schema fails at once.
    fn managed_guard(&self) -> Result<MutexGuard<'_, Option<crate::managed::ManagedStore>>> {
        let mut managed = self
            .managed
            .lock()
            .map_err(|_| Error::Conflict("managed store lock poisoned"))?;
        if managed.is_none() && self.root.join("managed/managed.sqlite").try_exists()? {
            *managed = if self.read_only {
                crate::managed::ManagedStore::open_read_only(&self.root)?
            } else {
                Some(crate::managed::ManagedStore::open(&self.root)?)
            };
        }
        Ok(managed)
    }

    /// The cached handle's active-task scan count, for churn regression tests.
    #[cfg(test)]
    pub(crate) fn managed_active_scans(&self) -> Option<u64> {
        self.managed
            .lock()
            .ok()
            .and_then(|managed| managed.as_ref().map(|m| m.active_scan_count()))
    }
    pub fn set_models(&self, provider: Provider, choices: &[ModelChoice]) -> Result<()> {
        if choices.len() > 4096 {
            return Err(xcb_core::Error::Limit("models").into());
        }
        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute("DELETE FROM models WHERE provider=?1", [provider.as_str()])?;
        for choice in choices {
            choice.validate()?;
            if choice.provider != provider {
                return Err(Error::Conflict("model provider mismatch"));
            }
            tx.execute(
                "INSERT INTO models VALUES(?1,?2,?3)",
                params![
                    provider.as_str(),
                    choice.key(),
                    serde_json::to_string(choice)?
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
    /// Replace one account's own catalog. Other accounts' catalogs and the
    /// provider-wide fallback are untouched. An empty list removes the
    /// account's observation, so it falls back to the provider-wide catalog.
    pub fn set_account_models(&self, account: &Id, choices: &[ModelChoice]) -> Result<()> {
        if choices.len() > MAX_ACCOUNT_MODELS {
            return Err(xcb_core::Error::Limit("models").into());
        }
        let provider = self.account(account)?.provider;
        for choice in choices {
            choice.validate()?;
            if choice.provider != provider {
                return Err(Error::Conflict("model provider mismatch"));
            }
        }
        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "DELETE FROM account_models WHERE account=?1",
            [account.as_str()],
        )?;
        let others: i64 =
            tx.query_row("SELECT count(*) FROM account_models", [], |row| row.get(0))?;
        if others as usize + choices.len() > MAX_CATALOG_ROWS {
            return Err(xcb_core::Error::Limit("models").into());
        }
        for choice in choices {
            tx.execute(
                "INSERT OR REPLACE INTO account_models VALUES(?1,?2,?3)",
                params![
                    account.as_str(),
                    choice.key(),
                    serde_json::to_string(choice)?
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
    /// Every model some account can use: each account's own catalog, plus
    /// the provider-wide fallback for any provider with an account (or no
    /// account at all) that still relies on it. One row per model key.
    pub fn models(&self) -> Result<Vec<ModelChoice>> {
        Ok(self.model_catalog()?.union())
    }
    /// The catalog `account` routes with: its own observation, else the
    /// provider-wide fallback.
    pub fn account_models(&self, account: &Id) -> Result<Vec<ModelChoice>> {
        let account = self.account(account)?;
        Ok(self
            .model_catalog()?
            .for_account(&account.id, account.provider)
            .to_vec())
    }
    pub fn model_catalog(&self) -> Result<ModelCatalog> {
        let accounts = self
            .accounts()?
            .into_iter()
            .map(|account| (account.id, account.provider))
            .collect();
        let db = self.db()?;
        let mut query = db.prepare("SELECT payload FROM models ORDER BY provider,id LIMIT 4097")?;
        let rows = query.query_map([], |row| row.get::<_, String>(0))?;
        let mut fallback = Vec::new();
        for row in rows {
            let choice: ModelChoice = decode(&row?)?;
            choice.validate()?;
            fallback.push(choice);
        }
        if fallback.len() > MAX_ACCOUNT_MODELS {
            return Err(xcb_core::Error::Limit("models").into());
        }
        let mut observed: BTreeMap<Id, Vec<ModelChoice>> = BTreeMap::new();
        // A read-only handle on an older database may predate the table.
        let available: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='account_models')",
            [],
            |row| row.get(0),
        )?;
        if available {
            let mut query = db.prepare(
                "SELECT account, payload FROM account_models ORDER BY account,id LIMIT ?1",
            )?;
            let rows = query.query_map([MAX_CATALOG_ROWS as i64 + 1], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            let mut count = 0;
            for row in rows {
                let (account, payload) = row?;
                count += 1;
                if count > MAX_CATALOG_ROWS {
                    return Err(xcb_core::Error::Limit("models").into());
                }
                let choice: ModelChoice = decode(&payload)?;
                choice.validate()?;
                observed.entry(Id::new(account)?).or_default().push(choice);
            }
        }
        Ok(ModelCatalog {
            accounts,
            fallback,
            observed,
        })
    }
    pub fn record_usage(&self, observation: &UsageObservation) -> Result<()> {
        observation.counters.total()?;
        observation.model.validate()?;
        let session = self
            .session(&observation.session)?
            .ok_or(Error::Unavailable("session not found"))?;
        if session.account != observation.account
            || session.model.provider != observation.model.provider
        {
            return Err(Error::Conflict("usage binding mismatch"));
        }
        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let prior: Option<String> = tx
            .query_row(
                "SELECT payload FROM usage WHERE id=?1",
                [observation.id.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(prior) = prior {
            let prior: UsageObservation = decode(&prior)?;
            if prior.session != observation.session
                || prior.account != observation.account
                || prior.model.key() != observation.model.key()
                || !observation.counters.dominates(prior.counters)
                || observation.at_ms < prior.at_ms
            {
                return Err(Error::Conflict("usage revision is inconsistent"));
            }
        }
        tx.execute("INSERT INTO usage VALUES(?1,?2,?3,?4,?5) ON CONFLICT(id) DO UPDATE SET observed_at=excluded.observed_at,payload=excluded.payload", params![observation.id.as_str(), observation.session.as_str(), observation.account.as_str(), sql(observation.at_ms)?, serde_json::to_string(observation)?])?;
        tx.commit()?;
        Ok(())
    }
    pub fn usage(&self, session: Option<&Id>, limit: usize) -> Result<Vec<UsageObservation>> {
        if !(1..=2048).contains(&limit) {
            return Err(xcb_core::Error::Invalid("usage page limit").into());
        }
        let db = self.db()?;
        let mut query = db.prepare("SELECT payload FROM usage WHERE (?1 IS NULL OR session=?1) ORDER BY observed_at DESC,id LIMIT ?2")?;
        let rows = query.query_map(params![session.map(Id::as_str), limit as i64], |row| {
            row.get::<_, String>(0)
        })?;
        let mut observations = Vec::new();
        for row in rows {
            let observation: UsageObservation = decode(&row?)?;
            observation.counters.total()?;
            observations.push(observation);
        }
        observations.reverse();
        Ok(observations)
    }
    /// Legacy/unbound telemetry. Native provider observations use the owned-run
    /// seam below so only fresh observations can acquire generation provenance.
    pub fn record_quota(&self, point: &QuotaPoint) -> Result<()> {
        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        insert_quota(&tx, QUOTAS, point)?;
        tx.commit()?;
        Ok(())
    }

    /// Derive the destination pool from the exact leased account, never from the
    /// incoming point. Adoption and insertion are one transaction; old/shared
    /// pools and their observations are preserved, not relabeled or copied.
    pub(crate) fn record_account_quota(&self, run: &RunRecord, point: &QuotaPoint) -> Result<()> {
        point.validate()?;
        self.record_bound_quota(run, QUOTAS, point.observed_at_ms, |_| Ok(point.clone()))
    }

    /// Record a usage-limit cooldown for the account the run holds: the
    /// provider refused a turn for this account's quota and reported no
    /// reset, so the account stays at a known limit until `until_ms` unless
    /// a provider-reported window observed later supersedes it (see
    /// `xcb_core::usage::quota_blocked_until`). The same hold, pool, and
    /// credential-generation rules apply as to a provider-reported meter.
    /// Model-scoped limits are not recorded here: admission is per account,
    /// and a model-specific window never implies account scope.
    pub(crate) fn record_quota_limit(
        &self,
        run: &RunRecord,
        observed_at_ms: u64,
        until_ms: u64,
    ) -> Result<()> {
        if until_ms <= observed_at_ms {
            return Err(xcb_core::Error::Invalid("quota cooldown").into());
        }
        self.record_bound_quota(run, QUOTA_LIMITS, observed_at_ms, |account| {
            Ok(QuotaPoint {
                pool: account.quota_pool.clone(),
                window: Id::new(xcb_core::usage::limit_window(account.provider))?,
                used_percent: 100.0,
                resets_at_ms: until_ms,
                observed_at_ms,
            })
        })
    }

    /// The cooldowns recorded for a pool, oldest first.
    pub fn quota_limits(&self, pool: &Id) -> Result<Vec<QuotaPoint>> {
        let db = self.db()?;
        quota_limits_from(&db, pool)
    }

    fn record_bound_quota(
        &self,
        run: &RunRecord,
        table: &str,
        observed_at_ms: u64,
        point_for: impl FnOnce(&Account) -> Result<QuotaPoint>,
    ) -> Result<()> {
        if observed_at_ms < run.created_at_ms {
            return Err(Error::Conflict(
                "quota observation predates its account lease",
            ));
        }
        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        self.owned_run_from(&tx, run)?;
        let payload: String = tx.query_row(
            "SELECT payload FROM accounts WHERE id=?1",
            [run.account.as_str()],
            |row| row.get(0),
        )?;
        let mut account: Account = decode(&payload)?;
        account.validate()?;
        if account.id != run.account {
            return Err(Error::Conflict("account identity changed"));
        }
        let pool = generation_pool(&self.root, &account)?;
        if let Some(pool) = &pool {
            account.quota_pool = pool.clone();
        }
        let mut point = point_for(&account)?;
        point.pool = account.quota_pool.clone();
        insert_quota(&tx, table, &point)?;
        if generation_pool(&self.root, &account)? != pool {
            return Err(Error::Conflict("account credential generation changed"));
        }
        self.owned_run_from(&tx, run)?;
        if tx.execute(
            "UPDATE accounts SET payload=?1 WHERE id=?2 AND payload=?3",
            params![
                serde_json::to_string(&account)?,
                account.id.as_str(),
                payload
            ],
        )? != 1
        {
            return Err(Error::Conflict("account identity changed"));
        }
        tx.commit()?;
        Ok(())
    }

    /// Batched observability checkpoint for the streaming path: pending
    /// velocity samples and quota observations land in ONE immediate
    /// transaction instead of a commit per event. Run custody and credential
    /// generation checks fail the batch exactly as the per-event recorders
    /// do. Telemetry that contradicts what is stored — a sample that would
    /// regress the velocity meter or an observation stamped before its run
    /// began (a backward clock step), or a different quota payload at an
    /// instant already recorded — is dropped and counted instead: stored
    /// rows stay monotonic and are never rewritten, and a meter disagreement
    /// never decides the outcome of the turn that reported it.
    pub(crate) fn record_observations(
        &self,
        run: &RunRecord,
        session: &Id,
        samples: &[xcb_core::usage::VelocitySample],
        observations: &[PendingQuota],
    ) -> Result<()> {
        for sample in samples {
            if sample.output_tokens > xcb_core::usage::COUNTER_LIMIT {
                return Err(xcb_core::Error::Limit("velocity counter").into());
            }
        }
        if samples.is_empty() && observations.is_empty() {
            return Ok(());
        }
        let mut dropped = 0u64;
        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if !samples.is_empty() {
            let mut previous: Option<(i64, i64)> = tx.query_row("SELECT at_ms,output_total FROM velocity WHERE session=?1 ORDER BY at_ms DESC LIMIT 1", [session.as_str()], |row| Ok((row.get(0)?, row.get(1)?))).optional()?;
            for sample in samples {
                if previous.is_some_and(|(at, count)| {
                    at > sample.at_ms as i64 || count > sample.output_tokens as i64
                }) {
                    dropped += 1;
                    continue;
                }
                tx.execute("INSERT INTO velocity VALUES(?1,?2,?3) ON CONFLICT(session,at_ms) DO UPDATE SET output_total=excluded.output_total", params![session.as_str(), sql(sample.at_ms)?, sql(sample.output_tokens)?])?;
                previous = Some((sample.at_ms as i64, sample.output_tokens as i64));
            }
            tx.execute("DELETE FROM velocity WHERE session=?1 AND at_ms NOT IN (SELECT at_ms FROM velocity WHERE session=?1 ORDER BY at_ms DESC LIMIT 2048)", [session.as_str()])?;
        }
        if !observations.is_empty() {
            self.owned_run_from(&tx, run)?;
            let payload: String = tx.query_row(
                "SELECT payload FROM accounts WHERE id=?1",
                [run.account.as_str()],
                |row| row.get(0),
            )?;
            let mut account: Account = decode(&payload)?;
            account.validate()?;
            if account.id != run.account {
                return Err(Error::Conflict("account identity changed"));
            }
            let pool = generation_pool(&self.root, &account)?;
            if let Some(pool) = &pool {
                account.quota_pool = pool.clone();
            }
            for observation in observations {
                let point = QuotaPoint {
                    pool: account.quota_pool.clone(),
                    window: observation.window.clone(),
                    used_percent: observation.used_percent,
                    observed_at_ms: observation.observed_at_ms,
                    resets_at_ms: observation.resets_at_ms,
                };
                // Malformed meter updates are dropped like the per-event path
                // drops them; they never reach the table.
                if point.validate().is_err() {
                    continue;
                }
                // Stamped before this lease began: freshness for this
                // account cannot be shown, so it is never attributed to it.
                // The first payload stored at an instant stays; a different
                // one at the same instant is dropped.
                if observation.observed_at_ms < run.created_at_ms
                    || !store_quota(&tx, QUOTAS, &point)?
                {
                    dropped += 1;
                }
            }
            if generation_pool(&self.root, &account)? != pool {
                return Err(Error::Conflict("account credential generation changed"));
            }
            self.owned_run_from(&tx, run)?;
            if tx.execute(
                "UPDATE accounts SET payload=?1 WHERE id=?2 AND payload=?3",
                params![
                    serde_json::to_string(&account)?,
                    account.id.as_str(),
                    payload
                ],
            )? != 1
            {
                return Err(Error::Conflict("account identity changed"));
            }
        }
        tx.commit()?;
        if dropped > 0 {
            self.dropped_observations
                .fetch_add(dropped, std::sync::atomic::Ordering::Relaxed);
        }
        #[cfg(test)]
        self.observation_commits
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    /// Telemetry observations the batched recorder dropped since this handle
    /// opened (see `record_observations`).
    pub fn dropped_observations(&self) -> u64 {
        self.dropped_observations
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn quota_blocked_until(&self, id: &Id, now: u64) -> Result<Option<u64>> {
        let db = self.db()?;
        let payload: String = db.query_row(
            "SELECT payload FROM accounts WHERE id=?1",
            [id.as_str()],
            |row| row.get(0),
        )?;
        let account: Account = decode(&payload)?;
        account.validate()?;
        if &account.id != id {
            return Err(Error::Conflict("account identity changed"));
        }
        blocked_until_from(&db, &self.root, &account, now)
    }

    /// Fresh spending pressure follows the same account/credential binding
    /// as quota admission. Old identity telemetry cannot improve a route.
    pub fn quota_spending_pressure(
        &self,
        id: &Id,
        now: u64,
    ) -> Result<Option<xcb_core::usage::QuotaSpendingPressure>> {
        let db = self.db()?;
        let payload: String = db.query_row(
            "SELECT payload FROM accounts WHERE id=?1",
            [id.as_str()],
            |row| row.get(0),
        )?;
        let account: Account = decode(&payload)?;
        account.validate()?;
        if &account.id != id {
            return Err(Error::Conflict("account identity changed"));
        }
        Ok(
            current_quota_points_from(&db, &self.root, &account)?.and_then(|points| {
                xcb_core::usage::quota_spending_pressure(
                    &points,
                    &account.quota_pool,
                    account.provider,
                    now,
                )
            }),
        )
    }

    pub(crate) fn require_quota_available(&self, id: &Id, now: u64) -> Result<()> {
        if self.quota_blocked_until(id, now)?.is_some() {
            return Err(Error::Unavailable(
                "account quota exhausted until its reported reset; inspect xcb accounts list or refresh account metadata",
            ));
        }
        Ok(())
    }
    pub fn select_pane(&self, id: &Id, pane: &Id) -> Result<()> {
        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut session = session_from(&tx, id)?.ok_or(Error::Unavailable("session not found"))?;
        let expected = session.revision;
        session.revision = expected
            .checked_add(1)
            .ok_or(Error::Conflict("revision overflow"))?;
        session.pane = pane.clone();
        update_session(&tx, &session, expected)?;
        tx.commit()?;
        Ok(())
    }
    pub fn rebind(&self, id: &Id, expected: u64, account: &Id, model: ModelChoice) -> Result<()> {
        let target = self.account(account)?;
        model.validate()?;
        if !target.enabled || target.provider != model.provider {
            return Err(Error::Conflict("target account mismatch"));
        }
        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut session = session_from(&tx, id)?.ok_or(Error::Unavailable("session not found"))?;
        let held: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM runs WHERE session=?1 AND phase!='settled')",
            [id.as_str()],
            |row| row.get(0),
        )?;
        if held || session.revision != expected {
            return Err(Error::Conflict("session is busy or changed"));
        }
        if !session.requirements.allows(model.provider) {
            return Err(Error::Conflict(
                "this task requires Codex; this session cannot move to another provider",
            ));
        }
        session.account = account.clone();
        session.model = model;
        session.revision = expected
            .checked_add(1)
            .ok_or(Error::Conflict("revision overflow"))?;
        session.state = State::Idle;
        update_session(&tx, &session, expected)?;
        tx.execute(
            "UPDATE sessions SET account=?1 WHERE id=?2",
            params![account.as_str(), id.as_str()],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub(crate) fn begin_tool(
        &self,
        run: &RunRecord,
        call: &str,
        operation: &str,
        input_digest: &str,
    ) -> Result<()> {
        label(call, 160)?;
        let db = self.db()?;
        let held: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM leases WHERE account=?1 AND run=?2)",
            params![run.account.as_str(), run.id.as_str()],
            |row| row.get(0),
        )?;
        if !held {
            return Err(Error::Conflict("tool run no longer owns the account"));
        }
        db.execute(
            "INSERT INTO tool_effects(run,call,operation,input_digest) VALUES(?1,?2,?3,?4)",
            params![run.id.as_str(), call, operation, input_digest],
        )?;
        Ok(())
    }
    /// Idempotent receipt cleanup for an independently proven unstarted run.
    /// Keep authority and prepared/no-pid checks in the same transaction as the
    /// update; missing receipts are expected when launch preparation failed early.
    pub(crate) fn discard_unstarted_tool(&self, run: &RunRecord, call: &str) -> Result<()> {
        label(call, 160)?;
        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let payload: Option<String> = tx.query_row(
            "SELECT payload FROM runs WHERE id=?1 AND account=?2 AND phase='prepared' AND EXISTS(SELECT 1 FROM leases WHERE run=?1 AND account=?2)",
            params![run.id.as_str(), run.account.as_str()], |row| row.get(0),
        ).optional()?;
        let current: RunRecord =
            decode(&payload.ok_or(Error::Conflict("unstarted run authority changed"))?)?;
        if current.pid.is_some()
            || current.phase != "prepared"
            || current.account != run.account
            || !current.owner.as_ref().is_some_and(|owner| {
                owner.instance == self.instance && owner.pid == std::process::id()
            })
        {
            return Err(Error::Conflict("unstarted run proof changed"));
        }
        tx.execute(
            "UPDATE tool_effects SET settled=1 WHERE run=?1 AND call=?2",
            params![run.id.as_str(), call],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub(crate) fn settle_tool(&self, run: &RunRecord, call: &str) -> Result<()> {
        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        settle_tool_in(&tx, run, call)?;
        tx.commit()?;
        Ok(())
    }
    pub fn record_velocity(
        &self,
        session: &Id,
        sample: xcb_core::usage::VelocitySample,
    ) -> Result<()> {
        if sample.output_tokens > xcb_core::usage::COUNTER_LIMIT {
            return Err(xcb_core::Error::Limit("velocity counter").into());
        }
        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let previous: Option<(i64, i64)> = tx.query_row("SELECT at_ms,output_total FROM velocity WHERE session=?1 ORDER BY at_ms DESC LIMIT 1", [session.as_str()], |row| Ok((row.get(0)?, row.get(1)?))).optional()?;
        if previous.is_some_and(|(at, count)| {
            at > sample.at_ms as i64 || count > sample.output_tokens as i64
        }) {
            return Err(Error::Conflict("velocity counter regressed"));
        }
        tx.execute("INSERT INTO velocity VALUES(?1,?2,?3) ON CONFLICT(session,at_ms) DO UPDATE SET output_total=excluded.output_total", params![session.as_str(), sql(sample.at_ms)?, sql(sample.output_tokens)?])?;
        tx.execute("DELETE FROM velocity WHERE session=?1 AND at_ms NOT IN (SELECT at_ms FROM velocity WHERE session=?1 ORDER BY at_ms DESC LIMIT 2048)", [session.as_str()])?;
        tx.commit()?;
        Ok(())
    }
    pub fn velocities(
        &self,
        session: &Id,
        since: u64,
    ) -> Result<Vec<xcb_core::usage::VelocitySample>> {
        let db = self.db()?;
        let mut statement = db.prepare("SELECT at_ms,output_total FROM velocity WHERE session=?1 AND at_ms>=?2 ORDER BY at_ms LIMIT 2048")?;
        let rows = statement.query_map(params![session.as_str(), sql(since)?], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
        })?;
        rows.map(|row| {
            let (at, count) = row?;
            Ok(xcb_core::usage::VelocitySample {
                at_ms: u64::try_from(at).map_err(|_| Error::Conflict("stored velocity time"))?,
                output_tokens: u64::try_from(count)
                    .map_err(|_| Error::Conflict("stored velocity counter"))?,
            })
        })
        .collect()
    }
    pub fn quotas(&self, pool: &Id) -> Result<Vec<QuotaPoint>> {
        let db = self.db()?;
        quotas_from(&db, pool)
    }

    /// The account's freshest remaining percentage: the minimum across the
    /// latest live observation in each window. `None` means usage is
    /// unmeasured, not that it is unlimited.
    pub fn remaining_percent(&self, pool: &Id, now: u64) -> Result<Option<f64>> {
        let mut by_window: BTreeMap<Id, Vec<QuotaPoint>> = BTreeMap::new();
        for point in self.quotas(pool)? {
            by_window
                .entry(point.window.clone())
                .or_default()
                .push(point);
        }
        Ok(by_window
            .values()
            .filter_map(|points| points.last())
            .filter(|point| point.fresh(now))
            .map(|point| 100.0 - point.used_percent)
            .reduce(f64::min))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use std::fs;
    use xcb_core::{
        Provider,
        models::{Mode, ModelChoice},
    };

    fn root() -> tempfile::TempDir {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join("work")).unwrap();
        directory
    }

    fn choice() -> ModelChoice {
        ModelChoice {
            provider: Provider::Claude,
            id: Id::new("claude-fable-5-1").unwrap(),
            label: "Fable 5.1".into(),
            mode: Mode::Fixed,
            resolved: None,
            effort: Some(Id::new("max").unwrap()),
            observed_at_ms: 1,
        }
    }

    fn orphaned(store: &Store, run: &RunRecord) -> RunRecord {
        let mut run = run.clone();
        run.owner.as_mut().unwrap().pid = i32::MAX as u32;
        store
            .db()
            .unwrap()
            .execute(
                "UPDATE runs SET payload=?1 WHERE id=?2",
                params![serde_json::to_string(&run).unwrap(), run.id.as_str()],
            )
            .unwrap();
        run
    }

    fn recovery_codex_auth(account: &str, access: &str) -> Vec<u8> {
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
    fn remove_account_deletes_record_and_credentials() {
        let directory = root();
        let state = xcb_core::canonical(directory.path()).unwrap().join("state");
        let store = Store::open(&state).unwrap();
        let account = store
            .add_account(Provider::Claude, "Subscription", 1, None)
            .unwrap();
        let credential = store
            .account_root(&account.id)
            .unwrap()
            .join("subscription-token");
        crate::private::create(&credential, b"stored-credential").unwrap();
        let account_path = state.join("accounts").join(account.id.as_str());

        let removed = store.remove_account(&account.id).unwrap();

        assert_eq!(removed.id, account.id);
        assert!(store.accounts().unwrap().is_empty());
        assert!(!account_path.exists());
        assert!(store.unsettled_runs().unwrap().is_empty());
    }

    #[test]
    fn remove_account_refuses_held_custody_without_releasing_it() {
        let directory = root();
        let state = xcb_core::canonical(directory.path()).unwrap().join("state");
        let store = Store::open(&state).unwrap();
        let account = store
            .add_account(Provider::Codex, "Subscription", 1, None)
            .unwrap();
        let account_path = state.join("accounts").join(account.id.as_str());
        let run = store.prepare_probe(&account.id, None, 2).unwrap();

        let error = store.remove_account(&account.id).unwrap_err();

        assert!(error.to_string().contains("unsettled run"));
        assert_eq!(store.accounts().unwrap().len(), 1);
        assert!(account_path.exists());
        assert_eq!(store.unsettled_runs().unwrap()[0].id, run.id);
        let held: bool = store
            .db()
            .unwrap()
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM leases WHERE account=?1)",
                [account.id.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert!(held);
    }

    #[test]
    fn codex_login_recovery_child_fixture() {
        let Ok(state) = std::env::var("XCB_SYNTHETIC_CODEX_RECOVERY_CHILD_STATE") else {
            return;
        };
        let store = Store::open(Path::new(&state)).unwrap();
        let account = store
            .add_account(Provider::Codex, "ChatGPT", 1, None)
            .unwrap();
        let original = recovery_codex_auth("original-account", "original-access");
        let persistent = store
            .account_root(&account.id)
            .unwrap()
            .join("profile/auth.json");
        crate::private::create(&persistent, &original).unwrap();
        crate::authentication_tests::fail_authentication(&store, &account.id);
        let run = store.prepare_probe(&account.id, None, 2).unwrap();
        let profile = store.root().join("runs/synthetic-other-account-login");
        let (executable, sha256) = crate::process::host_identity().unwrap();
        let pin = crate::process::Pin {
            provider: Provider::Codex,
            executable,
            sha256: sha256.clone(),
            version: "synthetic".into(),
            host_sha256: sha256,
            observed_at_ms: 2,
        };
        let plan = crate::auth::prepare_codex_login(&store, &run, &pin, &profile).unwrap();
        crate::private::create(
            &plan.credentials.profile().join("auth.json"),
            &recovery_codex_auth("different-account", "new-access"),
        )
        .unwrap();
        let started = store.mark_spawned(&run, i32::MAX as u32).unwrap();
        let (_, run_digest) = store.recovery_candidate(&started.id).unwrap().unwrap();
        assert!(store.recover_run(&started.id, &run_digest, 3).is_err());
        // Exiting this helper leaves the owned run and isolated credential
        // snapshot exactly as an interrupted interactive sign-in would.
    }

    #[test]
    fn ordinary_sessions_keep_legacy_json_until_a_route_requirement_is_set() {
        let dir = root();
        let base = xcb_core::canonical(dir.path()).unwrap();
        let store = Store::open(&base.join("state")).unwrap();
        let account = store.add_account(Provider::Claude, "Max", 1, None).unwrap();
        let session = store
            .create_session(&account.id, choice(), &base.join("work"), 2)
            .unwrap();
        let legacy = serde_json::to_value(&session).unwrap();
        assert!(legacy.get("requirements").is_none());
        assert!(legacy.get("route_pins").is_none());
        store
            .require_session_capabilities(
                &session.id,
                xcb_core::session::TaskRequirements {
                    signed_in_browser: true,
                    ..Default::default()
                },
            )
            .unwrap();
        store
            .set_session_route_pins(
                &session.id,
                xcb_core::session::RoutePins {
                    provider: Some(Provider::Claude),
                    ..Default::default()
                },
            )
            .unwrap();
        let updated = store.session(&session.id).unwrap().unwrap();
        let encoded = serde_json::to_value(&updated).unwrap();
        assert_eq!(encoded["requirements"]["signed_in_browser"], true);
        assert_eq!(encoded["route_pins"]["provider"], "claude");
        assert_eq!(updated.revision, session.revision);
    }

    // Codex sign-in recovery; provider sign-in is refused on Windows.

    #[cfg(unix)]
    #[test]
    fn stopped_login_with_another_identity_releases_lease_without_replacing_auth() {
        let dir = root();
        let state = xcb_core::canonical(dir.path()).unwrap().join("state");
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "store::tests::codex_login_recovery_child_fixture",
            ])
            .env("XCB_SYNTHETIC_CODEX_RECOVERY_CHILD_STATE", &state)
            .output()
            .unwrap();
        assert!(child.status.success());
        let store = Store::open(&state).unwrap();
        let account = store.accounts().unwrap().pop().unwrap();
        let original = recovery_codex_auth("original-account", "original-access");
        let persistent = store
            .account_root(&account.id)
            .unwrap()
            .join("profile/auth.json");
        let (run, run_digest) = store
            .recovery_candidate(&store.unsettled_runs().unwrap()[0].id)
            .unwrap()
            .unwrap();
        assert_eq!(run.phase, "running");
        assert!(store.authentication_required(&account.id).unwrap());

        store.recover_run(&run.id, &run_digest, 4).unwrap();
        assert!(store.unsettled_runs().unwrap().is_empty());
        assert_eq!(
            store
                .db()
                .unwrap()
                .query_row(
                    "SELECT count(*) FROM leases WHERE account=?1",
                    [account.id.as_str()],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0,
        );
        assert_eq!(
            store
                .db()
                .unwrap()
                .query_row(
                    "SELECT count(*) FROM tool_effects WHERE run=? AND settled=0",
                    [run.id.as_str()],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0,
        );
        assert_eq!(crate::private::read(&persistent, 65536).unwrap(), original);
        assert!(store.authentication_required(&account.id).unwrap());
    }

    fn quota_point(pool: &Id, window: &str, used: f64, observed: u64, reset: u64) -> QuotaPoint {
        QuotaPoint {
            pool: pool.clone(),
            window: Id::new(window).unwrap(),
            used_percent: used,
            observed_at_ms: observed,
            resets_at_ms: reset,
        }
    }

    #[test]
    fn terminal_outcomes_are_atomic_exact_and_legacy_safe() {
        use xcb_core::{
            policy::{EffectState, Terminal, TurnFacts},
            session::Role,
        };
        let directory = root();
        let base = xcb_core::canonical(directory.path()).unwrap();
        let state = base.join("state");
        let workspace = base.join("work");
        let store = Store::open(&state).unwrap();
        let account = store
            .add_account(Provider::Claude, "Fixture", 1, None)
            .unwrap();
        let session = store
            .create_session(&account.id, choice(), &workspace, 2)
            .unwrap();
        let input = Message {
            id: new_id("input"),
            role: Role::User,
            text: "finish the task".into(),
            attachments: vec![],
            at_ms: 3,
            provenance: None,
        };
        let session = store
            .append_message(&session.id, session.revision, &input)
            .unwrap();
        let run = store.prepare_run(&session.id, session.revision, 4).unwrap();
        let mut outcome = crate::runner::Outcome {
            tool_calls: Some(0),
            text_attention: false,
            diagnostic: None,
            text: "The turn limit interrupted the remaining work".into(),
            facts: TurnFacts {
                terminal: Terminal::TurnLimit,
                joined: true,
                effects: EffectState::None,
                pending_attention: false,
                failure: None,
            },
            state: State::Idle,
        };
        outcome.facts.joined = false;
        assert!(store.settle_outcome(&run, &input.id, &outcome, 5).is_err());
        assert_eq!(store.unsettled_runs().unwrap().len(), 1);
        outcome.facts.joined = true;
        assert!(
            store
                .settle_outcome(&run, &new_id("missing"), &outcome, 5)
                .is_err()
        );
        assert_eq!(store.unsettled_runs().unwrap().len(), 1);
        assert!(store.settled_outcome(&session.id, 0).unwrap().is_none());
        store.settle_outcome(&run, &input.id, &outcome, 6).unwrap();
        assert!(store.unsettled_runs().unwrap().is_empty());
        drop(store);

        let reader = Store::open_read_only(&state).unwrap();
        let recovered = reader.settled_outcome(&session.id, 0).unwrap().unwrap();
        assert_eq!(
            reader
                .latest_settled_outcome(&session.id)
                .unwrap()
                .unwrap()
                .text,
            recovered.text
        );
        assert_eq!(
            reader.settled_input_submission(&session.id, 0).unwrap(),
            None
        );
        assert_eq!(recovered.facts.terminal, Terminal::TurnLimit);
        assert_eq!(recovered.state, State::Idle);
        assert_eq!(recovered.text, outcome.text);
        assert!(reader.settled_outcome(&session.id, 1).unwrap().is_none());
        drop(reader);

        let store = Store::open(&state).unwrap();
        let current = store.session(&session.id).unwrap().unwrap();
        let next_input = Message {
            id: new_id("input"),
            at_ms: 7,
            ..input
        };
        let current = store
            .append_message(&session.id, current.revision, &next_input)
            .unwrap();
        assert!(
            store.settled_outcome(&session.id, 0).unwrap().is_none(),
            "a newer turn invalidates the old dispatch boundary"
        );
        assert!(store.latest_settled_outcome(&session.id).unwrap().is_none());
        let run = store.prepare_run(&session.id, current.revision, 8).unwrap();
        store.settle(&run, State::Idle, 9).unwrap();
        assert!(
            store.settled_outcome(&session.id, 1).unwrap().is_none(),
            "legacy Idle does not prove completion"
        );
        store
            .db()
            .unwrap()
            .execute_batch("DROP TABLE run_outcomes")
            .unwrap();
        drop(store);
        let reader = Store::open_read_only(&state).unwrap();
        assert!(reader.settled_outcome(&session.id, 1).unwrap().is_none());
        drop(reader);
        let reopened = Store::open(&state).unwrap();
        assert!(reopened.settled_outcome(&session.id, 1).unwrap().is_none());
    }

    #[test]
    fn quota_availability_adopts_only_new_observations_and_preserves_legacy_pool() {
        let dir = root();
        let base = xcb_core::canonical(dir.path()).unwrap();
        let store = Store::open(&base.join("state")).unwrap();
        let account = store
            .add_account(Provider::Claude, "Test", 1, None)
            .unwrap();
        let old = quota_point(&account.quota_pool, "seven_day", 100.0, 2, 9_000_000);
        store.record_quota(&old).unwrap();
        assert_eq!(
            store.quota_blocked_until(&account.id, 500_000).unwrap(),
            None
        );
        assert!(
            !store
                .account_root(&account.id)
                .unwrap()
                .join("application-generation.json")
                .exists()
        );
        let run = store.prepare_probe(&account.id, None, 3).unwrap();
        // Missing generations stay unbound; quota observation never creates one.
        store
            .record_account_quota(
                &run,
                &quota_point(&account.quota_pool, "five_hour", 100.0, 3, 2_000_000),
            )
            .unwrap();
        assert_eq!(
            store.account(&account.id).unwrap().quota_pool,
            account.quota_pool
        );
        assert_eq!(
            store.quota_blocked_until(&account.id, 500_000).unwrap(),
            None
        );
        crate::application_qualification::ensure_generation(&store, &run).unwrap();
        assert_eq!(
            store.quota_blocked_until(&account.id, 500_000).unwrap(),
            None
        );
        store
            .record_account_quota(
                &run,
                &quota_point(&account.quota_pool, "five_hour", 25.0, 4, 2_000_000),
            )
            .unwrap();
        let pool = store.account(&account.id).unwrap().quota_pool;
        assert_ne!(pool, account.quota_pool);
        assert_eq!(store.quotas(&pool).unwrap().len(), 1);
        assert_eq!(store.quotas(&account.quota_pool).unwrap().len(), 2);
        assert_eq!(
            store.quota_blocked_until(&account.id, 500_000).unwrap(),
            None
        );
        store.settle(&run, State::Idle, 5).unwrap();
    }

    #[test]
    fn quota_availability_rechecks_at_lease_acquisition_and_summary_keeps_stale_block() {
        let dir = root();
        let base = xcb_core::canonical(dir.path()).unwrap();
        let store = Store::open(&base.join("state")).unwrap();
        let account = store
            .add_account(Provider::Claude, "Test", 1, None)
            .unwrap();
        let session = store
            .create_session(&account.id, choice(), &base.join("work"), 2)
            .unwrap();
        assert_eq!(
            store.quota_blocked_until(&account.id, 500_000).unwrap(),
            None
        );
        // Another terminal records exhaustion between preflight and prepare_run.
        let other = Store::open(store.root()).unwrap();
        let run = other.prepare_probe(&account.id, None, 3).unwrap();
        crate::application_qualification::ensure_generation(&other, &run).unwrap();
        other
            .record_account_quota(
                &run,
                &quota_point(&account.quota_pool, "seven_day", 100.0, 3, 9_000_000),
            )
            .unwrap();
        other
            .record_account_quota(
                &run,
                &quota_point(&account.quota_pool, "five_hour", 50.0, 499_999, 2_000_000),
            )
            .unwrap();
        other.settle(&run, State::Idle, 500_000).unwrap();
        assert!(
            store
                .prepare_run(&session.id, session.revision, 500_000)
                .is_err()
        );
        assert!(store.unsettled_runs().unwrap().is_empty());
        assert_eq!(
            store.session(&session.id).unwrap().unwrap().revision,
            session.revision
        );
        let view =
            crate::summary::snapshot(&store, None, &crate::config::Config::default(), 500_000)
                .unwrap();
        assert_eq!(view.accounts[0].remaining_percent, Some(50.0));
        assert_eq!(view.accounts[0].quota_blocked_until_ms, Some(9_000_000));
        let probe = store.prepare_probe(&account.id, None, 500_001).unwrap();
        store
            .record_account_quota(
                &probe,
                &quota_point(&account.quota_pool, "seven_day", 5.0, 500_001, 10_000_000),
            )
            .unwrap();
        store.settle(&probe, State::Idle, 500_002).unwrap();
        let turn = store
            .prepare_run(&session.id, session.revision, 500_003)
            .unwrap();
        store.settle(&turn, State::Idle, 500_004).unwrap();
    }

    #[test]
    fn quota_availability_recording_requires_exact_owned_live_lease() {
        let dir = root();
        let base = xcb_core::canonical(dir.path()).unwrap();
        let store = Store::open(&base.join("state")).unwrap();
        let account = store
            .add_account(Provider::Claude, "Test", 1, None)
            .unwrap();
        let run = store.prepare_probe(&account.id, None, 2).unwrap();
        crate::application_qualification::ensure_generation(&store, &run).unwrap();
        let point = quota_point(&account.quota_pool, "five_hour", 100.0, 3, 1000);
        assert!(
            store
                .record_account_quota(
                    &run,
                    &quota_point(&account.quota_pool, "five_hour", 100.0, 1, 1000)
                )
                .is_err()
        );
        let foreign = Store::open(store.root()).unwrap();
        assert!(foreign.record_account_quota(&run, &point).is_err());
        let mut forged = run.clone();
        forged.revision += 1;
        assert!(store.record_account_quota(&forged, &point).is_err());
        assert_eq!(
            store.account(&account.id).unwrap().quota_pool,
            account.quota_pool
        );
        store.record_account_quota(&run, &point).unwrap();
        let pool = store.account(&account.id).unwrap().quota_pool;
        let mut conflict = point.clone();
        conflict.used_percent = 10.0;
        assert!(store.record_account_quota(&run, &conflict).is_err());
        assert_eq!(store.quotas(&pool).unwrap().len(), 1);
        store.settle(&run, State::Idle, 4).unwrap();
        assert!(store.record_account_quota(&run, &point).is_err());
        assert_eq!(
            store.quota_blocked_until(&account.id, 5).unwrap(),
            Some(1000)
        );
        assert_eq!(store.quota_blocked_until(&account.id, 1000).unwrap(), None);
    }

    #[test]
    fn quota_spending_pressure_follows_generation_and_provider_scope() {
        let dir = root();
        let store = Store::open(&xcb_core::canonical(dir.path()).unwrap().join("state")).unwrap();
        let claude = store
            .add_account(Provider::Claude, "Test", 1, None)
            .unwrap();
        let point = quota_point(&claude.quota_pool, "seven_day", 70.0, 3, 10_800_003);
        store.record_quota(&point).unwrap();
        assert_eq!(store.quota_spending_pressure(&claude.id, 3).unwrap(), None);
        let run = store.prepare_probe(&claude.id, None, 2).unwrap();
        crate::application_qualification::ensure_generation(&store, &run).unwrap();
        store.record_account_quota(&run, &point).unwrap();
        assert_eq!(
            store
                .quota_spending_pressure(&claude.id, 3)
                .unwrap()
                .unwrap()
                .percent_per_hour,
            10.0
        );
        store.settle(&run, State::Idle, 4).unwrap();
        let run = store.prepare_probe(&claude.id, None, 4).unwrap();
        crate::application_qualification::rotate_generation(&store, &run).unwrap();
        assert_eq!(store.quota_spending_pressure(&claude.id, 4).unwrap(), None);
        store.settle(&run, State::Idle, 5).unwrap();
        for provider in [Provider::Codex, Provider::Devin] {
            let account = store.add_account(provider, "Test", 1, None).unwrap();
            store
                .record_quota(&quota_point(
                    &account.quota_pool,
                    "codex.secondary",
                    70.0,
                    3,
                    10_800_003,
                ))
                .unwrap();
            let pressure = store.quota_spending_pressure(&account.id, 3).unwrap();
            assert_eq!(pressure.is_some(), provider == Provider::Codex);
            if let Some(pressure) = pressure {
                assert_eq!(pressure.percent_per_hour, 10.0);
            }
        }
    }

    #[test]
    fn quota_availability_generation_rotation_invalidates_without_copying_history() {
        let dir = root();
        let base = xcb_core::canonical(dir.path()).unwrap();
        let store = Store::open(&base.join("state")).unwrap();
        let account = store
            .add_account(Provider::Claude, "Test", 1, None)
            .unwrap();
        let run = store.prepare_probe(&account.id, None, 2).unwrap();
        let generation = crate::application_qualification::ensure_generation(&store, &run).unwrap();
        store
            .record_account_quota(
                &run,
                &quota_point(&account.quota_pool, "five_hour", 100.0, 3, 1000),
            )
            .unwrap();
        let old_pool = store.account(&account.id).unwrap().quota_pool;
        assert_eq!(
            crate::application_qualification::ensure_generation(&store, &run)
                .unwrap()
                .generation,
            generation.generation
        );
        assert_eq!(
            store.quota_blocked_until(&account.id, 4).unwrap(),
            Some(1000)
        );
        store.settle(&run, State::Idle, 4).unwrap();
        let run = store.prepare_probe(&account.id, None, 4).unwrap();
        crate::application_qualification::rotate_generation(&store, &run).unwrap();
        assert_eq!(store.quota_blocked_until(&account.id, 4).unwrap(), None);
        assert_eq!(store.account(&account.id).unwrap().quota_pool, old_pool);
        store
            .record_account_quota(&run, &quota_point(&old_pool, "five_hour", 20.0, 4, 1000))
            .unwrap();
        assert_ne!(store.account(&account.id).unwrap().quota_pool, old_pool);
        assert_eq!(store.quotas(&old_pool).unwrap()[0].used_percent, 100.0);
        assert_eq!(store.quota_blocked_until(&account.id, 5).unwrap(), None);
        store.settle(&run, State::Idle, 6).unwrap();
    }

    #[test]
    fn quota_availability_rejects_bad_generation_and_does_not_infer_other_scopes() {
        let dir = root();
        let base = xcb_core::canonical(dir.path()).unwrap();
        let store = Store::open(&base.join("state")).unwrap();
        for provider in Provider::SUPPORTED {
            let account = store.add_account(provider, "Test", 1, None).unwrap();
            let run = store.prepare_probe(&account.id, None, 2).unwrap();
            crate::application_qualification::ensure_generation(&store, &run).unwrap();
            let window = if provider == Provider::Claude {
                "seven_day_opus"
            } else {
                "five_hour"
            };
            store
                .record_account_quota(
                    &run,
                    &quota_point(&account.quota_pool, window, 100.0, 3, 1000),
                )
                .unwrap();
            assert_eq!(store.quota_blocked_until(&account.id, 4).unwrap(), None);
            let path = store
                .account_root(&account.id)
                .unwrap()
                .join("application-generation.json");
            let original = private::read(&path, 1024).unwrap();
            private::replace(&path, b"{}", &digest(original)).unwrap();
            if provider == Provider::Claude {
                assert!(store.quota_blocked_until(&account.id, 4).is_err());
                assert!(
                    store
                        .record_account_quota(
                            &run,
                            &quota_point(&account.quota_pool, "five_hour", 100.0, 4, 1000)
                        )
                        .is_err()
                );
            } else {
                assert_eq!(store.quota_blocked_until(&account.id, 4).unwrap(), None);
            }
            store.settle(&run, State::Idle, 5).unwrap();
        }
    }

    #[test]
    fn usage_limit_cooldown_blocks_until_it_ends_and_needs_the_held_account() {
        let dir = root();
        let base = xcb_core::canonical(dir.path()).unwrap();
        let store = Store::open(&base.join("state")).unwrap();
        for provider in Provider::SUPPORTED {
            let account = store.add_account(provider, "Test", 1, None).unwrap();
            let run = store.prepare_probe(&account.id, None, 2).unwrap();
            crate::application_qualification::ensure_generation(&store, &run).unwrap();
            // A cooldown needs the exact owned live hold, a positive span,
            // and an instant inside the hold, like a provider-reported meter.
            assert!(store.record_quota_limit(&run, 1, 1_801).is_err());
            assert!(store.record_quota_limit(&run, 3, 3).is_err());
            let foreign = Store::open(store.root()).unwrap();
            assert!(foreign.record_quota_limit(&run, 3, 1_803).is_err());
            let mut forged = run.clone();
            forged.revision += 1;
            assert!(store.record_quota_limit(&forged, 3, 1_803).is_err());
            assert_eq!(store.quota_blocked_until(&account.id, 4).unwrap(), None);
            store.record_quota_limit(&run, 3, 1_803).unwrap();
            let pool = store.account(&account.id).unwrap().quota_pool;
            assert_eq!(pool != account.quota_pool, provider == Provider::Claude);
            // The cooldown is admission state, never a meter: percentage,
            // reset, and runway projections do not see it.
            assert!(store.quotas(&pool).unwrap().is_empty());
            let limits = store.quota_limits(&pool).unwrap();
            assert_eq!(limits.len(), 1);
            assert_eq!(
                limits[0].window.as_str(),
                xcb_core::usage::limit_window(provider)
            );
            assert_eq!(store.remaining_percent(&pool, 4).unwrap(), None);
            assert_eq!(store.quota_blocked_until(&account.id, 2).unwrap(), None);
            assert_eq!(
                store.quota_blocked_until(&account.id, 3).unwrap(),
                Some(1_803)
            );
            assert_eq!(
                store.quota_blocked_until(&account.id, 1_802).unwrap(),
                Some(1_803)
            );
            assert_eq!(store.quota_blocked_until(&account.id, 1_803).unwrap(), None);
            assert_eq!(store.quota_spending_pressure(&account.id, 4).unwrap(), None);
            store.settle(&run, State::Idle, 5).unwrap();
            assert!(store.record_quota_limit(&run, 6, 1_806).is_err());
            let view = crate::summary::snapshot(&store, None, &crate::config::Config::default(), 4)
                .unwrap();
            let row = view
                .accounts
                .iter()
                .find(|row| row.id == account.id)
                .unwrap();
            assert_eq!(row.quota_blocked_until_ms, Some(1_803));
            assert_eq!(row.remaining_percent, None);
            assert_eq!(row.resets_at_ms, None);
        }
    }

    #[test]
    fn usage_limit_cooldown_yields_to_later_reported_windows_and_credential_rotation() {
        let dir = root();
        let base = xcb_core::canonical(dir.path()).unwrap();
        let store = Store::open(&base.join("state")).unwrap();
        let account = store.add_account(Provider::Codex, "Test", 1, None).unwrap();
        let run = store.prepare_probe(&account.id, None, 2).unwrap();
        store.record_quota_limit(&run, 3, 1_803).unwrap();
        assert_eq!(
            store.quota_blocked_until(&account.id, 4).unwrap(),
            Some(1_803)
        );
        // A provider-reported reset observed later wins even when shorter.
        store
            .record_account_quota(
                &run,
                &quota_point(&account.quota_pool, "codex.primary", 100.0, 5, 500),
            )
            .unwrap();
        assert_eq!(
            store.quota_blocked_until(&account.id, 6).unwrap(),
            Some(500)
        );
        assert_eq!(store.quota_blocked_until(&account.id, 500).unwrap(), None);
        // A newer cooldown after that report applies again until a meter
        // observed later shows capacity.
        store.record_quota_limit(&run, 600, 2_400).unwrap();
        assert_eq!(
            store.quota_blocked_until(&account.id, 601).unwrap(),
            Some(2_400)
        );
        store
            .record_account_quota(
                &run,
                &quota_point(&account.quota_pool, "codex.primary", 20.0, 700, 5_000),
            )
            .unwrap();
        assert_eq!(store.quota_blocked_until(&account.id, 701).unwrap(), None);
        // An exhausted window with a later reset still combines with a
        // cooldown recorded after it: the later reset wins.
        store
            .record_account_quota(
                &run,
                &quota_point(&account.quota_pool, "codex.secondary", 100.0, 800, 9_000),
            )
            .unwrap();
        store.record_quota_limit(&run, 900, 2_700).unwrap();
        assert_eq!(
            store.quota_blocked_until(&account.id, 901).unwrap(),
            Some(9_000)
        );
        store.settle(&run, State::Idle, 1_000).unwrap();

        // Claude cooldowns follow the credential generation like meters do.
        let claude = store
            .add_account(Provider::Claude, "Test", 1, None)
            .unwrap();
        let run = store.prepare_probe(&claude.id, None, 2).unwrap();
        crate::application_qualification::ensure_generation(&store, &run).unwrap();
        store.record_quota_limit(&run, 3, 1_803).unwrap();
        let bound = store.account(&claude.id).unwrap().quota_pool;
        assert_eq!(
            store.quota_blocked_until(&claude.id, 4).unwrap(),
            Some(1_803)
        );
        store.settle(&run, State::Idle, 4).unwrap();
        let run = store.prepare_probe(&claude.id, None, 5).unwrap();
        crate::application_qualification::rotate_generation(&store, &run).unwrap();
        assert_eq!(store.quota_blocked_until(&claude.id, 6).unwrap(), None);
        assert_eq!(store.quota_limits(&bound).unwrap().len(), 1);
        store.settle(&run, State::Idle, 7).unwrap();
    }

    #[test]
    fn read_only_discovery_reads_live_wal_without_initializing_or_writing() {
        let dir = root();
        let path = xcb_core::canonical(dir.path()).unwrap().join("state");
        let writer = Store::open(&path).unwrap();
        let account = writer
            .add_account(Provider::Claude, "Max", 1, None)
            .unwrap();
        fs::remove_file(path.join(".initialize.lock")).unwrap();
        let reader = Store::open_read_only(&path).unwrap();
        assert_eq!(reader.accounts().unwrap()[0].id, account.id);
        assert!(!path.join(".initialize.lock").exists());
        assert!(
            reader
                .db()
                .unwrap()
                .execute("DELETE FROM accounts", [])
                .is_err()
        );
        assert_eq!(writer.accounts().unwrap().len(), 1);
        let second = writer.add_account(Provider::Codex, "Pro", 2, None).unwrap();
        assert!(
            reader
                .accounts()
                .unwrap()
                .iter()
                .any(|row| row.id == second.id)
        );
    }

    #[test]
    fn account_catalogs_are_kept_apart_and_fall_back_to_the_provider_list() {
        let dir = root();
        let path = xcb_core::canonical(dir.path()).unwrap().join("state");
        let store = Store::open(&path).unwrap();
        let first = store.add_account(Provider::Devin, "Pro", 1, None).unwrap();
        let second = store.add_account(Provider::Devin, "Pro", 1, None).unwrap();
        let choice = |id: &str| ModelChoice {
            provider: Provider::Devin,
            id: Id::new(id).unwrap(),
            label: id.into(),
            mode: Mode::Fixed,
            resolved: None,
            effort: None,
            observed_at_ms: 1,
        };
        // Rows written before this change stay usable by every account.
        store
            .set_models(Provider::Devin, &[choice("legacy")])
            .unwrap();
        drop(store);
        let store = Store::open(&path).unwrap();
        for account in [&first.id, &second.id] {
            assert_eq!(
                store.account_models(account).unwrap(),
                vec![choice("legacy")]
            );
        }
        store
            .set_account_models(&first.id, &[choice("shared"), choice("first-only")])
            .unwrap();
        store
            .set_account_models(&second.id, &[choice("shared"), choice("second-only")])
            .unwrap();
        // The first account's next refresh keeps the second account's list.
        store
            .set_account_models(&first.id, &[choice("shared"), choice("first-only")])
            .unwrap();
        assert_eq!(
            store.account_models(&second.id).unwrap(),
            vec![choice("second-only"), choice("shared")]
        );
        let catalog = store.model_catalog().unwrap();
        assert!(!catalog.offers(&second.id, Provider::Devin, &choice("first-only")));
        let mut both = vec![first.id.clone(), second.id.clone()];
        both.sort();
        assert_eq!(catalog.accounts_offering(&choice("shared")), both);
        // Every account reported its own list, so the provider-wide list is
        // no longer offered.
        let keys: Vec<_> = store
            .models()
            .unwrap()
            .into_iter()
            .map(|model| model.id.to_string())
            .collect();
        assert_eq!(keys, ["first-only", "second-only", "shared"]);
        // A mismatched provider is refused and changes nothing.
        let mut codex = choice("codex-model");
        codex.provider = Provider::Codex;
        assert!(store.set_account_models(&first.id, &[codex]).is_err());
        assert_eq!(store.account_models(&first.id).unwrap().len(), 2);
        // Clearing an account's list returns it to the provider-wide list.
        store.set_account_models(&second.id, &[]).unwrap();
        assert_eq!(
            store.account_models(&second.id).unwrap(),
            vec![choice("legacy")]
        );
        let reader = Store::open_read_only(&path).unwrap();
        assert_eq!(reader.models().unwrap().len(), 3);
    }

    #[test]
    fn read_only_discovery_does_not_create_or_migrate_state() {
        let dir = root();
        let missing = xcb_core::canonical(dir.path()).unwrap().join("missing");
        assert!(Store::open_read_only(&missing).is_err());
        assert!(!missing.exists());
        let path = xcb_core::canonical(dir.path()).unwrap().join("state");
        let writer = Store::open(&path).unwrap();
        writer
            .db()
            .unwrap()
            .pragma_update(None, "user_version", 0)
            .unwrap();
        assert!(Store::open_read_only(&path).is_err());
        let version: u32 = writer
            .db()
            .unwrap()
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, 0);
    }

    #[test]
    fn recovery_rejects_live_original_owner_and_preserves_receipts() {
        let dir = root();
        let base = xcb_core::canonical(dir.path()).unwrap();
        let store = Store::open(&base.join("state")).unwrap();
        let account = store.add_account(Provider::Claude, "Max", 1, None).unwrap();
        let prepared = store.prepare_probe(&account.id, None, 2).unwrap();
        let running = store.mark_spawned(&prepared, i32::MAX as u32).unwrap();
        let digest = crate::digest(serde_json::to_string(&running).unwrap());
        assert!(store.recover_run(&running.id, &digest, 3).is_err());
        assert_eq!(store.unsettled_runs().unwrap().len(), 1);
        store
            .begin_tool(
                &running,
                "xcb_auth_settled",
                "host_auth_refresh",
                "synthetic",
            )
            .unwrap();
        store.settle_tool(&running, "xcb_auth_settled").unwrap();
        assert!(
            store.recover_run(&running.id, &digest, 3).is_err(),
            "even settled auth cannot override a living host"
        );
        store
            .begin_tool(&running, "xcb_auth_legacy", "host_auth_import", "synthetic")
            .unwrap();
        let running = orphaned(&store, &running);
        let digest = crate::digest(serde_json::to_string(&running).unwrap());
        assert!(store.recover_run(&running.id, &digest, 4).is_err());
        assert_eq!(store.unsettled_runs().unwrap().len(), 1);
        assert_eq!(
            store
                .db()
                .unwrap()
                .query_row(
                    "SELECT settled FROM tool_effects WHERE run=?1 AND call='xcb_auth_legacy'",
                    [running.id.as_str()],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
    }

    /// A refusal because a recorded process still exists says which
    /// process and what to check, and releases nothing.
    #[cfg(unix)]
    #[test]
    fn recovery_refusals_name_the_live_process_and_what_to_check() {
        use std::os::unix::process::CommandExt;
        let dir = root();
        let base = xcb_core::canonical(dir.path()).unwrap();
        let store = Store::open(&base.join("state")).unwrap();
        let account = store.add_account(Provider::Claude, "Max", 1, None).unwrap();
        let mut provider = std::process::Command::new("/bin/sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .unwrap();
        let group = provider.id();
        let prepared = store.prepare_probe(&account.id, None, 2).unwrap();
        let running = store.mark_spawned(&prepared, group).unwrap();
        // The owner (this process) is alive: the error names it.
        let owner = std::process::id();
        let text = running.verify_recovery_stop().unwrap_err().to_string();
        assert!(text.contains(&format!("process {owner}")), "{text}");
        assert!(text.contains(&format!("ps -p {owner}")), "{text}");
        assert!(text.contains("restart your computer"), "{text}");
        assert!(
            text.contains(&format!("xcb recover {} --yes", running.id)),
            "{text}"
        );
        // Owner gone, provider group still running: the group is named.
        let running = orphaned(&store, &running);
        let text = running.verify_recovery_stop().unwrap_err().to_string();
        assert!(text.contains(&format!("process group {group}")), "{text}");
        assert!(text.contains(&format!("pgrep -g {group}")), "{text}");
        let digest = crate::digest(serde_json::to_string(&running).unwrap());
        assert!(store.recover_run(&running.id, &digest, 3).is_err());
        assert_eq!(store.unsettled_runs().unwrap().len(), 1);
        provider.kill().unwrap();
        provider.wait().unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while running.verify_recovery_stop().is_err() {
            assert!(std::time::Instant::now() < deadline, "group never left");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn recovery_settles_running_run_and_marks_session_uncertain() {
        let dir = root();
        let base = xcb_core::canonical(dir.path()).unwrap();
        let store = Store::open(&base.join("state")).unwrap();
        let account = store.add_account(Provider::Claude, "Max", 1, None).unwrap();
        let session = store
            .create_session(&account.id, choice(), &base.join("work"), 2)
            .unwrap();
        let prepared = store.prepare_run(&session.id, session.revision, 3).unwrap();
        let running = orphaned(
            &store,
            &store.mark_spawned(&prepared, i32::MAX as u32).unwrap(),
        );
        let digest = digest(serde_json::to_string(&running).unwrap());

        let settled = store.recover_run(&running.id, &digest, 4).unwrap();

        assert_eq!(settled.phase, "settled");
        assert_eq!(settled.pid, Some(i32::MAX as u32));
        assert!(store.run(&running.id).unwrap().unwrap().phase == "settled");
        assert!(store.unsettled_runs().unwrap().is_empty());
        let session = store.session(&session.id).unwrap().unwrap();
        assert_eq!(session.state, State::Uncertain);
        assert_eq!(session.revision, 2);
        assert!(store.recover_run(&running.id, &digest, 5).is_err());
        assert_eq!(store.session(&session.id).unwrap().unwrap().revision, 2);
    }

    #[test]
    fn recovery_digest_binds_the_stored_serialization() {
        let dir = root();
        let base = xcb_core::canonical(dir.path()).unwrap();
        let store = Store::open(&base.join("state")).unwrap();
        let account = store.add_account(Provider::Claude, "Max", 1, None).unwrap();
        let session = store
            .create_session(&account.id, choice(), &base.join("work"), 2)
            .unwrap();
        let prepared = store.prepare_run(&session.id, session.revision, 3).unwrap();
        let running = orphaned(
            &store,
            &store.mark_spawned(&prepared, i32::MAX as u32).unwrap(),
        );
        let payload = format!(" {} ", serde_json::to_string(&running).unwrap());
        store
            .db()
            .unwrap()
            .execute(
                "UPDATE runs SET payload=?1 WHERE id=?2",
                params![payload, running.id.as_str()],
            )
            .unwrap();
        let (candidate, payload_digest) = store.recovery_candidate(&running.id).unwrap().unwrap();

        assert_eq!(candidate.id, running.id);
        assert_eq!(payload_digest, digest(payload.as_bytes()));
        assert!(store.recover_run(&running.id, &payload_digest, 4).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn prepared_recovery_retains_lease_without_independent_child_proof() {
        struct Child(std::process::Child);
        impl Drop for Child {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        for owner_kind in ["live", "missing", "absent"] {
            let dir = root();
            let base = xcb_core::canonical(dir.path()).unwrap();
            let store = Store::open(&base.join("state")).unwrap();
            let account = store.add_account(Provider::Claude, "Max", 1, None).unwrap();
            let session = store
                .create_session(&account.id, choice(), &base.join("work"), 2)
                .unwrap();
            let mut prepared = store.prepare_run(&session.id, session.revision, 3).unwrap();
            let prior_session = store.session(&session.id).unwrap().unwrap();
            // A child can exist before mark_spawned records its PID. Missing
            // owner metadata or an absent owner does not settle that child.
            let mut child = Child(
                std::process::Command::new("/bin/cat")
                    .stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::null())
                    .spawn()
                    .unwrap(),
            );
            match owner_kind {
                "missing" => prepared.owner = None,
                "absent" => prepared.owner.as_mut().unwrap().pid = i32::MAX as u32,
                _ => assert_eq!(prepared.owner.as_ref().unwrap().pid, std::process::id()),
            }
            let payload = serde_json::to_string(&prepared).unwrap();
            store
                .db()
                .unwrap()
                .execute(
                    "UPDATE runs SET payload=?1 WHERE id=?2",
                    params![payload, prepared.id.as_str()],
                )
                .unwrap();
            let run_digest = digest(payload.as_bytes());
            assert!(store.recover_run(&prepared.id, &run_digest, 4).is_err());
            assert!(child.0.try_wait().unwrap().is_none());
            assert_eq!(store.unsettled_runs().unwrap().len(), 1);
            assert!(store.prepare_probe(&account.id, None, 5).is_err());
            let unchanged = store.session(&session.id).unwrap().unwrap();
            assert_eq!(unchanged.state, prior_session.state);
            assert_eq!(unchanged.revision, prior_session.revision);
        }
    }

    #[test]
    fn recovery_rejects_already_settled_run() {
        let dir = root();
        let base = xcb_core::canonical(dir.path()).unwrap();
        let store = Store::open(&base.join("state")).unwrap();
        let account = store.add_account(Provider::Claude, "Max", 1, None).unwrap();
        let session = store
            .create_session(&account.id, choice(), &base.join("work"), 2)
            .unwrap();
        let prepared = store.prepare_run(&session.id, session.revision, 3).unwrap();
        store
            .settle(&prepared, State::Failed, 4)
            .expect("settle prepared run");
        let digest = digest(serde_json::to_string(&prepared).unwrap());
        assert!(store.recover_run(&prepared.id, &digest, 5).is_err());
        assert_eq!(store.unsettled_runs().unwrap().len(), 0);
    }

    #[test]
    fn recovery_rejects_run_that_changed_since_proof() {
        let dir = root();
        let base = xcb_core::canonical(dir.path()).unwrap();
        let store = Store::open(&base.join("state")).unwrap();
        let account = store.add_account(Provider::Claude, "Max", 1, None).unwrap();
        let session = store
            .create_session(&account.id, choice(), &base.join("work"), 2)
            .unwrap();
        let prepared = store.prepare_run(&session.id, session.revision, 3).unwrap();
        let running = store.mark_spawned(&prepared, 12345).unwrap();
        assert!(store.recover_run(&running.id, "not-the-digest", 4).is_err());
        assert_eq!(store.run(&running.id).unwrap().unwrap().phase, "running");
    }

    #[test]
    fn recovery_rejects_run_when_lease_is_absent() {
        let dir = root();
        let base = xcb_core::canonical(dir.path()).unwrap();
        let store = Store::open(&base.join("state")).unwrap();
        let account = store.add_account(Provider::Claude, "Max", 1, None).unwrap();
        let session = store
            .create_session(&account.id, choice(), &base.join("work"), 2)
            .unwrap();
        let prepared = store.prepare_run(&session.id, session.revision, 3).unwrap();
        let running = store.mark_spawned(&prepared, 12345).unwrap();
        let digest = digest(serde_json::to_string(&running).unwrap());
        store
            .db()
            .unwrap()
            .execute("DELETE FROM leases WHERE run=?1", [running.id.as_str()])
            .unwrap();
        assert!(store.recover_run(&running.id, &digest, 4).is_err());
        assert_eq!(store.run(&running.id).unwrap().unwrap().phase, "running");
    }

    #[test]
    fn run_record_roundtrip_persists_session_model() {
        let dir = root();
        let base = xcb_core::canonical(dir.path()).unwrap();
        let store = Store::open(&base.join("state")).unwrap();
        let account = store.add_account(Provider::Claude, "Max", 1, None).unwrap();
        let session = store
            .create_session(&account.id, choice(), &base.join("work"), 2)
            .unwrap();
        let expected = session.model.clone();
        let prepared = store.prepare_run(&session.id, session.revision, 3).unwrap();
        assert_eq!(prepared.model, Some(expected.clone()));
        let running = store.mark_spawned(&prepared, 12345).unwrap();
        assert_eq!(running.model, Some(expected.clone()));
        store.settle(&running, State::Idle, 4).unwrap();
        let settled = store.run(&running.id).unwrap().unwrap();
        assert_eq!(settled.model.as_ref().unwrap().id, expected.id);
        assert_eq!(settled.model.as_ref().unwrap().provider, expected.provider);
        assert_eq!(settled.model.as_ref().unwrap().effort, expected.effort);
        assert!(store.unsettled_runs().unwrap().is_empty());
    }

    #[test]
    fn rebind_after_settlement_preserves_run_record_model() {
        let dir = root();
        let base = xcb_core::canonical(dir.path()).unwrap();
        let store = Store::open(&base.join("state")).unwrap();
        let personal = store.add_account(Provider::Claude, "Max", 1, None).unwrap();
        let work = store
            .add_account(Provider::Claude, "Team", 1, None)
            .unwrap();
        let original = choice();
        let session = store
            .create_session(&personal.id, original.clone(), &base.join("work"), 2)
            .unwrap();
        let run = store.prepare_run(&session.id, session.revision, 3).unwrap();
        store.settle(&run, State::Idle, 4).unwrap();
        let next_model = ModelChoice {
            provider: Provider::Claude,
            id: Id::new("claude-sonnet-4").unwrap(),
            label: "Sonnet 4".into(),
            mode: Mode::Fixed,
            resolved: None,
            effort: Some(Id::new("high").unwrap()),
            observed_at_ms: 5,
        };
        let current = store.session(&session.id).unwrap().unwrap();
        store
            .rebind(&session.id, current.revision, &work.id, next_model.clone())
            .unwrap();
        let settled = store.run(&run.id).unwrap().unwrap();
        assert_eq!(settled.account, personal.id);
        assert_eq!(settled.model, Some(original));
        let rebound = store.session(&session.id).unwrap().unwrap();
        assert_eq!(rebound.account, work.id);
        assert_eq!(rebound.model.id, next_model.id);
    }

    #[test]
    fn custody_version_defaults_legacy_records_and_is_always_serialized() {
        let dir = root();
        let base = xcb_core::canonical(dir.path()).unwrap();
        let store = Store::open(&base.join("state")).unwrap();
        let account = store.add_account(Provider::Claude, "Max", 1, None).unwrap();
        let session = store
            .create_session(&account.id, choice(), &base.join("work"), 2)
            .unwrap();
        let run_id = Id::new("r_legacy001").unwrap();
        let payload = format!(
            "{{\"id\":\"{}\",\"session\":\"{}\",\"account\":\"{}\",\"revision\":1,\"phase\":\"running\",\"pid\":12345,\"created_at_ms\":1}}",
            run_id, session.id, account.id
        );
        store
            .db()
            .unwrap()
            .execute(
                "INSERT INTO runs(id,session,account,phase,payload) VALUES(?1,?2,?3,'running',?4)",
                params![
                    run_id.as_str(),
                    session.id.as_str(),
                    account.id.as_str(),
                    payload
                ],
            )
            .unwrap();
        let run = store.run(&run_id).unwrap().unwrap();
        assert_eq!(run.model, None);
        assert_eq!(run.phase, "running");
        assert_eq!(run.pid, Some(12345));
        assert_eq!(run.custody_version, 0);
        run.validate().unwrap();
        assert_eq!(serde_json::to_value(&run).unwrap()["custody_version"], 0);
    }

    #[test]
    fn custody_version_stamps_new_runs_and_rejects_older_readers() {
        // This is the complete pre-custody-version decoder. An already-open
        // older Store must reject the payload without reopening the database.
        #[derive(Debug, Deserialize)]
        #[serde(deny_unknown_fields)]
        #[allow(dead_code)]
        struct OldRunRecord {
            id: Id,
            session: Option<Id>,
            account: Id,
            revision: u64,
            phase: String,
            pid: Option<u32>,
            created_at_ms: u64,
            #[serde(default)]
            model: Option<ModelChoice>,
            #[serde(default)]
            owner: Option<RunOwner>,
        }

        let dir = root();
        let base = xcb_core::canonical(dir.path()).unwrap();
        let store = Store::open(&base.join("state")).unwrap();
        let account = store.add_account(Provider::Claude, "Max", 1, None).unwrap();
        let session = store
            .create_session(&account.id, choice(), &base.join("work"), 2)
            .unwrap();
        let run = store.prepare_run(&session.id, session.revision, 3).unwrap();
        store.settle(&run, State::Idle, 4).unwrap();
        let probe = store.prepare_probe(&account.id, None, 5).unwrap();
        for record in [run, probe] {
            assert_eq!(record.custody_version, 1);
            let stored = store.run(&record.id).unwrap().unwrap();
            assert_eq!(stored.custody_version, 1);
            let mut payload = serde_json::to_value(&stored).unwrap();
            let error = serde_json::from_value::<OldRunRecord>(payload.clone()).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("unknown field `custody_version`")
            );
            assert_eq!(
                payload.as_object_mut().unwrap().remove("custody_version"),
                Some(1.into())
            );
            serde_json::from_value::<OldRunRecord>(payload.clone()).unwrap();
            let legacy: RunRecord = serde_json::from_value(payload).unwrap();
            legacy.validate().unwrap();
            assert_eq!(legacy.custody_version, 0);
            let reserialized = serde_json::to_value(&legacy).unwrap();
            assert_eq!(reserialized["custody_version"], 0);
            assert!(serde_json::from_value::<OldRunRecord>(reserialized).is_err());
        }
    }

    #[test]
    fn custody_version_unknown_rejects_reads_and_recovery_without_releasing_lease() {
        let dir = root();
        let base = xcb_core::canonical(dir.path()).unwrap();
        let store = Store::open(&base.join("state")).unwrap();
        let account = store.add_account(Provider::Claude, "Max", 1, None).unwrap();
        let prepared = store.prepare_probe(&account.id, None, 2).unwrap();
        let mut running = orphaned(
            &store,
            &store.mark_spawned(&prepared, i32::MAX as u32).unwrap(),
        );
        running.verify_recovery_stop().unwrap();
        running.custody_version = 2;
        assert!(running.validate().is_err());
        let payload = serde_json::to_string(&running).unwrap();
        store
            .db()
            .unwrap()
            .execute(
                "UPDATE runs SET payload=?1 WHERE id=?2",
                params![payload, running.id.as_str()],
            )
            .unwrap();
        assert!(store.run(&running.id).is_err());
        assert!(store.unsettled_runs().is_err());
        assert!(store.recovery_candidate(&running.id).is_err());
        assert!(
            store
                .recover_run(&running.id, &digest(payload.as_bytes()), 3)
                .is_err()
        );
        let held: bool = store
            .db()
            .unwrap()
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM leases WHERE account=?1 AND run=?2)",
                params![account.id.as_str(), running.id.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert!(held);
    }

    #[test]
    fn probe_run_records_model_or_none() {
        let dir = root();
        let base = xcb_core::canonical(dir.path()).unwrap();
        let store = Store::open(&base.join("state")).unwrap();
        let account = store.add_account(Provider::Claude, "Max", 1, None).unwrap();
        let model = choice();
        let run = store
            .prepare_probe(&account.id, Some(model.clone()), 1)
            .unwrap();
        assert_eq!(run.model, Some(model.clone()));
        store.settle(&run, State::Idle, 2).unwrap();
        let stored = store.run(&run.id).unwrap().unwrap();
        assert_eq!(stored.model, Some(model));
        let unresolved = store.prepare_probe(&account.id, None, 3).unwrap();
        assert_eq!(unresolved.model, None);
        let devin = ModelChoice {
            provider: Provider::Devin,
            id: Id::new("x").unwrap(),
            label: "X".into(),
            mode: Mode::Fixed,
            resolved: None,
            effort: None,
            observed_at_ms: 1,
        };
        assert!(store.prepare_probe(&account.id, Some(devin), 4).is_err());
    }

    #[test]
    fn recovery_digest_matches_stored_payload_for_new_record() {
        let dir = root();
        let base = xcb_core::canonical(dir.path()).unwrap();
        let store = Store::open(&base.join("state")).unwrap();
        let account = store.add_account(Provider::Claude, "Max", 1, None).unwrap();
        let session = store
            .create_session(&account.id, choice(), &base.join("work"), 2)
            .unwrap();
        let expected = session.model.clone();
        let prepared = store.prepare_run(&session.id, session.revision, 3).unwrap();
        let running = orphaned(
            &store,
            &store.mark_spawned(&prepared, i32::MAX as u32).unwrap(),
        );
        let expected_digest = digest(serde_json::to_string(&running).unwrap());
        let (candidate, stored_digest) = store.recovery_candidate(&running.id).unwrap().unwrap();
        assert_eq!(candidate.model, Some(expected.clone()));
        assert_eq!(stored_digest, expected_digest);
        let settled = store.recover_run(&running.id, &stored_digest, 4).unwrap();
        assert_eq!(settled.model, Some(expected));
    }

    #[test]
    fn recovery_digest_matches_stored_payload_for_legacy_record() {
        let dir = root();
        let base = xcb_core::canonical(dir.path()).unwrap();
        let store = Store::open(&base.join("state")).unwrap();
        let account = store.add_account(Provider::Claude, "Max", 1, None).unwrap();
        let session = store
            .create_session(&account.id, choice(), &base.join("work"), 2)
            .unwrap();
        let run_id = Id::new("r_legacy002").unwrap();
        let payload = format!(
            "{{\"id\":\"{}\",\"session\":\"{}\",\"account\":\"{}\",\"revision\":1,\"phase\":\"running\",\"pid\":12345,\"created_at_ms\":1}}",
            run_id, session.id, account.id
        );
        store
            .db()
            .unwrap()
            .execute(
                "INSERT INTO runs(id,session,account,phase,payload) VALUES(?1,?2,?3,'running',?4)",
                params![
                    run_id.as_str(),
                    session.id.as_str(),
                    account.id.as_str(),
                    &payload
                ],
            )
            .unwrap();
        store
            .db()
            .unwrap()
            .execute(
                "INSERT INTO leases(account,run) VALUES(?1,?2)",
                params![account.id.as_str(), run_id.as_str()],
            )
            .unwrap();
        let expected_digest = digest(payload.as_bytes());
        let (candidate, stored_digest) = store.recovery_candidate(&run_id).unwrap().unwrap();
        assert_eq!(candidate.model, None);
        assert_eq!(stored_digest, expected_digest);
        assert!(store.recover_run(&run_id, &stored_digest, 4).is_err());
        assert_eq!(store.unsettled_runs().unwrap().len(), 1);
    }

    #[test]
    fn run_owner_distinguishes_a_live_foreign_run_from_an_unsettled_one() {
        let dir = root();
        let base = xcb_core::canonical(dir.path()).unwrap();
        let path = base.join("state");
        // Two handles on one state root stand in for two terminals.
        let owner = Store::open(&path).unwrap();
        let account = owner.add_account(Provider::Claude, "Max", 1, None).unwrap();
        let session = owner
            .create_session(&account.id, choice(), &base.join("work"), 2)
            .unwrap();
        let run = owner.prepare_run(&session.id, session.revision, 3).unwrap();
        let stamped = run.owner.as_ref().expect("new runs record their owner");
        assert_eq!(stamped.instance, owner.instance());
        assert_eq!(stamped.pid, std::process::id());
        assert!(stamped.alive());

        let viewer = Store::open(&path).unwrap();
        assert_ne!(viewer.instance(), owner.instance());
        // A live run owned elsewhere is remote work, not a recovery candidate.
        assert!(viewer.remote_active(&session.id).unwrap());
        // The owner itself never classifies its own run as remote.
        assert!(!owner.remote_active(&session.id).unwrap());

        // Once the owning process is gone the same row is genuinely unsettled.
        let dead = {
            let mut child = std::process::Command::new("true").spawn().unwrap();
            let pid = child.id();
            child.wait().unwrap();
            pid
        };
        let mut payload: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&run).unwrap()).unwrap();
        payload["owner"]["instance"] = serde_json::Value::String("i_foreign".into());
        payload["owner"]["pid"] = serde_json::Value::from(dead);
        owner
            .db()
            .unwrap()
            .execute(
                "UPDATE runs SET payload=?1 WHERE id=?2",
                params![payload.to_string(), run.id.as_str()],
            )
            .unwrap();
        assert!(!viewer.remote_active(&session.id).unwrap());

        // A legacy row written before owners existed is likewise unsettled.
        payload.as_object_mut().unwrap().remove("owner");
        owner
            .db()
            .unwrap()
            .execute(
                "UPDATE runs SET payload=?1 WHERE id=?2",
                params![payload.to_string(), run.id.as_str()],
            )
            .unwrap();
        assert!(!viewer.remote_active(&session.id).unwrap());
    }

    #[test]
    fn session_receipts_bound_recent_runs_and_effects_without_losing_counts() {
        let dir = root();
        let base = xcb_core::canonical(dir.path()).unwrap();
        let store = Store::open(&base.join("state")).unwrap();
        let account = store
            .add_account(Provider::Claude, "Test", 1, None)
            .unwrap();
        let session = store
            .create_session(&account.id, choice(), &base.join("work"), 2)
            .unwrap();
        let mut latest = None;
        for index in 0..17u64 {
            let current = store.session(&session.id).unwrap().unwrap();
            let run = store
                .prepare_run(&session.id, current.revision, 10 + index)
                .unwrap();
            if index == 16 {
                for effect in 0..257 {
                    store
                        .begin_tool(
                            &run,
                            &format!("call_{effect:03}"),
                            "workspace_native_exec",
                            &format!("digest_{effect}"),
                        )
                        .unwrap();
                }
                latest = Some(run.id.clone());
            } else {
                store.settle(&run, State::Idle, 20 + index).unwrap();
            }
        }
        let receipts = store.session_receipts(&session.id).unwrap();
        assert_eq!(receipts.run_count, 17);
        assert_eq!(receipts.runs.len(), 16);
        assert_eq!(receipts.effect_count, 257);
        assert_eq!(receipts.effects.values().map(Vec::len).sum::<usize>(), 256);
        assert!(receipts.runs[0].lease_held);
        assert!(receipts.runs.iter().skip(1).all(|run| !run.lease_held));
        assert_eq!(
            receipts.effects[&latest.unwrap()].len(),
            256,
            "the bounded effect window stays attached to its exact run"
        );
    }

    #[test]
    fn session_receipts_reject_changed_identity_or_invalid_effect_rows() {
        let dir = root();
        let base = xcb_core::canonical(dir.path()).unwrap();
        let store = Store::open(&base.join("state")).unwrap();
        let account = store
            .add_account(Provider::Claude, "Test", 1, None)
            .unwrap();
        let session = store
            .create_session(&account.id, choice(), &base.join("work"), 2)
            .unwrap();
        let other = store
            .create_session(&account.id, choice(), &base.join("work"), 3)
            .unwrap();
        let run = store.prepare_run(&session.id, session.revision, 4).unwrap();
        store
            .begin_tool(&run, "native-call", "workspace_native_exec", "digest")
            .unwrap();
        store
            .db()
            .unwrap()
            .execute(
                "UPDATE tool_effects SET settled=2 WHERE run=?1",
                [run.id.as_str()],
            )
            .unwrap();
        assert!(store.session_receipts(&session.id).is_err());
        store
            .db()
            .unwrap()
            .execute(
                "UPDATE tool_effects SET settled=1 WHERE run=?1",
                [run.id.as_str()],
            )
            .unwrap();
        let mut forged = run.clone();
        forged.id = Id::new("r_other").unwrap();
        store
            .db()
            .unwrap()
            .execute(
                "UPDATE runs SET payload=?1 WHERE id=?2",
                params![serde_json::to_string(&forged).unwrap(), run.id.as_str()],
            )
            .unwrap();
        assert!(store.session_receipts(&session.id).is_err());
        store
            .db()
            .unwrap()
            .execute(
                "UPDATE runs SET payload=?1 WHERE id=?2",
                params![serde_json::to_string(&run).unwrap(), run.id.as_str()],
            )
            .unwrap();
        store
            .db()
            .unwrap()
            .execute(
                "UPDATE runs SET session=?1 WHERE id=?2",
                params![other.id.as_str(), run.id.as_str()],
            )
            .unwrap();
        assert!(store.session_receipts(&other.id).is_err());
        assert_eq!(store.session_receipts(&session.id).unwrap().run_count, 0);
        store
            .db()
            .unwrap()
            .execute(
                "UPDATE runs SET session=?1,payload='not-json' WHERE id=?2",
                params![session.id.as_str(), run.id.as_str()],
            )
            .unwrap();
        assert!(store.session_receipts(&session.id).is_err());
    }

    fn command_custody(run: &RunRecord) -> crate::command::CommandCustody {
        crate::command::CommandCustody {
            version: 1,
            command_id: Id::new("cmd_synthetic").unwrap(),
            run_id: run.id.clone(),
            workspace_id: "a".repeat(64),
            snapshot_sha256: "b".repeat(64),
            request_sha256: "c".repeat(64),
            backend_sha256: "d".repeat(64),
            boot_id: "00000000-0000-0000-0000-000000000001".into(),
        }
    }

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    #[allow(dead_code)]
    struct CommandlessRunRecord {
        #[serde(default)]
        custody_version: u32,
        id: Id,
        session: Option<Id>,
        account: Id,
        revision: u64,
        phase: String,
        pid: Option<u32>,
        created_at_ms: u64,
        #[serde(default)]
        model: Option<ModelChoice>,
        #[serde(default)]
        owner: Option<RunOwner>,
    }

    #[test]
    fn capability_processes_block_settlement_and_preserve_exact_owner() {
        let dir = root();
        let path = xcb_core::canonical(dir.path()).unwrap().join("state");
        let store = Store::open(&path).unwrap();
        let sibling = Store::open(&path).unwrap();
        let account = store
            .add_account(Provider::Claude, "Test", 1, None)
            .unwrap();
        let run = store.prepare_probe(&account.id, None, 2).unwrap();
        assert!(sibling.mark_capability_starting(&run, "browser").is_err());
        store.mark_capability_starting(&run, "browser").unwrap();
        assert!(store.mark_capability_starting(&run, "browser").is_err());
        assert!(store.mark_capability_spawned(&run, "browser", 0).is_err());
        assert!(store.settle(&run, State::Idle, 3).is_err());
        let marked = store.run(&run.id).unwrap().unwrap();
        assert!(
            serde_json::from_value::<CommandlessRunRecord>(serde_json::to_value(&marked).unwrap())
                .is_err()
        );
        store
            .mark_capability_spawned(&run, "browser", i32::MAX as u32)
            .unwrap();
        let started = store.mark_spawned(&run, i32::MAX as u32).unwrap();
        assert_eq!(
            started.capability_processes.get("browser"),
            Some(&Some(i32::MAX as u32))
        );
        assert!(sibling.clear_capability_custody(&run, "browser").is_err());
        store.clear_capability_custody(&run, "browser").unwrap();
        assert!(store.clear_capability_custody(&run, "browser").is_err());
        store.settle(&run, State::Idle, 4).unwrap();
    }

    #[test]
    fn capability_recovery_requires_each_process_group_to_have_exited() {
        let dir = root();
        let store = Store::open(&xcb_core::canonical(dir.path()).unwrap().join("state")).unwrap();
        let account = store
            .add_account(Provider::Claude, "Test", 1, None)
            .unwrap();
        let run = store.prepare_probe(&account.id, None, 2).unwrap();
        store.mark_capability_starting(&run, "browser").unwrap();
        let running = store.mark_spawned(&run, i32::MAX as u32).unwrap();
        let mut dead = orphaned(&store, &running);
        assert!(dead.verify_recovery_stop().is_err());
        dead.capability_processes
            .insert("browser".into(), Some(i32::MAX as u32));
        dead.verify_recovery_stop().unwrap();
    }

    #[test]
    fn command_marker_survives_stale_spawn_and_blocks_older_readers_and_settle() {
        let dir = root();
        let store = Store::open(&xcb_core::canonical(dir.path()).unwrap().join("state")).unwrap();
        let account = store
            .add_account(Provider::Claude, "Test", 1, None)
            .unwrap();
        let prepared = store.prepare_probe(&account.id, None, 2).unwrap();
        let unmarked = serde_json::to_value(&prepared).unwrap();
        assert!(unmarked.get("command_custody").is_none());
        serde_json::from_value::<CommandlessRunRecord>(unmarked).unwrap();
        let custody = command_custody(&prepared);
        store.record_command_custody(&prepared, &custody).unwrap();
        let marked = store.run(&prepared.id).unwrap().unwrap();
        assert_eq!(marked.command_custody.as_ref(), Some(&custody));
        assert_eq!(marked.phase, "prepared");
        let payload = serde_json::to_value(&marked).unwrap();
        assert_eq!(payload["custody_version"], 1);
        let error = serde_json::from_value::<CommandlessRunRecord>(payload).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("unknown field `command_custody`")
        );
        assert!(store.settle(&prepared, State::Idle, 3).is_err());
        // Provider start still accepts the original prepared handle, preserving
        // the later durable guest marker instead of overwriting with None.
        let spawned = store.mark_spawned(&prepared, i32::MAX as u32).unwrap();
        assert_eq!(spawned.command_custody.as_ref(), Some(&custody));
        assert!(store.settle(&prepared, State::Idle, 4).is_err());
        assert!(store.prepare_probe(&account.id, None, 4).is_err());
        store.clear_command_custody(&prepared, &custody).unwrap();
        assert!(store.prepare_probe(&account.id, None, 4).is_err());
        store.settle(&prepared, State::Idle, 4).unwrap();
        let settled = store.run(&prepared.id).unwrap().unwrap();
        assert_eq!(settled.pid, Some(i32::MAX as u32));
        assert!(settled.command_custody.is_none());
        serde_json::from_value::<CommandlessRunRecord>(serde_json::to_value(&settled).unwrap())
            .unwrap();
        assert!(store.prepare_probe(&account.id, None, 5).is_ok());
    }

    #[test]
    fn command_custody_requires_exact_lease_owner_and_all_bound_fields() {
        let dir = root();
        let path = xcb_core::canonical(dir.path()).unwrap().join("state");
        let store = Store::open(&path).unwrap();
        let sibling = Store::open(&path).unwrap();
        let account = store
            .add_account(Provider::Claude, "Test", 1, None)
            .unwrap();
        let run = store.prepare_probe(&account.id, None, 2).unwrap();
        let custody = command_custody(&run);
        assert!(sibling.record_command_custody(&run, &custody).is_err());
        let mut forged = run.clone();
        forged.revision += 1;
        assert!(store.record_command_custody(&forged, &custody).is_err());
        store.record_command_custody(&run, &custody).unwrap();
        assert!(store.record_command_custody(&run, &custody).is_err());
        assert!(sibling.clear_command_custody(&run, &custody).is_err());
        assert!(sibling.settle(&run, State::Idle, 3).is_err());
        for field in 0..8 {
            let mut other = custody.clone();
            match field {
                0 => other.command_id = Id::new("cmd_other").unwrap(),
                1 => other.run_id = Id::new("r_other").unwrap(),
                2 => other.workspace_id = "0".repeat(64),
                3 => other.snapshot_sha256 = "0".repeat(64),
                4 => other.request_sha256 = "0".repeat(64),
                5 => other.backend_sha256 = "0".repeat(64),
                6 => other.boot_id = "00000000-0000-0000-0000-000000000002".into(),
                7 => other.version = 2,
                _ => unreachable!(),
            }
            assert!(store.clear_command_custody(&run, &other).is_err());
            assert_eq!(
                store
                    .run(&run.id)
                    .unwrap()
                    .unwrap()
                    .command_custody
                    .as_ref(),
                Some(&custody)
            );
        }
        store.clear_command_custody(&run, &custody).unwrap();
        assert!(store.clear_command_custody(&run, &custody).is_err());
        store.settle(&run, State::Idle, 3).unwrap();
        assert!(store.record_command_custody(&run, &custody).is_err());
        assert!(store.clear_command_custody(&run, &custody).is_err());
    }

    #[test]
    fn command_reconciliation_rechecks_stop_digest_and_custody_without_releasing_account() {
        let dir = root();
        let base = xcb_core::canonical(dir.path()).unwrap();
        let store = Store::open(&base.join("state")).unwrap();
        let account = store
            .add_account(Provider::Claude, "Test", 1, None)
            .unwrap();
        let session = store
            .create_session(&account.id, choice(), &base.join("work"), 2)
            .unwrap();
        let prepared = store.prepare_run(&session.id, session.revision, 3).unwrap();
        let run = store.mark_spawned(&prepared, i32::MAX as u32).unwrap();
        let custody = command_custody(&run);
        store.record_command_custody(&run, &custody).unwrap();
        let (live, live_digest) = store.recovery_candidate(&run.id).unwrap().unwrap();
        assert!(
            store
                .reconcile_command_custody(&run.id, &live_digest, &custody)
                .is_err()
        );
        let dead = orphaned(&store, &live);
        let (_, expected) = store.recovery_candidate(&run.id).unwrap().unwrap();
        dead.verify_recovery_stop().unwrap();
        assert!(store.recover_run(&run.id, &expected, 4).is_err());
        assert!(
            store
                .reconcile_command_custody(&run.id, &live_digest, &custody)
                .is_err()
        );
        let mut wrong = custody.clone();
        wrong.request_sha256 = "0".repeat(64);
        assert!(
            store
                .reconcile_command_custody(&run.id, &expected, &wrong)
                .is_err()
        );
        assert_eq!(
            store
                .run(&run.id)
                .unwrap()
                .unwrap()
                .command_custody
                .as_ref(),
            Some(&custody)
        );
        let cleared = store
            .reconcile_command_custody(&run.id, &expected, &custody)
            .unwrap();
        assert!(cleared.command_custody.is_none());
        assert_eq!(cleared.phase, "running");
        assert!(store.prepare_probe(&account.id, None, 4).is_err());
        assert_eq!(
            store.session(&session.id).unwrap().unwrap().state,
            State::Working
        );
        assert!(
            store
                .reconcile_command_custody(&run.id, &expected, &custody)
                .is_err()
        );
        assert!(store.recover_run(&run.id, &expected, 4).is_err());
        let (_, fresh_digest) = store.recovery_candidate(&run.id).unwrap().unwrap();
        store.recover_run(&run.id, &fresh_digest, 4).unwrap();
        assert_eq!(
            store.session(&session.id).unwrap().unwrap().state,
            State::Uncertain
        );
        assert!(store.prepare_probe(&account.id, None, 5).is_ok());
    }

    #[test]
    fn command_custody_rejects_malformed_records_and_absent_lease() {
        let dir = root();
        let store = Store::open(&xcb_core::canonical(dir.path()).unwrap().join("state")).unwrap();
        let account = store
            .add_account(Provider::Claude, "Test", 1, None)
            .unwrap();
        let prepared = store.prepare_probe(&account.id, None, 2).unwrap();
        let run = store.mark_spawned(&prepared, i32::MAX as u32).unwrap();
        let custody = command_custody(&run);
        for field in 0..4 {
            let mut invalid = custody.clone();
            match field {
                0 => invalid.workspace_id = "not-a-hash".into(),
                1 => invalid.run_id = Id::new("r_other").unwrap(),
                2 => invalid.boot_id = "invalid".into(),
                3 => invalid.version = 2,
                _ => unreachable!(),
            }
            assert!(store.record_command_custody(&run, &invalid).is_err());
            let mut malformed = run.clone();
            malformed.command_custody = Some(invalid);
            assert!(malformed.validate().is_err());
        }
        store.record_command_custody(&run, &custody).unwrap();
        let current = store.run(&run.id).unwrap().unwrap();
        let dead = orphaned(&store, &current);
        let (_, expected) = store.recovery_candidate(&run.id).unwrap().unwrap();
        store
            .db()
            .unwrap()
            .execute("DELETE FROM leases WHERE run=?1", [run.id.as_str()])
            .unwrap();
        assert!(
            store
                .reconcile_command_custody(&run.id, &expected, &custody)
                .is_err()
        );
        assert!(store.clear_command_custody(&dead, &custody).is_err());
        assert!(store.settle(&dead, State::Idle, 4).is_err());
        assert_eq!(
            store
                .run(&run.id)
                .unwrap()
                .unwrap()
                .command_custody
                .as_ref(),
            Some(&custody)
        );
    }

    #[test]
    fn native_status_fails_closed_on_changed_run_or_tool_record() {
        let dir = root();
        let base = xcb_core::canonical(dir.path()).unwrap();
        let store = Store::open(&base.join("state")).unwrap();
        let account = store
            .add_account(Provider::Claude, "Test", 1, None)
            .unwrap();
        let session = store
            .create_session(&account.id, choice(), &base.join("work"), 2)
            .unwrap();
        let run = store.prepare_run(&session.id, session.revision, 3).unwrap();
        let page = StatusPage {
            limit: 64,
            offset: 0,
        };

        let mut forged = run.clone();
        forged.id = Id::new("r_forged").unwrap();
        store
            .db()
            .unwrap()
            .execute(
                "UPDATE runs SET payload=?1 WHERE id=?2",
                params![serde_json::to_string(&forged).unwrap(), run.id.as_str()],
            )
            .unwrap();
        assert!(
            store
                .native_status(&StatusFilter::default(), page, 4)
                .is_err()
        );

        store
            .db()
            .unwrap()
            .execute(
                "UPDATE runs SET payload=?1 WHERE id=?2",
                params![serde_json::to_string(&run).unwrap(), run.id.as_str()],
            )
            .unwrap();
        store
            .begin_tool(&run, "call", "operation", "digest")
            .unwrap();
        store
            .db()
            .unwrap()
            .execute(
                "UPDATE tool_effects SET settled=2 WHERE run=?1 AND call='call'",
                [run.id.as_str()],
            )
            .unwrap();
        assert!(
            store
                .native_status(&StatusFilter::default(), page, 4)
                .is_err()
        );
    }

    #[test]
    fn native_status_counts_tool_rows_without_a_parent_run_separately() {
        let dir = root();
        let store = Store::open(&xcb_core::canonical(dir.path()).unwrap().join("state")).unwrap();
        store
            .db()
            .unwrap()
            .execute_batch(
                "PRAGMA foreign_keys=OFF;
                INSERT INTO tool_effects(run,call,operation,input_digest,settled)
                VALUES('r_missing','call','operation','digest',0);
                PRAGMA foreign_keys=ON;",
            )
            .unwrap();
        let status = store
            .native_status(
                &StatusFilter::default(),
                StatusPage {
                    limit: 64,
                    offset: 0,
                },
                1,
            )
            .unwrap();
        assert_eq!(status.totals.tool_effects, 1);
        assert_eq!(status.totals.unlinked_tool_effects, 1);
        assert_eq!(status.totals.unsettled_effects, 1);
        assert_eq!(status.effects.matched, 1);
        assert_eq!(status.effects.records.len(), 1);
        assert_eq!(status.effects.records[0].session, None);
        assert_eq!(status.effects.records[0].account, None);
        assert_eq!(status.effects.records[0].provider, None);
        assert_eq!(status.effects.records[0].run.as_str(), "r_missing");
        let unfinished = store
            .native_status(
                &StatusFilter {
                    unsettled_effects: true,
                    ..StatusFilter::default()
                },
                StatusPage {
                    limit: 64,
                    offset: 0,
                },
                1,
            )
            .unwrap();
        assert_eq!(unfinished.effects.matched, 1);
        let codex = store
            .native_status(
                &StatusFilter {
                    provider: Some(Provider::Codex),
                    ..StatusFilter::default()
                },
                StatusPage {
                    limit: 64,
                    offset: 0,
                },
                1,
            )
            .unwrap();
        assert_eq!(codex.effects.matched, 0);
    }
}

#[cfg(test)]
mod observation_tests {
    use super::*;
    use xcb_core::{
        Provider,
        models::{Mode, ModelChoice},
        usage::VelocitySample,
    };

    fn fixture() -> (tempfile::TempDir, Store, RunRecord, Session) {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir(directory.path().join("work")).unwrap();
        let base = xcb_core::canonical(directory.path()).unwrap();
        let store = Store::open(&base.join("state")).unwrap();
        let account = store
            .add_account(Provider::Codex, "Fixture", 1, None)
            .unwrap();
        let session = store
            .create_session(
                &account.id,
                ModelChoice {
                    provider: Provider::Codex,
                    id: Id::new("gpt-6-astra").unwrap(),
                    label: "Astra".into(),
                    mode: Mode::Fixed,
                    resolved: None,
                    effort: None,
                    observed_at_ms: 1,
                },
                &base.join("work"),
                2,
            )
            .unwrap();
        let run = store.prepare_run(&session.id, session.revision, 4).unwrap();
        (directory, store, run, session)
    }

    #[test]
    fn a_turns_observations_commit_once_not_once_per_event() {
        let (_dir, store, run, session) = fixture();
        // One streamed turn's worth of meters: a baseline, four decimated
        // velocity samples and two quota updates all land in one commit.
        let samples: Vec<VelocitySample> = (0..5)
            .map(|i| VelocitySample {
                at_ms: 1_000 + i * 250,
                output_tokens: 40 + i * 20,
            })
            .collect();
        let observations: Vec<PendingQuota> = ["primary", "secondary"]
            .iter()
            .enumerate()
            .map(|(i, window)| PendingQuota {
                window: Id::new(*window).unwrap(),
                used_percent: 40.0 + i as f64,
                observed_at_ms: 1_000 + i as u64,
                resets_at_ms: 9_999_999,
            })
            .collect();
        store
            .record_observations(&run, &session.id, &samples, &observations)
            .unwrap();
        assert_eq!(
            store
                .observation_commits
                .load(std::sync::atomic::Ordering::SeqCst),
            1,
            "seven stream events must cost one fsync'd commit, not seven"
        );
        // Every buffered observation landed, velocity stayed monotonic and
        // quota points were bound to the leased account's pool at record time.
        let stored = store.velocities(&session.id, 0).unwrap();
        assert_eq!(stored.len(), samples.len());
        assert_eq!(stored.last().unwrap().output_tokens, 120);
        let account = store.account(&run.account).unwrap();
        let quotas = store.quotas(&account.quota_pool).unwrap();
        assert_eq!(quotas.len(), 2);
        assert_eq!(quotas[0].used_percent, 40.0);
        // A second flush with new events is exactly one more commit; the
        // empty flush common at turn end costs none.
        store
            .record_observations(&run, &session.id, &[], &[])
            .unwrap();
        assert_eq!(
            store
                .observation_commits
                .load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        store
            .record_observations(
                &run,
                &session.id,
                &[VelocitySample {
                    at_ms: 2_000,
                    output_tokens: 200,
                }],
                &[],
            )
            .unwrap();
        assert_eq!(
            store
                .observation_commits
                .load(std::sync::atomic::Ordering::SeqCst),
            2
        );
    }

    /// A regressing sample or a quota observation stamped before its run
    /// began (a backward clock step) is dropped, never stored and never an
    /// error: the turn that reported it still settles on its own outcome.
    #[test]
    fn regressed_or_early_telemetry_is_dropped_without_failing_the_turn() {
        let (_dir, store, run, session) = fixture();
        store
            .record_observations(
                &run,
                &session.id,
                &[VelocitySample {
                    at_ms: 1_000,
                    output_tokens: 100,
                }],
                &[],
            )
            .unwrap();
        // A later sample under an earlier counter, and one from before the
        // stored sample, are dropped; the valid sample in the batch lands.
        let regressed = [
            VelocitySample {
                at_ms: 1_500,
                output_tokens: 150,
            },
            VelocitySample {
                at_ms: 1_600,
                output_tokens: 90,
            },
            VelocitySample {
                at_ms: 900,
                output_tokens: 400,
            },
        ];
        store
            .record_observations(&run, &session.id, &regressed, &[])
            .unwrap();
        let stored = store.velocities(&session.id, 0).unwrap();
        assert_eq!(stored.len(), 2);
        assert_eq!(stored.last().unwrap().output_tokens, 150);
        assert_eq!(store.dropped_observations(), 2);
        // A quota observation predating its lease is dropped the same way,
        // and never attributed to the leased account's pool.
        store
            .record_observations(
                &run,
                &session.id,
                &[],
                &[PendingQuota {
                    window: Id::new("primary").unwrap(),
                    used_percent: 50.0,
                    observed_at_ms: 1,
                    resets_at_ms: 9_999_999,
                }],
            )
            .unwrap();
        let account = store.account(&run.account).unwrap();
        assert!(store.quotas(&account.quota_pool).unwrap().is_empty());
        assert_eq!(store.dropped_observations(), 3);
        // Run custody still fails closed: a forged run records nothing.
        let mut forged = run.clone();
        forged.revision += 1;
        assert!(
            store
                .record_observations(
                    &forged,
                    &session.id,
                    &[],
                    &[PendingQuota {
                        window: Id::new("primary").unwrap(),
                        used_percent: 50.0,
                        observed_at_ms: 5_000,
                        resets_at_ms: 9_999_999,
                    }],
                )
                .is_err()
        );
        assert!(store.quotas(&account.quota_pool).unwrap().is_empty());
    }

    /// Two quota events for one window in the same millisecond with
    /// different resets: the first stays, the second is dropped, and the
    /// flush that carried them succeeds — within one batch and across two.
    #[test]
    fn same_instant_quota_conflicts_keep_the_first_row() {
        let (_dir, store, run, session) = fixture();
        let point = |used: f64, reset: u64| PendingQuota {
            window: Id::new("primary").unwrap(),
            used_percent: used,
            observed_at_ms: 5_000,
            resets_at_ms: reset,
        };
        store
            .record_observations(
                &run,
                &session.id,
                &[],
                &[point(40.0, 9_000_000), point(41.0, 9_500_000)],
            )
            .unwrap();
        store
            .record_observations(&run, &session.id, &[], &[point(99.0, 9_900_000)])
            .unwrap();
        let account = store.account(&run.account).unwrap();
        let quotas = store.quotas(&account.quota_pool).unwrap();
        assert_eq!(quotas.len(), 1);
        assert_eq!(quotas[0].used_percent, 40.0);
        assert_eq!(quotas[0].resets_at_ms, 9_000_000);
        assert_eq!(store.dropped_observations(), 2);
        // An identical repeat is not a conflict.
        store
            .record_observations(&run, &session.id, &[], &[point(40.0, 9_000_000)])
            .unwrap();
        assert_eq!(store.dropped_observations(), 2);
    }

    fn concurrent_sessions(
        store: &Store,
        account: &Id,
        workspace: &Path,
        count: usize,
    ) -> Vec<Session> {
        (0..count)
            .map(|index| {
                store
                    .create_session(
                        account,
                        ModelChoice {
                            provider: Provider::Codex,
                            id: Id::new("gpt-6-astra").unwrap(),
                            label: "Astra".into(),
                            mode: Mode::Fixed,
                            resolved: None,
                            effort: None,
                            observed_at_ms: 1,
                        },
                        workspace,
                        index as u64 + 2,
                    )
                    .unwrap()
            })
            .collect()
    }

    /// Above one, the configured account run limit admits concurrent session
    /// runs on the same subscription while probes still hold it alone.
    #[test]
    fn account_run_limit_admits_concurrent_sessions_and_keeps_probes_exclusive() {
        let directory = tempfile::tempdir().unwrap();
        let base = xcb_core::canonical(directory.path()).unwrap();
        let state = base.join("state");
        crate::private::directory(&state).unwrap();
        let workspace = crate::private::directory(&base.join("work")).unwrap();
        let config = crate::config::Config {
            max_runs_per_account: 2,
            ..crate::config::Config::default()
        };
        crate::private::create(
            &state.join("config.json"),
            serde_json::to_string(&config).unwrap().as_bytes(),
        )
        .unwrap();
        let store = Store::open(&state).unwrap();
        let account = store
            .add_account(Provider::Codex, "Fixture", 1, None)
            .unwrap();
        let sessions = concurrent_sessions(&store, &account.id, &workspace, 3);
        let first = store
            .prepare_run(&sessions[0].id, sessions[0].revision, 3)
            .unwrap();
        let second = store
            .prepare_run(&sessions[1].id, sessions[1].revision, 3)
            .unwrap();
        let third = store
            .prepare_run(&sessions[2].id, sessions[2].revision, 3)
            .unwrap_err();
        assert!(
            third.to_string().contains("concurrent run limit"),
            "{third}"
        );
        assert_eq!(store.unsettled_runs().unwrap().len(), 2);
        // A probe is a credential-mutating operation: it is refused while any
        // run is held, no matter the configured limit.
        assert!(store.prepare_probe(&account.id, None, 4).is_err());
        store.settle(&first, State::Idle, 5).unwrap();
        // One run still holds the account: the probe stays refused.
        assert!(store.prepare_probe(&account.id, None, 6).is_err());
        store.settle(&second, State::Idle, 6).unwrap();
        let probe = store.prepare_probe(&account.id, None, 7).unwrap();
        // And while the probe is held, session runs cannot start.
        let fourth = store
            .prepare_run(&sessions[2].id, sessions[2].revision, 8)
            .unwrap_err();
        assert!(fourth.to_string().contains("unsettled probe"), "{fourth}");
        store.settle(&probe, State::Idle, 9).unwrap();
        store
            .prepare_run(&sessions[2].id, sessions[2].revision, 10)
            .unwrap();
        assert_eq!(store.unsettled_runs().unwrap().len(), 1);
    }

    /// The default of one keeps the previous exclusive-account behavior.
    #[test]
    fn default_run_limit_keeps_one_session_run_per_account() {
        let directory = tempfile::tempdir().unwrap();
        let base = xcb_core::canonical(directory.path()).unwrap();
        let state = base.join("state");
        let workspace = crate::private::directory(&base.join("work")).unwrap();
        let store = Store::open(&state).unwrap();
        let account = store
            .add_account(Provider::Codex, "Fixture", 1, None)
            .unwrap();
        let sessions = concurrent_sessions(&store, &account.id, &workspace, 2);
        store
            .prepare_run(&sessions[0].id, sessions[0].revision, 3)
            .unwrap();
        assert!(
            store
                .prepare_run(&sessions[1].id, sessions[1].revision, 4)
                .is_err()
        );
    }

    /// A version-one store rekeys `leases` by run on open, preserving the
    /// held row; schema reads stay (account, run)-scoped either way.
    #[test]
    fn version_two_migration_rekeys_leases_by_run() {
        let directory = tempfile::tempdir().unwrap();
        let base = xcb_core::canonical(directory.path()).unwrap();
        let state = base.join("state");
        let store = Store::open(&state).unwrap();
        let account = store
            .add_account(Provider::Codex, "Fixture", 1, None)
            .unwrap();
        let probe = store.prepare_probe(&account.id, None, 2).unwrap();
        // Rebuild the version-one shape through the store's own connection:
        // the store's file checks admit only files it opened itself.
        store
            .db()
            .unwrap()
            .execute_batch(
                "CREATE TABLE leases_v1(
                account TEXT PRIMARY KEY REFERENCES accounts(id),
                run TEXT NOT NULL UNIQUE REFERENCES runs(id));
                INSERT INTO leases_v1(account,run) SELECT account,run FROM leases;
                DROP TABLE leases;
                ALTER TABLE leases_v1 RENAME TO leases;
                PRAGMA user_version=1;",
            )
            .unwrap();
        drop(store);
        let store = Store::open(&state).unwrap();
        let version: u32 = store
            .db()
            .unwrap()
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, 2);
        // The held row migrated intact and still gates custody.
        let kept: String = store
            .db()
            .unwrap()
            .query_row(
                "SELECT run FROM leases WHERE account=?1",
                [account.id.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(kept, probe.id.as_str());
        let run_pk: bool = store
            .db()
            .unwrap()
            .query_row(
                "SELECT pk FROM pragma_table_info('leases') WHERE name='run'",
                [],
                |row| row.get::<_, u32>(0).map(|pk| pk == 1),
            )
            .unwrap();
        assert!(run_pk);
        assert!(store.prepare_probe(&account.id, None, 3).is_err());
    }
}
