use crate::{Error, Result, digest, new_id, private};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::OpenOptionsExt,
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
const MAX_SESSIONS: i64 = 10_000;
const MAX_MESSAGES: i64 = 10_000;

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

fn quotas_from(db: &Connection, pool: &Id) -> Result<Vec<QuotaPoint>> {
    let mut query =
        db.prepare("SELECT payload FROM quotas WHERE pool=?1 ORDER BY observed_at LIMIT 2049")?;
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

fn blocked_until_from(
    db: &Connection,
    root: &Path,
    account: &Account,
    now: u64,
) -> Result<Option<u64>> {
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
    let points = quotas_from(db, &pool)?;
    if account.provider == Provider::Claude
        && generation_pool(root, account)?.as_ref() != Some(&pool)
    {
        return Err(Error::Conflict("account credential generation changed"));
    }
    Ok(xcb_core::usage::quota_blocked_until(
        &points,
        &pool,
        account.provider,
        now,
    ))
}

fn insert_quota(tx: &Transaction<'_>, point: &QuotaPoint) -> Result<()> {
    point.validate()?;
    let json = serde_json::to_string(point)?;
    let prior: Option<String> = tx
        .query_row(
            "SELECT payload FROM quotas WHERE pool=?1 AND window=?2 AND observed_at=?3",
            params![
                point.pool.as_str(),
                point.window.as_str(),
                sql(point.observed_at_ms)?
            ],
            |row| row.get(0),
        )
        .optional()?;
    if prior.as_ref().is_some_and(|old| old != &json) {
        return Err(Error::Conflict("conflicting quota observation"));
    }
    tx.execute(
        "INSERT OR IGNORE INTO quotas VALUES(?1,?2,?3,?4)",
        params![
            point.pool.as_str(),
            point.window.as_str(),
            sql(point.observed_at_ms)?,
            json
        ],
    )?;
    tx.execute("DELETE FROM quotas WHERE pool=?1 AND window=?2 AND observed_at NOT IN (SELECT observed_at FROM quotas WHERE pool=?1 AND window=?2 ORDER BY observed_at DESC LIMIT 128)", params![point.pool.as_str(), point.window.as_str()])?;
    Ok(())
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
        let Some(pid) = i32::try_from(self.pid)
            .ok()
            .and_then(rustix::process::Pid::from_raw)
        else {
            return false;
        };
        matches!(
            rustix::process::test_kill_process(pid),
            Ok(()) | Err(rustix::io::Errno::PERM)
        )
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
}

impl RunRecord {
    /// Recovery is unavailable while the owning host could still be joining
    /// processes or persisting credentials. Unknown identities fail closed.
    pub fn verify_recovery_stop(&self) -> Result<()> {
        if self.phase != "running" {
            return Err(Error::Conflict("run is not in running phase"));
        }
        let owner = self.owner.as_ref().ok_or(Error::Conflict(
            "run has no recorded owner; recovery custody cannot be proven",
        ))?;
        let owner_pid = i32::try_from(owner.pid)
            .ok()
            .filter(|pid| *pid > 1)
            .and_then(rustix::process::Pid::from_raw)
            .ok_or(Error::Conflict("run owner identity is invalid"))?;
        if rustix::process::test_kill_process(owner_pid) != Err(rustix::io::Errno::SRCH) {
            return Err(Error::Conflict(
                "run owner is still present or its stop is unproven",
            ));
        }
        let pid = self
            .pid
            .filter(|pid| *pid > 1)
            .ok_or(Error::Conflict("run has no valid recorded process group"))?;
        crate::process::prove_process_group_absent(pid)
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
    let hash = |value: &str| {
        value.len() == 64
            && value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    };
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

pub struct Store {
    root: PathBuf,
    /// Unique identity of this open handle — one per terminal process — stamped
    /// on every run this store prepares so other terminals can recognise
    /// foreign-owned live runs.
    instance: String,
    connection: Mutex<Connection>,
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
fn sql(value: u64) -> Result<i64> {
    i64::try_from(value).map_err(|_| xcb_core::Error::Invalid("database integer").into())
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
        if version != 1 {
            return Err(Error::Unavailable(
                "existing xcb database schema is unavailable",
            ));
        }
        Ok(Self {
            root,
            instance: new_id("i").to_string(),
            connection: Mutex::new(connection),
        })
    }

    pub fn open(root: &Path) -> Result<Self> {
        crate::process::initialize_host()?;
        let root = private::directory(root)?;
        let lock_path = root.join(".initialize.lock");
        let initialization = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(
                (rustix::fs::OFlags::NOFOLLOW
                    | rustix::fs::OFlags::NONBLOCK
                    | rustix::fs::OFlags::CLOEXEC)
                    .bits() as i32,
            )
            .open(&lock_path)?;
        private::check_file(&initialization, 0)?;
        private::lock(&initialization)?;
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
        if version > 1 {
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
        Ok(Self {
            root,
            instance: new_id("i").to_string(),
            connection: Mutex::new(connection),
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
    pub fn resolve_account(&self, value: &str) -> Result<Account> {
        let matches: Vec<_> = self
            .accounts()?
            .into_iter()
            .filter(|account| {
                account.id.as_str() == value
                    || account.name() == value
                    // Legacy labels still resolve so stored references keep working.
                    || account.label == value
            })
            .collect();
        if matches.len() != 1 {
            return Err(Error::Unavailable(
                "account not found or name is ambiguous; use its id",
            ));
        }
        Ok(matches.into_iter().next().expect("one account"))
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
        let account = self.account(account_id)?;
        model.validate()?;
        let workspace = workspace.canonicalize()?;
        if !workspace.is_dir()
            || workspace.starts_with(&self.root)
            || self.root.starts_with(&workspace)
            || model.provider != account.provider
            || !account.enabled
        {
            return Err(Error::Conflict("account or workspace unavailable"));
        }
        let session = Session {
            id: new_id("s"),
            account: account_id.clone(),
            model,
            workspace: workspace.to_str().ok_or(Error::PrivateState)?.to_owned(),
            title: "New session".into(),
            pane: Id::new("focus")?,
            state: State::Idle,
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
            let session: Session = decode(&row?)?;
            session.validate()?;
            sessions.push(session);
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
        let mut session = session_from(&tx, id)?.ok_or(Error::Unavailable("session not found"))?;
        if session.revision != expected_revision {
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
        update_session(&tx, &session, expected_revision)?;
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
    pub fn messages(&self, id: &Id, limit: usize) -> Result<Vec<Message>> {
        if !(1..=512).contains(&limit) {
            return Err(xcb_core::Error::Invalid("message page limit").into());
        }
        let db = self.db()?;
        let mut query = db.prepare("SELECT payload FROM (SELECT sequence,payload FROM messages WHERE session=?1 ORDER BY sequence DESC LIMIT ?2) ORDER BY sequence")?;
        let rows = query.query_map(params![id.as_str(), limit as i64], |row| {
            row.get::<_, String>(0)
        })?;
        let mut messages = Vec::new();
        let mut bytes = 0usize;
        for row in rows {
            let row = row?;
            bytes += row.len();
            if bytes > 8 * 1024 * 1024 {
                return Err(xcb_core::Error::Limit("transcript page").into());
            }
            let message: Message = decode(&row)?;
            message.validate()?;
            messages.push(message);
        }
        Ok(messages)
    }
    pub fn prepare_run(
        &self,
        session_id: &Id,
        expected_revision: u64,
        now: u64,
    ) -> Result<RunRecord> {
        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut session =
            session_from(&tx, session_id)?.ok_or(Error::Unavailable("session not found"))?;
        if session.revision != expected_revision {
            return Err(Error::Conflict("session revision changed"));
        }
        let held: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM leases WHERE account=?1)",
            [session.account.as_str()],
            |row| row.get(0),
        )?;
        if held {
            return Err(Error::Conflict(
                "account has an unsettled run; time alone cannot release custody",
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
        tx.execute(
            "INSERT INTO leases VALUES(?1,?2)",
            params![session.account.as_str(), run.id.as_str()],
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
        let held: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM leases WHERE account=?1)",
            [account.as_str()],
            |row| row.get(0),
        )?;
        if held {
            return Err(Error::Conflict("account has an unsettled run"));
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
        };
        tx.execute(
            "INSERT INTO runs VALUES(?1,NULL,?2,'prepared',?3)",
            params![
                run.id.as_str(),
                account.as_str(),
                serde_json::to_string(&run)?
            ],
        )?;
        tx.execute(
            "INSERT INTO leases VALUES(?1,?2)",
            params![account.as_str(), run.id.as_str()],
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
        self.settle_inner(run, state, now, None)
    }

    pub(crate) fn settle_outcome(
        &self,
        run: &RunRecord,
        input: &Id,
        outcome: &crate::runner::Outcome,
        now: u64,
    ) -> Result<()> {
        validate_outcome(outcome)?;
        self.settle_inner(run, outcome.state, now, Some((input, outcome)))
    }

    fn settle_inner(
        &self,
        run: &RunRecord,
        state: State,
        now: u64,
        outcome: Option<(&Id, &crate::runner::Outcome)>,
    ) -> Result<()> {
        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (current, payload) = self.owned_run_from(&tx, run)?;
        if current.command_custody.is_some() {
            return Err(Error::Conflict(
                "command guest stop is unproven; reconcile command custody before settling",
            ));
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
            if let Some((input, outcome)) = outcome {
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
        Ok(Some(record.outcome))
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
        if let Some(managed) = self.existing_managed_store()?
            && managed.has_active_session(id)?
        {
            return Err(Error::Conflict("session belongs to an active managed task"));
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
        if let Some(managed) = self.existing_managed_store()? {
            let mut eligible = Vec::new();
            for id in candidates {
                if !managed.has_active_session(&id)? {
                    eligible.push(id);
                }
            }
            Ok(eligible)
        } else {
            Ok(candidates)
        }
    }

    fn existing_managed_store(&self) -> Result<Option<crate::managed::ManagedStore>> {
        if self.root.join("managed/managed.sqlite").try_exists()? {
            Ok(Some(crate::managed::ManagedStore::open(&self.root)?))
        } else {
            Ok(None)
        }
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
    pub fn models(&self) -> Result<Vec<ModelChoice>> {
        let db = self.db()?;
        let mut query = db.prepare("SELECT payload FROM models ORDER BY provider,id LIMIT 4097")?;
        let rows = query.query_map([], |row| row.get::<_, String>(0))?;
        let mut choices = Vec::new();
        for row in rows {
            let choice: ModelChoice = decode(&row?)?;
            choice.validate()?;
            choices.push(choice);
        }
        if choices.len() > 4096 {
            return Err(xcb_core::Error::Limit("models").into());
        }
        Ok(choices)
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
        insert_quota(&tx, point)?;
        tx.commit()?;
        Ok(())
    }

    /// Derive the destination pool from the exact leased account, never from the
    /// incoming point. Adoption and insertion are one transaction; old/shared
    /// pools and their observations are preserved, not relabeled or copied.
    pub(crate) fn record_account_quota(&self, run: &RunRecord, point: &QuotaPoint) -> Result<()> {
        point.validate()?;
        if point.observed_at_ms < run.created_at_ms {
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
        let mut point = point.clone();
        point.pool = account.quota_pool.clone();
        insert_quota(&tx, &point)?;
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
        if self.db()?.execute(
            "UPDATE tool_effects SET settled=1 WHERE run=?1 AND call=?2 AND settled=0",
            params![run.id.as_str(), call],
        )? != 1
        {
            return Err(Error::Conflict("tool receipt changed"));
        }
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
        let base = directory.path().canonicalize().unwrap();
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
        let base = dir.path().canonicalize().unwrap();
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
        let base = dir.path().canonicalize().unwrap();
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
        let base = dir.path().canonicalize().unwrap();
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
    fn quota_availability_generation_rotation_invalidates_without_copying_history() {
        let dir = root();
        let base = dir.path().canonicalize().unwrap();
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
        let base = dir.path().canonicalize().unwrap();
        let store = Store::open(&base.join("state")).unwrap();
        for provider in Provider::ALL {
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
    fn read_only_discovery_reads_live_wal_without_initializing_or_writing() {
        let dir = root();
        let path = dir.path().canonicalize().unwrap().join("state");
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
    fn read_only_discovery_does_not_create_or_migrate_state() {
        let dir = root();
        let missing = dir.path().canonicalize().unwrap().join("missing");
        assert!(Store::open_read_only(&missing).is_err());
        assert!(!missing.exists());
        let path = dir.path().canonicalize().unwrap().join("state");
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
        let base = dir.path().canonicalize().unwrap();
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

    #[test]
    fn recovery_settles_running_run_and_marks_session_uncertain() {
        let dir = root();
        let base = dir.path().canonicalize().unwrap();
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
        let base = dir.path().canonicalize().unwrap();
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

    #[test]
    fn recovery_rejects_prepared_run_with_no_recorded_pid() {
        let dir = root();
        let base = dir.path().canonicalize().unwrap();
        let store = Store::open(&base.join("state")).unwrap();
        let account = store.add_account(Provider::Claude, "Max", 1, None).unwrap();
        let session = store
            .create_session(&account.id, choice(), &base.join("work"), 2)
            .unwrap();
        let prepared = store.prepare_run(&session.id, session.revision, 3).unwrap();
        let digest = digest(serde_json::to_string(&prepared).unwrap());
        assert!(store.recover_run(&prepared.id, &digest, 4).is_err());
        assert_eq!(store.unsettled_runs().unwrap().len(), 1);
    }

    #[test]
    fn recovery_rejects_already_settled_run() {
        let dir = root();
        let base = dir.path().canonicalize().unwrap();
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
        let base = dir.path().canonicalize().unwrap();
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
        let base = dir.path().canonicalize().unwrap();
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
        let base = dir.path().canonicalize().unwrap();
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
        let base = dir.path().canonicalize().unwrap();
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
        let base = dir.path().canonicalize().unwrap();
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
        let base = dir.path().canonicalize().unwrap();
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
        let base = dir.path().canonicalize().unwrap();
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
        let base = dir.path().canonicalize().unwrap();
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
        let base = dir.path().canonicalize().unwrap();
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
        let base = dir.path().canonicalize().unwrap();
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
        let base = dir.path().canonicalize().unwrap();
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
    fn command_marker_survives_stale_spawn_and_blocks_older_readers_and_settle() {
        let dir = root();
        let store = Store::open(&dir.path().canonicalize().unwrap().join("state")).unwrap();
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
        let path = dir.path().canonicalize().unwrap().join("state");
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
        let base = dir.path().canonicalize().unwrap();
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
        let store = Store::open(&dir.path().canonicalize().unwrap().join("state")).unwrap();
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
}
